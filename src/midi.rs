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
        // MSB=0x7F, LSB=0x7F → 16256 - 8192 = 8064
        let event = parse_midi_message(&[0xE0, 0x7F, 0x7F]);
        assert_eq!(
            event,
            MidiEvent::PitchBend {
                channel: 0,
                value: 8064
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
                value: 8064
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
}
