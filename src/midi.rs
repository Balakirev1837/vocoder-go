use std::sync::mpsc;

use anyhow::{anyhow, Result};
use midir::{MidiInput, MidiInputConnection};

/// Parsed MIDI events relevant to the vocoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiEvent {
    NoteOn {
        channel: u8,
        note: u8,
        velocity: u8,
    },
    NoteOff {
        channel: u8,
        note: u8,
        velocity: u8,
    },
    ControlChange {
        channel: u8,
        controller: u8,
        value: u8,
    },
    PitchBend {
        channel: u8,
        value: i16,
    },
    Unknown,
}

/// Handle returned by [`start_midi_input`]. Dropping it disconnects the MIDI
/// input port, which also closes the sender side of the channel.
pub struct MidiInputHandle {
    /// The connection must be kept alive for the callback to keep firing.
    #[allow(dead_code)]
    connection: MidiInputConnection<()>,
    /// Receiver end of the channel that the MIDI callback writes into.
    pub receiver: mpsc::Receiver<MidiEvent>,
}

/// Parse a voice-channel MIDI message payload into a [`MidiEvent`].
///
/// `channel` is the 4-bit MIDI channel, `message_type` is the upper nibble
/// of the status byte (e.g. `0x90` for Note On), and `data` contains only
/// the data bytes that follow the status byte.
fn parse_voice_message(channel: u8, message_type: u8, data: &[u8]) -> MidiEvent {
    match message_type {
        0x90 if data.len() >= 2 => {
            let note = data[0];
            let velocity = data[1];
            // A Note On with velocity 0 is equivalent to Note Off.
            if velocity == 0 {
                MidiEvent::NoteOff {
                    channel,
                    note,
                    velocity: 0,
                }
            } else {
                MidiEvent::NoteOn {
                    channel,
                    note,
                    velocity,
                }
            }
        }
        0x80 if data.len() >= 2 => MidiEvent::NoteOff {
            channel,
            note: data[0],
            velocity: data[1],
        },
        0xB0 if data.len() >= 2 => MidiEvent::ControlChange {
            channel,
            controller: data[0],
            value: data[1],
        },
        0xE0 if data.len() >= 2 => {
            let lsb = data[0] as i16;
            let msb = data[1] as i16;
            let value = (msb << 7) | lsb;
            // Centre value is 0x2000 (8192); shift so centre = 0.
            MidiEvent::PitchBend {
                channel,
                value: value - 8192,
            }
        }
        _ => MidiEvent::Unknown,
    }
}

/// Stateful MIDI parser that tracks running status.
///
/// MIDI's "running status" optimisation allows a sender to omit the status
/// byte on consecutive messages of the same type. Without tracking the
/// previous status byte, such messages would be silently dropped.
///
/// ```
/// use vocoder::midi::MidiParser;
/// let mut parser = MidiParser::new();
/// ```
pub struct MidiParser {
    running_status: u8,
}

impl MidiParser {
    /// Create a new parser with no running-status context.
    pub fn new() -> Self {
        Self { running_status: 0 }
    }

    /// Parse a raw MIDI message, using running-status tracking to
    /// reconstruct the full message when the status byte is omitted.
    pub fn parse(&mut self, data: &[u8]) -> MidiEvent {
        if data.is_empty() {
            return MidiEvent::Unknown;
        }

        let first = data[0];

        // Real-time messages (0xF8–0xFF): single byte, don't affect running status.
        if first >= 0xF8 {
            return MidiEvent::Unknown;
        }

        // System common messages (0xF0–0xF7): cancel running status.
        if first >= 0xF0 {
            self.running_status = 0;
            return MidiEvent::Unknown;
        }

        let (status, payload) = if first >= 0x80 {
            // New voice-category status byte — update running status.
            self.running_status = first;
            (first, &data[1..])
        } else {
            // Data byte first — apply running status.
            if self.running_status == 0 {
                return MidiEvent::Unknown;
            }
            (self.running_status, data)
        };

        let channel = status & 0x0F;
        let message_type = status & 0xF0;
        parse_voice_message(channel, message_type, payload)
    }
}

/// Parse a raw 3-byte MIDI message into a [`MidiEvent`].
///
/// This is a stateless convenience wrapper around [`parse_voice_message`].
/// It does **not** handle running status; for that, use [`MidiParser`].
#[cfg(test)]
pub fn parse_midi_message(data: &[u8]) -> MidiEvent {
    if data.is_empty() {
        return MidiEvent::Unknown;
    }

    let status = data[0];
    if status < 0x80 {
        // No status byte and no running-state context — cannot parse.
        return MidiEvent::Unknown;
    }

    // System messages: return Unknown, don't try voice parsing.
    if status >= 0xF0 {
        return MidiEvent::Unknown;
    }

    let channel = status & 0x0F;
    let message_type = status & 0xF0;
    parse_voice_message(channel, message_type, &data[1..])
}

/// List the names of all available MIDI input ports.
///
/// Returns an empty vector when no ports are present.
pub fn list_midi_input_ports() -> Result<Vec<String>> {
    let midi_in = MidiInput::new("vocoder")?;
    let ports = midi_in.ports();
    let mut names = Vec::with_capacity(ports.len());
    for port in ports {
        let name = midi_in
            .port_name(&port)
            .unwrap_or_else(|_| "unknown".to_string());
        names.push(name);
    }
    Ok(names)
}

/// Connect to a MIDI input port and start receiving events.
///
/// If `port_name` is `Some(name)`, the port with the matching name is opened.
/// If `port_name` is `None`, the first available input port is used.
///
/// Events are sent over the channel returned inside [`MidiInputHandle`].
/// Dropping the handle disconnects from the port.
pub fn start_midi_input(port_name: Option<&str>) -> Result<MidiInputHandle> {
    let midi_in = MidiInput::new("vocoder")?;
    let ports = midi_in.ports();

    if ports.is_empty() {
        return Err(anyhow!("No MIDI input ports available"));
    }

    let port = match port_name {
        Some(name) => ports
            .iter()
            .find(|p| midi_in.port_name(p).map(|n| n == name).unwrap_or(false))
            .ok_or_else(|| anyhow!("MIDI input port '{}' not found", name))?,
        None => &ports[0],
    };

    let resolved_name = midi_in
        .port_name(port)
        .unwrap_or_else(|_| "unknown".to_string());

    let (sender, receiver) = mpsc::channel::<MidiEvent>();

    let mut parser = MidiParser::new();
    let connection = midi_in
        .connect(
            port,
            &resolved_name,
            move |_timestamp, data, _| {
                let event = parser.parse(data);
                // Ignore send errors — the receiver may have been dropped.
                let _ = sender.send(event);
            },
            (),
        )
        .map_err(|e| anyhow!("MIDI connection failed: {e}"))?;

    Ok(MidiInputHandle {
        connection,
        receiver,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_note_on() {
        let event = parse_midi_message(&[0x90, 60, 100]);
        assert_eq!(
            event,
            MidiEvent::NoteOn {
                channel: 0,
                note: 60,
                velocity: 100
            }
        );
    }

    #[test]
    fn parse_note_on_velocity_zero_is_note_off() {
        let event = parse_midi_message(&[0x90, 60, 0]);
        assert_eq!(
            event,
            MidiEvent::NoteOff {
                channel: 0,
                note: 60,
                velocity: 0
            }
        );
    }

    #[test]
    fn parse_note_off() {
        let event = parse_midi_message(&[0x82, 64, 0]);
        assert_eq!(
            event,
            MidiEvent::NoteOff {
                channel: 2,
                note: 64,
                velocity: 0
            }
        );
    }

    #[test]
    fn parse_control_change() {
        let event = parse_midi_message(&[0xB1, 7, 120]);
        assert_eq!(
            event,
            MidiEvent::ControlChange {
                channel: 1,
                controller: 7,
                value: 120
            }
        );
    }

    #[test]
    fn parse_pitch_bend_centre() {
        let event = parse_midi_message(&[0xE0, 0x00, 0x40]);
        assert_eq!(
            event,
            MidiEvent::PitchBend {
                channel: 0,
                value: 0
            }
        );
    }

    #[test]
    fn parse_pitch_bend_max() {
        // MSB=0x7F, LSB=0x7F → 16383 - 8192 = 8191
        let event = parse_midi_message(&[0xE0, 0x7F, 0x7F]);
        assert_eq!(
            event,
            MidiEvent::PitchBend {
                channel: 0,
                value: 8191
            }
        );
    }

    #[test]
    fn parse_unknown_empty() {
        let event = parse_midi_message(&[]);
        assert_eq!(event, MidiEvent::Unknown);
    }

    #[test]
    fn parse_unknown_single_byte() {
        let event = parse_midi_message(&[0xFE]);
        assert_eq!(event, MidiEvent::Unknown);
    }

    // --- Running-status tests ---

    #[test]
    fn running_status_note_on_after_status() {
        let mut parser = MidiParser::new();
        // First message sets running status to 0x90 (Note On, channel 0).
        let e1 = parser.parse(&[0x90, 60, 100]);
        assert_eq!(
            e1,
            MidiEvent::NoteOn {
                channel: 0,
                note: 60,
                velocity: 100
            }
        );
        // Second message omits status — running status applies.
        let e2 = parser.parse(&[62, 100]);
        assert_eq!(
            e2,
            MidiEvent::NoteOn {
                channel: 0,
                note: 62,
                velocity: 100
            }
        );
    }

    #[test]
    fn running_status_multiple_notes() {
        let mut parser = MidiParser::new();
        // Note On C4
        parser.parse(&[0x90, 60, 100]);
        // Running status: Note On D4
        let e1 = parser.parse(&[62, 80]);
        // Running status: Note On E4
        let e2 = parser.parse(&[64, 70]);
        assert_eq!(
            e1,
            MidiEvent::NoteOn {
                channel: 0,
                note: 62,
                velocity: 80
            }
        );
        assert_eq!(
            e2,
            MidiEvent::NoteOn {
                channel: 0,
                note: 64,
                velocity: 70
            }
        );
    }

    #[test]
    fn running_status_note_off_velocity_zero() {
        let mut parser = MidiParser::new();
        parser.parse(&[0x90, 60, 100]);
        // Running status with velocity 0 is Note Off.
        let event = parser.parse(&[60, 0]);
        assert_eq!(
            event,
            MidiEvent::NoteOff {
                channel: 0,
                note: 60,
                velocity: 0
            }
        );
    }

    #[test]
    fn running_status_control_change() {
        let mut parser = MidiParser::new();
        parser.parse(&[0xB1, 7, 120]);
        // Running status: CC on channel 1.
        let event = parser.parse(&[10, 64]);
        assert_eq!(
            event,
            MidiEvent::ControlChange {
                channel: 1,
                controller: 10,
                value: 64
            }
        );
    }

    #[test]
    fn running_status_pitch_bend() {
        let mut parser = MidiParser::new();
        parser.parse(&[0xE0, 0x00, 0x40]);
        // Running status: pitch bend with new value.
        let event = parser.parse(&[0x7F, 0x7F]);
        assert_eq!(
            event,
            MidiEvent::PitchBend {
                channel: 0,
                value: 8191
            }
        );
    }

    #[test]
    fn running_status_no_context_returns_unknown() {
        let mut parser = MidiParser::new();
        // No previous status byte set — data bytes alone cannot be parsed.
        let event = parser.parse(&[60, 100]);
        assert_eq!(event, MidiEvent::Unknown);
    }

    #[test]
    fn running_status_new_status_overrides() {
        let mut parser = MidiParser::new();
        parser.parse(&[0x90, 60, 100]); // Running status = 0x90
        parser.parse(&[0x80, 60, 0]); // Running status changes to 0x80
        let event = parser.parse(&[64, 64]); // Should use 0x80 (Note Off)
        assert_eq!(
            event,
            MidiEvent::NoteOff {
                channel: 0,
                note: 64,
                velocity: 64
            }
        );
    }

    #[test]
    fn running_status_system_common_clears() {
        let mut parser = MidiParser::new();
        parser.parse(&[0x90, 60, 100]); // Running status = 0x90
        parser.parse(&[0xF0, 0x01, 0xF7]); // SysEx start clears running status
        let event = parser.parse(&[62, 100]); // No running status — Unknown
        assert_eq!(event, MidiEvent::Unknown);
    }

    #[test]
    fn running_status_real_time_does_not_clear() {
        let mut parser = MidiParser::new();
        parser.parse(&[0x90, 60, 100]); // Running status = 0x90
        let rt = parser.parse(&[0xF8]); // Timing clock — doesn't affect running status
        assert_eq!(rt, MidiEvent::Unknown);
        let event = parser.parse(&[62, 100]); // Running status still active
        assert_eq!(
            event,
            MidiEvent::NoteOn {
                channel: 0,
                note: 62,
                velocity: 100
            }
        );
    }

    // --- Edge cases & boundary values (MID-PA-01 .. MID-PA-11) ---

    #[test]
    fn parse_pitch_bend_min() {
        // MID-PA-01: Pitch bend minimum value.
        let event = parse_midi_message(&[0xE0, 0x00, 0x00]);
        assert_eq!(
            event,
            MidiEvent::PitchBend {
                channel: 0,
                value: -8192
            }
        );
    }

    #[test]
    fn parse_note_on_channel_15() {
        // MID-PA-02
        let event = parse_midi_message(&[0x9F, 60, 100]);
        assert_eq!(
            event,
            MidiEvent::NoteOn {
                channel: 15,
                note: 60,
                velocity: 100
            }
        );
    }

    #[test]
    fn parse_note_off_channel_15() {
        // MID-PA-03
        let event = parse_midi_message(&[0x8F, 60, 0]);
        assert_eq!(
            event,
            MidiEvent::NoteOff {
                channel: 15,
                note: 60,
                velocity: 0
            }
        );
    }

    #[test]
    fn parse_control_change_channel_15() {
        // MID-PA-04
        let event = parse_midi_message(&[0xBF, 7, 127]);
        assert_eq!(
            event,
            MidiEvent::ControlChange {
                channel: 15,
                controller: 7,
                value: 127
            }
        );
    }

    #[test]
    fn parse_pitch_bend_channel_15() {
        // MID-PA-05
        let event = parse_midi_message(&[0xEF, 0x00, 0x40]);
        assert_eq!(
            event,
            MidiEvent::PitchBend {
                channel: 15,
                value: 0
            }
        );
    }

    #[test]
    fn parse_program_change_is_unknown() {
        // MID-PA-06: Two-byte Program Change is not a handled voice message.
        let event = parse_midi_message(&[0xC0, 5]);
        assert_eq!(event, MidiEvent::Unknown);
    }

    #[test]
    fn parse_channel_pressure_is_unknown() {
        // MID-PA-07: Two-byte Channel Pressure is not handled.
        let event = parse_midi_message(&[0xD1, 100]);
        assert_eq!(event, MidiEvent::Unknown);
    }

    #[test]
    fn parse_sysex_start_is_unknown() {
        // MID-PA-08: SysEx start (system message) → Unknown.
        let event = parse_midi_message(&[0xF0, 0x01, 0x02]);
        assert_eq!(event, MidiEvent::Unknown);
    }

    #[test]
    fn parse_realtime_clock_is_unknown() {
        // MID-PA-09: Real-time Clock byte → Unknown.
        let event = parse_midi_message(&[0xF8]);
        assert_eq!(event, MidiEvent::Unknown);
    }

    #[test]
    fn parse_active_sensing_is_unknown() {
        // MID-PA-10: Active Sensing byte → Unknown.
        let event = parse_midi_message(&[0xFE]);
        assert_eq!(event, MidiEvent::Unknown);
    }

    #[test]
    fn parse_song_position_pointer_is_unknown() {
        // MID-PA-11: Song Position Pointer (system common) → Unknown.
        let event = parse_midi_message(&[0xF2, 0x00, 0x00]);
        assert_eq!(event, MidiEvent::Unknown);
    }

    // --- Malformed & corrupted input (MID-PA-12 .. MID-PA-19) ---

    #[test]
    fn parse_corrupted_data_bytes_no_panic() {
        // MID-PA-12: Data bytes >= 0x80 are technically invalid but must not panic.
        let event = parse_midi_message(&[0x90, 0x80, 0x90]);
        // Parser accepts these as-is; just verify no panic and a recognised variant.
        assert!(matches!(event, MidiEvent::NoteOn { .. }));
    }

    #[test]
    fn parse_truncated_voice_message_2_bytes() {
        // MID-PA-13: Only 2 bytes (status + 1 data) → Unknown (need 3 for voice).
        let event = parse_midi_message(&[0x90, 60]);
        assert_eq!(event, MidiEvent::Unknown);
    }

    #[test]
    fn parse_single_status_byte() {
        // MID-PA-14: Only a status byte → Unknown (no data bytes).
        let event = parse_midi_message(&[0x90]);
        assert_eq!(event, MidiEvent::Unknown);
    }

    #[test]
    fn parse_all_zeros() {
        // MID-PA-15: 0x00 is not a recognised status byte → Unknown.
        let event = parse_midi_message(&[0x00, 0x00, 0x00]);
        assert_eq!(event, MidiEvent::Unknown);
    }

    #[test]
    fn parse_note_on_velocity_zero_nonzero_channel() {
        // MID-PA-16: NoteOn velocity 0 on channel 1 → NoteOff.
        let event = parse_midi_message(&[0x91, 64, 0]);
        assert_eq!(
            event,
            MidiEvent::NoteOff {
                channel: 1,
                note: 64,
                velocity: 0
            }
        );
    }

    #[test]
    fn parse_note_on_note_zero() {
        // MID-PA-17: Lowest note number.
        let event = parse_midi_message(&[0x90, 0, 100]);
        assert_eq!(
            event,
            MidiEvent::NoteOn {
                channel: 0,
                note: 0,
                velocity: 100
            }
        );
    }

    #[test]
    fn parse_note_on_note_127() {
        // MID-PA-18: Highest note number.
        let event = parse_midi_message(&[0x90, 127, 100]);
        assert_eq!(
            event,
            MidiEvent::NoteOn {
                channel: 0,
                note: 127,
                velocity: 100
            }
        );
    }

    #[test]
    fn parse_note_on_velocity_127() {
        // MID-PA-19: Maximum velocity.
        let event = parse_midi_message(&[0x90, 60, 127]);
        assert_eq!(
            event,
            MidiEvent::NoteOn {
                channel: 0,
                note: 60,
                velocity: 127
            }
        );
    }

    // --- Property-based tests (MID-PB-01 .. MID-PB-03) ---

    proptest::proptest! {
        /// MID-PB-01: Round-trip channel preservation.
        /// For any valid NoteOn/NoteOff/CC/PitchBend message the parsed channel
        /// must equal `status & 0x0F`.
        #[test]
        fn prop_channel_preservation(
            mt_idx in 0u8..4u8,
            channel in 0u8..=15u8,
            d1 in 0u8..=127u8,
            d2 in 0u8..=127u8,
        ) {
            let msg_type = [0x80u8, 0x90, 0xB0, 0xE0][mt_idx as usize];
            let status = msg_type | channel;
            let event = parse_midi_message(&[status, d1, d2]);
            match event {
                MidiEvent::NoteOn { channel: ch, .. }
                | MidiEvent::NoteOff { channel: ch, .. }
                | MidiEvent::ControlChange { channel: ch, .. }
                | MidiEvent::PitchBend { channel: ch, .. } => {
                    assert_eq!(ch, channel);
                }
                MidiEvent::Unknown => {
                    // Only expected for NoteOn vel==0 edge case that somehow
                    // isn't caught — should not happen for the selected msg types.
                    panic!("Expected recognised event for status 0x{:02X}", status);
                }
            }
        }

        /// MID-PB-02: No-panic guarantee.
        /// `parse_midi_message()` must not panic for any arbitrary byte slice.
        #[test]
        fn prop_no_panic(data in proptest::collection::vec(proptest::num::u8::ANY, 0..=4usize)) {
            let _ = parse_midi_message(&data);
        }

        /// MID-PB-03: Pitch bend range.
        /// For any valid LSB/MSB the resulting value must be in [-8192, 8191].
        #[test]
        fn prop_pitch_bend_range(lsb in 0u8..=127u8, msb in 0u8..=127u8) {
            let event = parse_midi_message(&[0xE0, lsb, msb]);
            match event {
                MidiEvent::PitchBend { value, .. } => {
                    assert!(
                        (-8192..=8191).contains(&value),
                        "value {} out of range for lsb={} msb={}",
                        value, lsb, msb
                    );
                }
                _ => panic!("Expected PitchBend event"),
            }
        }

        /// PROP-05: Valid NoteOn round-trip.
        /// For any NoteOn with velocity > 0 the result is NoteOn with correct fields.
        #[test]
        fn prop_valid_note_on(
            channel in 0u8..=15u8,
            note in 0u8..=127u8,
            velocity in 1u8..=127u8, // vel > 0 guarantees NoteOn
        ) {
            let status = 0x90 | channel;
            let event = parse_midi_message(&[status, note, velocity]);
            assert_eq!(
                event,
                MidiEvent::NoteOn { channel, note, velocity }
            );
        }

        /// PROP-06: Valid NoteOff round-trip.
        #[test]
        fn prop_valid_note_off(
            channel in 0u8..=15u8,
            note in 0u8..=127u8,
            velocity in 0u8..=127u8,
        ) {
            let status = 0x80 | channel;
            let event = parse_midi_message(&[status, note, velocity]);
            assert_eq!(
                event,
                MidiEvent::NoteOff { channel, note, velocity }
            );
        }
    }
}
