# Code Review: `src/midi.rs`

**Reviewer:** Critter agent (automated deep review)
**Date:** 2026-04-30
**File:** `src/midi.rs` (250 lines)
**Scope:** MIDI parsing edge cases, connection-drop handling, thread safety

---

## Summary

`midi.rs` provides three public items: a `MidiEvent` enum, a `parse_midi_message` parser, and a `start_midi_input` function that connects to a MIDI port and returns events over an `mpsc` channel. The code is clean and well-structured. However, a distrustful review reveals several correctness risks, robustness gaps, and design concerns that could bite under real-world MIDI workloads.

---

## 1. MIDI Parsing (`parse_midi_message`)

### 1.1 [HIGH] No handling of running status

MIDI's "running status" optimization allows a sender to omit the status byte on consecutive messages of the same type. For example:

```
90 3C 64   // Note On, channel 0, middle C, velocity 100
3D 64      // Note On, channel 0, note D4, velocity 100 (running status: reuses 0x90)
```

`parse_midi_message` is stateless — it has no memory of the previous status byte. If `midir`'s backend (ALSA Raw MIDI / CoreMIDI / WinMM) does not pre-process running status, the parser will see a bare `[0x3D, 0x64]` slice, interpret `0x3D` as a status byte (which it isn't), and silently produce `Unknown`.

**Impact:** On Linux ALSA Raw MIDI, running status is often passed through unprocessed. This can cause dropped notes in real sessions.

**Recommendation:** Either (a) add stateful running-status tracking, or (b) document that the caller/midir backend must normalize messages, or (c) use `midir`'s `Ignore` flags to filter at the input level.

### 1.2 [MEDIUM] SysEx messages silently fall through

SysEx messages start with `0xF0` and are variable-length, ending with `0xF7`. The current parser's match on `status & 0xF0` means a SysEx start byte `0xF0` falls into the `_ => Unknown` arm — which is correct. However, a SysEx message body could contain arbitrary bytes including bytes in the `0x00–0xEF` range. If `midir` ever delivers SysEx payload bytes individually (it shouldn't, but the contract isn't checked), those bytes would be misinterpreted as short voice messages.

**Recommendation:** Add an explicit early return for `status >= 0xF0` (system messages) before the voice-message match, with a comment explaining why.

### 1.3 [MEDIUM] No validation of data byte ranges

MIDI data bytes must be in `0x00–0x7F` (7 bits). The parser does not validate or mask `data[1]` and `data[2]`. If a corrupted or malformed message contains `0x80–0xFF` in a data byte position, the parser will accept it without complaint, producing nonsensical `MidiEvent` values (e.g., `note: 200`, `velocity: 255`).

**Impact:** Downstream code in `main.rs` compares `note < 128` to decide whether a note is active, so a corrupted note value of `200` would be treated as "no note" — silently dropping the event. This is arguably safe, but the parser is lying about what it parsed.

**Recommendation:** Mask data bytes with `& 0x7F`, or at minimum assert/document the assumption that data bytes are always < 128.

### 1.4 [LOW] Two-byte voice messages silently ignored

Program Change (`0xC0–0xCF`) and Channel Pressure / Aftertouch (`0xD0–0xDF`) are 2-byte voice messages. The parser requires `data.len() >= 3` for all recognized types, so these correctly fall through to `Unknown`. This is fine for a vocoder, but worth documenting as intentional.

### 1.5 [LOW] Pitch bend minimum is untested

The parser handles pitch bend correctly: `(msb << 7) | lsb - 8192`. The test suite covers center (`0x00, 0x40` → 0) and max (`0x7F, 0x7F` → 8064), but not the minimum (`0x00, 0x00` → -8192).

---

## 2. Connection Drops & Error Handling

### 2.1 [HIGH] No detection of MIDI device disconnection

When a MIDI device is physically disconnected (USB cable pulled, Bluetooth dropout), `midir` closes the connection internally. The `MidiInputConnection` held inside `MidiInputHandle` remains in memory, but the callback stops firing. The `receiver` channel will simply return `Err(mpsc::TryRecvError::Empty)` on every `try_recv()` call.

In `main.rs`, `midi_connected` is set to `true` at startup and never updated to `false` on disconnect. The TUI will continue showing "MIDI: Connected" even after the device is long gone.

**Impact:** User has no feedback that MIDI has stopped working. No reconnection is attempted.

**Recommendation:**
- Use a separate "heartbeat" mechanism: the main loop can track time since last MIDI event and mark the connection as suspect after a configurable timeout.
- Alternatively, periodically call `list_midi_input_ports()` and check if the connected port still exists.
- Consider wrapping `MidiInputHandle` in a type that detects receiver closure (`TryRecvError::Disconnected`).

### 2.2 [MEDIUM] `mpsc::channel()` is unbounded — potential memory growth

The channel created on line 137 is an unbounded `mpsc::channel`. Under normal operation, the TUI loop drains it every 50 ms iteration. But if the main thread blocks (e.g., terminal redraw stalls, CPU spike), MIDI events (especially MIDI clock at 24 PPQN × 300 BPM ≈ 120 events/sec) could accumulate.

**Impact:** In pathological cases, memory could grow without bound. In practice, this is low risk for a vocoder.

**Recommendation:** Consider `mpsc::sync_channel` with a bounded capacity (e.g., 256). When the channel is full, `sender.send()` would fail, and the existing `let _ = sender.send(event)` would silently drop the event — which is acceptable for real-time MIDI.

### 2.3 [LOW] `start_midi_input` error messages lose port name context

Line 129: `anyhow!("MIDI input port '{}' not found", name)` is good. But line 150: `anyhow!("MIDI connection failed: {e}")` does not include the port name, making debugging harder when multiple ports exist.

### 2.4 [LOW] `list_midi_input_ports` and `start_midi_input` create separate `MidiInput` instances

Both functions call `MidiInput::new("vocoder")` independently. This creates two separate `MidiInput` objects. The port list obtained from one instance is used to find ports by name in a string comparison — not by port identity. If the system's port list changes between the `list` call and the `start` call, the wrong port could be opened.

**Impact:** Race condition window is small (typically seconds between TUI setup and user selection). Low severity.

---

## 3. Thread Safety

### 3.1 [OK] `Sender<MidiEvent>` in callback — correct ownership

The `Sender` is moved into the `midir` callback closure. `Sender<T>` is `Send` but not `Sync`, so only one thread (the MIDI callback thread) can send at a time. This is correct. No data races possible.

### 3.2 [OK] `parse_midi_message` is pure

The parser is a pure function with no shared mutable state. Safe to call from any thread, including the real-time MIDI callback.

### 3.3 [OK] Drop ordering of `MidiInputHandle`

Fields are dropped in declaration order: `connection` first, then `receiver`. When `connection` drops, `midir` disconnects the port and drops the closure (which contains the `Sender`). The `Sender` is dropped, closing the channel. Then `receiver` is dropped. Any in-flight events still in the channel buffer are lost, which is acceptable.

### 3.4 [OK] `Ordering::Relaxed` on `active_note` in main.rs

The `AtomicU8` for `active_note` uses `Relaxed` ordering. This is correct here because:
- There's only one writer (the main thread, via the channel read)
- The note value is independent of other shared state
- No memory ordering guarantees are needed relative to other atomics

### 3.5 [MEDIUM] Monophonic voice allocation bug (main.rs, not midi.rs)

While this is in `main.rs`, it's a direct consequence of how `MidiEvent` is consumed:

```rust
MidiEvent::NoteOn { note, .. } => {
    active_note.store(note, Ordering::Relaxed);
}
MidiEvent::NoteOff { note, .. } => {
    if active_note.load(Ordering::Relaxed) == note {
        active_note.store(255, Ordering::Relaxed);
    }
}
```

Scenario: User presses C4 (active = 60), then E4 (active = 64, overwrites C4), then releases E4 (NoteOff 64 matches → active = 255). Now the user is still holding C4 but no sound plays — a "lost note". This is the classic monophonic last-note priority issue.

**Recommendation:** Maintain a stack (Vec) of held notes. On NoteOn, push. On NoteOff, remove. `active_note` = top of stack, or 255 if empty. This provides last-note priority with correct release behavior.

---

## 4. Test Coverage Gaps

The existing tests cover the happy paths well. Missing test cases:

| Test Case | Input | Expected | Why |
|---|---|---|---|
| Pitch bend minimum | `[0xE0, 0x00, 0x00]` | `PitchBend { value: -8192 }` | Boundary value |
| Program Change (2-byte) | `[0xC0, 5]` | `Unknown` | Shouldn't parse as 3-byte |
| Channel Pressure (2-byte) | `[0xD1, 100]` | `Unknown` | Shouldn't parse as 3-byte |
| SysEx start | `[0xF0, 0x01, 0x02]` | `Unknown` | System exclusive |
| Real-time Clock | `[0xF8]` | `Unknown` | Single-byte real-time |
| Active Sensing | `[0xFE]` | `Unknown` | Single-byte real-time |
| Note on channel 15 | `[0x9F, 60, 100]` | `NoteOn { channel: 15, ... }` | Boundary channel |
| Corrupted data byte >= 128 | `[0x90, 0x80, 0x90]` | Behavior undefined | Should at least not panic |
| Two-byte truncated voice msg | `[0x90, 60]` | `Unknown` | len < 3 |
| `try_recv` on empty channel | (no MIDI sent) | `Err(TryRecvError::Empty)` | Channel behavior |

---

## 5. Findings Summary

| # | Severity | Category | Description |
|---|----------|----------|-------------|
| 1.1 | HIGH | Parsing | No running-status handling — dropped notes on Linux ALSA |
| 2.1 | HIGH | Robustness | No disconnection detection — TUI shows stale "connected" status |
| 1.2 | MEDIUM | Parsing | SysEx messages not explicitly filtered |
| 1.3 | MEDIUM | Parsing | No validation of data byte ranges (should be 0–127) |
| 2.2 | MEDIUM | Robustness | Unbounded channel — potential memory growth |
| 3.5 | MEDIUM | Design | Monophonic voice allocation loses held notes |
| 1.4 | LOW | Parsing | Two-byte messages silently ignored (by design, undocumented) |
| 1.5 | LOW | Testing | Pitch bend minimum untested |
| 2.3 | LOW | DX | Error message omits port name on connection failure |
| 2.4 | LOW | Robustness | Port list/connection race window |
| 3.1 | OK | Thread Safety | Sender ownership in callback — correct |
| 3.2 | OK | Thread Safety | Parser is pure — correct |
| 3.3 | OK | Thread Safety | Drop ordering — correct |
| 3.4 | OK | Thread Safety | Relaxed ordering — appropriate |

---

## 6. Recommended Priority

1. **Immediate:** Investigate whether `midir` normalizes running status on each target platform. If not, add stateful parsing or an adapter layer. This is the most likely source of real-world bugs.
2. **Short-term:** Add disconnection detection (check for `TryRecvError::Disconnected` in the TUI loop, or use a heartbeat timeout).
3. **Short-term:** Add monophonic note stack in `main.rs` for correct last-note priority.
4. **Nice-to-have:** Switch to bounded `sync_channel`, add data-byte validation, expand test suite.
