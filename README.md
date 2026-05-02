# vocoder (Go Edition)

A real-time polyphonic channel vocoder written in Go, featuring a beautiful terminal user interface built with [Bubble Tea](https://github.com/charmbracelet/bubbletea) and [Lipgloss](https://github.com/charmbracelet/lipgloss).

Takes a **modulator** signal (e.g. microphone / voice) and a **carrier** signal (a synthesizer driven by MIDI — with full **chord / polyphony** support) and imposes the spectral envelope of the modulator onto the carrier — the classic robot-voice effect.

## How it works

The DSP core splits both signals through a bank of bandpass filters, tracks the modulator's energy in each band with envelope followers, and uses those envelopes to gate the matching carrier bands. The result is summed back into a single output stream.

## Features

### Polyphony (Chords) & Waveforms

Multiple MIDI notes can sound simultaneously. The carrier is synthesised as a sum of oscillators — one per active note — with energy-normalised mixing (`1/√N` scaling) to prevent clipping when playing chords.

You can choose between three carrier waveforms:
- **Sine**: Smooth and classic.
- **Sawtooth**: Rich in harmonics, perfect for aggressive, robotic Daft Punk-style vocoding.
- **Square**: Hollow and retro.

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
| **Keyboard** | Bypasses the vocoder DSP. The raw carrier signal (synth) is output directly — useful for testing your MIDI setup. |

### Audio Quality & Performance

- **Low-latency Audio**: Powered by [malgo](https://github.com/gen2brain/malgo) (miniaudio) for rock-solid, low-latency audio I/O.
- **Zero-allocation audio callbacks**: The audio processing loop is completely allocation-free, ensuring the Go garbage collector never interrupts your audio stream.
- **Precomputed MIDI Frequencies**: MIDI note frequencies are precomputed in a lookup table to save thousands of `math.Pow` calls per second.
- **Fast Soft clipping**: Output is passed through a fast, branchless soft-clipper (`x / (1 + |x|)`) to prevent harsh digital clipping when levels are hot.
- **Denormal flushing**: Filter state variables and envelope followers flush subnormal floats to zero to prevent CPU performance penalties.

### Beautiful Terminal UI

Built with the Charm ecosystem (`bubbletea`, `lipgloss`, `bubbles`), the UI features:
- Colorful, rounded-border panels.
- Smooth gradient progress bars for audio input/output levels.
- A live spinner animation that indicates when the audio engine is actively running.

## Building & Running

### System Dependencies

On Linux, you will need the ALSA development headers for audio and MIDI support.

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
| Bands | 8, 12, 16, 20, 24, 32 | 20 |
| Waveform | Sine, Sawtooth, Square | Sine |
| Env Speed | Fast, Medium, Slow | Medium |

## Testing

```bash
go test ./...
```
