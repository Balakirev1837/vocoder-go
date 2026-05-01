# Code Review: `src/audio.rs`

**Reviewer:** Automated deep review  
**Date:** 2026-04-30  
**File:** `src/audio.rs` (336 lines)  
**Focus:** Concurrency, error handling (device disconnection), latency bottlenecks

---

## Executive Summary

The audio module has a clean surface API but contains several issues that would cause real problems in production: an unbounded channel that introduces growing latency under load, silent swallowing of stream errors (including device disconnection), allocations on the real-time audio thread, and a config selection path that assumes input and output devices share the same capabilities. The findings are grouped by severity below.

---

## CRITICAL — Concurrency Issues

### C1. Unbounded `mpsc::channel` produces unbounded latency

**Lines:** 126, 208/218/228 (input), 258 (output)

`mpsc::channel()` creates an unbounded channel. In a real-time audio pipeline this is dangerous:

- If the input callback fires faster than the output callback consumes (e.g., different hardware buffer sizes, OS scheduling jitter), the channel grows without bound.
- The output callback calls `rx.try_recv()` in FIFO order — it always gets the **oldest** block, not the newest. This means the processed audio falls further and further behind the live input.
- No backpressure is applied to the producer, so the input callback happily allocates and sends indefinitely.

**Fix:** Use a single-slot or bounded mechanism. A `std::sync::mpsc::sync_channel(1)` (capacity 1) with `try_send` would give "latest wins" semantics and bounded memory. Alternatively, use an `AtomicPtr`-based triple buffer or `crossbeam::atomic::AtomicCell` for lock-free latest-value passing.

### C2. Output callback processes stale data

**Lines:** 258

```rust
let input_block = rx.try_recv().ok();
```

If three input blocks have accumulated, the output callback gets block #1 (the oldest) on the first call, block #2 on the next, etc. For a real-time vocoder this means the output is delayed by `(queued_blocks * buffer_duration)` and the audio is increasingly stale. The system never catches up or skips to the latest.

**Fix:** Drain all pending blocks and use only the most recent one:
```rust
let mut latest = None;
while let Ok(block) = rx.try_recv() {
    latest = Some(block);
}
let input_block = latest;
```

### C3. No synchronization of stream lifetime with process closure

**Lines:** 87–90, 138–141

`AudioIo` holds `_input_stream` and `_output_stream` as plain `Stream` fields. The closure captures `rx` (the `Receiver`) by move. This is correct for ownership, but when `AudioIo` is dropped:
1. Both streams are dropped, which stops the audio threads.
2. If the output callback is currently mid-execution (inside the `process` closure), dropping the stream may tear down the callback mid-frame.

There is no `Drop` impl that explicitly pauses the streams before the struct is torn down, and no guarantee about the ordering of input vs. output stream drop.

**Fix:** Implement `Drop` for `AudioIo` that explicitly calls `.pause()` on both streams before they're dropped, or at minimum document the teardown behavior.

---

## HIGH — Error Handling / Device Disconnection

### E1. Stream error callback only prints to stderr — caller cannot detect failure

**Lines:** 199–201, 250–252

```rust
let err_fn = |err: cpal::StreamError| {
    eprintln!("[audio input error] {}", err);
};
```

When a USB audio device is unplugged, cpal fires this error callback. The `AudioIo` struct has no field or method to expose stream health. The caller (in `main.rs`) only recreates streams when the user explicitly changes devices — **device disconnection goes entirely unnoticed by the application**. The TUI will continue showing "audio running: true" while the streams are dead.

**Fix:** Add a stream-health signaling mechanism, e.g.:
- An `AtomicBool` (or `AtomicU8` with state enum) shared between the error callback and `AudioIo`.
- A method `AudioIo::is_alive(&self) -> bool` that the main loop can poll.
- The main loop should attempt to re-establish streams when `is_alive()` returns false.

### E2. `tx.send()` failure is silently discarded

**Lines:** 208, 218, 228

```rust
let _ = tx.send(block);
```

If the receiver has been dropped (e.g., the output stream failed and was dropped), every subsequent input callback invocation allocates a full `AudioBlock` and attempts a send that always fails. This wastes CPU on the (still-running) input callback thread.

**Fix:** Check the result of `send()`. On failure, set a shared flag that the input stream is orphaned. Optionally, the error callback could signal this as well.

### E3. No recovery path for stream creation failure

**Lines:** 103–142

`AudioIo::new` is all-or-nothing. If the input stream is created successfully but the output stream creation fails, the input stream is dropped silently. If either `play()` call fails (lines 131–136), the other stream may already be running.

**Fix:** Ensure cleanup of partial resources. Consider creating both streams first, then starting both only after both succeed.

### E4. Devices with failing `.name()` are silently hidden

**Lines:** 25, 40

```rust
if let Ok(name) = device.name() {
    list.push(DeviceInfo { name });
}
```

Devices where `.name()` returns `Err` are silently omitted from the list. The user cannot see or select them. This may be fine for most cases, but if a device is partially functional (can be opened but name query fails due to driver bug), it's invisible.

**Fix:** Consider logging a warning for devices whose names can't be queried, or including them with a fallback name like `"<unnamed device>"`.

---

## HIGH — Latency Bottlenecks

### L1. Heap allocation on every input audio callback

**Lines:** 207, 297–306

`deinterleave(data, channels)` allocates a new `Vec<Vec<f32>>` on every input callback invocation. This is called on cpal's real-time audio thread, where allocation can trigger:
- `mmap`/`brk` syscalls (page faults)
- Lock contention in the global allocator
- Priority inversion if the allocator lock is held by a lower-priority thread

For a 512-frame, 2-channel buffer at 44.1 kHz, this fires ~86 times/second, each time allocating ~4 KB.

**Fix:** Pre-allocate a reusable buffer outside the callback and pass it in. A triple-buffer or ring-buffer pattern avoids allocation in the hot path.

### L2. Heap allocation on every output callback for non-f32 formats

**Lines:** 268, 281

```rust
let mut f32_buf = vec![0.0f32; data.len()];
```

Same problem as L1, but on the output side. For I16/U16 formats, a temporary conversion buffer is allocated per callback.

**Fix:** Pre-allocate the conversion buffer once and reuse it.

### L3. `Vec::push` per sample in `deinterleave`

**Lines:** 302–304

```rust
for (c, sample) in frame.iter().enumerate() {
    block[c].push(*sample);
}
```

Although `Vec::with_capacity(frames)` avoids reallocation, `push` still has a branch on `len < capacity` per sample. For the real-time path, this branch is predictable but still overhead. A direct index-based write would be faster.

**Fix:** Pre-size vectors and use index assignment:
```rust
let mut block = vec![vec![0.0f32; frames]; ch];
for (i, frame) in interleaved.chunks_exact(ch).enumerate() {
    for (c, sample) in frame.iter().enumerate() {
        block[c][i] = *sample;
    }
}
```

### L4. Input config is derived from output device capabilities

**Lines:** 124, 127

```rust
let (config, sample_format) = build_low_latency_config(&output_device, io_config)?;
// ...
let input_stream = build_input_stream(&input_device, &config, sample_format, tx)?;
```

`build_low_latency_config` queries only the **output** device's supported configs. The resulting `config` (sample rate, buffer size, sample format) is then applied to the **input** device without checking whether the input device supports it. If the devices are different hardware (very common — built-in mic + USB DAC), the input stream creation will fail with a confusing error.

**Fix:** Query both devices and compute a compatible config. At minimum, validate the config against the input device's supported configs before attempting to create the stream.

### L5. No validation of `buffer_size` config value

**Line:** 186

```rust
stream_config.buffer_size = BufferSize::Fixed(size);
```

If `io_config.buffer_size` is `Some(0)` or some tiny unsupported value, cpal may panic or return an error at stream creation. There's no validation.

**Fix:** Add a minimum bound check (e.g., `buffer_size >= 16`) and warn/error on unreasonable values.

---

## MEDIUM — Numeric Correctness

### N1. I16 → f32 conversion is asymmetric

**Line:** 216

```rust
data.iter().map(|&s| s as f32 / i16::MAX as f32)
```

For `s = -32768`, this yields `-32768.0 / 32767.0 = -1.0000305...`, which is slightly below -1.0. Standard audio practice divides by 32768.0 for symmetric range `[-1.0, +1.0)`.

### N2. U16 → f32 conversion has asymmetric range

**Line:** 228

```rust
(s as f32 - 32768.0) / 32767.0
```

- `u16::MIN (0)` → `-32768.0 / 32767.0 ≈ -1.00003`  
- `32768` → `0.0 / 32767.0 ≈ 0.0`  
- `u16::MAX (65535)` → `32767.0 / 32767.0 = 1.0`

The midpoint (32768) doesn't map to exactly 0.0 (it maps to ~0.00003). Divide by 32768.0 for correct centering.

### N3. f32 → U16 output conversion is slightly asymmetric

**Line:** 284

```rust
(*s * 32767.0 + 32768.0).clamp(0.0, 65535.0) as u16
```

When `s = -1.0`: `(-32767.0 + 32768.0) = 1.0` → output is `1`, not `0`.  
When `s = 0.0`: `32768.0` → output is `32768` (correct midpoint).  
When `s = 1.0`: `65535.0` → output is `65535` (correct max).

For proper `[-1.0, 1.0] → [0, 65535]` mapping: `(*s * 32768.0 + 32768.0).clamp(0.0, 65535.0)`.

---

## LOW — Code Quality / Style

### Q1. `find_input_device` / `find_output_device` create a new host per call

**Lines:** 49, 60

`cpal::default_host()` is called every time. This is potentially non-trivial on some platforms and is redundant when called from `AudioIo::new` which already has a host.

**Fix:** Accept a `&cpal::Host` parameter, or refactor to create the host once and pass it through.

### Q2. Underscore-prefixed struct fields

**Lines:** 88–89

```rust
_input_stream: Stream,
_output_stream: Stream,
```

The `_` prefix suppresses unused-field warnings, which is fine — these fields exist solely to keep the streams alive. However, this also prevents any external access. Consider exposing a health-check method (see E1).

### Q3. `AudioIo` is not `Send` or `Sync`

`cpal::Stream` is `Send` but not `Sync`, so `AudioIo` can be sent between threads but not shared. This is fine for the current use case (held in `main.rs` as a local variable) but should be documented if the API is ever made public.

### Q4. Duplicated code between input/output stream builders

**Lines:** 192–239 and 242–294

The `match sample_format` blocks in `build_input_stream` and `build_output_stream` share the same error handler pattern and format-handling branches. A generic helper or macro would reduce duplication.

### Q5. No documentation on thread safety guarantees

The module does not document which closures run on which threads, what the thread safety requirements are for shared state, or what operations are safe to perform inside the `process` callback. This is critical information for anyone using the API.

---

## Summary Table

| ID   | Severity | Category              | Short Description                                       |
|------|----------|-----------------------|---------------------------------------------------------|
| C1   | CRITICAL | Concurrency           | Unbounded channel → unbounded latency                   |
| C2   | CRITICAL | Concurrency           | Output processes stale (oldest) data, not latest        |
| C3   | HIGH     | Concurrency           | No explicit stream pause on drop; potential mid-frame teardown |
| E1   | HIGH     | Error Handling        | Stream errors invisible to caller (device disconnect)   |
| E2   | MEDIUM   | Error Handling        | `tx.send()` failure silently discarded, wasted CPU      |
| E3   | MEDIUM   | Error Handling        | Partial resource leak on stream creation failure        |
| E4   | LOW      | Error Handling        | Devices with failing `.name()` silently hidden          |
| L1   | HIGH     | Latency               | Heap allocation on every input callback                 |
| L2   | MEDIUM   | Latency               | Heap allocation on every output callback (non-f32)      |
| L3   | LOW      | Latency               | Per-sample `Vec::push` in deinterleave                  |
| L4   | HIGH     | Latency               | Input config derived from output device capabilities    |
| L5   | MEDIUM   | Latency               | No validation of buffer_size config value               |
| N1   | MEDIUM   | Numeric               | I16→f32 conversion asymmetric                          |
| N2   | MEDIUM   | Numeric               | U16→f32 conversion imprecise midpoint                   |
| N3   | MEDIUM   | Numeric               | f32→U16 output conversion slightly off                  |
| Q1   | LOW      | Quality               | Redundant `default_host()` calls                        |
| Q2   | LOW      | Quality               | Underscore-prefixed fields prevent health-check access   |
| Q3   | LOW      | Quality               | Send/Sync not documented                                |
| Q4   | LOW      | Quality               | Duplicated format-match code in builders                |
| Q5   | LOW      | Quality               | No documentation on thread safety / callback constraints|

---

## Recommended Priority Order for Fixes

1. **C1 + C2** — Switch to a bounded/latest-value channel mechanism. This single change fixes the two most critical issues.
2. **E1** — Add stream health signaling so the application can detect and recover from device disconnection.
3. **L1** — Eliminate per-callback allocation with pre-allocated buffers.
4. **L4** — Reconcile input/output device configs before creating streams.
5. **N1–N3** — Fix sample format conversion math.
6. **C3, E2, E3, L2, L3, L5** — Address remaining issues in order of impact.
