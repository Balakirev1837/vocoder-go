# vocoder

A real-time polyphonic channel vocoder written in Rust.

Takes a **modulator** signal (e.g. microphone / voice) and a **carrier** signal (a synthesizer driven by MIDI — with full **chord / polyphony** support) and imposes the spectral envelope of the modulator onto the carrier — the classic robot-voice effect.

## How it works

The DSP core splits both signals through a bank of bandpass filters, tracks the modulator's energy in each band with envelope followers, and uses those envelopes to gate the matching carrier bands. The result is summed back into a single output stream.

Key parameters:

- **Bands** — number of frequency channels (default: 20)
- **Frequency range** — lower and upper bounds of the filter bank (default: 200–8000 Hz, scaled by formant shift)
- **Q** — quality factor of each bandpass filter (default: 4.0)
- **Attack / Release** — envelope follower time constants

## Features

### Polyphony (Chords)

Multiple MIDI notes can sound simultaneously. The carrier is synthesised as a sum of sine-wave oscillators — one per active note — with energy-normalised mixing (`1/√N` scaling) to prevent clipping when playing chords.

### Device Selection (TUI)

The TUI configuration panel lets you choose audio and MIDI devices at runtime without restarting the application:

- **Audio Input** — select the microphone / input device
- **Audio Output** — select the speakers / output device
- **MIDI Input** — select the MIDI controller port

Devices are cycled with `←`/`→` (or `h`/`l`). When a device or audio parameter changes, the stream is automatically restarted.

### Keyboard / Vocoder Mode Toggle

Press **Tab** to switch between two modes:

| Mode | Behaviour |
|------|-----------|
| **Vocoder** (default) | Full vocoder DSP: modulator spectral envelope is imposed onto the carrier. A 4× makeup gain compensates for energy lost in the filter bank. |
| **Keyboard** | Bypasses the vocoder DSP. The raw carrier signal (sine-wave synth) is output directly — useful for testing your MIDI setup. |

The current mode is displayed in the title bar of the TUI.

### Audio Quality

- **Soft clipping** — output is passed through a `tanh()` waveshaper to prevent harsh digital clipping when levels are hot.
- **Makeup gain** — a 4× gain is applied after the vocoder filter bank to compensate for energy loss across the bandpass filters.
- **Zero-allocation audio callbacks** — audio input and output callbacks use a block-recycling scheme (`SharedBuffer`) that reuses previously allocated buffers, avoiding heap allocations on the real-time audio thread.
- **Denormal flushing** — filter state variables and envelope followers flush subnormal floats to zero to prevent CPU performance penalties.
- **Pre-allocated format conversion buffers** — when the audio device uses i16 or u16 sample formats, conversion buffers are allocated once and reused.

### MIDI Running Status

The MIDI parser (`MidiParser`) fully supports the MIDI running-status optimisation: consecutive messages of the same type can omit the status byte. Real-time messages (`0xF8`–`0xFF`) pass through without affecting running status, and system-common messages (`0xF0`–`0xF7`) correctly reset the running-status context.

## Building

### System Dependencies

On Linux, you will need the ALSA development headers for audio and MIDI support (`cpal` and `midir` dependencies).

**Ubuntu/Debian:**
```bash
sudo apt install libasound2-dev pkg-config
```

**Fedora/Nobara:**
```bash
sudo dnf install alsa-lib-devel pkgconf-pkg-config
```

**Arch Linux:**
```bash
sudo pacman -S alsa-lib pkgconf
```

Once dependencies are installed, build the project:

```bash
cargo build
```

Requires Rust (edition 2024). All features are enabled by default.

## Running

```bash
cargo run
```

This opens a terminal UI showing live audio levels, MIDI status, and all active notes. Plug in a MIDI controller and play chords while speaking into your mic.

### Keyboard controls (TUI)

| Key | Action |
|-----|--------|
| `↑` / `k` | Move selection up |
| `↓` / `j` | Move selection down |
| `←` / `h` | Decrease value / cycle device |
| `→` / `l` | Increase value / cycle device |
| `Tab` | Toggle Keyboard / Vocoder mode |
| `q` / `Esc` | Quit |

### Configurable parameters (TUI)

| Parameter | Range | Default |
|-----------|-------|---------|
| Audio Input | available devices | first device |
| Audio Output | available devices | first device |
| MIDI Input | available ports | first port |
| Sample Rate | 22050, 44100, 48000, 96000 Hz | 44100 Hz |
| Buffer Size | 128, 256, 512, 1024, 2048 samples | 512 |
| MIDI Channel | 1–16 | 1 |
| Formant Shift | 0.25×–4.0× (step 0.05) | 1.0× |
| Pitch Shift | −24 to +24 semitones (step 0.5) | 0 |
| Gain | 0%–1000% (step 5%) | 100% |

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
  lib.rs    — re-exports the DSP and MIDI modules
  dsp.rs    — biquad filters, envelope followers, filter banks, Vocoder struct
  audio.rs  — real-time audio input/output with zero-alloc callbacks (feature-gated)
  midi.rs   — MIDI input parsing with running-status support (feature-gated)
  tui.rs    — terminal user interface with device selection (feature-gated)
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

The MIDI parser is also available as a library feature:

```rust
use vocoder::midi::MidiParser;

let mut parser = MidiParser::new();

// Full message
let event = parser.parse(&[0x90, 60, 100]);

// Running status — status byte omitted
let event = parser.parse(&[64, 80]);
```

See `src/dsp.rs` and `src/midi.rs` for full API documentation.
