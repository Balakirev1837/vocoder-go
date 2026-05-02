# vocoder

A real-time polyphonic channel vocoder written in Go.

It imposes the spectral envelope of a modulator signal (e.g., microphone input) onto a carrier signal (a built-in polyphonic synthesizer driven by MIDI).

## Architecture

The DSP core splits both signals through a bank of bandpass filters, tracks the modulator's energy in each band using envelope followers, and uses those envelopes to gate the matching carrier bands. The result is summed into a single output stream.

### Audio Engine

- **Low-latency I/O**: Uses `malgo` (miniaudio) for duplex audio streams.
- **Zero-allocation callbacks**: The audio processing loop is allocation-free to prevent garbage collection pauses during real-time processing.
- **Precomputed Frequencies**: MIDI note frequencies are precomputed in a lookup table to avoid per-sample `math.Pow` calls.
- **Soft clipping**: Output is passed through a branchless soft-clipper (`x / (1 + |x|)`) to prevent digital clipping.
- **Denormal flushing**: Filter state variables and envelope followers flush subnormal floats to zero to prevent CPU performance penalties.

### Carrier Synthesizer

- **Polyphony**: Supports multiple simultaneous MIDI notes. Carrier oscillators are summed and energy-normalized (`1/√N` scaling) to prevent clipping.
- **Waveforms**: Sine, Sawtooth, Square.

### Terminal UI

Built with `bubbletea` and `lipgloss`, the TUI allows runtime configuration without restarting the application.

- **Device Selection**: Select Audio Input, Audio Output, and MIDI Input devices.
- **Modes**:
  - *Vocoder*: Full DSP pipeline with 4x makeup gain.
  - *Keyboard*: Bypasses vocoder DSP, outputting the raw carrier signal for testing.

## Building & Running

### System Dependencies

On Linux, ALSA development headers are required for audio and MIDI support.

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

### Running

Requires Go 1.22 or later.

```bash
go run ./cmd/vocoder
```

### Keyboard Controls

| Key | Action |
|-----|--------|
| `↑` / `k` | Move selection up |
| `↓` / `j` | Move selection down |
| `←` / `h` | Decrease value / cycle device |
| `→` / `l` | Increase value / cycle device |
| `Tab` | Toggle Keyboard / Vocoder mode |
| `q` / `Esc` | Quit |

### Configurable Parameters

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
| Bands | 8, 12, 16, 20, 24, 32 | 20 |
| Waveform | Sine, Sawtooth, Square | Sine |
| Env Speed | Fast, Medium, Slow | Medium |

## Testing

```bash
go test ./...
```
