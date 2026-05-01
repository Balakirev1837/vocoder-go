# vocoder

A real-time channel vocoder written in Rust.

Takes a **modulator** signal (e.g. microphone / voice) and a **carrier** signal (e.g. a synthesizer tone driven by MIDI) and imposes the spectral envelope of the modulator onto the carrier — the classic robot-voice effect.

## How it works

The DSP core splits both signals through a bank of bandpass filters, tracks the modulator's energy in each band with envelope followers, and uses those envelopes to gate the matching carrier bands. The result is summed back into a single output stream.

Key parameters:

- **Bands** — number of frequency channels (default: 20)
- **Frequency range** — lower and upper bounds of the filter bank (default: 200–8000 Hz)
- **Q** — quality factor of each bandpass filter (default: 4.0)
- **Attack / Release** — envelope follower time constants

## Building

```bash
cargo build
```

Requires Rust (edition 2024). All features are enabled by default.

## Running

```bash
cargo run
```

This opens a terminal UI showing live audio levels, MIDI status, and the active note. Plug in a MIDI controller and play notes while speaking into your mic.

### Keyboard controls (TUI)

| Key | Action |
|-----|--------|
| `↑` / `k` | Move selection up |
| `↓` / `j` | Move selection down |
| `←` / `h` | Decrease value |
| `→` / `l` | Increase value |
| `q` / `Esc` | Quit |

## Feature flags

| Feature | Default | Description |
|---------|---------|-------------|
| `audio` | yes | Real-time audio I/O via [cpal](https://crates.io/crates/cpal) |
| `midi` | yes | MIDI input via [midir](https://crates.io/crates/midir) |
| `tui` | yes | Terminal UI via [ratatui](https://crates.io/crates/ratatui) |

To build only the DSP core (no audio/MIDI/TUI):

```bash
cargo build --no-default-features
```

## Testing

```bash
make test
# or: cargo test --lib --no-default-features
```

## Project layout

```
src/
  lib.rs    — re-exports the DSP module
  dsp.rs    — biquad filters, envelope followers, filter banks, Vocoder struct
  audio.rs  — real-time audio input/output (feature-gated)
  midi.rs   — MIDI input parsing (feature-gated)
  tui.rs    — terminal user interface (feature-gated)
  main.rs   — wires everything together
```

## Using the DSP library

The vocoder DSP is usable as a standalone library (no audio hardware needed):

```rust
use vocoder::dsp::Vocoder;

let mut vocoder = Vocoder::new(
    20,         // bands
    200.0,      // low freq (Hz)
    8000.0,     // high freq (Hz)
    4.0,        // Q
    0.001,      // attack (s)
    0.05,       // release (s)
    44100.0,    // sample rate
);

let output = vocoder.process(modulator_sample, carrier_sample);
```

See `src/dsp.rs` for full API documentation.
