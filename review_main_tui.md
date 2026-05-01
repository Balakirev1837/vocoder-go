# Code Review: `main.rs` & `tui.rs`

**Reviewer:** Critter (automated deep review)
**Date:** 2026-04-30
**Scope:** State sharing (atomics), UI responsiveness, thread safety during device switching
**Approach:** Adversarial — treat the code with distrust.

---

## Summary

The application has a solid foundation: the three-atomics pattern for cross-thread communication is sound, the TUI event loop is well-structured, and device switching correctly drops old streams before building new ones. However, there are several real bugs, phantom features, and edge-case hazards that should be addressed before the codebase grows further.

**Severity key:** 🔴 Bug (wrong behavior) · 🟠 Hazard (could bite under pressure) · 🟡 Concern (code smell / maintainability)

---

## 1. Hardcoded Audio Parameters vs. Configurable UI

### 🔴 `build_audio_io` ignores `Config` entirely

`main.rs:30-43` — The audio callback and `AudioIoConfig` are built with hardcoded values:

```rust
let mut vocoder = dsp::Vocoder::new(
    20,          // bands
    200.0, 8000.0,  // freq range
    4.0,          // Q
    0.001, 0.05,    // attack / release
    44100.0,        // sample_rate
);
// ...
let config = audio::AudioIoConfig {
    sample_rate: Some(44100),
    buffer_size: Some(512),
};
```

The TUI exposes and allows adjusting **six parameters** that have zero effect on the audio pipeline:

| Config Field | Adjustable in TUI | Wired to DSP? |
|---|---|---|
| `sample_rate` | Yes | **No** — hardcoded to 44100 |
| `buffer_size` | Yes | **No** — hardcoded to 512 |
| `formant_shift` | Yes | **No** — never passed to `Vocoder` |
| `pitch_shift` | Yes | **No** — never used anywhere |
| `gain` | Yes | **No** — output is never scaled |
| `midi_channel` | Yes | **No** — MIDI events are unfiltered |

Every time the user adjusts these parameters, the UI reflects the change but the audio behavior is unchanged. This is the most significant disconnect in the codebase.

### 🟠 Carrier pitch will be wrong at non-44100 Hz sample rates

`main.rs:38,74` — The carrier oscillator uses a hardcoded `sample_rate_f64 = 44100.0` for phase increment calculation:

```rust
let sample_rate_f64: f64 = 44100.0;
// ...
phase += 2.0 * std::f64::consts::PI * carrier_freq / sample_rate_f64;
```

If the actual audio device runs at 48 kHz (or any rate other than 44100), the carrier frequency will be detuned by `44100/actual_rate`. This should read the actual sample rate from the stream config.

---

## 2. Thread Safety & State Sharing

### Shared Atomics Assessment

Three `Arc<Atomic*>` values bridge the audio callback thread, MIDI callback thread, and the TUI main thread:

| Atomic | Writers | Readers | `Ordering` |
|---|---|---|---|
| `active_note: AtomicU8` | MIDI thread (`store`), TUI thread (NoteOff `store`) | Audio callback (`load`), TUI thread (`load`) | `Relaxed` |
| `input_level: AtomicU32` | Audio callback (`store`) | TUI thread (`load`) | `Relaxed` |
| `output_level: AtomicU32` | Audio callback (`store`) | TUI thread (`load`) | `Relaxed` |

**`Ordering::Relaxed` is correct here.** These are single-word indicators, not part of a happens-before protocol. The audio callback writes a value; the TUI reads it eventually. No additional synchronization is needed.

### 🟡 Level meters read non-atomically as a pair

`main.rs:220-221`:

```rust
app.status.input_level = f32::from_bits(input_level.load(Ordering::Relaxed));
app.status.output_level = f32::from_bits(output_level.load(Ordering::Relaxed));
```

The two loads are not atomic with respect to each other. The TUI might read `input_level` from buffer *N* and `output_level` from buffer *N-1* or *N+1*. For level meters this is cosmetically acceptable, but if these values were ever used for gain control or feedback, the skew would matter. Worth a comment.

### 🟡 NoteOff race: TUI and MIDI thread both write `active_note`

`main.rs:199-204`:

```rust
midi::MidiEvent::NoteOn { note, .. } => {
    active_note.store(note, Ordering::Relaxed);
}
midi::MidiEvent::NoteOff { note, .. } => {
    if active_note.load(Ordering::Relaxed) == note {
        active_note.store(255, Ordering::Relaxed);
    }
}
```

This is a classic TOCTOU (time-of-check/time-of-use) pattern. Between the `load` and the `store` on line 203, the MIDI thread could write a new NoteOn. The NoteOff would then clear the newly playing note. In practice, the TUI loop runs at ~20 Hz and MIDI events arrive asynchronously, so the window is small — but it exists.

A correct solution would be to compare-and-swap (`compare_exchange`) instead of load-then-store, or to route all `active_note` mutations through a single owner.

### ✅ Device switch correctly drops before rebuilding

`main.rs:242-254`:

```rust
// Drop old stream first
_audio_io = None;

let input_name = app.config.audio_input_device.as_deref();
let output_name = app.config.audio_output_device.as_deref();
match build_audio_io(...) { ... }
```

This is correct. The old `AudioIo` (which holds both `Stream` objects) is dropped, which stops both audio callbacks. Only then is a new `AudioIo` built with fresh callbacks that clone the same `Arc<Atomic*>` handles. There is no concurrent access to the `Vocoder` or `phase` state because those are captured by-value into each callback closure. **No thread safety issue here.**

---

## 3. Device Switching Hazards

### 🔴 Initial device mismatch

On startup:

1. `App::new()` auto-selects the first enumerated device: `config.audio_input_device = Some(first.clone())` (`tui.rs:180-182`)
2. `build_audio_io()` is called with `None, None` — using the system default device (`main.rs:123`)
3. `prev_audio_input` is initialized from `app.config.audio_input_device` — i.e., `Some("first_device")` (`main.rs:186`)

If the system default input device differs from the first enumerated input device, the app runs audio on the default device but displays the first enumerated device name. The user believes they're hearing device A but are actually hearing device B. Worse, switching away from and back to device A won't trigger a restart (because `prev_audio_input` already matches), so the mismatch persists.

**Fix:** Either start audio with the same device the config selects, or initialize `prev_audio_input` to `None` to match the initial `build_audio_io` call.

### 🟠 Device lists are stale

`main.rs:158-173` — `audio_inputs`, `audio_outputs`, and `midi_ports` are enumerated once before the TUI loop. They are never refreshed. If a USB audio interface or MIDI controller is hot-plugged, the user cannot select it without restarting the application.

### 🟠 No recovery from failed device switch

`main.rs:253-261` — If `build_audio_io` fails during a device switch:

```rust
Err(e) => {
    eprintln!("Audio restart failed: {}", e);
    audio_running = false;
}
```

- The error goes to `stderr`, which is invisible while the TUI has the alternate screen buffer active.
- The old streams are already dropped (`_audio_io = None`), so there's no fallback.
- The user sees "Stopped" but cannot tell why or recover without restarting the app.

**Recommendation:** Store the error in a field on `App` (or `Status`) and display it in the TUI. Consider falling back to the previous device.

---

## 4. MIDI Handling

### 🟡 Last-note priority with stuck-note risk

`main.rs:201-204` — The NoteOff handler only clears `active_note` if it matches:

```rust
if active_note.load(Ordering::Relaxed) == note {
    active_note.store(255, Ordering::Relaxed);
}
```

This implements "last note priority" — if NoteOn(60) → NoteOn(64) → NoteOff(60), the note stays at 64. Correct. But if NoteOn(64) → NoteOff(64) is missed (e.g., MIDI cable disconnect), the note sticks forever. Consider adding an "all notes off" panic key (e.g., pressing Space).

### 🟡 MIDI channel filtering is not implemented

`midi.rs` parses the channel from every event, but `main.rs` never filters by `app.config.midi_channel`. Adjusting "MIDI Channel" in the TUI has no effect on which channel's notes are processed.

---

## 5. TUI Rendering & Responsiveness

### ✅ 50ms poll timeout is reasonable

`main.rs:227`:

```rust
if event::poll(Duration::from_millis(50))? {
```

50ms = 20 FPS minimum. The `terminal.draw()` call happens every iteration regardless of input, so the level meters and status updates render at ~20 Hz. This is adequate for a terminal UI.

### 🟡 `render_config` clones `ListState` on every frame

`tui.rs:393`:

```rust
f.render_stateful_widget(config_list, area, &mut app.list_state.clone());
```

This clones the `ListState` every frame (60+ times/sec). The `render_stateful_widget` API requires `&mut ListState` so it can update the view offset. Cloning defeats this — the widget can't adjust the view offset for scrolling. For 9 items this is invisible, but it's still unnecessary allocation. Use `&mut app.list_state` directly.

### 🟠 CPU usage is always 0%

`Status::cpu_usage` is initialized to `0.0` and never written to anywhere in the codebase. The TUI renders a CPU gauge (`render_cpu_and_note`) that will always show 0%. This is a phantom feature — either wire it up or remove the dead display.

### 🟡 Dead code: `tui::run()` function

`tui.rs:597-623` — The `run()` function is a standalone TUI event loop marked `#[allow(dead_code)]`. It:
- Uses a 100ms poll timeout (slower than the main loop's 50ms)
- Doesn't handle MIDI events
- Doesn't refresh shared state (`input_level`, `output_level`, etc.)
- Doesn't support device switching

This appears to be an early prototype that was superseded by the inline loop in `main.rs`. It should be removed to avoid confusion, or the main loop logic should be extracted into it to eliminate duplication.

---

## 6. Error Handling & Robustness

### 🟡 Terminal restore on panic

If the main loop panics (e.g., from a `?` propagation after `terminal.draw()` fails), `disable_raw_mode()` and `LeaveAlternateScreen` are never called. The user's terminal is left in a broken state. Wrap the TUI block in a function that uses a `Drop` guard or `scopeguard` to ensure terminal restore.

### 🟡 No graceful shutdown of MIDI on exit

When `should_quit` becomes true, the loop exits. `_audio_io` (if `#[cfg(audio)]`) is dropped naturally. `midi_handle` is also dropped. This is fine for the happy path, but if the MIDI connection thread is in the middle of processing, the drop behavior depends on `midir`'s implementation. Consider explicitly disconnecting before exit.

---

## 7. Minor Issues

| Location | Issue |
|---|---|
| `tui.rs:141` | `MidiChannel` clamp range is `1..=16`, stored as `u8`. Standard MIDI channels are `0..=15`. If this value is ever used for filtering, it must be converted. The display says "Channel 1" for what is actually channel 0 in the protocol. This is a common UI convention but should be documented. |
| `main.rs:37-38` | `phase` and `sample_rate_f64` are separate mutable variables in the closure. They could drift if someone edits one without the other. Consider bundling them into a small struct. |
| `main.rs:56-61` | The `note < 128` guard is correct but the else branch silently maps any value ≥ 128 to 0.0 Hz (silence). The only "invalid" value in practice is 255 (the sentinel). Consider an explicit `note == 255` check instead. |
| `tui.rs:30-44` | `Config::default()` sets `midi_channel: 1`. Since the value is never used, this is cosmetic, but if it were wired up, the default would skip channel 0 events. |

---

## Findings Summary

| # | Severity | Description |
|---|---|---|
| 1 | 🔴 | `build_audio_io` hardcodes all audio/DSP parameters; TUI config changes have no effect |
| 2 | 🔴 | Initial audio device may not match the device shown in the TUI |
| 3 | 🟠 | Carrier pitch uses hardcoded 44100 Hz; wrong pitch at other sample rates |
| 4 | 🟠 | CPU usage meter is always 0% (never wired) |
| 5 | 🟠 | Device lists never refreshed after startup |
| 6 | 🟠 | Device switch failure is invisible to the user (stderr in alternate screen) |
| 7 | 🟠 | No recovery/fallback from failed device switch |
| 8 | 🟡 | TOCTOU race on `active_note` between NoteOff check and store |
| 9 | 🟡 | Level meter reads are non-atomic as a pair |
| 10 | 🟡 | MIDI channel filtering not implemented despite UI for it |
| 11 | 🟡 | `ListState` cloned every frame instead of passed by `&mut` |
| 12 | 🟡 | `tui::run()` is dead code duplicating the main loop |
| 13 | 🟡 | Terminal not restored on panic |
| 14 | 🟡 | No "all notes off" panic key for stuck MIDI notes |

---

## Recommended Priority

1. **Wire config to DSP** (#1) — this is the biggest user-facing lie in the current UI
2. **Fix initial device mismatch** (#2) — straightforward one-line fix
3. **Fix hardcoded sample rate in carrier** (#3) — will produce audibly wrong pitch at 48 kHz
4. **Remove or implement CPU meter** (#4) — dead UI element erodes trust
5. **Show errors in TUI** (#6) — users can't debug what they can't see
