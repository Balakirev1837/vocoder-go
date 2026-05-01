#[cfg(feature = "audio")]
mod audio;
#[cfg(feature = "midi")]
mod midi;
#[cfg(feature = "tui")]
mod tui;

#[cfg(feature = "tui")]
use tui::{App, Status};
#[cfg(feature = "audio")]
use vocoder::dsp;

use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use std::sync::Arc;

fn main() -> anyhow::Result<()> {
    // ── Shared state between audio callback, MIDI, and TUI ───────────
    // 255 = no active note (valid MIDI notes are 0..=127).
    let active_note = Arc::new(AtomicU8::new(255));
    let input_level = Arc::new(AtomicU32::new(0.0f32.to_bits()));
    let output_level = Arc::new(AtomicU32::new(0.0f32.to_bits()));

    // ── Start MIDI input ─────────────────────────────────────────────
    #[cfg(feature = "midi")]
    let midi_handle = match midi::start_midi_input() {
        Ok(h) => Some(h),
        Err(e) => {
            eprintln!("MIDI: {}", e);
            None
        }
    };

    #[cfg(feature = "midi")]
    let midi_connected = midi_handle.is_some();
    #[cfg(not(feature = "midi"))]
    let midi_connected = false;

    // ── Start audio I/O with vocoder DSP ─────────────────────────────
    #[cfg(feature = "audio")]
    let audio_result = {
        let active_note = Arc::clone(&active_note);
        let input_level = Arc::clone(&input_level);
        let output_level = Arc::clone(&output_level);

        let mut vocoder = dsp::Vocoder::new(
            20, // bands
            200.0, 8000.0, // freq range
            4.0,    // Q
            0.001, 0.05, // attack / release
            44100.0,
        );
        let mut phase: f64 = 0.0;
        let sample_rate_f64: f64 = 44100.0;

        let config = audio::AudioIoConfig {
            sample_rate: Some(44100),
            buffer_size: Some(512),
        };

        audio::AudioIo::new(&config, move |input_block, output, channels| {
            let ch = channels as usize;
            let mod_samples: &[f32] = input_block
                .and_then(|b| b.get(0))
                .map(|s| s.as_slice())
                .unwrap_or(&[]);

            let note = active_note.load(Ordering::Relaxed);
            let carrier_freq = if note < 128 {
                440.0 * 2.0_f64.powf((note as f64 - 69.0) / 12.0)
            } else {
                0.0
            };

            let mut max_out = 0.0f32;
            let mut max_in = 0.0f32;

            for (i, frame) in output.chunks_mut(ch).enumerate() {
                // Modulator: mic input (channel 0)
                let mod_sample = mod_samples.get(i).copied().unwrap_or(0.0) as f64;
                max_in = max_in.max(mod_sample.abs() as f32);

                // Carrier: sine at the MIDI note frequency
                let carrier_sample = if carrier_freq > 0.0 {
                    let s = phase.sin();
                    phase += 2.0 * std::f64::consts::PI * carrier_freq / sample_rate_f64;
                    if phase >= 2.0 * std::f64::consts::PI {
                        phase -= 2.0 * std::f64::consts::PI;
                    }
                    s
                } else {
                    0.0
                };

                // Vocode: impose modulator spectral envelope onto carrier
                let out = vocoder.process(mod_sample, carrier_sample) as f32;
                max_out = max_out.max(out.abs());

                for sample in frame.iter_mut() {
                    *sample = out;
                }
            }

            input_level.store(max_in.to_bits(), Ordering::Relaxed);
            output_level.store(max_out.to_bits(), Ordering::Relaxed);
        })
    };

    #[cfg(feature = "audio")]
    let audio_running = audio_result.is_ok();
    #[cfg(feature = "audio")]
    let _audio_io = match audio_result {
        Ok(io) => Some(io),
        Err(e) => {
            eprintln!("Audio: {}", e);
            None
        }
    };
    #[cfg(not(feature = "audio"))]
    let audio_running = false;

    // ── Run TUI with live status updates ─────────────────────────────
    #[cfg(feature = "tui")]
    {
        use crossterm::event;
        use crossterm::terminal::{
            disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
        };
        use ratatui::backend::CrosstermBackend;
        use ratatui::Terminal;
        use std::io;
        use std::time::Duration;

        // Set up terminal
        crossterm::execute!(io::stdout(), EnterAlternateScreen)?;
        enable_raw_mode()?;
        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::new(backend)?;
        terminal.clear()?;

        let mut app = App::new().with_status(Status {
            audio_running,
            midi_connected,
            current_note: None,
            cpu_usage: 0.0,
            input_level: 0.0,
            output_level: 0.0,
        });

        while !app.should_quit {
            // Poll MIDI events and update the shared active note
            #[cfg(feature = "midi")]
            if let Some(ref handle) = midi_handle {
                while let Ok(evt) = handle.receiver.try_recv() {
                    match evt {
                        midi::MidiEvent::NoteOn { note, .. } => {
                            active_note.store(note, Ordering::Relaxed);
                        }
                        midi::MidiEvent::NoteOff { note, .. } => {
                            if active_note.load(Ordering::Relaxed) == note {
                                active_note.store(255, Ordering::Relaxed);
                            }
                        }
                        _ => {}
                    }
                }
            }

            // Refresh TUI status from shared state
            let note_u8 = active_note.load(Ordering::Relaxed);
            app.status.audio_running = audio_running;
            app.status.midi_connected = midi_connected;
            app.status.current_note = if note_u8 < 128 {
                Some(midi_note_to_name(note_u8))
            } else {
                None
            };
            app.status.input_level = f32::from_bits(input_level.load(Ordering::Relaxed));
            app.status.output_level = f32::from_bits(output_level.load(Ordering::Relaxed));

            // Render
            terminal.draw(|f| tui::draw(f, &app))?;

            // Handle keyboard input (50 ms timeout keeps the UI responsive)
            if event::poll(Duration::from_millis(50))? {
                let evt = event::read()?;
                app.handle_event(&evt);
            }
        }

        // Restore terminal
        disable_raw_mode()?;
        crossterm::execute!(io::stdout(), LeaveAlternateScreen)?;
    }

    #[cfg(not(feature = "tui"))]
    {
        println!("vocoder: DSP core ready (no TUI)");
        println!("  audio running: {}", audio_running);
        println!("  midi connected: {}", midi_connected);
    }

    Ok(())
}

/// Convert a MIDI note number (0–127) to a human-readable name like "C4" or "A#3".
#[cfg(feature = "tui")]
fn midi_note_to_name(note: u8) -> String {
    let names = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    let name = names[(note % 12) as usize];
    let octave = (note as i32 / 12) - 1;
    format!("{}{}", name, octave)
}
