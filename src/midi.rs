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

/// Parse a raw 3-byte MIDI message into a [`MidiEvent`].
pub fn parse_midi_message(data: &[u8]) -> MidiEvent {
    if data.is_empty() {
        return MidiEvent::Unknown;
    }

    let status = data[0];
    let channel = status & 0x0F;
    let message_type = status & 0xF0;

    match message_type {
        0x90 if data.len() >= 3 => {
            let note = data[1];
            let velocity = data[2];
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
        0x80 if data.len() >= 3 => MidiEvent::NoteOff {
            channel,
            note: data[1],
            velocity: data[2],
        },
        0xB0 if data.len() >= 3 => MidiEvent::ControlChange {
            channel,
            controller: data[1],
            value: data[2],
        },
        0xE0 if data.len() >= 3 => {
            let lsb = data[1] as i16;
            let msb = data[2] as i16;
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

/// Connect to the first available MIDI input port and start receiving events.
///
/// Events are sent over the channel returned inside [`MidiInputHandle`].
/// Dropping the handle disconnects from the port.
pub fn start_midi_input() -> Result<MidiInputHandle> {
    let midi_in = MidiInput::new("vocoder")?;
    let ports = midi_in.ports();

    if ports.is_empty() {
        return Err(anyhow!("No MIDI input ports available"));
    }

    // Use the first available input port.
    let port = &ports[0];
    let port_name = midi_in
        .port_name(port)
        .unwrap_or_else(|_| "unknown".to_string());

    let (sender, receiver) = mpsc::channel::<MidiEvent>();

    let connection = midi_in
        .connect(
            port,
            &port_name,
            move |_timestamp, data, _| {
                let event = parse_midi_message(data);
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
}
