# Integration Test Design: Audio / MIDI / DSP Pipeline

**Author:** Critter agent  
**Date:** 2026-04-30  
**Scope:** End-to-end and cross-module integration tests for the vocoder pipeline  
**Depends on:** Code reviews `review_dsp.md`, `review_audio.md`, `review_midi.md`, `review_main_tui.md`

---

## 1. Purpose

Integration tests validate that the vocoder's three major subsystems — DSP processing (`dsp.rs`), audio I/O (`audio.rs`), and MIDI input (`midi.rs`) — work correctly when composed together as they are in `main.rs`. Unit tests (which already exist in each module) verify individual components in isolation. Integration tests verify the contracts *between* components: correct data flow, correct sample format handling, correct state propagation, and correct behaviour under realistic operating conditions.

---

## 2. Test Architecture

### 2.1 Why "offline" integration tests

The full pipeline requires hardware (audio devices, MIDI controllers) and a terminal (TUI). Integration tests must be **automatable in CI** without hardware. The strategy is:

1. **Test the DSP + audio callback logic offline.** Extract the core callback closure from `build_audio_io` into a testable function or replicate its logic in tests. Feed synthetic audio buffers and MIDI events, assert on output.
2. **Test the MIDI → shared-state → DSP chain.** Simulate MIDI events flowing through `mpsc::channel`, updating `AtomicU8` state, and verify the carrier oscillator responds correctly.
3. **Test the audio I/O plumbing without real devices.** Use `cpal`'s null/host APIs or mock the stream layer to verify channel wiring, deinterleaving, and format conversion.
4. **Skip TUI rendering tests.** The TUI is a thin view layer; its integration with shared atomics is trivially correct. Testing it requires a real terminal emulator, which is out of scope.

### 2.2 Test module layout

```
tests/
  integration/
    mod.rs                 -- test harness, helpers
    dsp_audio_chain.rs     -- DSP + audio callback integration
    midi_state_chain.rs    -- MIDI → shared state → DSP chain
    audio_format.rs        -- Sample format conversion round-trips
    pipeline_stress.rs     -- Long-running / edge-case stress tests
```

All tests run under `cargo test --test integration` (no feature flags needed for DSP/MIDI tests; audio I/O tests can be feature-gated).

---

## 3. Test Categories

### 3.1 DSP ↔ Audio Callback Integration

These tests validate that the `Vocoder` behaves correctly when driven with the same callback logic used in `main.rs::build_audio_io`.

#### 3.1.1 End-to-end vocoder with synthetic signals

| Test ID | Name | Description |
|---------|------|-------------|
| I-3.1.1a | `e2e_modulated_tone_produces_output` | Feed a 440 Hz sine modulator and a 220 Hz sine carrier into the vocoder callback for 4096 samples. Assert output RMS is above a noise floor (e.g., > 1e-6). |
| I-3.1.1b | `e2e_silence_in_silence_out` | Feed zero modulator and zero carrier for 2048 samples. Assert all output samples are exactly 0.0. |
| I-3.1.1c | `e2e_carrier_only_near_silent` | Feed zero modulator with an active sine carrier for 4096 samples. Assert output energy is negligible (< 1e-10 RMS) because envelope followers start at zero and no modulator energy excites them. |
| I-3.1.1d | `e2e_no_modulator_no_output_after_settling` | After initial signal, stop the modulator (feed 0.0) and keep the carrier. After enough samples for the release envelope to decay, assert output approaches zero. |

**Rationale:** These four tests together verify the core vocoder contract: output energy is proportional to modulator energy.

#### 3.1.2 Carrier oscillator pitch accuracy

| Test ID | Name | Description |
|---------|------|-------------|
| I-3.1.2a | `carrier_pitch_matches_midi_note_69` | Set `active_note` to 69 (A4 = 440 Hz). Run the carrier oscillator logic for a large buffer. Measure zero-crossing rate or FFT peak. Assert fundamental is within ±2 Hz of 440 Hz at 44100 Hz sample rate. |
| I-3.1.2b | `carrier_pitch_matches_midi_note_60` | Same for MIDI note 60 (C4 ≈ 261.63 Hz). Assert within ±2 Hz. |
| I-3.1.2c | `carrier_silent_when_no_note` | Set `active_note` to 255 (sentinel). Assert carrier samples are all 0.0. |
| I-3.1.2d | `carrier_pitch_at_nonstandard_sample_rate` | Repeat test I-3.1.2a with sample rate 48000 Hz. This exposes the hardcoded 44100 Hz bug identified in `review_main_tui.md` §1 (carrier pitch will be wrong until the fix lands). |

**Rationale:** The carrier oscillator is the bridge between MIDI state and DSP. Its pitch accuracy directly determines musical correctness. Test I-3.1.2d acts as a regression test for the known hardcoded sample-rate bug.

#### 3.1.3 Output level metering

| Test ID | Name | Description |
|---------|------|-------------|
| I-3.1.3a | `output_level_tracks_peak` | Simulate the audio callback logic with a known-amplitude signal. Verify the `AtomicU32` level meter is updated to the correct peak value (stored as `f32::to_bits`). |
| I-3.1.3b | `input_level_tracks_peak` | Same for the input (modulator) level meter. |
| I-3.1.3c | `levels_reset_on_silence` | After processing signal, feed silence for enough samples. Verify level meters decay toward zero (or exactly zero if the callback computes per-buffer peak). |

**Rationale:** Level meters are the user's primary feedback mechanism. The `f32::to_bits()` / `f32::from_bits()` pattern through `AtomicU32` is correct but unusual; it deserves explicit testing.

---

### 3.2 MIDI → Shared State → DSP Chain

These tests validate that MIDI events correctly propagate through the channel, update shared atomics, and affect DSP output.

#### 3.2.1 MIDI event → active_note propagation

| Test ID | Name | Description |
|---------|------|-------------|
| I-3.2.1a | `note_on_sets_active_note` | Send `MidiEvent::NoteOn { channel: 0, note: 60, velocity: 100 }` through an `mpsc::channel`. Receive it and store to `AtomicU8`. Assert `active_note.load() == 60`. |
| I-3.2.1b | `note_off_clears_active_note` | Set `active_note` to 60. Send `MidiEvent::NoteOff { channel: 0, note: 60, velocity: 0 }`. Assert `active_note.load() == 255`. |
| I-3.2.1c | `note_off_different_note_does_not_clear` | Set `active_note` to 60. Send `NoteOff { note: 64, .. }`. Assert `active_note` is still 60. |
| I-3.2.1d | `note_on_velocity_zero_is_note_off` | Verify that `parse_midi_message(&[0x90, 60, 0])` produces `NoteOff`, and that it correctly clears `active_note` when it matches. |
| I-3.2.1e | `note_on_overwrites_previous` | Send NoteOn(60), then NoteOn(64). Assert `active_note == 64`. This validates last-note priority. |

**Rationale:** These tests replicate the exact logic from `main.rs:196-208` to ensure the MIDI-to-state pipeline is correct in isolation.

#### 3.2.2 Monophonic voice allocation edge cases

| Test ID | Name | Description |
|---------|------|-------------|
| I-3.2.2a | `lost_note_scenario` | Send NoteOn(60), NoteOn(64), NoteOff(60). Assert `active_note == 64` (not 255). Then send NoteOff(64). Assert `active_note == 255`. This documents the current "lost note" behaviour where NoteOff(60) doesn't match the active note (64), so it's ignored. |
| I-3.2.2b | `stuck_note_after_disconnect` | Set `active_note = 60`. Simulate MIDI disconnect (drop the sender). Assert `active_note` remains 60 forever — documenting the stuck-note risk from `review_midi.md` §3.5. |

**Rationale:** These tests document the current monophonic voice allocation behaviour and serve as regression guards if/when a note stack is implemented.

#### 3.2.3 MIDI parse → channel → DSP full chain

| Test ID | Name | Description |
|---------|------|-------------|
| I-3.2.3a | `raw_midi_to_dsp_output` | Feed raw MIDI bytes `[0x90, 60, 100]` to `parse_midi_message`. Send the resulting `MidiEvent` through a channel. Receive, update `AtomicU8`. Run the vocoder callback with this active note. Assert output is non-zero (carrier is active). |
| I-3.2.3b | `raw_midi_note_off_stops_output` | Same as above, then send `[0x80, 60, 0]`. After processing, assert the carrier is silent (all zeros) and `active_note == 255`. |
| I-3.2.3c | `control_change_does_not_affect_note` | Send `MidiEvent::ControlChange`. Assert `active_note` is unchanged. |
| I-3.2.3d | `pitch_bend_does_not_affect_note` | Send `MidiEvent::PitchBend`. Assert `active_note` is unchanged. |

**Rationale:** These are true end-to-end tests covering all three modules in sequence: raw bytes → parsed event → shared state → DSP output.

---

### 3.3 Audio Format Conversion Integration

These tests validate the sample format handling paths in `audio.rs`.

#### 3.3.1 Deinterleave → DSP → re-interleave round-trip

| Test ID | Name | Description |
|---------|------|-------------|
| I-3.3.1a | `mono_round_trip` | Create a mono interleaved buffer of known samples. Deinterleave (1 channel). Feed through the vocoder. Assert output shape matches input shape. |
| I-3.3.1b | `stereo_deinterleave_preserves_channels` | Create a stereo interleaved buffer `[L0, R0, L1, R1, ...]`. Deinterleave. Assert `block[0]` contains only L samples and `block[1]` contains only R samples. |
| I-3.3.1c | `dsp_output_broadcast_to_all_channels` | The `main.rs` callback writes the same vocoder output to all output channels. Replicate this logic and verify that for a stereo output buffer, both channels receive identical samples. |

#### 3.3.2 Sample format conversion correctness

| Test ID | Name | Description |
|---------|------|-------------|
| I-3.3.2a | `f32_identity` | f32 input/output is the identity path. Feed f32 samples through, verify output matches DSP output exactly (within floating-point tolerance). |
| I-3.3.2b | `i16_conversion_round_trip` | Convert a known f32 buffer to i16 (using the same formula as `audio.rs:270-271`), then back to f32 (using `audio.rs:216`). Assert round-trip error is within ±1 LSB. This also validates whether the asymmetric conversion noted in `review_audio.md` N1 causes audible artifacts. |
| I-3.3.2c | `u16_conversion_round_trip` | Same as above for u16 conversion. Validates N2/N3 from the audio review. |
| I-3.3.2d | `i16_full_scale` | Verify that `i16::MIN` maps to ≈ -1.0 and `i16::MAX` maps to ≈ +1.0 in the conversion. Check boundary values. |
| I-3.3.2e | `u16_midpoint_is_zero` | Verify that `u16` value 32768 maps to ≈ 0.0 in the f32 conversion. This catches the imprecise midpoint bug in `review_audio.md` N2. |

**Rationale:** Sample format conversion sits at the boundary between cpal's hardware layer and the DSP core. Errors here cause subtle audio artifacts (DC offset, asymmetric clipping) that are hard to diagnose by ear.

---

### 3.4 Pipeline Stress and Robustness

These tests validate behaviour under edge conditions and sustained operation.

#### 3.4.1 Sustained processing stability

| Test ID | Name | Description |
|---------|------|-------------|
| I-3.4.1a | `one_second_no_nan_no_inf` | Run the vocoder at 44100 Hz for 44100 samples (1 second) with realistic modulator + carrier signals. Assert no output sample is NaN or Inf. |
| I-3.4.1b | `ten_seconds_no_memory_growth` | Run the vocoder for 10×44100 samples, tracking peak memory (or at minimum, assert the vocoder struct size doesn't grow). The vocoder has no dynamic allocation in `process()`, so this should trivially pass — but it guards against regressions. |
| I-3.4.1c | `output_remains_bounded` | Run the vocoder with maximum-amplitude modulator and carrier for 44100 samples. Assert `output.abs() < f64::MAX * 0.5`. This validates that output accumulation (review finding H3) doesn't produce astronomically large values within a reasonable timeframe. |

#### 3.4.2 Input edge cases through the full pipeline

| Test ID | Name | Description |
|---------|------|-------------|
| I-3.4.2a | `nan_input_does_not_poison_pipeline` | Feed `f64::NAN` as modulator to `Vocoder::process()` with a valid carrier. Check whether the output becomes NaN. This documents the NaN propagation behaviour identified in `review_dsp.md` C2/C3. |
| I-3.4.2b | `inf_input_handling` | Feed `f64::INFINITY` as modulator. Assert output is bounded or document that it overflows. |
| I-3.4.2c | `extreme_frequency_carrier` | Set MIDI note to 127 (G9 ≈ 12543 Hz) and run vocoder at 44100 Hz. The carrier frequency exceeds the Nyquist/2 threshold of some filter bands. Assert no NaN/Inf in output. |
| I-3.4.2d | `dc_input_modulator` | Feed constant 1.0 as modulator (DC). After the bandpass filters settle, assert output is near zero (bandpass filters should reject DC). |
| I-3.4.2e | `impulse_modulator` | Feed a single impulse (1.0 followed by zeros) as modulator with a continuous carrier. Assert the output has a characteristic attack-decay shape: initially loud, decaying over time as the envelope followers release. |

#### 3.4.3 Concurrency safety

| Test ID | Name | Description |
|---------|------|-------------|
| I-3.4.3a | `concurrent_note_on_off` | Spawn two threads: one rapidly sending NoteOn(60) and NoteOff(60) events through a channel; the other reading them and updating `AtomicU8`. Run for 10000 iterations. Assert no panic and `active_note` is either 60 or 255 at the end. |
| I-3.4.3b | `concurrent_level_meter_reads` | Spawn one thread writing to `AtomicU32` (simulating audio callback) and another reading it (simulating TUI). Run for 10000 iterations. Assert no panic and all reads produce valid `f32` values (no NaN when decoded via `f32::from_bits`). |

**Rationale:** The codebase uses `Relaxed` ordering which is correct for these use cases, but concurrent access patterns should be explicitly tested to guard against future regressions.

#### 3.4.4 Reset and reconfiguration

| Test ID | Name | Description |
|---------|------|-------------|
| I-3.4.4a | `vocoder_reset_clears_state` | Create a vocoder, process 1000 samples of loud signal. Call `reset()`. Process 100 samples of silence. Assert output is exactly 0.0 (no state leakage). |
| I-3.4.4b | `simulate_device_switch` | Create a vocoder instance A, process some signal. Drop A. Create a new vocoder instance B with the same parameters. Feed B the same inputs as A's first few samples. Assert B's output matches A's output on the same inputs from a fresh state (i.e., the vocoder is deterministic given same initial state). |
| I-3.4.4c | `config_change_sample_rate` | Create a vocoder at 44100 Hz and another at 48000 Hz with otherwise identical parameters. Feed the same signal. Assert the outputs differ (different filter coefficients). This validates that sample rate is actually used in filter design. |

---

### 3.5 Audio Block Processing Integration

These tests focus on the block-based processing path used in the real-time callback.

#### 3.5.1 Block vs. sample consistency

| Test ID | Name | Description |
|---------|------|-------------|
| I-3.5.1a | `block_matches_sample_by_sample` | Create a vocoder. Process 1024 samples one at a time via `process()`, recording output. Create an identical vocoder and process the same 1024 samples via `process_block()`. Assert the two output arrays are bitwise identical. |
| I-3.5.1b | `block_length_mismatch_safe` | Call `process_block` with modulator=[1024], carrier=[512], output=[256]. Assert only the first 256 samples are written and no panic occurs. This validates the `min()` length calculation. |
| I-3.5.1c | `empty_block_safe` | Call `process_block` with empty slices. Assert no panic. |

**Rationale:** `process_block` is the path used in production but `process` is simpler to reason about. If they disagree, something is fundamentally wrong.

---

## 4. Test Infrastructure

### 4.1 Helper functions

```rust
/// Generate a sine wave of given frequency and sample rate.
fn generate_sine(freq: f64, sample_rate: f64, num_samples: usize) -> Vec<f64> {
    (0..num_samples)
        .map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / sample_rate).sin())
        .collect()
}

/// Compute RMS energy of a sample buffer.
fn rms(samples: &[f64]) -> f64 {
    if samples.is_empty() { return 0.0; }
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
    if note >= 128 { return vec![0.0; num_samples]; }
    let freq = 440.0 * 2.0_f64.powf((note as f64 - 69.0) / 12.0);
    let mut phase = 0.0;
    (0..num_samples)
        .map(|_| {
            let s = phase.sin();
            phase += 2.0 * std::f64::consts::PI * freq / sample_rate;
            if phase >= 2.0 * std::f64::consts::PI { phase -= 2.0 * std::f64::consts::PI; }
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
```

### 4.2 Test configuration

- All tests use `44100.0` Hz sample rate unless explicitly testing another rate.
- Default vocoder parameters: 20 bands, 200–8000 Hz, Q=4.0, attack=0.001s, release=0.05s (matching `main.rs`).
- Buffer size for block tests: 512 samples (matching `main.rs` default).
- Tolerance for floating-point comparisons: `1e-6` for amplitude, `1e-10` for silence.

### 4.3 Feature gating

Tests that depend on `cpal` or `midir` should be gated behind `#[cfg(feature = "audio")]` and `#[cfg(feature = "midi")]` respectively. Pure DSP + MIDI parse tests need no features. The Makefile's `cargo test --lib --no-default-features` should continue to work for unit tests; integration tests live under `tests/` and run separately.

---

## 5. Traceability to Known Issues

This section maps integration tests to specific findings from the four code reviews, ensuring each known issue has test coverage.

| Review Finding | Integration Test(s) | Coverage |
|---|---|---|
| `review_dsp.md` C1: `FilterBank::analyse()` panics | Not testable (panic is expected). Document as known. | N/A |
| `review_dsp.md` C2: Division by zero at q=0 | I-3.4.2a (NaN propagation through pipeline) | Partial — tests NaN output, not the constructor crash |
| `review_dsp.md` C3: NaN from invalid freq inputs | I-3.4.2a, I-3.4.2c | Yes |
| `review_dsp.md` H1: Denormal performance | I-3.4.1a (1-second stability) — may catch denormal stalls as timeout | Indirect |
| `review_dsp.md` H3: Output accumulation overflow | I-3.4.1c (output bounded) | Yes |
| `review_audio.md` C1/C2: Unbounded/stale channel | Cannot test offline (requires real cpal streams). Document as HW-only test. | N/A |
| `review_audio.md` N1: I16→f32 asymmetric | I-3.3.2b, I-3.3.2d | Yes |
| `review_audio.md` N2/N3: U16 conversion | I-3.3.2c, I-3.3.2e | Yes |
| `review_audio.md` L1: Per-callback allocation | I-3.4.1b (no memory growth) | Indirect |
| `review_midi.md` 1.1: Running status | I-3.2.3a (raw bytes to output) — uses full messages only | Partial |
| `review_midi.md` 1.3: Data byte validation | I-3.4.2a (corrupted input) | Indirect |
| `review_midi.md` 2.1: No disconnect detection | I-3.2.2b (stuck note after disconnect) | Yes |
| `review_midi.md` 3.5: Lost note (monophonic) | I-3.2.2a | Yes |
| `review_main_tui.md` #1: Hardcoded params | I-3.4.4c (sample rate affects output) | Partial |
| `review_main_tui.md` #2: Initial device mismatch | Cannot test offline (requires cpal device enumeration). | N/A |
| `review_main_tui.md` #3: Hardcoded 44100 Hz | I-3.1.2d (pitch at nonstandard rate) | Yes |

---

## 6. Out of Scope

The following are explicitly **not** covered by these integration tests:

1. **Real hardware audio I/O** — requires physical devices; should be tested manually or with a CI loopback device.
2. **TUI rendering** — requires a terminal emulator; the rendering logic is a pure function of `App` state and can be verified visually.
3. **`cpal` stream lifecycle** — starting/stopping/pausing real audio streams; OS-specific.
4. **`midir` port lifecycle** — connecting/disconnecting real MIDI ports; OS-specific.
5. **Performance benchmarks** — DSP throughput, latency measurements. These are benchmark concerns, not correctness tests.

---

## 7. Running the Tests

```bash
# Run all integration tests (DSP + MIDI offline, no hardware needed)
cargo test --test integration

# Run only DSP-chain tests
cargo test --test integration dsp_audio

# Run only MIDI-chain tests
cargo test --test integration midi_state

# Run stress tests (may be slow)
cargo test --test integration stress -- --ignored
```

Unit tests remain unchanged:
```bash
make test  # cargo test --lib --no-default-features
```

---

## 8. Summary

This design defines **42 integration tests** across 5 categories:

| Category | Count | Validates |
|---|---|---|
| DSP ↔ Audio Callback | 11 | Vocoder output correctness, carrier pitch, level meters |
| MIDI → State → DSP | 11 | Event parsing, shared state, end-to-end chain |
| Audio Format Conversion | 8 | Deinterleave, sample format round-trips, channel broadcast |
| Pipeline Stress & Robustness | 9 | Stability, edge cases, concurrency, reset |
| Block Processing | 3 | Block vs. sample consistency, boundary safety |
| **Total** | **42** | |

Each test is designed to be deterministic, fast (< 1 second), and runnable without hardware. Together they exercise every data-flow path in the pipeline: MIDI bytes → parsed events → shared atomics → carrier oscillator → vocoder DSP → output samples → level meters.
