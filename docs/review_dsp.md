# Code Review: `src/dsp.rs`

**Reviewer:** Automated deep review  
**Date:** 2026-04-30  
**Scope:** Performance bottlenecks, numerical stability (denormals, infinities, NaN), edge cases in filter banks

---

## Summary

`dsp.rs` implements a classic channel vocoder with biquad bandpass filters, envelope followers, and a `Vocoder` struct that combines analysis and synthesis. The overall structure is clean and well-documented. However, the review identifies several issues of varying severity that could cause runtime panics, silent numerical corruption, performance degradation in real-time contexts, and one public API that is unimplemented.

Findings are organised by severity: **Critical** (will cause crashes or wrong results), **High** (likely to cause problems in production), **Medium** (correctness or performance concern under edge conditions), and **Low** (style, minor improvements).

---

## Critical Findings

### C1. `FilterBank::analyse()` is `unimplemented!()` — will panic at runtime

**Location:** Lines 194–209

```rust
pub fn analyse(&mut self, sample: f64) -> &[f64] {
    // ...
    unimplemented!("use Vocoder instead which combines analysis and synthesis")
}
```

`FilterBank` is a public struct with a public `analyse` method. Any caller who constructs a `FilterBank` and calls `analyse` will get a panic. The method is partially implemented (the loop body runs before the `unimplemented!` macro), meaning the panic is hit *after* doing work, which is worse than failing fast.

**Recommendation:** Either remove the dead code and mark the method with `#[deprecated]` pointing to `Vocoder`, or finish the implementation. If it is intentionally unused, remove the method entirely or gate it behind a feature flag. Do not ship `unimplemented!()` in a public API.

### C2. Division by zero when `q == 0` in `BiquadFilter::bandpass()`

**Location:** Line 45

```rust
let alpha = sin_w0 / (2.0 * q);
```

When `q` is 0.0, this produces `inf` (or `NaN` if `sin_w0` is also 0). The resulting coefficients will be `inf` or `NaN`, which silently poisons all subsequent filter output. No error is raised — the filter simply outputs `NaN` forever.

**Recommendation:** Validate `q > 0` and return an error (e.g., `Result<Self>`) or panic with a clear message. Alternatively, clamp `q` to a sensible minimum (e.g., 0.1).

### C3. NaN/inf propagation from invalid `spread_frequencies` inputs

**Location:** Lines 379–394

```rust
let log_low = low.ln();
let log_high = high.ln();
```

If `low <= 0` or `high <= 0`, `ln()` returns `NaN` (for negative) or `-inf` (for zero). This propagates NaN into every center frequency, then into every biquad coefficient, then into every sample processed. The entire DSP pipeline silently produces `NaN`.

Additionally, if `low > high`, the logarithmic spacing still "works" mathematically but produces center frequencies in descending order, which is almost certainly not intended.

**Recommendation:** Assert or validate `low > 0 && high > 0 && low <= high`. Return an error or panic with a descriptive message.

---

## High Findings

### H1. Denormal (subnormal) floats destroy real-time performance

**Location:** `BiquadFilter::process()` (lines 67–72), `EnvelopeFollower::process()` (lines 122–131)

Denormal (subnormal) floating-point numbers are values very close to zero (|x| < ~2.2e-308 for f64). On most x86 CPUs, operations on denormals are handled in microcode and run 10–100× slower than normal floating-point operations. This is a well-known performance killer in real-time audio.

**Biquad filter:** After processing silence, the filter state `z1` and `z2` decay toward zero through the feedback path. For high-Q filters at low frequencies, the state can linger in the denormal range for thousands of samples.

**Envelope follower:** During the release phase after a loud signal, `self.envelope` decays exponentially toward zero. It asymptotically approaches zero but may spend significant time in the denormal range.

**Impact:** In a real-time audio callback (as used in `main.rs`), a denormal stall can cause buffer underruns (glitches/pops) because the callback takes too long.

**Recommendation:** Flush denormals to zero. Options:
1. Use the CPU's FTZ (flush-to-zero) flag via `std::arch::x86_64::_MM_SET_FLUSH_ZERO_MODE` at the start of the audio callback. This is the industry-standard approach.
2. Manually flush: `if v.abs() < 1e-30 { v = 0.0; }` on the filter state after each sample.
3. Add a small DC offset (noise floor) to prevent the state from reaching zero. This is the "analog" approach but introduces a slight noise floor.

### H2. No validation of Nyquist constraint in `BiquadFilter::bandpass()`

**Location:** Lines 41–63

If `center_freq >= sample_rate / 2.0`, the digital filter design is invalid:
- At `center_freq == sample_rate / 2` (Nyquist): `w0 = π`, `sin(w0) = 0`, `alpha = 0`, so `b0 = b2 = 0`. The filter passes nothing.
- At `center_freq > sample_rate / 2`: `w0 > π`. The bilinear transform is only valid for `w0 ∈ (0, π)`. The resulting filter may be unstable or produce unexpected frequency response.

Currently the code silently produces degenerate or unstable filters.

**Recommendation:** Validate `0 < center_freq < sample_rate / 2`. Clamp, return an error, or at minimum document the constraint.

### H3. Output accumulation has no ceiling — potential for extremely large output

**Location:** `Vocoder::process()`, line 324

```rust
output += self.envelopes[i] * filtered_carrier;
```

With `num_bands` filters (e.g., 20), the output is the sum of all band outputs. If the carrier has broad-spectrum energy and the modulator has strong signal across many bands, the output can accumulate to values much larger than 1.0. The caller in `main.rs` casts to `f32` and writes directly to the audio buffer with no clipping:

```rust
let out = vocoder.process(mod_sample, carrier_sample) as f32;
```

This can produce harsh digital clipping at the DAC.

**Recommendation:** Either normalise the output by dividing by `num_bands` (or `sqrt(num_bands)`), or apply a soft-clipper/limiter. At minimum, document that the caller is responsible for output level management.

### H4. `EnvelopeFollower` negative time constants silently produce wrong behaviour

**Location:** Lines 106–117

```rust
attack: if attack_time > 0.0 {
    (-1.0 / (attack_time * sample_rate)).exp()
} else {
    0.0
},
```

If `attack_time` is negative (a programming error), the branch falls through to `0.0`, meaning "instantaneous attack". This silently masks a bug. The same applies to `release_time`.

**Recommendation:** Validate that time constants are non-negative (or strictly positive). At minimum, use `debug_assert!` to catch bugs during development.

---

## Medium Findings

### M1. `Vocoder::process()` inner loop is not vectorisation-friendly

**Location:** Lines 313–325

The inner loop accesses three separate `Vec`s by index:

```rust
for i in 0..self.num_bands {
    let filtered_mod = self.analysis_filters[i].process(modulator);
    self.envelopes[i] = self.envelope_followers[i].process(filtered_mod);
    let filtered_carrier = self.synthesis_filters[i].process(carrier);
    output += self.envelopes[i] * filtered_carrier;
}
```

Each `BiquadFilter::process()` has a data dependency on its own previous state (`z1`, `z2`), so the individual filters cannot be unrolled across iterations. However, the coefficient loads (`self.analysis_filters[i]`) are sequential access through a `Vec`, which is cache-friendly and prefetchable. The main concern is that the `b0/b1/b2/a1/a2` coefficients and the `z1/z2` state are in the same struct — since each filter has only 7 f64 values (56 bytes), the entire analysis bank for 20 bands is only ~1120 bytes, fitting comfortably in L1 cache. So this is acceptable for the current band count.

**Recommendation:** If the band count grows significantly (64+), consider SoA (struct-of-arrays) layout to improve prefetch: separate `Vec<f64>` arrays for `b0`, `b1`, `b2`, `a1`, `a2`, `z1`, `z2`.

### M2. f64 throughout but f32 at audio boundary — conversion overhead

**Location:** `main.rs` line 84

```rust
let out = vocoder.process(mod_sample, carrier_sample) as f32;
```

Every sample undergoes `f64 -> f32` truncation on output and `f32 -> f64` promotion on input. The extra precision of f64 is beneficial for IIR filter coefficient accuracy, but the conversion cost is non-trivial in a tight real-time loop. With 20 bands at 44.1 kHz, the computation per sample is ~20 biquad evaluations × 5 FLOPs each = ~100 FLOPs per sample, plus envelope following. The conversion is a small fraction of this.

**Recommendation:** The f64 choice is defensible for numerical stability in IIR filters. Keep it, but document the design rationale. Consider an f32 mode for platforms where the performance difference matters (e.g., embedded).

### M3. Redundant `envelopes` Vec in `Vocoder`

**Location:** Line 252, 318–319

```rust
envelopes: Vec<f64>,  // declared in struct
// ...
self.envelopes[i] = self.envelope_followers[i].process(filtered_mod);
```

Each `EnvelopeFollower` already stores its current `envelope` value. The `Vocoder.envelopes` Vec duplicates this state, requiring a write to both the follower and the Vec on every sample. The only consumer is `envelopes()` for introspection, which could read from the followers directly.

**Recommendation:** Remove the `envelopes` field. Add an `envelopes()` method that collects from followers on demand (for the rare introspection case), or store a `Vec<&f64>` referencing follower internals. This saves one write per band per sample.

### M4. `spread_frequencies` with `low == high` produces degenerate filter bank

**Location:** Lines 379–394

If `low == high`, all center frequencies are identical. Every filter has the same coefficients, so the bank provides no spectral resolution. The vocoder output becomes equivalent to a single bandpass filter with gain equal to the band count.

**Recommendation:** Validate `low < high` (strict inequality). If equal, either return an error or produce a single band.

### M5. `process_block` silently truncates on buffer length mismatch

**Location:** Lines 334–339

```rust
let len = modulator.len().min(carrier.len()).min(output.len());
```

If `modulator` has 512 samples but `output` has only 256, half the modulator is silently dropped. This is documented ("must have the same length") but not enforced.

**Recommendation:** Add a `debug_assert!` that all three lengths are equal. This catches bugs during development without affecting release performance.

---

## Low Findings

### L1. Missing `#[inline]` on small accessors

`EnvelopeFollower::value()` (line 134) and `EnvelopeFollower::reset()` (line 139) are trivial methods not marked `#[inline]`. While the compiler will likely inline them anyway in release mode, explicit `#[inline]` is conventional for hot-path accessors in DSP code.

### L2. No `#[inline]` on `BiquadFilter::bandpass()` constructor

The constructor performs trigonometric computations but is only called at init time, so inlining is unimportant. However, for consistency with the `#[inline]` on `process()`, it could be annotated.

### L3. `Vocoder::num_bands` field is redundant with `analysis_filters.len()`

The field `num_bands` duplicates information available from any of the `Vec` fields. This introduces a consistency risk: if a bug causes the Vecs to have different lengths, `num_bands` could disagree.

**Recommendation:** Remove the field and derive it: `fn num_bands(&self) -> usize { self.analysis_filters.len() }`.

### L4. Tests use imprecise floating-point assertions

**Location:** Line 530

```rust
assert!(out.abs() < 1e-12, "silence should produce silence, got {out}");
```

For a 100-sample run with zero input through biquad filters starting from zero state, the output should be *exactly* 0.0 (not approximately). Using `< 1e-12` masks potential issues. Consider `assert_eq!(out, 0.0)` for exact comparison, or at minimum tighten the tolerance.

### L5. `test_biquad_bandpass_resonates_at_center` feeds DC, not noise

**Location:** Lines 446–448

```rust
for _ in 0..64 {
    bp.process(1.0);
}
```

The comment says "short burst of white noise" but the test feeds DC (constant 1.0). This doesn't invalidate the test — the filter still rings — but the comment is misleading.

### L6. No documentation of thread safety constraints

The `Vocoder` struct is `!Sync` (contains `Vec<_>` with interior mutability through `&mut self`). It is `Send` (all fields are `Send`). The audio callback in `main.rs` moves it into the callback closure, which is correct. But there is no documentation that `Vocoder` is designed for single-threaded use and must not be shared across threads.

### L7. Carrier is a pure sine — limited vocoder effect

**Location:** `main.rs` lines 72–81 (usage, not `dsp.rs` itself)

The carrier in `main.rs` is a single sine wave at the MIDI note frequency. A single sine has energy at only one frequency. After passing through 20 bandpass filters, only 1–2 filters will pass non-trivial signal. The vocoder effect will be extremely weak compared to using a harmonically rich carrier (e.g., sawtooth, pulse wave, or noise).

**Recommendation:** This is a usage issue, not a DSP bug, but worth documenting as a known limitation or adding a richer oscillator.

---

## Detailed Numerical Analysis

### Biquad coefficient range analysis

For `center_freq ∈ (0, sr/2)` and `q ∈ (0, ∞)`:

| Parameter | Range | Notes |
|-----------|-------|-------|
| `w0` | `(0, π)` | Outside this range the design is invalid |
| `alpha` | `(0, 0.5]` | At low freq with high Q, alpha is very small → narrow bandwidth |
| `a0 = 1 + alpha` | `(1, 1.5]` | Normalisation denominator |
| `a1 = -2·cos(w0)` | `[-2, 2]` | Pole angle; magnitude < 2 for stability |
| `a2 = 1 - alpha` | `[0.5, 1)` | Poles inside unit circle → stable |
| `b0 = alpha/a0` | `(0, 0.5)` | Forward gain |
| `b2 = -alpha/a0` | `(-0.5, 0)` | Forward gain (negative) |

Stability is guaranteed as long as `|a2| < 1` and `|a1| < 2` and `a1 + a2 < 1`, which holds for all valid inputs. However, as `Q → ∞`, `alpha → 0` and the poles approach the unit circle. In finite precision, very high Q can cause marginal instability.

### Envelope follower asymptotic behaviour

With constant input `x` and coefficient `c`:
- `envelope[n+1] = c · envelope[n] + (1-c) · |x|`
- Steady state: `envelope = |x|` (correct)
- Time constant: `τ = -1 / ln(c)` samples

When input is zero: `envelope[n+1] = c · envelope[n]`, so envelope decays as `c^n`. For `c ≈ 1` (long release), this takes many samples to reach zero — and passes through the denormal range.

---

## Recommended Priority of Fixes

| Priority | Finding | Effort |
|----------|---------|--------|
| 1 | C1: Remove/fix `unimplemented!()` in `analyse()` | Low |
| 2 | C2: Validate `q > 0` in bandpass | Low |
| 3 | C3: Validate frequency inputs in `spread_frequencies` | Low |
| 4 | H1: Flush denormals (FTZ flag or manual) | Medium |
| 5 | H2: Validate Nyquist constraint | Low |
| 6 | H3: Normalise or document output level | Low |
| 7 | H4: Validate envelope time constants | Low |
| 8 | M3: Remove redundant `envelopes` Vec | Low |
| 9 | M5: Add `debug_assert!` on buffer lengths | Low |
| 10 | M4: Validate `low < high` | Low |

---

## Conclusion

The module is a solid and readable implementation of a channel vocoder. The most urgent issues are the unimplemented `analyse()` method (C1), the lack of input validation (C2, C3) which can produce silent NaN corruption, and the denormal performance hazard (H1) which will cause audio glitches in production. All critical and high findings are straightforward to fix and should be addressed before the code is used in a real-time audio pipeline.
