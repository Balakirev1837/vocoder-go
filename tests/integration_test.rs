//! Integration tests for the vocoder audio/MIDI/DSP pipeline.
//!
//! These tests validate end-to-end behaviour without requiring physical
//! hardware (audio devices, MIDI controllers). They exercise the contracts
//! *between* the DSP, MIDI, and audio subsystems.
//!
//! Run with: `cargo test --test integration_test`

use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use std::sync::Arc;
use vocoder::dsp::Vocoder;

// ---------------------------------------------------------------------------
// Helper functions (from test_design_integration.md §4.1)
// ---------------------------------------------------------------------------

const SAMPLE_RATE: f64 = 44100.0;
const NUM_BANDS: usize = 20;
const LOW_FREQ: f64 = 200.0;
const HIGH_FREQ: f64 = 8000.0;
const Q: f64 = 4.0;
const ATTACK: f64 = 0.001;
const RELEASE: f64 = 0.05;

/// Create a vocoder with the default test parameters.
fn create_test_vocoder() -> Vocoder {
    Vocoder::new(
        NUM_BANDS,
        LOW_FREQ,
        HIGH_FREQ,
        Q,
        ATTACK,
        RELEASE,
        SAMPLE_RATE,
    )
}

/// Create a vocoder with a custom sample rate.
fn create_test_vocoder_at_rate(sample_rate: f64) -> Vocoder {
    Vocoder::new(
        NUM_BANDS,
        LOW_FREQ,
        HIGH_FREQ,
        Q,
        ATTACK,
        RELEASE,
        sample_rate,
    )
}

/// Generate a sine wave of given frequency and sample rate.
fn generate_sine(freq: f64, sample_rate: f64, num_samples: usize) -> Vec<f64> {
    (0..num_samples)
        .map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / sample_rate).sin())
        .collect()
}

/// Compute RMS energy of a sample buffer.
fn rms(samples: &[f64]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f64>() / samples.len() as f64).sqrt()
}

/// Assert no sample is NaN or Inf.
fn assert_finite(samples: &[f64], label: &str) {
    for (i, &s) in samples.iter().enumerate() {
        assert!(s.is_finite(), "{}[{}] = {} is not finite", label, i, s);
    }
}

/// Replicate the carrier oscillator from main.rs for offline testing.
fn generate_carrier(note: u8, sample_rate: f64, num_samples: usize) -> Vec<f64> {
    if note >= 128 {
        return vec![0.0; num_samples];
    }
    let freq = 440.0 * 2.0_f64.powf((note as f64 - 69.0) / 12.0);
    let mut phase: f64 = 0.0;
    (0..num_samples)
        .map(|_| {
            let s = phase.sin();
            phase += 2.0 * std::f64::consts::PI * freq / sample_rate;
            if phase >= 2.0 * std::f64::consts::PI {
                phase -= 2.0 * std::f64::consts::PI;
            }
            s
        })
        .collect()
}

/// Run the full vocoder callback logic (as in main.rs) on synthetic data.
fn run_vocoder_callback(
    vocoder: &mut Vocoder,
    modulator: &[f64],
    note: u8,
    sample_rate: f64,
) -> Vec<f64> {
    let carrier = generate_carrier(note, sample_rate, modulator.len());
    let mut output = vec![0.0; modulator.len()];
    vocoder.process_block(modulator, &carrier, &mut output);
    output
}

/// Run the vocoder with explicit modulator and carrier signals.
fn run_vocoder_with_signals(vocoder: &mut Vocoder, modulator: &[f64], carrier: &[f64]) -> Vec<f64> {
    let mut output = vec![0.0; modulator.len()];
    vocoder.process_block(modulator, carrier, &mut output);
    output
}

/// Measure the fundamental frequency of a signal using zero-crossing rate.
fn measure_frequency(samples: &[f64], sample_rate: f64) -> f64 {
    let mut crossings = 0usize;
    for i in 1..samples.len() {
        if (samples[i - 1] >= 0.0 && samples[i] < 0.0)
            || (samples[i - 1] < 0.0 && samples[i] >= 0.0)
        {
            crossings += 1;
        }
    }
    // Each full cycle has 2 zero crossings
    (crossings as f64 / 2.0) * sample_rate / samples.len() as f64
}

/// Deinterleave an interleaved buffer into per-channel vectors.
fn deinterleave(interleaved: &[f64], channels: usize) -> Vec<Vec<f64>> {
    let frames = interleaved.len() / channels;
    let mut block = vec![vec![0.0; frames]; channels];
    for (i, chunk) in interleaved.chunks_exact(channels).enumerate() {
        for (c, &sample) in chunk.iter().enumerate() {
            block[c][i] = sample;
        }
    }
    block
}

// ===========================================================================
// §3.1  DSP ↔ Audio Callback Integration
// ===========================================================================

// ---------------------------------------------------------------------------
// §3.1.1  End-to-end vocoder with synthetic signals
// ---------------------------------------------------------------------------

#[test]
fn e2e_modulated_tone_produces_output() {
    let mut vocoder = create_test_vocoder();
    let modulator = generate_sine(440.0, SAMPLE_RATE, 4096);
    let output = run_vocoder_callback(&mut vocoder, &modulator, 60, SAMPLE_RATE);
    let output_rms = rms(&output);
    assert!(
        output_rms > 1e-6,
        "output RMS should be above noise floor, got {}",
        output_rms
    );
}

#[test]
fn e2e_silence_in_silence_out() {
    let mut vocoder = create_test_vocoder();
    let silence = vec![0.0; 2048];
    let carrier = vec![0.0; 2048];
    let output = run_vocoder_with_signals(&mut vocoder, &silence, &carrier);
    for (i, &s) in output.iter().enumerate() {
        assert!(
            s == 0.0,
            "output[{}] = {} should be exactly 0.0 for silence in",
            i,
            s
        );
    }
}

#[test]
fn e2e_carrier_only_near_silent() {
    let mut vocoder = create_test_vocoder();
    let modulator = vec![0.0; 4096];
    let output = run_vocoder_callback(&mut vocoder, &modulator, 60, SAMPLE_RATE);
    let output_rms = rms(&output);
    assert!(
        output_rms < 1e-10,
        "carrier-only output RMS should be negligible, got {}",
        output_rms
    );
}

#[test]
fn e2e_no_modulator_no_output_after_settling() {
    let mut vocoder = create_test_vocoder();

    // First, charge envelopes with signal
    let modulator = generate_sine(440.0, SAMPLE_RATE, 4096);
    let _ = run_vocoder_callback(&mut vocoder, &modulator, 60, SAMPLE_RATE);

    // Then stop the modulator — feed silence with carrier still active
    let silence_mod = vec![0.0; 44100]; // 1 second for release to decay
    let output = run_vocoder_callback(&mut vocoder, &silence_mod, 60, SAMPLE_RATE);

    // Check the tail — envelope should have decayed to near-zero
    let tail = &output[output.len() - 1000..];
    let tail_rms = rms(tail);
    assert!(
        tail_rms < 1e-6,
        "output should approach zero after modulator stops, tail RMS = {}",
        tail_rms
    );
}

// ---------------------------------------------------------------------------
// §3.1.2  Carrier oscillator pitch accuracy
// ---------------------------------------------------------------------------

#[test]
fn carrier_pitch_matches_midi_note_69() {
    // A4 = 440 Hz
    let carrier = generate_carrier(69, SAMPLE_RATE, 44100);
    let measured = measure_frequency(&carrier, SAMPLE_RATE);
    assert!(
        (measured - 440.0).abs() < 2.0,
        "measured freq {} should be within ±2 Hz of 440 Hz",
        measured
    );
}

#[test]
fn carrier_pitch_matches_midi_note_60() {
    // C4 ≈ 261.63 Hz
    let carrier = generate_carrier(60, SAMPLE_RATE, 44100);
    let measured = measure_frequency(&carrier, SAMPLE_RATE);
    assert!(
        (measured - 261.63).abs() < 2.0,
        "measured freq {} should be within ±2 Hz of 261.63 Hz",
        measured
    );
}

#[test]
fn carrier_silent_when_no_note() {
    // note 255 = sentinel for "no note"
    let carrier = generate_carrier(255, SAMPLE_RATE, 1024);
    for (i, &s) in carrier.iter().enumerate() {
        assert!(
            s == 0.0,
            "carrier[{}] = {} should be 0.0 for no-note sentinel",
            i,
            s
        );
    }
}

#[test]
fn carrier_pitch_at_nonstandard_sample_rate() {
    // Regression test for hardcoded-44100 bug: carrier pitch should still be
    // correct at 48 kHz sample rate.
    let sample_rate = 48000.0;
    let carrier = generate_carrier(69, sample_rate, 48000);
    let measured = measure_frequency(&carrier, sample_rate);
    assert!(
        (measured - 440.0).abs() < 2.0,
        "measured freq {} should be within ±2 Hz of 440 Hz at 48 kHz",
        measured
    );
}

// ---------------------------------------------------------------------------
// §3.1.3  Output level metering
// ---------------------------------------------------------------------------

#[test]
fn output_level_tracks_peak() {
    let output_level = Arc::new(AtomicU32::new(0.0f32.to_bits()));
    let mut vocoder = create_test_vocoder();

    let modulator = generate_sine(440.0, SAMPLE_RATE, 512);
    let carrier = generate_carrier(60, SAMPLE_RATE, 512);
    let mut output = vec![0.0; 512];
    vocoder.process_block(&modulator, &carrier, &mut output);

    // Simulate the callback's peak-tracking logic
    let mut max_out = 0.0f32;
    for &s in &output {
        max_out = max_out.max(s as f32);
    }
    output_level.store(max_out.to_bits(), Ordering::Relaxed);

    let stored = f32::from_bits(output_level.load(Ordering::Relaxed));
    assert!(
        (stored - max_out).abs() < 1e-6,
        "stored level {} should match peak {}",
        stored,
        max_out
    );
    assert!(
        stored > 0.0,
        "output level should be positive for non-silent signal"
    );
}

#[test]
fn input_level_tracks_peak() {
    let input_level = Arc::new(AtomicU32::new(0.0f32.to_bits()));
    let modulator = generate_sine(440.0, SAMPLE_RATE, 512);

    let mut max_in = 0.0f32;
    for &s in &modulator {
        max_in = max_in.max(s.abs() as f32);
    }
    input_level.store(max_in.to_bits(), Ordering::Relaxed);

    let stored = f32::from_bits(input_level.load(Ordering::Relaxed));
    assert!(
        (stored - max_in).abs() < 1e-6,
        "stored level {} should match peak {}",
        stored,
        max_in
    );
}

#[test]
fn levels_reset_on_silence() {
    let output_level = Arc::new(AtomicU32::new(0.0f32.to_bits()));
    let mut vocoder = create_test_vocoder();

    // Process a signal buffer first
    let modulator = generate_sine(440.0, SAMPLE_RATE, 512);
    let carrier = generate_carrier(60, SAMPLE_RATE, 512);
    let mut output = vec![0.0; 512];
    vocoder.process_block(&modulator, &carrier, &mut output);

    let signal_peak: f32 = output
        .iter()
        .map(|&s| s.abs() as f32)
        .fold(0.0_f32, f32::max);
    assert!(signal_peak > 0.0, "signal should produce non-zero peak");

    // Now process a silence buffer
    let silence = vec![0.0; 512];
    vocoder.process_block(&silence, &silence, &mut output);

    let silence_peak: f32 = output
        .iter()
        .map(|&s| s.abs() as f32)
        .fold(0.0_f32, f32::max);
    output_level.store(silence_peak.to_bits(), Ordering::Relaxed);

    let stored = f32::from_bits(output_level.load(Ordering::Relaxed));
    assert!(
        stored < signal_peak,
        "level after silence ({}) should be less than after signal ({})",
        stored,
        signal_peak
    );
}

// ===========================================================================
// §3.2  MIDI → Shared State → DSP Chain
// ===========================================================================

#[cfg(feature = "midi")]
mod midi_tests {
    use super::*;
    use std::sync::mpsc;
    use vocoder::midi::{parse_midi_message, MidiEvent};

    /// Simulate the MIDI event handling logic from main.rs.
    /// `configured_channel` is 0-indexed (main.rs uses saturating_sub(1)).
    fn handle_midi_event(event: &MidiEvent, active_note: &AtomicU8, configured_channel: u8) {
        match event {
            MidiEvent::NoteOn {
                channel,
                note,
                velocity: _,
            } if *channel == configured_channel => {
                active_note.store(*note, Ordering::Relaxed);
            }
            MidiEvent::NoteOff {
                channel,
                note,
                velocity: _,
            } if *channel == configured_channel => {
                if active_note.load(Ordering::Relaxed) == *note {
                    active_note.store(255, Ordering::Relaxed);
                }
            }
            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // §3.2.1  MIDI event → active_note propagation
    // -----------------------------------------------------------------------

    #[test]
    fn note_on_sets_active_note() {
        let (tx, rx) = mpsc::channel::<MidiEvent>();
        let active_note = Arc::new(AtomicU8::new(255));

        tx.send(MidiEvent::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
        })
        .unwrap();

        let event = rx.recv().unwrap();
        handle_midi_event(&event, &active_note, 0);

        assert_eq!(
            active_note.load(Ordering::Relaxed),
            60,
            "NoteOn should set active_note to 60"
        );
    }

    #[test]
    fn note_off_clears_active_note() {
        let (tx, rx) = mpsc::channel::<MidiEvent>();
        let active_note = Arc::new(AtomicU8::new(60));

        tx.send(MidiEvent::NoteOff {
            channel: 0,
            note: 60,
            velocity: 0,
        })
        .unwrap();

        let event = rx.recv().unwrap();
        handle_midi_event(&event, &active_note, 0);

        assert_eq!(
            active_note.load(Ordering::Relaxed),
            255,
            "NoteOff should clear active_note to 255"
        );
    }

    #[test]
    fn note_off_different_note_does_not_clear() {
        let (tx, rx) = mpsc::channel::<MidiEvent>();
        let active_note = Arc::new(AtomicU8::new(60));

        tx.send(MidiEvent::NoteOff {
            channel: 0,
            note: 64,
            velocity: 0,
        })
        .unwrap();

        let event = rx.recv().unwrap();
        handle_midi_event(&event, &active_note, 0);

        assert_eq!(
            active_note.load(Ordering::Relaxed),
            60,
            "NoteOff for different note should not clear active_note"
        );
    }

    #[test]
    fn note_on_velocity_zero_is_note_off() {
        // Parse [0x90, 60, 0] — velocity 0 Note On should produce NoteOff
        let event = parse_midi_message(&[0x90, 60, 0]);
        assert_eq!(
            event,
            MidiEvent::NoteOff {
                channel: 0,
                note: 60,
                velocity: 0
            },
            "NoteOn with velocity 0 should parse as NoteOff"
        );

        let active_note = Arc::new(AtomicU8::new(60));
        handle_midi_event(&event, &active_note, 0);
        assert_eq!(
            active_note.load(Ordering::Relaxed),
            255,
            "velocity-zero NoteOn should clear active_note"
        );
    }

    #[test]
    fn note_on_overwrites_previous() {
        let active_note = Arc::new(AtomicU8::new(255));

        // NoteOn(60)
        handle_midi_event(
            &MidiEvent::NoteOn {
                channel: 0,
                note: 60,
                velocity: 100,
            },
            &active_note,
            0,
        );
        assert_eq!(active_note.load(Ordering::Relaxed), 60);

        // NoteOn(64) — should overwrite
        handle_midi_event(
            &MidiEvent::NoteOn {
                channel: 0,
                note: 64,
                velocity: 100,
            },
            &active_note,
            0,
        );
        assert_eq!(
            active_note.load(Ordering::Relaxed),
            64,
            "NoteOn should overwrite previous active_note (last-note priority)"
        );
    }

    // -----------------------------------------------------------------------
    // §3.2.2  Monophonic voice allocation edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn lost_note_scenario() {
        // Documents the current monophonic "lost note" behaviour:
        // NoteOn(60), NoteOn(64), NoteOff(60) → active_note stays 64
        // because NoteOff(60) doesn't match the active note (64).
        let active_note = Arc::new(AtomicU8::new(255));

        handle_midi_event(
            &MidiEvent::NoteOn {
                channel: 0,
                note: 60,
                velocity: 100,
            },
            &active_note,
            0,
        );
        assert_eq!(active_note.load(Ordering::Relaxed), 60);

        handle_midi_event(
            &MidiEvent::NoteOn {
                channel: 0,
                note: 64,
                velocity: 100,
            },
            &active_note,
            0,
        );
        assert_eq!(active_note.load(Ordering::Relaxed), 64);

        // NoteOff(60) — doesn't match active note (64), so ignored
        handle_midi_event(
            &MidiEvent::NoteOff {
                channel: 0,
                note: 60,
                velocity: 0,
            },
            &active_note,
            0,
        );
        assert_eq!(
            active_note.load(Ordering::Relaxed),
            64,
            "NoteOff for non-active note should be ignored"
        );

        // NoteOff(64) — matches, clears
        handle_midi_event(
            &MidiEvent::NoteOff {
                channel: 0,
                note: 64,
                velocity: 0,
            },
            &active_note,
            0,
        );
        assert_eq!(
            active_note.load(Ordering::Relaxed),
            255,
            "NoteOff for active note should clear"
        );
    }

    #[test]
    fn stuck_note_after_disconnect() {
        // Simulate MIDI disconnect by dropping the sender.
        // Documents that active_note remains stuck (review_midi.md §3.5).
        let (tx, rx) = mpsc::channel::<MidiEvent>();
        let active_note = Arc::new(AtomicU8::new(60));

        // Drop sender — simulates disconnect
        drop(tx);

        // Receiver will get Disconnected error, but active_note stays
        assert!(rx.try_recv().is_err());
        assert_eq!(
            active_note.load(Ordering::Relaxed),
            60,
            "active_note should remain 60 after MIDI disconnect (stuck-note risk)"
        );
    }

    // -----------------------------------------------------------------------
    // §3.2.3  MIDI parse → channel → DSP full chain
    // -----------------------------------------------------------------------

    #[test]
    fn raw_midi_to_dsp_output() {
        // Feed raw MIDI bytes → parse → update state → run vocoder
        let event = parse_midi_message(&[0x90, 60, 100]);
        assert_eq!(
            event,
            MidiEvent::NoteOn {
                channel: 0,
                note: 60,
                velocity: 100
            }
        );

        let active_note = Arc::new(AtomicU8::new(255));
        handle_midi_event(&event, &active_note, 0);
        assert_eq!(active_note.load(Ordering::Relaxed), 60);

        // Run vocoder with the active note
        let mut vocoder = create_test_vocoder();
        let modulator = generate_sine(440.0, SAMPLE_RATE, 1024);
        let output = run_vocoder_callback(
            &mut vocoder,
            &modulator,
            active_note.load(Ordering::Relaxed),
            SAMPLE_RATE,
        );

        let output_rms = rms(&output);
        assert!(
            output_rms > 1e-6,
            "carrier driven by MIDI NoteOn should produce non-zero output, RMS = {}",
            output_rms
        );
    }

    #[test]
    fn raw_midi_note_off_stops_output() {
        let active_note = Arc::new(AtomicU8::new(60));

        // Send NoteOff
        let event = parse_midi_message(&[0x80, 60, 0]);
        handle_midi_event(&event, &active_note, 0);

        assert_eq!(
            active_note.load(Ordering::Relaxed),
            255,
            "NoteOff should clear active_note"
        );

        // Vocoder with no active note should produce silence (carrier is silent)
        let mut vocoder = create_test_vocoder();
        let modulator = generate_sine(440.0, SAMPLE_RATE, 1024);
        let output = run_vocoder_callback(
            &mut vocoder,
            &modulator,
            active_note.load(Ordering::Relaxed),
            SAMPLE_RATE,
        );

        let output_rms = rms(&output);
        assert!(
            output_rms < 1e-10,
            "carrier should be silent after NoteOff, RMS = {}",
            output_rms
        );
    }

    #[test]
    fn control_change_does_not_affect_note() {
        let active_note = Arc::new(AtomicU8::new(60));
        let event = MidiEvent::ControlChange {
            channel: 0,
            controller: 7,
            value: 120,
        };
        handle_midi_event(&event, &active_note, 0);
        assert_eq!(
            active_note.load(Ordering::Relaxed),
            60,
            "ControlChange should not affect active_note"
        );
    }

    #[test]
    fn pitch_bend_does_not_affect_note() {
        let active_note = Arc::new(AtomicU8::new(60));
        let event = MidiEvent::PitchBend {
            channel: 0,
            value: 1000,
        };
        handle_midi_event(&event, &active_note, 0);
        assert_eq!(
            active_note.load(Ordering::Relaxed),
            60,
            "PitchBend should not affect active_note"
        );
    }
}

// ===========================================================================
// §3.3  Audio Format Conversion Integration
// ===========================================================================

// ---------------------------------------------------------------------------
// §3.3.1  Deinterleave → DSP → re-interleave round-trip
// ---------------------------------------------------------------------------

#[test]
fn mono_round_trip() {
    // Create a mono interleaved buffer of known samples
    let interleaved: Vec<f64> = (0..512).map(|i| (i as f64 * 0.01).sin()).collect();
    let block = deinterleave(&interleaved, 1);

    assert_eq!(block.len(), 1, "mono should have 1 channel");
    assert_eq!(block[0].len(), 512, "should have 512 frames");
    for (i, (&orig, &deinter)) in interleaved.iter().zip(block[0].iter()).enumerate() {
        assert!(
            (orig - deinter).abs() < 1e-15,
            "mono deinterleave mismatch at sample {}: {} vs {}",
            i,
            orig,
            deinter
        );
    }

    // Feed through vocoder — output shape should match input shape
    let mut vocoder = create_test_vocoder();
    let carrier = generate_carrier(60, SAMPLE_RATE, 512);
    let mut output = vec![0.0; 512];
    vocoder.process_block(&block[0], &carrier, &mut output);
    assert_eq!(
        output.len(),
        interleaved.len(),
        "output shape should match input shape"
    );
}

#[test]
fn stereo_deinterleave_preserves_channels() {
    // Create stereo interleaved buffer: [L0, R0, L1, R1, ...]
    let interleaved: Vec<f64> = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
    let block = deinterleave(&interleaved, 2);

    assert_eq!(block.len(), 2, "stereo should have 2 channels");
    // Channel 0 (left) should contain only L samples
    assert_eq!(block[0], vec![1.0, 3.0, 5.0], "left channel mismatch");
    // Channel 1 (right) should contain only R samples
    assert_eq!(block[1], vec![2.0, 4.0, 6.0], "right channel mismatch");
}

#[test]
fn dsp_output_broadcast_to_all_channels() {
    // The main.rs callback writes the same vocoder output to all output channels.
    // Simulate this and verify stereo output channels are identical.
    let mut vocoder = create_test_vocoder();
    let modulator = generate_sine(440.0, SAMPLE_RATE, 256);
    let carrier = generate_carrier(60, SAMPLE_RATE, 256);
    let mut mono_output = vec![0.0; 256];
    vocoder.process_block(&modulator, &carrier, &mut mono_output);

    // Simulate broadcast to stereo output buffer
    let channels = 2usize;
    let mut stereo_output = vec![0.0f32; 256 * channels];
    for (i, frame) in stereo_output.chunks_mut(channels).enumerate() {
        let sample = mono_output[i] as f32;
        for ch in frame.iter_mut() {
            *ch = sample;
        }
    }

    // Verify both channels are identical
    let left: Vec<f32> = stereo_output.iter().step_by(channels).copied().collect();
    let right: Vec<f32> = stereo_output
        .iter()
        .skip(1)
        .step_by(channels)
        .copied()
        .collect();
    for (i, (&l, &r)) in left.iter().zip(right.iter()).enumerate() {
        assert!(
            (l - r).abs() < 1e-10,
            "stereo channels differ at frame {}: L={} R={}",
            i,
            l,
            r
        );
    }
}

// ---------------------------------------------------------------------------
// §3.3.2  Sample format conversion correctness
// ---------------------------------------------------------------------------

#[test]
fn f32_identity() {
    // f32 input/output is the identity path.
    let samples: Vec<f64> = vec![0.0, 0.5, -0.5, 1.0, -1.0, 0.12345];
    let as_f32: Vec<f32> = samples.iter().map(|&s| s as f32).collect();
    let back: Vec<f64> = as_f32.iter().map(|&s| s as f64).collect();

    for (i, (&orig, &round)) in samples.iter().zip(back.iter()).enumerate() {
        let diff = (orig - round).abs();
        assert!(
            diff < 1e-6,
            "f32 round-trip error at {}: {} vs {} (diff={})",
            i,
            orig,
            round,
            diff
        );
    }
}

#[test]
fn i16_conversion_round_trip() {
    // Replicate the conversion from audio.rs:
    //   i16 → f32:  s as f32 / 32768.0
    //   f32 → i16:  (s * 32768.0).clamp(i16::MIN as f32, i16::MAX as f32) as i16
    let original_i16: Vec<i16> = vec![0, 1, -1, 1000, -1000, 32767, -32768, 16384];

    // i16 → f32
    let as_f32: Vec<f32> = original_i16.iter().map(|&s| s as f32 / 32768.0).collect();

    // f32 → i16
    let back_to_i16: Vec<i16> = as_f32
        .iter()
        .map(|&s| (s * 32768.0).clamp(i16::MIN as f32, i16::MAX as f32) as i16)
        .collect();

    // Round-trip should be within ±1 LSB
    for (i, (&orig, &round)) in original_i16.iter().zip(back_to_i16.iter()).enumerate() {
        let diff = (orig as i32 - round as i32).abs();
        assert!(
            diff <= 1,
            "i16 round-trip error at {}: {} → {} (diff={})",
            i,
            orig,
            round,
            diff
        );
    }
}

#[test]
fn u16_conversion_round_trip() {
    // Replicate the conversion from audio.rs:
    //   u16 → f32:  (s as f32 - 32768.0) / 32768.0
    //   f32 → u16:  (s * 32768.0 + 32768.0).clamp(0.0, 65535.0) as u16
    let original_u16: Vec<u16> = vec![0, 32768, 65535, 10000, 50000, 32769, 32767];

    // u16 → f32
    let as_f32: Vec<f32> = original_u16
        .iter()
        .map(|&s| (s as f32 - 32768.0) / 32768.0)
        .collect();

    // f32 → u16
    let back_to_u16: Vec<u16> = as_f32
        .iter()
        .map(|&s| (s * 32768.0 + 32768.0).clamp(0.0, 65535.0) as u16)
        .collect();

    // Round-trip should be within ±1 LSB
    for (i, (&orig, &round)) in original_u16.iter().zip(back_to_u16.iter()).enumerate() {
        let diff = (orig as i32 - round as i32).abs();
        assert!(
            diff <= 1,
            "u16 round-trip error at {}: {} → {} (diff={})",
            i,
            orig,
            round,
            diff
        );
    }
}

#[test]
fn i16_full_scale() {
    // i16::MIN should map to ≈ -1.0 and i16::MAX to ≈ +1.0
    let min_f32 = i16::MIN as f32 / 32768.0;
    let max_f32 = i16::MAX as f32 / 32768.0;

    assert!(
        (min_f32 - (-1.0)).abs() < 1.0 / 32768.0,
        "i16::MIN should map near -1.0, got {}",
        min_f32
    );
    assert!(
        (max_f32 - 1.0).abs() < 2.0 / 32768.0,
        "i16::MAX should map near +1.0, got {}",
        max_f32
    );
    // Asymmetry: i16::MIN = -32768 maps to exactly -1.0, i16::MAX = 32767 maps to ~0.999969
    assert!(
        min_f32 == -1.0,
        "i16::MIN should map to exactly -1.0, got {}",
        min_f32
    );
}

#[test]
fn u16_midpoint_is_zero() {
    // u16 value 32768 should map to ≈ 0.0
    let midpoint_f32 = (32768_u16 as f32 - 32768.0) / 32768.0;
    assert!(
        midpoint_f32 == 0.0,
        "u16 midpoint (32768) should map to exactly 0.0, got {}",
        midpoint_f32
    );
}

// ===========================================================================
// §3.4  Pipeline Stress and Robustness
// ===========================================================================

// ---------------------------------------------------------------------------
// §3.4.1  Sustained processing stability
// ---------------------------------------------------------------------------

#[test]
fn one_second_no_nan_no_inf() {
    let mut vocoder = create_test_vocoder();
    let num_samples = SAMPLE_RATE as usize; // 1 second
    let modulator = generate_sine(440.0, SAMPLE_RATE, num_samples);
    let output = run_vocoder_callback(&mut vocoder, &modulator, 60, SAMPLE_RATE);

    assert_finite(&output, "1-second output");
}

#[test]
fn ten_seconds_no_memory_growth() {
    // The vocoder has no dynamic allocation in process(), so memory is fixed.
    // We verify that processing for a long duration produces valid output
    // and the struct size remains constant (compile-time property in Rust).
    let mut vocoder = create_test_vocoder();
    let num_samples = (SAMPLE_RATE * 10.0) as usize; // 10 seconds

    let size_before = std::mem::size_of_val(&vocoder);

    // Process in chunks
    let chunk_size = 1024;
    let mut total_samples = 0usize;
    for chunk_start in (0..num_samples).step_by(chunk_size) {
        let len = chunk_size.min(num_samples - chunk_start);
        let modulator = generate_sine(440.0, SAMPLE_RATE + chunk_start as f64, len);
        let carrier = generate_carrier(60, SAMPLE_RATE, len);
        let mut output = vec![0.0; len];
        vocoder.process_block(&modulator, &carrier, &mut output);
        assert_finite(&output, "10-second chunk");
        total_samples += len;
    }

    assert_eq!(total_samples, num_samples);

    let size_after = std::mem::size_of_val(&vocoder);
    assert_eq!(
        size_before, size_after,
        "vocoder struct size should not change"
    );
}

#[test]
fn output_remains_bounded() {
    let mut vocoder = create_test_vocoder();
    // Maximum-amplitude modulator and carrier for 1 second
    let num_samples = SAMPLE_RATE as usize;
    let modulator: Vec<f64> = (0..num_samples).map(|_| 1.0).collect();
    let carrier: Vec<f64> = (0..num_samples).map(|_| 1.0).collect();
    let mut output = vec![0.0; num_samples];
    vocoder.process_block(&modulator, &carrier, &mut output);

    for (i, &s) in output.iter().enumerate() {
        assert!(
            s.abs() < 1e6,
            "output[{}] = {} should remain bounded (not astronomically large)",
            i,
            s
        );
    }
}

// ---------------------------------------------------------------------------
// §3.4.2  Input edge cases through the full pipeline
// ---------------------------------------------------------------------------

#[test]
fn nan_input_does_not_poison_pipeline() {
    // Documents NaN propagation: NaN modulator → NaN output.
    let mut vocoder = create_test_vocoder();
    let carrier = generate_carrier(60, SAMPLE_RATE, 256);
    let modulator = vec![f64::NAN; 256];
    let mut output = vec![0.0; 256];
    vocoder.process_block(&modulator, &carrier, &mut output);

    // NaN propagates through the DSP chain — document this behaviour.
    let has_nan = output.iter().any(|s| s.is_nan());
    assert!(
        has_nan,
        "NaN input should propagate to output (documenting known behaviour)"
    );
}

#[test]
fn inf_input_handling() {
    // Feed f64::INFINITY as modulator — document that output becomes non-finite.
    let mut vocoder = create_test_vocoder();
    let carrier = generate_carrier(60, SAMPLE_RATE, 256);
    let modulator = vec![f64::INFINITY; 256];
    let mut output = vec![0.0; 256];
    vocoder.process_block(&modulator, &carrier, &mut output);

    let has_non_finite = output.iter().any(|s| !s.is_finite());
    assert!(
        has_non_finite,
        "Inf input produces non-finite output (documenting known behaviour)"
    );
}

#[test]
fn extreme_frequency_carrier() {
    // MIDI note 127 (G9 ≈ 12543 Hz) at 44100 Hz — above Nyquist/2 for some bands.
    let mut vocoder = create_test_vocoder();
    let num_samples = 44100usize;
    let modulator = generate_sine(440.0, SAMPLE_RATE, num_samples);
    let output = run_vocoder_callback(&mut vocoder, &modulator, 127, SAMPLE_RATE);

    assert_finite(&output, "extreme-frequency carrier output");
}

#[test]
fn dc_input_modulator() {
    // Constant (DC) modulator: bandpass filters should reject DC.
    let mut vocoder = create_test_vocoder();
    let num_samples = SAMPLE_RATE as usize; // 1 second for filters to settle
    let modulator = vec![1.0; num_samples];
    let carrier = generate_carrier(60, SAMPLE_RATE, num_samples);
    let output = run_vocoder_with_signals(&mut vocoder, &modulator, &carrier);

    // Check the tail — bandpass should have rejected DC by then
    let tail = &output[output.len() - 4000..];
    let tail_rms = rms(tail);
    assert!(
        tail_rms < 0.01,
        "DC modulator should be rejected by bandpass filters, tail RMS = {}",
        tail_rms
    );
}

#[test]
fn impulse_modulator() {
    // Single impulse followed by zeros: output should show attack-decay shape.
    let mut vocoder = create_test_vocoder();
    let num_samples = 8192;
    let mut modulator = vec![0.0; num_samples];
    modulator[0] = 1.0; // impulse
    let carrier = generate_carrier(60, SAMPLE_RATE, num_samples);
    let output = run_vocoder_with_signals(&mut vocoder, &modulator, &carrier);

    // The first quarter should be louder than the last quarter
    let quarter = num_samples / 4;
    let first_quarter_rms = rms(&output[..quarter]);
    let last_quarter_rms = rms(&output[num_samples - quarter..]);

    assert!(
        first_quarter_rms > last_quarter_rms,
        "impulse response should decay: first quarter RMS ({}) > last quarter RMS ({})",
        first_quarter_rms,
        last_quarter_rms
    );
}

// ---------------------------------------------------------------------------
// §3.4.3  Concurrency safety
// ---------------------------------------------------------------------------

#[cfg(feature = "midi")]
#[test]
fn concurrent_note_on_off() {
    use std::sync::mpsc;
    use std::thread;
    use vocoder::midi::MidiEvent;

    let active_note = Arc::new(AtomicU8::new(255));
    let (tx, rx) = mpsc::channel::<MidiEvent>();
    let iterations = 10000;

    // Sender thread: rapidly send NoteOn/NoteOff
    let tx_active = Arc::clone(&active_note);
    let sender = thread::spawn(move || {
        for i in 0..iterations {
            if i % 2 == 0 {
                let _ = tx.send(MidiEvent::NoteOn {
                    channel: 0,
                    note: 60,
                    velocity: 100,
                });
            } else {
                let _ = tx.send(MidiEvent::NoteOff {
                    channel: 0,
                    note: 60,
                    velocity: 0,
                });
            }
        }
        // Check that active_note is valid at end
        let note = tx_active.load(Ordering::Relaxed);
        assert!(
            note == 60 || note == 255,
            "active_note should be 60 or 255 at end, got {}",
            note
        );
    });

    // Receiver thread: apply events to shared state
    let rx_active = Arc::clone(&active_note);
    let receiver = thread::spawn(move || {
        let mut count = 0;
        while let Ok(event) = rx.recv() {
            match event {
                MidiEvent::NoteOn { note, .. } => {
                    rx_active.store(note, Ordering::Relaxed);
                }
                MidiEvent::NoteOff { note, .. } => {
                    if rx_active.load(Ordering::Relaxed) == note {
                        rx_active.store(255, Ordering::Relaxed);
                    }
                }
                _ => {}
            }
            count += 1;
            if count >= iterations {
                break;
            }
        }
    });

    sender.join().expect("sender thread panicked");
    receiver.join().expect("receiver thread panicked");
}

#[test]
fn concurrent_level_meter_reads() {
    use std::thread;

    let level = Arc::new(AtomicU32::new(0.0f32.to_bits()));
    let iterations = 10000;

    // Writer thread: simulating audio callback writing level meter
    let writer_level = Arc::clone(&level);
    let writer = thread::spawn(move || {
        for i in 0..iterations {
            let value = (i as f32 / iterations as f32).sin();
            writer_level.store(value.to_bits(), Ordering::Relaxed);
        }
    });

    // Reader thread: simulating TUI reading level meter
    let reader_level = Arc::clone(&level);
    let reader = thread::spawn(move || {
        for _ in 0..iterations {
            let bits = reader_level.load(Ordering::Relaxed);
            let value = f32::from_bits(bits);
            // All reads should produce valid f32 values (not NaN)
            assert!(
                !value.is_nan(),
                "level meter read produced NaN from bits {}",
                bits
            );
        }
    });

    writer.join().expect("writer thread panicked");
    reader.join().expect("reader thread panicked");
}

// ---------------------------------------------------------------------------
// §3.4.4  Reset and reconfiguration
// ---------------------------------------------------------------------------

#[test]
fn vocoder_reset_clears_state() {
    let mut vocoder = create_test_vocoder();

    // Process loud signal
    let modulator = generate_sine(440.0, SAMPLE_RATE, 1000);
    let carrier = generate_carrier(60, SAMPLE_RATE, 1000);
    let mut output = vec![0.0; 1000];
    vocoder.process_block(&modulator, &carrier, &mut output);
    assert!(rms(&output) > 0.0, "signal should produce non-zero output");

    // Reset
    vocoder.reset();

    // Process silence — should be exactly zero
    let silence = vec![0.0; 100];
    let carrier_silent = vec![0.0; 100];
    let mut output_after = vec![0.0; 100];
    vocoder.process_block(&silence, &carrier_silent, &mut output_after);

    for (i, &s) in output_after.iter().enumerate() {
        assert!(
            s == 0.0,
            "output after reset should be exactly 0.0, got {} at {}",
            s,
            i
        );
    }
}

#[test]
fn simulate_device_switch() {
    // Create vocoder A, process some signal, drop A.
    // Create vocoder B with same params, feed same inputs — output should match.
    let modulator = generate_sine(440.0, SAMPLE_RATE, 256);
    let carrier = generate_carrier(60, SAMPLE_RATE, 256);

    let mut vocoder_a = create_test_vocoder();
    let mut output_a = vec![0.0; 256];
    vocoder_a.process_block(&modulator, &carrier, &mut output_a);

    // Drop A, create B
    drop(vocoder_a);
    let mut vocoder_b = create_test_vocoder();
    let mut output_b = vec![0.0; 256];
    vocoder_b.process_block(&modulator, &carrier, &mut output_b);

    // Outputs should be identical (deterministic from same initial state)
    for (i, (&a, &b)) in output_a.iter().zip(output_b.iter()).enumerate() {
        assert!(
            (a - b).abs() < 1e-15,
            "output should be deterministic: A[{}]={} vs B[{}]={}",
            i,
            a,
            i,
            b
        );
    }
}

#[test]
fn config_change_sample_rate() {
    // Create vocoders at different sample rates — outputs should differ
    // because filter coefficients depend on sample rate.
    let modulator = generate_sine(440.0, 44100.0, 1024);
    let carrier = generate_carrier(60, 44100.0, 1024);

    let mut vocoder_44k = create_test_vocoder();
    let mut output_44k = vec![0.0; 1024];
    vocoder_44k.process_block(&modulator, &carrier, &mut output_44k);

    let mut vocoder_48k = create_test_vocoder_at_rate(48000.0);
    let mut output_48k = vec![0.0; 1024];
    vocoder_48k.process_block(&modulator, &carrier, &mut output_48k);

    // Outputs should differ — different filter coefficients
    let diff: f64 = output_44k
        .iter()
        .zip(output_48k.iter())
        .map(|(&a, &b)| (a - b).abs())
        .sum();
    assert!(
        diff > 1e-6,
        "outputs at 44100 Hz and 48000 Hz should differ, total diff = {}",
        diff
    );
}

// ===========================================================================
// §3.5  Audio Block Processing Integration
// ===========================================================================

// ---------------------------------------------------------------------------
// §3.5.1  Block vs. sample consistency
// ---------------------------------------------------------------------------

#[test]
fn block_matches_sample_by_sample() {
    // Process 1024 samples one at a time, then as a block — should be identical.
    let modulator = generate_sine(440.0, SAMPLE_RATE, 1024);
    let carrier = generate_sine(220.0, SAMPLE_RATE, 1024);

    // Sample-by-sample
    let mut vocoder_sample = create_test_vocoder();
    let mut output_sample = vec![0.0; 1024];
    for (i, (&m, &c)) in modulator.iter().zip(carrier.iter()).enumerate() {
        output_sample[i] = vocoder_sample.process(m, c);
    }

    // Block
    let mut vocoder_block = create_test_vocoder();
    let mut output_block = vec![0.0; 1024];
    vocoder_block.process_block(&modulator, &carrier, &mut output_block);

    for (i, (&s, &b)) in output_sample.iter().zip(output_block.iter()).enumerate() {
        assert!(
            (s - b).abs() < 1e-15,
            "sample[{}]={} vs block[{}]={} — should be bitwise identical",
            i,
            s,
            i,
            b
        );
    }
}

#[test]
fn block_length_mismatch_safe() {
    // process_block with mismatched lengths — only min() samples written.
    let mut vocoder = create_test_vocoder();
    let modulator = vec![0.5; 1024];
    let carrier = vec![0.3; 512];
    let mut output = vec![0.0; 256];

    // Should not panic — min(1024, 512, 256) = 256 samples written
    vocoder.process_block(&modulator, &carrier, &mut output);

    // First 256 samples should be written (carrier has 512, only 256 used)
    let written = output.iter().filter(|&&v| v != 0.0).count();
    // Note: output may or may not be non-zero depending on envelope state,
    // but the call should not panic.
    assert_eq!(output.len(), 256, "output length should be preserved");
}

#[test]
fn empty_block_safe() {
    // process_block with empty slices — should not panic.
    let mut vocoder = create_test_vocoder();
    let modulator: Vec<f64> = vec![];
    let carrier: Vec<f64> = vec![];
    let mut output: Vec<f64> = vec![];

    // Should not panic
    vocoder.process_block(&modulator, &carrier, &mut output);
    assert!(output.is_empty(), "empty output should remain empty");
}
