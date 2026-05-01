# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.2.0] — 2026-05-01

### Added

- **Polyphony (chords) support** — multiple MIDI notes can sound simultaneously.
  The carrier is synthesised as a sum of sine-wave oscillators with `1/√N` energy
  normalisation to prevent clipping when playing chords.
- **Device selection in TUI** — Audio Input, Audio Output, and MIDI Input devices
  can be switched at runtime via the configuration panel (`←`/`→` to cycle).
  Stream is automatically restarted on device change.
- **Keyboard / Vocoder mode toggle** — press `Tab` to switch between Keyboard mode
  (raw carrier output, no vocoder DSP) and Vocoder mode (full spectral processing).
  Current mode is shown in the TUI title bar.
- **Configurable parameters in TUI** — Sample Rate, Buffer Size, MIDI Channel,
  Formant Shift, Pitch Shift, and Gain are all adjustable at runtime.
- **MIDI running status support** — `MidiParser` correctly handles the MIDI
  running-status optimisation where consecutive messages of the same type omit
  the status byte. Real-time messages pass through without affecting running
  status; system-common messages reset it.
- **Audio device filtering** — ALSA devices matching known non-working patterns
  (surround, front, dmix, dsnoop, null, etc.) are automatically filtered out,
  and duplicate device names are deduplicated.
- **MIDI note name display** — active notes are shown as human-readable names
  (e.g. "C4, E4, G4") in the TUI status panel.

### Changed

- **Audio callback is zero-allocation** — audio input and output callbacks use a
  block-recycling scheme (`SharedBuffer`) that reuses previously allocated buffers,
  eliminating heap allocations on the real-time audio thread.
- **Soft clipping on output** — output audio is passed through `tanh()` waveshaping
  to prevent harsh digital clipping.
- **Makeup gain** — a 4× gain is applied after the vocoder filter bank to compensate
  for energy lost across the bandpass filters.
- **Denormal flushing** — biquad filter state variables and envelope followers flush
  subnormal (denormal) floats to zero to avoid severe CPU performance penalties.
- **Pre-allocated format conversion buffers** — when the audio device uses i16 or u16
  sample format, conversion buffers are allocated once at stream creation and reused
  on every callback, avoiding per-buffer allocations.

### Fixed

- Biquad filter no longer produces NaN/Inf for edge-case parameters (zero or negative
  Q, zero or negative frequency, zero sample rate, frequency at or above Nyquist).
- `spread_frequencies` clamps negative, zero, and inverted frequency ranges to valid
  positive values.
- Envelope follower handles zero/negative attack and release times gracefully
  (treated as instantaneous).
- DSP core is robust against degenerate inputs (zero bands, low == high frequency range).

## [0.1.0] — Initial Release

### Added

- Core DSP: biquad bandpass filters, envelope followers, analysis and synthesis
  filter banks, and the `Vocoder` struct.
- Real-time audio I/O via `cpal` (feature-gated).
- MIDI input via `midir` with `MidiEvent` parsing (feature-gated).
- Terminal UI via `ratatui` with live audio level meters and MIDI status (feature-gated).
- Library crate (`vocoder::dsp`) usable without audio hardware.
- Unit tests and property-based tests for DSP, audio, MIDI, and TUI modules.
