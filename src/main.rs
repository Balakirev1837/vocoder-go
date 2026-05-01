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

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// Build a new [`audio::AudioIo`] with the vocoder DSP callback bound to the
/// given shared state. Used both at startup and when the user switches devices
/// or changes audio-related config parameters.
#[cfg(feature = "audio")]
fn build_audio_io(
    active_notes: &Arc<Mutex<Vec<u8>>>,
    input_level: &Arc<AtomicU32>,
    output_level: &Arc<AtomicU32>,
    input_device: Option<&str>,
    output_device: Option<&str>,
    sample_rate: u32,
    buffer_size: u32,
    formant_shift: f32,
    gain: &Arc<AtomicU32>,
    pitch_shift: &Arc<AtomicU32>,
) -> anyhow::Result<audio::AudioIo> {
    let active_notes = Arc::clone(active_notes);
    let input_level = Arc::clone(input_level);
    let output_level = Arc::clone(output_level);
    let gain = Arc::clone(gain);
    let pitch_shift = Arc::clone(pitch_shift);

    let sample_rate_f64 = sample_rate as f64;

    // Scale the analysis frequency range by formant_shift, clamped below Nyquist.
    let nyquist = sample_rate_f64 / 2.0;
    let low_freq = 200.0 * formant_shift as f64;
    let high_freq = (8000.0 * formant_shift as f64).min(nyquist * 0.95);

    let mut vocoder = dsp::Vocoder::new(
        20, // bands
        low_freq,
        high_freq,
        4.0, // Q
        0.001,
        0.05, // attack / release
        sample_rate_f64,
    );
    let mut phases = [0.0f64; 128];

    let config = audio::AudioIoConfig {
        sample_rate: Some(sample_rate),
        buffer_size: Some(buffer_size),
    };

    audio::AudioIo::new(
        &config,
        input_device,
        output_device,
        move |input_block, output, channels| {
            let ch = channels as usize;
            let mod_samples: &[f32] = input_block
                .and_then(|b| b.get(0))
                .map(|s| s.as_slice())
                .unwrap_or(&[]);

            // Collect active notes for polyphonic carrier synthesis.
            let notes = active_notes.lock().unwrap().clone();
            let note_count = notes.len();
            let ps = f32::from_bits(pitch_shift.load(Ordering::Relaxed)) as f64;

            // Read gain from shared atomic.
            let g = f32::from_bits(gain.load(Ordering::Relaxed));

            let mut max_out = 0.0f32;
            let mut max_in = 0.0f32;

            for (i, frame) in output.chunks_mut(ch).enumerate() {
                // Modulator: mic input (channel 0)
                let mod_sample = mod_samples.get(i).copied().unwrap_or(0.0) as f64;
                max_in = max_in.max(mod_sample.abs() as f32);

                // Carrier: sum sine waves for all active notes, divide by count to prevent clipping.
                let carrier_sample = if note_count > 0 {
                    let mut sum = 0.0f64;
                    for &note in &notes {
                        let idx = note as usize;
                        sum += phases[idx].sin();
                        let base_freq = 440.0 * 2.0_f64.powf((note as f64 - 69.0) / 12.0);
                        let freq = base_freq * 2.0_f64.powf(ps / 12.0);
                        phases[idx] += 2.0 * std::f64::consts::PI * freq / sample_rate_f64;
                        if phases[idx] >= 2.0 * std::f64::consts::PI {
                            phases[idx] -= 2.0 * std::f64::consts::PI;
                        }
                    }
                    sum / note_count as f64
                } else {
                    0.0
                };

                // Vocode: impose modulator spectral envelope onto carrier, then apply gain.
                let out = (vocoder.process(mod_sample, carrier_sample) as f32) * g;
                max_out = max_out.max(out.abs());

                for sample in frame.iter_mut() {
                    *sample = out;
                }
            }

            input_level.store(max_in.to_bits(), Ordering::Relaxed);
            output_level.store(max_out.to_bits(), Ordering::Relaxed);
        },
    )
}

fn main() -> anyhow::Result<()> {
    // ── Shared state between audio callback, MIDI, and TUI ───────────
    // Polyphonic: Vec of currently active MIDI note numbers (0..=127).
    let active_notes = Arc::new(Mutex::new(Vec::<u8>::new()));
    let input_level = Arc::new(AtomicU32::new(0.0f32.to_bits()));
    let output_level = Arc::new(AtomicU32::new(0.0f32.to_bits()));

    // Shared atomics for real-time adjustable parameters (read in audio callback).
    #[cfg(feature = "tui")]
    let gain = Arc::new(AtomicU32::new(tui::Config::default().gain.to_bits()));
    #[cfg(feature = "tui")]
    let pitch_shift = Arc::new(AtomicU32::new(tui::Config::default().pitch_shift.to_bits()));

    // ── Start MIDI input ─────────────────────────────────────────────
    #[cfg(feature = "midi")]
    let mut midi_handle = match midi::start_midi_input(None) {
        Ok(h) => Some(h),
        Err(e) => {
            eprintln!("MIDI: {}", e);
            None
        }
    };

    #[cfg(feature = "midi")]
    let mut midi_connected = midi_handle.is_some();
    #[cfg(not(feature = "midi"))]
    let mut midi_connected = false;

    // ── Start audio I/O with vocoder DSP ─────────────────────────────
    #[cfg(feature = "audio")]
    let mut _audio_io: Option<audio::AudioIo> = {
        #[cfg(feature = "tui")]
        let (sr, bs, fs) = {
            let cfg = tui::Config::default();
            (cfg.sample_rate, cfg.buffer_size, cfg.formant_shift)
        };
        #[cfg(not(feature = "tui"))]
        let (sr, bs, fs) = (44_100u32, 512u32, 1.0f32);

        #[cfg(feature = "tui")]
        let (g, ps) = (gain.clone(), pitch_shift.clone());
        #[cfg(not(feature = "tui"))]
        let (g, ps) = (
            Arc::new(AtomicU32::new(0.8f32.to_bits())),
            Arc::new(AtomicU32::new(0.0f32.to_bits())),
        );

        match build_audio_io(
            &active_notes,
            &input_level,
            &output_level,
            None,
            None,
            sr,
            bs,
            fs,
            &g,
            &ps,
        ) {
            Ok(io) => Some(io),
            Err(e) => {
                eprintln!("Audio: {}", e);
                None
            }
        }
    };

    #[cfg(feature = "audio")]
    let mut audio_running = _audio_io.is_some();
    #[cfg(not(feature = "audio"))]
    let mut audio_running = false;

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

        // Gather available devices
        #[cfg(feature = "audio")]
        let (audio_inputs, audio_outputs) = {
            let inputs = audio::list_input_devices()
                .map(|ds| ds.into_iter().map(|d| d.name).collect())
                .unwrap_or_default();
            let outputs = audio::list_output_devices()
                .map(|ds| ds.into_iter().map(|d| d.name).collect())
                .unwrap_or_default();
            (inputs, outputs)
        };
        #[cfg(not(feature = "audio"))]
        let (audio_inputs, audio_outputs) = (Vec::new(), Vec::new());

        #[cfg(feature = "midi")]
        let midi_ports = midi::list_midi_input_ports().unwrap_or_default();
        #[cfg(not(feature = "midi"))]
        let midi_ports: Vec<String> = Vec::new();

        let mut app = App::new(audio_inputs, audio_outputs, midi_ports).with_status(Status {
            audio_running,
            midi_connected,
            active_notes: Vec::new(),
            cpu_usage: 0.0,
            input_level: 0.0,
            output_level: 0.0,
        });

        // Track currently active devices and audio config so we can detect changes.
        #[cfg(feature = "audio")]
        let mut prev_audio_input = app.config.audio_input_device.clone();
        #[cfg(feature = "audio")]
        let mut prev_audio_output = app.config.audio_output_device.clone();
        #[cfg(feature = "audio")]
        let mut prev_sample_rate = app.config.sample_rate;
        #[cfg(feature = "audio")]
        let mut prev_buffer_size = app.config.buffer_size;
        #[cfg(feature = "audio")]
        let mut prev_formant_shift = app.config.formant_shift;
        #[cfg(feature = "midi")]
        let mut prev_midi_port = app.config.midi_input_port.clone();

        while !app.should_quit {
            // Poll MIDI events and update the shared active note
            #[cfg(feature = "midi")]
            if let Some(ref handle) = midi_handle {
                let configured_channel = app.config.midi_channel.saturating_sub(1);
                while let Ok(evt) = handle.receiver.try_recv() {
                    match evt {
                        midi::MidiEvent::NoteOn { channel, note, .. }
                            if channel == configured_channel =>
                        {
                            let mut notes = active_notes.lock().unwrap();
                            if !notes.contains(&note) {
                                notes.push(note);
                            }
                        }
                        midi::MidiEvent::NoteOff { channel, note, .. }
                            if channel == configured_channel =>
                        {
                            let mut notes = active_notes.lock().unwrap();
                            notes.retain(|&n| n != note);
                        }
                        _ => {}
                    }
                }
            }

            // Refresh TUI status from shared state
            app.status.audio_running = audio_running;
            app.status.midi_connected = midi_connected;
            {
                let notes = active_notes.lock().unwrap();
                app.status.active_notes = notes.iter().map(|&n| midi_note_to_name(n)).collect();
            }
            app.status.input_level = f32::from_bits(input_level.load(Ordering::Relaxed));
            app.status.output_level = f32::from_bits(output_level.load(Ordering::Relaxed));

            // Push real-time config values into shared atomics for the audio callback.
            #[cfg(feature = "audio")]
            {
                gain.store(app.config.gain.to_bits(), Ordering::Relaxed);
                pitch_shift.store(app.config.pitch_shift.to_bits(), Ordering::Relaxed);
            }

            // Render
            terminal.draw(|f| tui::draw(f, &app))?;

            // Handle keyboard input (50 ms timeout keeps the UI responsive)
            if event::poll(Duration::from_millis(50))? {
                let evt = event::read()?;
                app.handle_event(&evt);
            }

            // ── Detect config changes and restart streams ─────────────
            #[cfg(feature = "audio")]
            {
                let input_changed = app.config.audio_input_device != prev_audio_input;
                let output_changed = app.config.audio_output_device != prev_audio_output;
                let sr_changed = app.config.sample_rate != prev_sample_rate;
                let bs_changed = app.config.buffer_size != prev_buffer_size;
                let fs_changed = app.config.formant_shift != prev_formant_shift;
                if input_changed || output_changed || sr_changed || bs_changed || fs_changed {
                    prev_audio_input = app.config.audio_input_device.clone();
                    prev_audio_output = app.config.audio_output_device.clone();
                    prev_sample_rate = app.config.sample_rate;
                    prev_buffer_size = app.config.buffer_size;
                    prev_formant_shift = app.config.formant_shift;

                    // Drop old stream first
                    _audio_io = None;

                    let input_name = app.config.audio_input_device.as_deref();
                    let output_name = app.config.audio_output_device.as_deref();
                    match build_audio_io(
                        &active_notes,
                        &input_level,
                        &output_level,
                        input_name,
                        output_name,
                        app.config.sample_rate,
                        app.config.buffer_size,
                        app.config.formant_shift,
                        &gain,
                        &pitch_shift,
                    ) {
                        Ok(io) => {
                            _audio_io = Some(io);
                            audio_running = true;
                        }
                        Err(e) => {
                            eprintln!("Audio restart failed: {}", e);
                            audio_running = false;
                        }
                    }
                }
            }

            #[cfg(feature = "midi")]
            {
                if app.config.midi_input_port != prev_midi_port {
                    prev_midi_port = app.config.midi_input_port.clone();

                    // Drop old connection first
                    midi_handle = None;

                    let port_name = app.config.midi_input_port.as_deref();
                    match midi::start_midi_input(port_name) {
                        Ok(h) => {
                            midi_handle = Some(h);
                            midi_connected = true;
                        }
                        Err(e) => {
                            eprintln!("MIDI restart failed: {}", e);
                            midi_connected = false;
                        }
                    }
                }
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
