# Test Design: Unit Tests

**Date:** 2026-04-30
**Scope:** Comprehensive unit test design derived from code review findings across `dsp.rs`, `midi.rs`, `audio.rs`, `main.rs`, and `tui.rs`.
**Focus:** Edge cases, error conditions, numerical correctness, and property-based testing.

---

## Overview

This document defines a unit test suite organized by module. Each test is cross-referenced to the code review finding that motivates it. Tests are written for pure functions and internal logic only — I/O-dependent code (audio streams, MIDI ports, terminal rendering) is covered by integration tests and is out of scope.

Test priorities:
- **P0** — Must-have: catches bugs that silently corrupt output or cause panics.
- **P1** — Should-have: catches edge cases that are likely in real usage.
- **P2** — Nice-to-have: property-based or invariants tests that strengthen confidence.

---

## 1. DSP Module (`src/dsp.rs`)

### 1.1 `BiquadFilter::bandpass()` — Input Validation

| Test ID | Priority | Description | Review Ref |
|---------|----------|-------------|------------|
| DSP-BP-01 | P0 | `q == 0.0` produces `inf` coefficients. Verify that calling `bandpass(1000.0, 0.0, 44100.0)` does not silently produce a filter with `NaN`/`inf` coefficients. Assert that all five coefficients (`b0, b1, b2, a1, a2`) are finite. | C2 |
| DSP-BP-02 | P0 | `q < 0` (negative Q). Same check — coefficients must be finite and the filter must not produce `NaN` on `process()`. | C2 |
| DSP-BP-03 | P0 | `center_freq == 0.0`. `w0 = 0`, `sin(w0) = 0`, `alpha = 0`, so `b0 = b2 = 0`. Filter should produce no output (or be rejected). Verify the filter processes without `NaN`. | H2 |
| DSP-BP-04 | P0 | `center_freq == sample_rate / 2.0` (Nyquist). `w0 = π`, `sin(w0) ≈ 0`, `alpha ≈ 0`. Verify coefficients are finite and the filter is stable (bounded output for bounded input). | H2 |
| DSP-BP-05 | P1 | `center_freq > sample_rate / 2.0` (above Nyquist). `w0 > π`. Verify the filter does not produce unbounded output over 1000 samples. | H2 |
| DSP-BP-06 | P1 | `center_freq < 0.0` (negative frequency). Verify no `NaN` in coefficients. | H2 |
| DSP-BP-07 | P1 | Very high Q (`q = 10000.0`). Verify the filter remains stable (bounded output) when processing a 1.0 step for 1000 samples. Marginal instability is acceptable; divergent output is not. | H2 |
| DSP-BP-08 | P1 | `sample_rate == 0.0`. Division by zero in `w0` calculation. Verify no panic or `NaN` propagation. | C2 |

### 1.2 `BiquadFilter::process()` — Correctness & Numerics

| Test ID | Priority | Description | Review Ref |
|---------|----------|-------------|------------|
| DSP-BP-09 | P0 | Zero input always produces exactly 0.0 from zero state. Feed 100 samples of 0.0 to a freshly constructed bandpass; assert `output == 0.0` (exact, not approximate). Existing test uses `< 1e-12` which masks potential issues. | L4 |
| DSP-BP-10 | P1 | Filter is linear: `process(a * x) == a * process(x)` for scalar `a`. Verify for `a = 2.0` over 100 samples. | — |
| DSP-BP-11 | P1 | Filter is time-invariant: same input sequence always produces the same output sequence regardless of when it is fed. Reset, process 50 samples, record output. Reset, process same 50 samples again, assert identical output. | — |
| DSP-BP-12 | P2 | Bounded-Input Bounded-Output (BIBO) stability property: for any input bounded by `[-1.0, 1.0]`, the output is bounded by some constant `M` (which depends on the filter). Verify for 10,000 random samples in `[-1, 1]` that output stays within a reasonable bound (e.g., `[-10.0, 10.0]` for a bandpass). | — |

### 1.3 `BiquadFilter::reset()`

| Test ID | Priority | Description | Review Ref |
|---------|----------|-------------|------------|
| DSP-BP-13 | P1 | After `reset()`, state variables are zero. Process 100 non-zero samples, call `reset()`, then verify `process(0.0) == 0.0` exactly. | — |

### 1.4 `EnvelopeFollower` — Construction & Edge Cases

| Test ID | Priority | Description | Review Ref |
|---------|----------|-------------|------------|
| DSP-EF-01 | P0 | Negative `attack_time` (e.g., `-0.001`). Currently silently sets attack to `0.0` (instantaneous). Document the behavior in the test and verify the output converges correctly. | H4 |
| DSP-EF-02 | P0 | Negative `release_time` (e.g., `-0.05`). Same as above. | H4 |
| DSP-EF-03 | P1 | `attack_time == 0.0` (not negative, but zero). Branch falls to `attack = 0.0`. Verify that processing a step input converges to the input value. | H4 |
| DSP-EF-04 | P1 | `release_time == 0.0`. Same as above for release. | H4 |
| DSP-EF-05 | P1 | `sample_rate == 0.0`. `attack_time * sample_rate = 0.0`, `(-1/0.0).exp() = 0.0`. Verify no panic. | — |
| DSP-EF-06 | P1 | Very large `attack_time` (e.g., 1000.0 seconds). Coefficient approaches 1.0. Verify the envelope barely rises over 1000 samples. | — |
| DSP-EF-07 | P1 | `NaN` input. Verify `process(f64::NAN)` does not permanently poison the envelope — subsequent valid inputs should produce finite output. | — |
| DSP-EF-08 | P1 | `inf` input. Verify `process(f64::INFINITY)` is handled without permanent corruption. | — |

### 1.5 `EnvelopeFollower` — Correctness

| Test ID | Priority | Description | Review Ref |
|---------|----------|-------------|------------|
| DSP-EF-09 | P1 | Steady-state accuracy: constant input `x` for many samples should converge to `|x|`. Verify `|envelope - |x|| < 1e-6` after 100,000 samples. | — |
| DSP-EF-10 | P1 | Attack is faster than release. Existing test covers this; add a quantitative check: e.g., rise to 90% happens in fewer samples than fall from 90% to 10%. | — |
| DSP-EF-11 | P2 | Envelope is monotonically non-decreasing when fed a constant input (after initial transient). Verify for 1000 samples. | — |

### 1.6 `spread_frequencies()` — Edge Cases

| Test ID | Priority | Description | Review Ref |
|---------|----------|-------------|------------|
| DSP-SF-01 | P0 | `low == 0.0`. `0.0_f64.ln() == -inf`. Verify the function returns `NaN` or `-inf` center frequencies (document the bug), or panics. | C3 |
| DSP-SF-02 | P0 | `low < 0.0` (negative). `(-1.0_f64).ln() == NaN`. Verify behavior — currently produces `NaN` frequencies. | C3 |
| DSP-SF-03 | P0 | `high == 0.0`. Same as DSP-SF-01 for the upper bound. | C3 |
| DSP-SF-04 | P1 | `low > high` (inverted range). Frequencies are produced in descending order (mathematically valid but semantically wrong). Document the behavior. | C3 |
| DSP-SF-05 | P1 | `low == high`. All frequencies are identical. Verify the Vec contains `n` copies of the same value. | M4 |
| DSP-SF-06 | P1 | `n == 0`. Returns empty Vec. Existing test covers this. | — |
| DSP-SF-07 | P1 | `n == 1`. Returns the geometric mean. Existing test covers this. | — |
| DSP-SF-08 | P1 | `n == 2`. Verify first frequency == `low`, last == `high`. | — |
| DSP-SF-09 | P2 | Logarithmic spacing invariant: for `n >= 3`, the ratio `freqs[i+1] / freqs[i]` is constant for all `i`. Existing test covers this for `n=4`; extend to `n=20`. | — |

### 1.7 `FilterBank::analyse()` — Panic Check

| Test ID | Priority | Description | Review Ref |
|---------|----------|-------------|------------|
| DSP-FB-01 | P0 | `analyse()` panics with `unimplemented!()`. Test that calling `analyse()` on a `FilterBank` panics (use `#[should_panic]`). | C1 |

### 1.8 `Vocoder` — Integration Tests

| Test ID | Priority | Description | Review Ref |
|---------|----------|-------------|------------|
| DSP-VO-01 | P0 | Silence in produces exactly 0.0 out (tighten existing test to exact equality). | L4 |
| DSP-VO-02 | P1 | `process()` output is always finite for bounded inputs. Feed 10,000 random `(mod, carrier)` pairs in `[-1, 1]` and assert `output.is_finite()`. | C2, C3 |
| DSP-VO-03 | P1 | Output accumulation check: for a 20-band vocoder with full-spectrum inputs, the output magnitude can exceed 1.0. Verify this property holds (document, don't fix). | H3 |
| DSP-VO-04 | P1 | `process_block()` truncates on mismatched lengths. Verify that with `modulator.len() = 256, carrier.len() = 128, output.len() = 256`, only 128 samples are written. | M5 |
| DSP-VO-05 | P1 | `reset()` clears all state. After processing non-zero samples and calling `reset()`, verify `envelopes()` are all 0.0 and silence produces 0.0. Existing test covers this. | — |
| DSP-VO-06 | P2 | `Vocoder::new()` with `num_bands = 0`. Verify it doesn't panic — the inner loop runs 0 times and `process()` returns 0.0. | — |
| DSP-VO-07 | P2 | `Vocoder::new()` with `low = high`. All filters have the same coefficients. Verify output is non-zero with signal input (single bandpass with gain). | M4 |
| DSP-VO-08 | P2 | Consistency: `process()` called N times produces the same output as `process_block()` on the same N-length arrays. | — |

---

## 2. MIDI Module (`src/midi.rs`)

### 2.1 `parse_midi_message()` — Happy Path (existing tests)

Already covered: NoteOn, NoteOff, NoteOn velocity-0, ControlChange, PitchBend center, PitchBend max, Unknown (empty), Unknown (single byte).

### 2.2 `parse_midi_message()` — Edge Cases & Boundary Values

| Test ID | Priority | Description | Review Ref |
|---------|----------|-------------|------------|
| MID-PA-01 | P0 | Pitch bend minimum: `[0xE0, 0x00, 0x00]` should produce `PitchBend { value: -8192 }`. | 1.5 |
| MID-PA-02 | P1 | NoteOn channel 15: `[0x9F, 60, 100]` should produce `NoteOn { channel: 15, note: 60, velocity: 100 }`. | — |
| MID-PA-03 | P1 | NoteOff channel 15: `[0x8F, 60, 0]` should produce `NoteOff { channel: 15, ... }`. | — |
| MID-PA-04 | P1 | ControlChange channel 15: `[0xBF, 7, 127]`. | — |
| MID-PA-05 | P1 | PitchBend channel 15: `[0xEF, 0x00, 0x40]` should produce `PitchBend { channel: 15, value: 0 }`. | — |
| MID-PA-06 | P1 | Two-byte message (Program Change): `[0xC0, 5]` should produce `Unknown` (len < 3). | 1.4 |
| MID-PA-07 | P1 | Two-byte message (Channel Pressure): `[0xD1, 100]` should produce `Unknown`. | 1.4 |
| MID-PA-08 | P0 | SysEx start: `[0xF0, 0x01, 0x02]` should produce `Unknown`. | 1.2 |
| MID-PA-09 | P1 | Real-time Clock: `[0xF8]` should produce `Unknown`. | — |
| MID-PA-10 | P1 | Active Sensing: `[0xFE]` should produce `Unknown`. | — |
| MID-PA-11 | P1 | Song Position Pointer: `[0xF2, 0x00, 0x00]` should produce `Unknown` (system common, not voice). | 1.2 |

### 2.3 `parse_midi_message()` — Malformed & Corrupted Input

| Test ID | Priority | Description | Review Ref |
|---------|----------|-------------|------------|
| MID-PA-12 | P0 | Corrupted data bytes >= 128: `[0x90, 0x80, 0x90]`. Verify it does not panic. The parser currently accepts these as-is. | 1.3 |
| MID-PA-13 | P1 | Truncated voice message (2 bytes): `[0x90, 60]` should produce `Unknown` (len < 3). | — |
| MID-PA-14 | P1 | Single status byte: `[0x90]` should produce `Unknown`. | — |
| MID-PA-15 | P1 | All-zeros: `[0x00, 0x00, 0x00]` — status 0x00 is not a recognized message type; should produce `Unknown`. | — |
| MID-PA-16 | P1 | NoteOn with velocity 0 on non-zero channel: `[0x91, 64, 0]` should produce `NoteOff { channel: 1, note: 64, velocity: 0 }`. | — |
| MID-PA-17 | P2 | Note with value 0: `[0x90, 0, 100]`. Verify `NoteOn { note: 0, ... }`. | — |
| MID-PA-18 | P2 | Note with value 127: `[0x90, 127, 100]`. Verify `NoteOn { note: 127, ... }`. | — |
| MID-PA-19 | P2 | Velocity 127: `[0x90, 60, 127]`. Verify `NoteOn { velocity: 127, ... }`. | — |

### 2.4 `parse_midi_message()` — Property-Based Tests

| Test ID | Priority | Description | Review Ref |
|---------|----------|-------------|------------|
| MID-PB-01 | P2 | **Round-trip channel preservation**: For any valid NoteOn/NoteOff/CC message, the channel in the output matches `status & 0x0F`. Generate random `status` bytes where `(status & 0xF0) ∈ {0x80, 0x90, 0xB0, 0xE0}` and random data bytes, verify channel extraction. | — |
| MID-PB-02 | P2 | **No panic guarantee**: For any arbitrary `&[u8]` of length 0–4 containing any byte values (0x00–0xFF), `parse_midi_message()` must not panic. | — |
| MID-PB-03 | P2 | **Pitch bend range**: For any `[0xE0, lsb, msb]` where `lsb, msb ∈ 0..=127`, the resulting `value` must be in `[-8192, 8064]`. | — |

---

## 3. Audio Module (`src/audio.rs`)

### 3.1 `deinterleave()` — Pure Function Tests

| Test ID | Priority | Description | Review Ref |
|---------|----------|-------------|------------|
| AUD-DI-01 | P1 | Empty input: `deinterleave(&[], 2)` should return `Vec<Vec<f32>>` with 2 empty Vecs (or handle gracefully — verify current behavior). | — |
| AUD-DI-02 | P1 | Single frame stereo: `deinterleave(&[1.0, 2.0], 2)` → `[[1.0], [2.0]]`. | — |
| AUD-DI-03 | P1 | Three channels: `deinterleave(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 3)` → `[[1.0, 4.0], [2.0, 5.0], [3.0, 6.0]]`. | — |
| AUD-DI-04 | P1 | Channel count == 0: `deinterleave(&[1.0, 2.0], 0)`. This would cause division by zero (`frames = len / 0`). Verify behavior (likely panic — document). | — |
| AUD-DI-05 | P2 | Round-trip property: For any interleaved stereo buffer, `deinterleave` then re-interleave produces the original buffer. | — |

### 3.2 Sample Format Conversion — Numerical Correctness

> Note: These conversions are inline closures in `build_input_stream` / `build_output_stream`. To test them properly, they should be extracted into named functions. The tests below assume that refactoring or test the underlying formula directly.

| Test ID | Priority | Description | Review Ref |
|---------|----------|-------------|------------|
| AUD-SF-01 | P0 | I16 → f32: `i16::MIN (-32768)` maps to `-32768.0 / 32767.0 ≈ -1.0000305`. Verify current behavior and document that it exceeds `[-1.0, 1.0]` range. | N1 |
| AUD-SF-02 | P0 | I16 → f32: `i16::MAX (32767)` maps to `32767.0 / 32767.0 = 1.0`. Correct. | N1 |
| AUD-SF-03 | P1 | I16 → f32: `0` maps to `0.0`. | N1 |
| AUD-SF-04 | P1 | U16 → f32: `0` maps to `(-32768.0) / 32767.0 ≈ -1.00003`. Verify and document. | N2 |
| AUD-SF-05 | P1 | U16 → f32: `32768` maps to `0.0 / 32767.0 ≈ 0.00003` (not exactly 0.0). Verify. | N2 |
| AUD-SF-06 | P1 | U16 → f32: `65535` maps to `32767.0 / 32767.0 = 1.0`. | N2 |
| AUD-SF-07 | P1 | f32 → U16: `-1.0` maps to `(-32767.0 + 32768.0) = 1` (not 0). Verify. | N3 |
| AUD-SF-08 | P1 | f32 → U16: `0.0` maps to `32768`. Verify. | N3 |
| AUD-SF-09 | P1 | f32 → U16: `1.0` maps to `65535`. Verify. | N3 |

### 3.3 `AudioIoConfig` — Default Values

| Test ID | Priority | Description | Review Ref |
|---------|----------|-------------|------------|
| AUD-CF-01 | P2 | `AudioIoConfig::default()` has `sample_rate: None, buffer_size: None`. Existing test covers this. | — |

---

## 4. TUI Module (`src/tui.rs`)

### 4.1 `Config::default()` — Values

| Test ID | Priority | Description | Review Ref |
|---------|----------|-------------|------------|
| TUI-CF-01 | P1 | Default config values: `sample_rate == 44100`, `buffer_size == 512`, `midi_channel == 1`, `formant_shift == 1.0`, `pitch_shift == 0.0`, `gain == 0.8`. | — |
| TUI-CF-02 | P1 | Default devices are `None`. | — |

### 4.2 `ConfigField::adjust()` — Boundary Values

| Test ID | Priority | Description | Review Ref |
|---------|----------|-------------|------------|
| TUI-AD-01 | P1 | `SampleRate` clamps at minimum: adjust down from 22050 → stays 22050. | — |
| TUI-AD-02 | P1 | `SampleRate` clamps at maximum: adjust up from 96000 → stays 96000. | — |
| TUI-AD-03 | P1 | `BufferSize` clamps at 128 (min) and 2048 (max). | — |
| TUI-AD-04 | P1 | `MidiChannel` clamps at 1 (min) and 16 (max). Verify that the range is `1..=16` (display convention), not `0..=15` (protocol). | tui.rs:141 |
| TUI-AD-05 | P1 | `FormantShift` clamps at 0.25 (min) and 4.0 (max). | — |
| TUI-AD-06 | P1 | `PitchShift` clamps at -24.0 (min) and 24.0 (max). | — |
| TUI-AD-07 | P1 | `Gain` clamps at 0.0 (min) and 1.5 (max). | — |
| TUI-AD-08 | P2 | `FormantShift` step size is `0.05`. Verify that a single adjustment from 1.0 produces 1.05. | — |
| TUI-AD-09 | P2 | `PitchShift` step size is `0.5`. Verify that a single adjustment from 0.0 produces 0.5. | — |

### 4.3 `App::cycle_device()` — Edge Cases

| Test ID | Priority | Description | Review Ref |
|---------|----------|-------------|------------|
| TUI-CD-01 | P1 | Empty device list: `cycle_device` should not panic and should leave `current` unchanged. | — |
| TUI-CD-02 | P1 | Single device: cycling forward and backward stays on the same device. | — |
| TUI-CD-03 | P1 | Wrap-around forward: from last device, cycle +1 wraps to first. | — |
| TUI-CD-04 | P1 | Wrap-around backward: from first device, cycle -1 wraps to last. | — |
| TUI-CD-05 | P1 | `current` is `None` and devices are available: cycling selects the first device. | — |
| TUI-CD-06 | P2 | `current` names a device not in the list: falls back to index 0. | — |

### 4.4 `App::handle_event()` — Keyboard Input

| Test ID | Priority | Description | Review Ref |
|---------|----------|-------------|------------|
| TUI-HE-01 | P1 | `KeyCode::Char('q')` sets `should_quit = true`. | — |
| TUI-HE-02 | P1 | `KeyCode::Esc` sets `should_quit = true`. | — |
| TUI-HE-03 | P1 | `KeyCode::Up` decrements `selected_field` (bounded at 0). | — |
| TUI-HE-04 | P1 | `KeyCode::Down` increments `selected_field` (bounded at `CONFIG_FIELDS.len() - 1`). | — |
| TUI-HE-05 | P1 | `KeyCode::Char('k')` acts like Up, `KeyCode::Char('j')` acts like Down (vim keys). | — |
| TUI-HE-06 | P1 | `KeyCode::Char('h')` acts like Left, `KeyCode::Char('l')` acts like Right. | — |
| TUI-HE-07 | P2 | Non-press key events (KeyKind::Release) are ignored. | — |
| TUI-HE-08 | P2 | Unmapped keys (e.g., `KeyCode::Char('x')`) are ignored without side effects. | — |

### 4.5 `ConfigField::label()` and `display_value()` — Coverage

| Test ID | Priority | Description | Review Ref |
|---------|----------|-------------|------------|
| TUI-DV-01 | P2 | Every `ConfigField` variant has a non-empty `label()`. | — |
| TUI-DV-02 | P2 | `display_value()` for each variant produces a non-empty string for default config. | — |
| TUI-DV-03 | P2 | `display_value()` for device fields shows `"(default)"` when `None`. | — |

---

## 5. Main Module (`src/main.rs`)

### 5.1 `midi_note_to_name()` — Note Name Conversion

| Test ID | Priority | Description | Review Ref |
|---------|----------|-------------|------------|
| MAIN-MN-01 | P1 | Note 0 → `"C-1"` (lowest MIDI note). | — |
| MAIN-MN-02 | P1 | Note 60 → `"C4"` (middle C). | — |
| MAIN-MN-03 | P1 | Note 69 → `"A4"` (concert pitch, 440 Hz). | — |
| MAIN-MN-04 | P1 | Note 127 → `"G9"` (highest MIDI note). | — |
| MAIN-MN-05 | P1 | Note 12 → `"C0"`. | — |
| MAIN-MN-06 | P1 | Note 24 → `"C1"`. | — |
| MAIN-MN-07 | P2 | Note 1 → `"C#-1"`. | — |
| MAIN-MN-08 | P2 | Note 11 → `"B-1"`. | — |

### 5.2 MIDI-to-Frequency Mapping (Carrier Pitch)

> The formula `440.0 * 2^((note - 69) / 12)` is inline in `build_audio_io`. Extract it for testability.

| Test ID | Priority | Description | Review Ref |
|---------|----------|-------------|------------|
| MAIN-FR-01 | P1 | Note 69 → 440.0 Hz exactly. | — |
| MAIN-FR-02 | P1 | Note 60 → 261.626 Hz (approximately, within 0.01 Hz). | — |
| MAIN-FR-03 | P1 | Note 12 → 16.352 Hz (C0, approximately). | — |
| MAIN-FR-04 | P1 | Note 0 → 8.176 Hz (approximately). | — |
| MAIN-FR-05 | P1 | Note 127 → 12543.854 Hz (approximately). | — |
| MAIN-FR-06 | P1 | Note 255 (sentinel) → 0.0 Hz (silence). Current code: `note >= 128` → 0.0. | main.rs:57 |
| MAIN-FR-07 | P2 | Each semitone step multiplies frequency by `2^(1/12) ≈ 1.0595`. Verify for note 69 → 70. | — |

### 5.3 Atomic State Interaction Patterns

> These tests verify the logic that handles atomic reads/writes in the main loop. They don't test real threading but verify the single-threaded correctness of the decision logic.

| Test ID | Priority | Description | Review Ref |
|---------|----------|-------------|------------|
| MAIN-AT-01 | P1 | NoteOn sets `active_note` to the note value. | — |
| MAIN-AT-02 | P1 | NoteOff clears `active_note` to 255 only if it matches. | — |
| MAIN-AT-03 | P1 | NoteOff for a different note does not clear `active_note`. | — |
| MAIN-AT-04 | P2 | Simulated TOCTOU race: load note, then store a different note, then check — verify the logic handles stale reads gracefully. | review_main_tui #8 |

---

## 6. Property-Based Test Suite (Cross-Cutting)

These tests use random input generation to verify invariants. In Rust, this can be done with the `proptest` crate or simple manual random generation.

| Test ID | Priority | Module | Invariant |
|---------|----------|--------|-----------|
| PROP-01 | P2 | DSP | For any `BiquadFilter` constructed with `center_freq ∈ (0.1, 20000)`, `q ∈ (0.01, 1000)`, `sample_rate ∈ (8000, 192000)`, and `center_freq < sample_rate / 2`: all coefficients are finite, and `process()` never returns `NaN` or `inf` for finite input. |
| PROP-02 | P2 | DSP | For any `EnvelopeFollower` with `attack_time, release_time > 0`, `sample_rate > 0`: `process()` never returns negative values for any input. |
| PROP-03 | P2 | DSP | For any `Vocoder` with valid params, `process()` returns a finite value for any pair of finite inputs in `[-1.0, 1.0]`. |
| PROP-04 | P2 | MIDI | `parse_midi_message()` never panics for any `&[u8]` of length 0..=256 with any byte values. |
| PROP-05 | P2 | MIDI | For any valid NoteOn `[0x90..0x9F, note, vel]` with `vel > 0`, the returned `MidiEvent` is `NoteOn` with correct `channel`, `note`, `velocity`. |
| PROP-06 | P2 | MIDI | For any valid NoteOff `[0x80..0x8F, note, vel]`, the returned `MidiEvent` is `NoteOff` with correct fields. |
| PROP-07 | P2 | DSP | `spread_frequencies(n, low, high)` with `n >= 2, low > 0, high > low` produces strictly increasing frequencies where `freqs[0] == low` and `freqs[n-1] == high`. |

---

## 7. Test Implementation Notes

### 7.1 Recommended Test Dependencies

Add to `[dev-dependencies]` in `Cargo.toml`:
```toml
[dev-dependencies]
proptest = "1"
```

### 7.2 Extracting Inline Logic for Testability

The following inline code should be extracted into testable functions:

1. **`note_to_freq(note: u8) -> f64`** — The MIDI-to-frequency formula in `main.rs:57-59`.
2. **`i16_to_f32(s: i16) -> f32`** — The I16→f32 conversion in `audio.rs:216`.
3. **`u16_to_f32(s: u16) -> f32`** — The U16→f32 conversion in `audio.rs:228`.
4. **`f32_to_u16(s: f32) -> u16`** — The f32→U16 conversion in `audio.rs:284`.
5. **`f32_to_i16(s: f32) -> i16`** — The f32→I16 conversion in `audio.rs:271`.

### 7.3 Feature Flags and Test Organization

Tests that depend on `audio.rs` or `midi.rs` must be gated behind the `#[cfg(feature = "audio")]` / `#[cfg(feature = "midi")]` attributes, matching the production code's feature gates. The `dsp.rs` tests require no feature flags.

### 7.4 Test Count Summary

| Module | P0 | P1 | P2 | Total |
|--------|----|----|-----|-------|
| DSP    | 8  | 17 | 7   | 32    |
| MIDI   | 4  | 12 | 5   | 21    |
| Audio  | 2  | 8  | 2   | 12    |
| TUI    | 0  | 16 | 8   | 24    |
| Main   | 0  | 9  | 4   | 13    |
| Cross  | 0  | 0  | 7   | 7     |
| **Total** | **14** | **62** | **33** | **109** |

### 7.5 Priority Execution Order

Run P0 tests first (they catch crashes and silent data corruption). Then P1 (edge cases). P2 (property-based) can run last as they are slower.

---

## 8. Cross-Reference: Review Findings → Tests

| Review Finding | Test IDs |
|----------------|----------|
| dsp C1: `analyse()` unimplemented | DSP-FB-01 |
| dsp C2: Division by zero at `q == 0` | DSP-BP-01, DSP-BP-02, DSP-VO-02 |
| dsp C3: NaN from `spread_frequencies` | DSP-SF-01, DSP-SF-02, DSP-SF-03, DSP-VO-02 |
| dsp H1: Denormal performance | (Not unit-testable; requires benchmarks) |
| dsp H2: Nyquist constraint | DSP-BP-03, DSP-BP-04, DSP-BP-05, DSP-BP-06 |
| dsp H3: Output accumulation overflow | DSP-VO-03 |
| dsp H4: Negative time constants | DSP-EF-01, DSP-EF-02, DSP-EF-03, DSP-EF-04 |
| dsp M4: `low == high` | DSP-SF-05, DSP-VO-07 |
| dsp M5: Buffer truncation | DSP-VO-04 |
| dsp L4: Imprecise assertions | DSP-BP-09, DSP-VO-01 |
| midi 1.1: Running status | (Requires stateful parser — document limitation) |
| midi 1.2: SysEx fallthrough | MID-PA-08, MID-PA-11 |
| midi 1.3: Data byte validation | MID-PA-12 |
| midi 1.5: Pitch bend min | MID-PA-01 |
| audio C1/C2: Unbounded/stale channel | (Integration test — out of scope) |
| audio N1: I16→f32 asymmetry | AUD-SF-01, AUD-SF-02, AUD-SF-03 |
| audio N2: U16→f32 midpoint | AUD-SF-04, AUD-SF-05, AUD-SF-06 |
| audio N3: f32→U16 offset | AUD-SF-07, AUD-SF-08, AUD-SF-09 |
| main: Hardcoded sample rate | MAIN-FR-01..07 |
| main: NoteOff TOCTOU | MAIN-AT-04 |

---

## 9. Out of Scope

The following are important but require integration or system-level testing:

- **Audio stream lifecycle** (device enumeration, stream creation/teardown)
- **MIDI port connection/disconnection** (requires hardware or mock `midir` backend)
- **TUI rendering** (visual correctness — requires snapshot testing with `ratatui::backend::TestBackend`)
- **Real-time performance** (denormal stalls, allocation latency — requires benchmarks)
- **Concurrency correctness** (TOCTOU on atomics, unbounded channel growth — requires stress tests)
