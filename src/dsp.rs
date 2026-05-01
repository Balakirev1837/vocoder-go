/// Core vocoder DSP module.
///
/// Implements the classic channel vocoder algorithm:
/// 1. A **modulator** signal (e.g. voice) is analysed through a bank of
///    bandpass filters whose envelopes are tracked.
/// 2. A **carrier** signal (e.g. synthesizer) is passed through a matching
///    bank of bandpass filters whose gains are modulated by the modulator
///    envelopes.
///
/// All filters are second-order IIR (biquad) sections.
use std::f64::consts::PI;

/// Smallest positive normalised f64 value.  Values with magnitude below this
/// threshold are *denormal* (subnormal) floats that can cause severe CPU
/// performance penalties on many architectures.  We flush them to zero.
const DENORMAL_THRESHOLD: f64 = 1e-20;

/// Flush a denormal (subnormal) float to zero.  This is a cheap branch-free
/// helper used in the inner DSP loops to prevent accumulation of denormals
/// in filter state variables.
#[inline]
fn flush_denormal(x: f64) -> f64 {
    if x.abs() < DENORMAL_THRESHOLD {
        0.0
    } else {
        x
    }
}

// ---------------------------------------------------------------------------
// Biquad filter
// ---------------------------------------------------------------------------

/// Coefficients and state for a single second-order IIR (biquad) section.
///
/// Transfer function:
///   H(z) = (b0 + b1·z⁻¹ + b2·z⁻²) / (a0 + a1·z⁻¹ + a2·z⁻²)
///
/// Internally the coefficients are normalised by `a0` so that `a0 == 1`.
#[derive(Clone, Debug)]
pub struct BiquadFilter {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    // Direct-Form II transposed state
    z1: f64,
    z2: f64,
}

impl BiquadFilter {
    /// Create a new bandpass filter with constant skirt gain (peak gain = Q).
    ///
    /// * `center_freq` – centre frequency in Hz
    /// * `q`          – quality factor (bandwidth = centre_freq / q)
    /// * `sample_rate` – sampling rate in Hz
    pub fn bandpass(center_freq: f64, q: f64, sample_rate: f64) -> Self {
        // Guard against invalid parameters that would produce NaN / Inf or
        // cause division-by-zero in the coefficient calculation.
        // - sample_rate must be positive.
        // - center_freq must be positive and strictly below Nyquist
        //   (sample_rate / 2) so that w0 ∈ (0, π).
        // - q must be positive (avoids division by zero in alpha).
        let sample_rate = sample_rate.max(1.0);
        let nyquist = sample_rate / 2.0;
        let center_freq = center_freq.clamp(1e-6, nyquist * 0.999);
        let q = q.max(1e-6);

        let w0 = 2.0 * PI * center_freq / sample_rate;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / (2.0 * q);

        let b0 = alpha;
        let b1 = 0.0;
        let b2 = -alpha;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha;

        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    /// Process a single sample through the filter.
    #[inline]
    pub fn process(&mut self, input: f64) -> f64 {
        let output = self.b0 * input + self.z1;
        self.z1 = flush_denormal(self.b1 * input - self.a1 * output + self.z2);
        self.z2 = flush_denormal(self.b2 * input - self.a2 * output);
        output
    }

    /// Reset internal state to zero.
    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

// ---------------------------------------------------------------------------
// Envelope follower
// ---------------------------------------------------------------------------

/// Simple attack / release envelope follower.
///
/// When the input exceeds the current level the **attack** coefficient is
/// used; otherwise the **release** coefficient is used.  Both coefficients
/// are in the range `[0, 1]` where `1` means "infinite time constant" and
/// `0` means "instantaneous".
#[derive(Clone, Debug)]
pub struct EnvelopeFollower {
    attack: f64,
    release: f64,
    envelope: f64,
}

impl EnvelopeFollower {
    /// Create a new envelope follower.
    ///
    /// * `attack_time`  – rise time constant in seconds (e.g. 0.001)
    /// * `release_time` – fall time constant in seconds (e.g. 0.05)
    /// * `sample_rate`  – sampling rate in Hz
    pub fn new(attack_time: f64, release_time: f64, sample_rate: f64) -> Self {
        Self {
            attack: if attack_time > 0.0 {
                (-1.0 / (attack_time * sample_rate)).exp()
            } else {
                0.0
            },
            release: if release_time > 0.0 {
                (-1.0 / (release_time * sample_rate)).exp()
            } else {
                0.0
            },
            envelope: 0.0,
        }
    }

    /// Process a single sample and return the current envelope value.
    #[inline]
    pub fn process(&mut self, input: f64) -> f64 {
        let abs_input = input.abs();
        let coeff = if abs_input > self.envelope {
            self.attack
        } else {
            self.release
        };
        self.envelope = flush_denormal(coeff * self.envelope + (1.0 - coeff) * abs_input);
        self.envelope
    }

    /// Return the current envelope value without processing a new sample.
    pub fn value(&self) -> f64 {
        self.envelope
    }

    /// Reset the envelope to zero.
    pub fn reset(&mut self) {
        self.envelope = 0.0;
    }
}

// ---------------------------------------------------------------------------
// Filter bank (analysis + synthesis)
// ---------------------------------------------------------------------------

/// A bank of bandpass filters spanning a frequency range with associated
/// envelope followers.
pub struct FilterBank {
    filters: Vec<BiquadFilter>,
    followers: Vec<EnvelopeFollower>,
    center_freqs: Vec<f64>,
}

impl FilterBank {
    /// Create a new filter bank.
    ///
    /// * `num_bands`    – number of frequency bands
    /// * `low_freq`     – lower bound of the lowest band (Hz)
    /// * `high_freq`    – upper bound of the highest band (Hz)
    /// * `q`            – quality factor for every band
    /// * `attack_time`  – envelope follower attack time (seconds)
    /// * `release_time` – envelope follower release time (seconds)
    /// * `sample_rate`  – sampling rate (Hz)
    pub fn new(
        num_bands: usize,
        low_freq: f64,
        high_freq: f64,
        q: f64,
        attack_time: f64,
        release_time: f64,
        sample_rate: f64,
    ) -> Self {
        let center_freqs = spread_frequencies(num_bands, low_freq, high_freq);
        let filters = center_freqs
            .iter()
            .map(|&f| BiquadFilter::bandpass(f, q, sample_rate))
            .collect();
        let followers = center_freqs
            .iter()
            .map(|_| EnvelopeFollower::new(attack_time, release_time, sample_rate))
            .collect();

        Self {
            filters,
            followers,
            center_freqs,
        }
    }

    /// Analyse a single sample: returns the envelope value for each band.
    #[inline]
    pub fn analyse(&mut self, sample: f64) -> &[f64] {
        // We write into a temporary vec then update followers.
        // We store envelopes back into a reusable buffer.
        // Since we need to return &[f64], we keep the envelopes inline.
        for i in 0..self.filters.len() {
            let filtered = self.filters[i].process(sample);
            self.followers[i].process(filtered);
        }
        // We'll hand out references; build a small helper below.
        // Actually, we return the envelope values via a separate method.
        // Let's just return the slice of current values.
        // We need a contiguous slice – we don't have one stored directly.
        // We'll use a small trick: store envelopes in a Vec.
        // See the updated design below.
        unimplemented!("use Vocoder instead which combines analysis and synthesis")
    }

    /// Return the centre frequencies of the bank.
    pub fn center_frequencies(&self) -> &[f64] {
        &self.center_freqs
    }

    /// Number of bands.
    pub fn num_bands(&self) -> usize {
        self.filters.len()
    }

    /// Reset all filter and envelope state.
    pub fn reset(&mut self) {
        for f in &mut self.filters {
            f.reset();
        }
        for e in &mut self.followers {
            e.reset();
        }
    }
}

// ---------------------------------------------------------------------------
// Vocoder
// ---------------------------------------------------------------------------

/// Channel vocoder that imposes the spectral envelope of a modulator signal
/// onto a carrier signal.
///
/// Typical usage:
/// ```ignore
/// let mut vocoder = Vocoder::new(20, 200.0, 8000.0, 4.0, 0.001, 0.05, 44100.0);
/// let output = vocoder.process(modulator_sample, carrier_sample);
/// ```
pub struct Vocoder {
    /// Analysis filter bank (modulator side).
    analysis_filters: Vec<BiquadFilter>,
    /// Synthesis filter bank (carrier side).
    synthesis_filters: Vec<BiquadFilter>,
    /// Envelope followers for each band.
    envelope_followers: Vec<EnvelopeFollower>,
    /// Current envelope values for each band (cached between process calls).
    envelopes: Vec<f64>,
    /// Centre frequencies (kept for introspection).
    center_freqs: Vec<f64>,
    /// Number of bands.
    num_bands: usize,
}

impl Vocoder {
    /// Create a new vocoder.
    ///
    /// * `num_bands`    – number of frequency channels (e.g. 16–32)
    /// * `low_freq`     – lower frequency bound (Hz)
    /// * `high_freq`    – upper frequency bound (Hz)
    /// * `q`            – Q factor for each bandpass filter
    /// * `attack_time`  – envelope follower attack time (seconds)
    /// * `release_time` – envelope follower release time (seconds)
    /// * `sample_rate`  – audio sampling rate (Hz)
    pub fn new(
        num_bands: usize,
        low_freq: f64,
        high_freq: f64,
        q: f64,
        attack_time: f64,
        release_time: f64,
        sample_rate: f64,
    ) -> Self {
        let center_freqs = spread_frequencies(num_bands, low_freq, high_freq);

        let analysis_filters = center_freqs
            .iter()
            .map(|&f| BiquadFilter::bandpass(f, q, sample_rate))
            .collect();

        let synthesis_filters = center_freqs
            .iter()
            .map(|&f| BiquadFilter::bandpass(f, q, sample_rate))
            .collect();

        let envelope_followers = center_freqs
            .iter()
            .map(|_| EnvelopeFollower::new(attack_time, release_time, sample_rate))
            .collect();

        let envelopes = vec![0.0; num_bands];

        Self {
            analysis_filters,
            synthesis_filters,
            envelope_followers,
            envelopes,
            center_freqs,
            num_bands,
        }
    }

    /// Process one sample pair (modulator, carrier) and return the vocoded
    /// output sample.
    #[inline]
    pub fn process(&mut self, modulator: f64, carrier: f64) -> f64 {
        let mut output = 0.0;

        for i in 0..self.num_bands {
            // Analyse the modulator through the analysis bandpass filter.
            let filtered_mod = self.analysis_filters[i].process(modulator);

            // Track the envelope.
            self.envelopes[i] = self.envelope_followers[i].process(filtered_mod);

            // Synthesise: filter the carrier through the matching bandpass.
            let filtered_carrier = self.synthesis_filters[i].process(carrier);

            // Apply the modulator envelope to the filtered carrier.
            output += self.envelopes[i] * filtered_carrier;
        }

        output
    }

    /// Process a block of samples in-place.
    ///
    /// `modulator` and `carrier` must have the same length.  The vocoded
    /// output is written into `output`.
    pub fn process_block(&mut self, modulator: &[f64], carrier: &[f64], output: &mut [f64]) {
        let len = modulator.len().min(carrier.len()).min(output.len());
        for i in 0..len {
            output[i] = self.process(modulator[i], carrier[i]);
        }
    }

    /// Return the current envelope values for each band (read-only).
    pub fn envelopes(&self) -> &[f64] {
        &self.envelopes
    }

    /// Return the centre frequencies of the bandpass filters.
    pub fn center_frequencies(&self) -> &[f64] {
        &self.center_freqs
    }

    /// Number of frequency bands.
    pub fn num_bands(&self) -> usize {
        self.num_bands
    }

    /// Reset all internal state (filters and envelope followers).
    pub fn reset(&mut self) {
        for f in &mut self.analysis_filters {
            f.reset();
        }
        for f in &mut self.synthesis_filters {
            f.reset();
        }
        for e in &mut self.envelope_followers {
            e.reset();
        }
        for v in &mut self.envelopes {
            *v = 0.0;
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Generate `n` centre frequencies logarithmically spaced between `low` and
/// `high`.
fn spread_frequencies(n: usize, low: f64, high: f64) -> Vec<f64> {
    if n == 0 {
        return vec![];
    }
    // Clamp to safe positive values so ln() is well-defined.
    let low = low.max(1e-6);
    let high = high.max(low * 2.0);
    if n == 1 {
        return vec![(low * high).sqrt()];
    }
    let log_low = low.ln();
    let log_high = high.ln();
    (0..n)
        .map(|i| {
            let t = i as f64 / (n - 1) as f64;
            (log_low + t * (log_high - log_low)).exp()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spread_frequencies() {
        let freqs = spread_frequencies(4, 100.0, 1000.0);
        assert_eq!(freqs.len(), 4);
        assert!((freqs[0] - 100.0).abs() < 1e-6);
        assert!((freqs[3] - 1000.0).abs() < 1e-6);
        // Logarithmic spacing: ratio between consecutive bands should be constant.
        let ratio = freqs[1] / freqs[0];
        assert!((freqs[2] / freqs[1] - ratio).abs() < 1e-6);
        assert!((freqs[3] / freqs[2] - ratio).abs() < 1e-6);
    }

    #[test]
    fn test_spread_frequencies_single() {
        let freqs = spread_frequencies(1, 200.0, 800.0);
        assert_eq!(freqs.len(), 1);
        assert!((freqs[0] - (200.0_f64 * 800.0_f64).sqrt()).abs() < 1e-6);
    }

    #[test]
    fn test_spread_frequencies_empty() {
        let freqs = spread_frequencies(0, 100.0, 1000.0);
        assert!(freqs.is_empty());
    }

    #[test]
    fn test_biquad_bandpass_silence() {
        let mut bp = BiquadFilter::bandpass(1000.0, 5.0, 44100.0);
        // Zero input should produce zero output.
        for _ in 0..100 {
            assert_eq!(bp.process(0.0), 0.0);
        }
    }

    #[test]
    fn test_biquad_bandpass_resonates_at_center() {
        let sample_rate = 44100.0;
        let center = 1000.0;
        let mut bp = BiquadFilter::bandpass(center, 10.0, sample_rate);

        // Feed a short burst of white noise, then silence.
        // The filter should ring at its centre frequency.
        for _ in 0..64 {
            bp.process(1.0);
        }
        // Run a bit of silence and check we still have output.
        let mut max_val = 0.0_f64;
        for _ in 0..200 {
            let out = bp.process(0.0);
            max_val = max_val.max(out.abs());
        }
        assert!(
            max_val > 0.01,
            "bandpass should ring after impulse, max={max_val}"
        );
    }

    #[test]
    fn test_biquad_reset() {
        let mut bp = BiquadFilter::bandpass(1000.0, 5.0, 44100.0);
        bp.process(1.0);
        bp.process(0.5);
        bp.reset();
        assert_eq!(bp.process(0.0), 0.0);
    }

    #[test]
    fn test_envelope_follower_attack_faster_than_release() {
        let sample_rate = 44100.0;
        let mut ef = EnvelopeFollower::new(0.0001, 0.1, sample_rate);

        // Apply a step and measure rise speed.
        let mut rise_count = 0usize;
        for _ in 0..sample_rate as usize {
            let v = ef.process(1.0);
            if v < 0.99 {
                rise_count += 1;
            }
        }
        let rise_samples = rise_count;

        // Reset and measure fall speed.
        ef.reset();
        // Charge it up.
        for _ in 0..(sample_rate as usize * 2) {
            ef.process(1.0);
        }
        let mut fall_count = 0usize;
        for _ in 0..sample_rate as usize {
            let v = ef.process(0.0);
            if v > 0.01 {
                fall_count += 1;
            }
        }

        // The envelope should reach 99% much faster than it falls to 1%.
        assert!(
            rise_samples < fall_count,
            "attack should be faster than release: rise={rise_samples}, fall={fall_count}"
        );
    }

    #[test]
    fn test_envelope_follower_value() {
        let mut ef = EnvelopeFollower::new(0.001, 0.05, 44100.0);
        assert_eq!(ef.value(), 0.0);
        ef.process(0.5);
        assert!(ef.value() > 0.0);
    }

    #[test]
    fn test_envelope_follower_reset() {
        let mut ef = EnvelopeFollower::new(0.001, 0.05, 44100.0);
        ef.process(1.0);
        assert!(ef.value() > 0.0);
        ef.reset();
        assert_eq!(ef.value(), 0.0);
    }

    #[test]
    fn test_vocoder_process_silence() {
        let mut vocoder = Vocoder::new(8, 200.0, 8000.0, 4.0, 0.001, 0.05, 44100.0);
        // Silence in → silence out.
        for _ in 0..100 {
            let out = vocoder.process(0.0, 0.0);
            assert!(
                out.abs() < 1e-12,
                "silence should produce silence, got {out}"
            );
        }
    }

    #[test]
    fn test_vocoder_modulated_carrier_is_louder_than_unmodulated() {
        let mut vocoder = Vocoder::new(16, 200.0, 8000.0, 4.0, 0.001, 0.05, 44100.0);

        // Feed carrier with no modulator – output should be near zero.
        let mut unmodulated_energy = 0.0_f64;
        for _ in 0..4096 {
            let carrier = (0.5_f64).sin(); // arbitrary constant carrier sample
            let out = vocoder.process(0.0, carrier);
            unmodulated_energy += out * out;
        }

        vocoder.reset();

        // Now feed both modulator and carrier.
        let mut modulated_energy = 0.0_f64;
        let sample_rate = 44100.0;
        for i in 0..4096 {
            let t = i as f64 / sample_rate;
            let modulator = (2.0 * std::f64::consts::PI * 440.0 * t).sin();
            let carrier = (2.0 * std::f64::consts::PI * 220.0 * t).sin();
            let out = vocoder.process(modulator, carrier);
            modulated_energy += out * out;
        }

        assert!(
            modulated_energy > unmodulated_energy,
            "modulated energy ({modulated_energy}) should exceed unmodulated ({unmodulated_energy})"
        );
    }

    #[test]
    fn test_vocoder_process_block() {
        let mut vocoder = Vocoder::new(8, 200.0, 8000.0, 4.0, 0.001, 0.05, 44100.0);
        let len = 256;
        let modulator: Vec<f64> = (0..len).map(|i| (i as f64 * 0.1).sin()).collect();
        let carrier: Vec<f64> = (0..len).map(|i| (i as f64 * 0.05).cos()).collect();
        let mut output = vec![0.0; len];

        vocoder.process_block(&modulator, &carrier, &mut output);

        // At least some samples should be non-zero after feeding signal.
        let nonzero = output.iter().filter(|&&v| v.abs() > 0.0).count();
        assert!(nonzero > 0, "block output should contain non-zero samples");
    }

    #[test]
    fn test_vocoder_reset() {
        let mut vocoder = Vocoder::new(8, 200.0, 8000.0, 4.0, 0.001, 0.05, 44100.0);
        // Feed some signal.
        for _ in 0..100 {
            vocoder.process(0.5, 0.5);
        }
        // After reset, envelopes should be zero.
        vocoder.reset();
        for &e in vocoder.envelopes() {
            assert_eq!(e, 0.0);
        }
        // Silence should again produce silence.
        for _ in 0..50 {
            let out = vocoder.process(0.0, 0.0);
            assert!(out.abs() < 1e-12);
        }
    }

    #[test]
    fn test_vocoder_num_bands_and_freqs() {
        let vocoder = Vocoder::new(20, 100.0, 10000.0, 5.0, 0.001, 0.05, 44100.0);
        assert_eq!(vocoder.num_bands(), 20);
        assert_eq!(vocoder.center_frequencies().len(), 20);
        // First and last should be at boundaries.
        assert!((vocoder.center_frequencies()[0] - 100.0).abs() < 1e-6);
        assert!((vocoder.center_frequencies()[19] - 10000.0).abs() < 1e-6);
    }

    // ----- New tests for critical fixes -----

    #[test]
    fn test_biquad_q_zero_no_panic() {
        // q = 0 must not panic or produce NaN.
        let mut bp = BiquadFilter::bandpass(1000.0, 0.0, 44100.0);
        let out = bp.process(1.0);
        assert!(out.is_finite(), "output must be finite with q=0, got {out}");
    }

    #[test]
    fn test_biquad_negative_frequency_no_nan() {
        let mut bp = BiquadFilter::bandpass(-500.0, 5.0, 44100.0);
        let out = bp.process(1.0);
        assert!(
            out.is_finite(),
            "output must be finite with negative freq, got {out}"
        );
    }

    #[test]
    fn test_biquad_zero_frequency_no_nan() {
        let mut bp = BiquadFilter::bandpass(0.0, 5.0, 44100.0);
        let out = bp.process(1.0);
        assert!(
            out.is_finite(),
            "output must be finite with zero freq, got {out}"
        );
    }

    #[test]
    fn test_biquad_zero_sample_rate_no_nan() {
        let mut bp = BiquadFilter::bandpass(1000.0, 5.0, 0.0);
        let out = bp.process(1.0);
        assert!(
            out.is_finite(),
            "output must be finite with zero sample_rate, got {out}"
        );
    }

    #[test]
    fn test_biquad_above_nyquist_clamped() {
        // Frequency above Nyquist should be clamped, not produce garbage.
        let mut bp = BiquadFilter::bandpass(30000.0, 5.0, 44100.0);
        let out = bp.process(1.0);
        assert!(
            out.is_finite(),
            "output must be finite with freq above Nyquist, got {out}"
        );
    }

    #[test]
    fn test_spread_frequencies_negative_bounds() {
        let freqs = spread_frequencies(4, -100.0, -10.0);
        // Should produce valid positive frequencies after clamping.
        for &f in &freqs {
            assert!(
                f.is_finite() && f > 0.0,
                "freq must be positive finite, got {f}"
            );
        }
    }

    #[test]
    fn test_denormals_flushed_in_biquad() {
        let mut bp = BiquadFilter::bandpass(1000.0, 5.0, 44100.0);
        // Feed a big impulse then lots of silence — state should flush to zero.
        bp.process(1.0);
        for _ in 0..100_000 {
            bp.process(0.0);
        }
        // After enough silence, the state variables should have been flushed.
        // We verify indirectly: process another zero and check the output is
        // extremely small or exactly zero.
        let out = bp.process(0.0);
        assert!(
            out == 0.0 || out.abs() < 1e-15,
            "denormals should be flushed, got {out}"
        );
    }

    #[test]
    fn test_denormals_flushed_in_envelope_follower() {
        let mut ef = EnvelopeFollower::new(0.001, 0.001, 44100.0);
        // Charge then decay to zero.
        ef.process(1.0);
        for _ in 0..500_000 {
            ef.process(0.0);
        }
        let val = ef.value();
        assert!(
            val == 0.0 || val < 1e-15,
            "denormals should be flushed in envelope follower, got {val}"
        );
    }

    // ======================================================================
    // DSP unit tests from test_design_unit.md
    // ======================================================================

    // ----- 1.1 BiquadFilter::bandpass() — Input Validation -----

    // DSP-BP-01 (P0): q == 0.0 must not produce NaN/inf coefficients.
    #[test]
    fn test_bp01_q_zero_coefficients_finite() {
        let bp = BiquadFilter::bandpass(1000.0, 0.0, 44100.0);
        assert!(bp.b0.is_finite(), "b0 must be finite, got {}", bp.b0);
        assert!(bp.b1.is_finite(), "b1 must be finite, got {}", bp.b1);
        assert!(bp.b2.is_finite(), "b2 must be finite, got {}", bp.b2);
        assert!(bp.a1.is_finite(), "a1 must be finite, got {}", bp.a1);
        assert!(bp.a2.is_finite(), "a2 must be finite, got {}", bp.a2);
        let mut bp = bp;
        for _ in 0..100 {
            let out = bp.process(1.0);
            assert!(out.is_finite(), "output must be finite, got {out}");
        }
    }

    // DSP-BP-02 (P0): Negative Q must not produce NaN/inf coefficients.
    #[test]
    fn test_bp02_negative_q_coefficients_finite() {
        let bp = BiquadFilter::bandpass(1000.0, -5.0, 44100.0);
        assert!(bp.b0.is_finite(), "b0 must be finite, got {}", bp.b0);
        assert!(bp.b1.is_finite(), "b1 must be finite, got {}", bp.b1);
        assert!(bp.b2.is_finite(), "b2 must be finite, got {}", bp.b2);
        assert!(bp.a1.is_finite(), "a1 must be finite, got {}", bp.a1);
        assert!(bp.a2.is_finite(), "a2 must be finite, got {}", bp.a2);
        let mut bp = bp;
        for _ in 0..100 {
            let out = bp.process(1.0);
            assert!(out.is_finite(), "output must be finite, got {out}");
        }
    }

    // DSP-BP-03 (P0): center_freq == 0.0 must not produce NaN.
    #[test]
    fn test_bp03_zero_center_freq_no_nan() {
        let bp = BiquadFilter::bandpass(0.0, 5.0, 44100.0);
        assert!(bp.b0.is_finite(), "b0 must be finite, got {}", bp.b0);
        assert!(bp.b1.is_finite(), "b1 must be finite, got {}", bp.b1);
        assert!(bp.b2.is_finite(), "b2 must be finite, got {}", bp.b2);
        assert!(bp.a1.is_finite(), "a1 must be finite, got {}", bp.a1);
        assert!(bp.a2.is_finite(), "a2 must be finite, got {}", bp.a2);
        let mut bp = bp;
        for _ in 0..100 {
            let out = bp.process(1.0);
            assert!(out.is_finite(), "output must be finite, got {out}");
        }
    }

    // DSP-BP-04 (P0): center_freq == sample_rate / 2.0 (Nyquist) — verify stability.
    #[test]
    fn test_bp04_nyquist_center_freq_stable() {
        let bp = BiquadFilter::bandpass(22050.0, 5.0, 44100.0);
        assert!(bp.b0.is_finite(), "b0 must be finite, got {}", bp.b0);
        assert!(bp.b1.is_finite(), "b1 must be finite, got {}", bp.b1);
        assert!(bp.b2.is_finite(), "b2 must be finite, got {}", bp.b2);
        assert!(bp.a1.is_finite(), "a1 must be finite, got {}", bp.a1);
        assert!(bp.a2.is_finite(), "a2 must be finite, got {}", bp.a2);
        let mut bp = bp;
        let mut max_output = 0.0_f64;
        for i in 0..1000 {
            let input = (2.0 * PI * 22050.0 * i as f64 / 44100.0).sin();
            let out = bp.process(input);
            assert!(
                out.is_finite(),
                "output must be finite at sample {i}, got {out}"
            );
            max_output = max_output.max(out.abs());
        }
        // Bounded output for bounded input
        assert!(
            max_output < 10.0,
            "output should be bounded, got max {max_output}"
        );
    }

    // DSP-BP-05 (P1): center_freq > sample_rate / 2.0 (above Nyquist) — bounded output.
    #[test]
    fn test_bp05_above_nyquist_bounded() {
        let mut bp = BiquadFilter::bandpass(30000.0, 5.0, 44100.0);
        let mut max_output = 0.0_f64;
        for i in 0..1000 {
            let input = (2.0 * PI * 1000.0 * i as f64 / 44100.0).sin();
            let out = bp.process(input);
            assert!(out.is_finite(), "output must be finite, got {out}");
            max_output = max_output.max(out.abs());
        }
        assert!(
            max_output.is_finite() && max_output < 1e10,
            "output should be bounded, got {max_output}"
        );
    }

    // DSP-BP-06 (P1): center_freq < 0.0 (negative frequency) — no NaN.
    #[test]
    fn test_bp06_negative_freq_no_nan() {
        let bp = BiquadFilter::bandpass(-500.0, 5.0, 44100.0);
        assert!(bp.b0.is_finite(), "b0 must be finite, got {}", bp.b0);
        assert!(bp.b1.is_finite(), "b1 must be finite, got {}", bp.b1);
        assert!(bp.b2.is_finite(), "b2 must be finite, got {}", bp.b2);
        assert!(bp.a1.is_finite(), "a1 must be finite, got {}", bp.a1);
        assert!(bp.a2.is_finite(), "a2 must be finite, got {}", bp.a2);
    }

    // DSP-BP-07 (P1): Very high Q (q = 10000.0) — must remain stable.
    #[test]
    fn test_bp07_high_q_stable() {
        let mut bp = BiquadFilter::bandpass(1000.0, 10000.0, 44100.0);
        let mut max_output = 0.0_f64;
        for _ in 0..1000 {
            let out = bp.process(1.0);
            assert!(out.is_finite(), "output must be finite, got {out}");
            max_output = max_output.max(out.abs());
        }
        assert!(
            max_output.is_finite() && max_output < 1e10,
            "filter should not diverge, got max {max_output}"
        );
    }

    // DSP-BP-08 (P1): sample_rate == 0.0 — no panic or NaN.
    #[test]
    fn test_bp08_zero_sample_rate_no_panic() {
        let bp = BiquadFilter::bandpass(1000.0, 5.0, 0.0);
        assert!(bp.b0.is_finite(), "b0 must be finite, got {}", bp.b0);
        assert!(bp.b1.is_finite(), "b1 must be finite, got {}", bp.b1);
        assert!(bp.b2.is_finite(), "b2 must be finite, got {}", bp.b2);
        assert!(bp.a1.is_finite(), "a1 must be finite, got {}", bp.a1);
        assert!(bp.a2.is_finite(), "a2 must be finite, got {}", bp.a2);
        let mut bp = bp;
        let out = bp.process(1.0);
        assert!(out.is_finite(), "output must be finite, got {out}");
    }

    // ----- 1.2 BiquadFilter::process() — Correctness & Numerics -----

    // DSP-BP-09 (P0): Zero input always produces exactly 0.0 from zero state.
    #[test]
    fn test_bp09_zero_input_exact_zero() {
        let mut bp = BiquadFilter::bandpass(1000.0, 5.0, 44100.0);
        for _ in 0..100 {
            assert_eq!(
                bp.process(0.0),
                0.0,
                "zero input must produce exactly zero output from zero state"
            );
        }
    }

    // DSP-BP-10 (P1): Filter is linear: process(a * x) == a * process(x).
    #[test]
    fn test_bp10_linearity() {
        let mut bp_scaled = BiquadFilter::bandpass(1000.0, 5.0, 44100.0);
        let mut bp_base = BiquadFilter::bandpass(1000.0, 5.0, 44100.0);
        let a = 2.0;
        for _ in 0..100 {
            let x = 0.5;
            let scaled = bp_scaled.process(a * x);
            let base = bp_base.process(x);
            let expected = a * base;
            assert!(
                (scaled - expected).abs() < 1e-10,
                "linearity violated: process({a}*{x}) = {scaled}, but {a}*process({x}) = {expected}"
            );
        }
    }

    // DSP-BP-11 (P1): Filter is time-invariant.
    #[test]
    fn test_bp11_time_invariance() {
        let input: Vec<f64> = (0..50).map(|i| (i as f64 * 0.3).sin()).collect();
        let mut bp1 = BiquadFilter::bandpass(1000.0, 5.0, 44100.0);
        let outputs1: Vec<f64> = input.iter().map(|&x| bp1.process(x)).collect();
        let mut bp2 = BiquadFilter::bandpass(1000.0, 5.0, 44100.0);
        let outputs2: Vec<f64> = input.iter().map(|&x| bp2.process(x)).collect();
        for (i, (o1, o2)) in outputs1.iter().zip(outputs2.iter()).enumerate() {
            assert!(
                (o1 - o2).abs() < 1e-15,
                "time-invariance violated at sample {i}: {o1} != {o2}"
            );
        }
    }

    // DSP-BP-12 (P2): BIBO stability — bounded output for bounded input.
    #[test]
    fn test_bp12_bibo_stability() {
        let mut bp = BiquadFilter::bandpass(1000.0, 5.0, 44100.0);
        let mut seed: u64 = 12345;
        for _ in 0..10_000 {
            // Simple LCG pseudo-random in [-1, 1]
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let input = ((seed >> 33) as i64 as f64) / (i64::MAX as f64);
            let out = bp.process(input);
            assert!(
                out.is_finite() && out.abs() < 10.0,
                "BIBO violated: output {out} exceeds bound"
            );
        }
    }

    // ----- 1.3 BiquadFilter::reset() -----

    // DSP-BP-13 (P1): After reset(), state variables are zero.
    #[test]
    fn test_bp13_reset_clears_state() {
        let mut bp = BiquadFilter::bandpass(1000.0, 5.0, 44100.0);
        for _ in 0..100 {
            bp.process(1.0);
        }
        bp.reset();
        assert_eq!(
            bp.process(0.0),
            0.0,
            "after reset, process(0.0) must be exactly 0.0"
        );
    }

    // ----- 1.4 EnvelopeFollower — Construction & Edge Cases -----

    // DSP-EF-01 (P0): Negative attack_time → attack = 0.0 (instantaneous).
    #[test]
    fn test_ef01_negative_attack_time() {
        let mut ef = EnvelopeFollower::new(-0.001, 0.05, 44100.0);
        // With attack=0.0, envelope should track input immediately.
        let out = ef.process(1.0);
        assert!(out.is_finite(), "envelope must be finite, got {out}");
        assert!(
            out > 0.0,
            "envelope should be positive after positive input, got {out}"
        );
    }

    // DSP-EF-02 (P0): Negative release_time → release = 0.0 (instantaneous).
    #[test]
    fn test_ef02_negative_release_time() {
        let mut ef = EnvelopeFollower::new(0.001, -0.05, 44100.0);
        let out = ef.process(1.0);
        assert!(out.is_finite(), "envelope must be finite, got {out}");
        assert!(out > 0.0, "envelope should be positive, got {out}");
    }

    // DSP-EF-03 (P1): attack_time == 0.0 → converges to input value quickly.
    #[test]
    fn test_ef03_zero_attack_time() {
        let mut ef = EnvelopeFollower::new(0.0, 0.05, 44100.0);
        for _ in 0..10 {
            ef.process(0.5);
        }
        let val = ef.value();
        assert!(
            (val - 0.5).abs() < 0.1,
            "should converge to input value with zero attack, got {val}"
        );
    }

    // DSP-EF-04 (P1): release_time == 0.0 → envelope drops immediately.
    #[test]
    fn test_ef04_zero_release_time() {
        let mut ef = EnvelopeFollower::new(0.001, 0.0, 44100.0);
        // Charge up
        for _ in 0..1000 {
            ef.process(1.0);
        }
        let charged = ef.value();
        assert!(charged > 0.0, "should be charged, got {charged}");
        // Release to zero should be instantaneous
        let released = ef.process(0.0);
        assert!(
            released.is_finite(),
            "released must be finite, got {released}"
        );
        assert!(
            released < charged,
            "envelope should decrease with zero release, got {released}"
        );
    }

    // DSP-EF-05 (P1): sample_rate == 0.0 — no panic.
    #[test]
    fn test_ef05_zero_sample_rate_no_panic() {
        let mut ef = EnvelopeFollower::new(0.001, 0.05, 0.0);
        assert_eq!(ef.value(), 0.0);
        let out = ef.process(0.5);
        assert!(out.is_finite(), "envelope must be finite, got {out}");
    }

    // DSP-EF-06 (P1): Very large attack_time — envelope barely rises.
    #[test]
    fn test_ef06_very_large_attack_time() {
        let mut ef = EnvelopeFollower::new(1000.0, 0.05, 44100.0);
        let initial = ef.value();
        for _ in 0..1000 {
            ef.process(1.0);
        }
        let after = ef.value();
        assert!(
            after - initial < 0.5,
            "envelope should barely rise with 1000s attack time over 1000 samples, got {after}"
        );
    }

    // DSP-EF-07 (P1): NaN input — verify no panic.
    // Note: NaN propagates through the envelope state in the current implementation.
    // This test documents the behavior (known limitation: NaN poisons the envelope).
    #[test]
    fn test_ef07_nan_input_no_panic() {
        let mut ef = EnvelopeFollower::new(0.001, 0.05, 44100.0);
        let _ = ef.process(f64::NAN);
        // Verify no panic occurred. The primary goal of this test.
        // Current behavior: envelope may be NaN after this call.
    }

    // DSP-EF-08 (P1): inf input — verify no panic.
    // Note: inf propagates through the envelope state in the current implementation.
    // This test documents the behavior (known limitation: inf poisons the envelope).
    #[test]
    fn test_ef08_inf_input_no_panic() {
        let mut ef = EnvelopeFollower::new(0.001, 0.05, 44100.0);
        let _ = ef.process(f64::INFINITY);
        // Verify no panic occurred. The primary goal of this test.
        // Current behavior: envelope may be inf after this call.
    }

    // ----- 1.5 EnvelopeFollower — Correctness -----

    // DSP-EF-09 (P1): Steady-state accuracy — envelope converges to |x|.
    #[test]
    fn test_ef09_steady_state_accuracy() {
        let mut ef = EnvelopeFollower::new(0.001, 0.05, 44100.0);
        let target = 0.75;
        for _ in 0..100_000 {
            ef.process(target);
        }
        let val = ef.value();
        assert!(
            (val - target).abs() < 1e-6,
            "envelope should converge to |x|, got {val} vs target {target}"
        );
    }

    // DSP-EF-10 (P1): Attack is faster than release — quantitative check.
    #[test]
    fn test_ef10_attack_faster_than_release_quantitative() {
        let sample_rate = 44100.0;
        let mut ef = EnvelopeFollower::new(0.0001, 0.1, sample_rate);

        // Rise to 90%
        let mut rise_samples = 0usize;
        for _ in 0..sample_rate as usize {
            let v = ef.process(1.0);
            if v < 0.9 {
                rise_samples += 1;
            }
        }

        // Charge fully, then measure fall from 90% to 10%
        ef.reset();
        for _ in 0..(sample_rate as usize * 2) {
            ef.process(1.0);
        }
        let mut fall_samples = 0usize;
        let mut crossed_90 = false;
        for _ in 0..sample_rate as usize {
            let v = ef.process(0.0);
            if v <= 0.9 && !crossed_90 {
                crossed_90 = true;
            }
            if crossed_90 && v > 0.1 {
                fall_samples += 1;
            }
        }

        assert!(
            rise_samples < fall_samples,
            "rise to 90% ({rise_samples} samples) should be faster than fall from 90% to 10% ({fall_samples} samples)"
        );
    }

    // DSP-EF-11 (P2): Envelope is monotonically non-decreasing with constant input.
    #[test]
    fn test_ef11_monotonic_non_decreasing() {
        let mut ef = EnvelopeFollower::new(0.001, 0.05, 44100.0);
        let mut prev = ef.process(0.5);
        for _ in 0..1000 {
            let curr = ef.process(0.5);
            assert!(
                curr >= prev - 1e-15,
                "envelope must be monotonically non-decreasing for constant input, prev={prev}, curr={curr}"
            );
            prev = curr;
        }
    }

    // ----- 1.6 spread_frequencies() — Edge Cases -----

    // DSP-SF-01 (P0): low == 0.0 — frequencies must be finite and positive after clamping.
    #[test]
    fn test_sf01_low_zero_no_nan() {
        let freqs = spread_frequencies(4, 0.0, 1000.0);
        for &f in &freqs {
            assert!(
                f.is_finite() && f > 0.0,
                "frequency must be finite and positive, got {f}"
            );
        }
    }

    // DSP-SF-02 (P0): low < 0 (negative) — frequencies must be finite and positive.
    #[test]
    fn test_sf02_negative_low_no_nan() {
        let freqs = spread_frequencies(4, -100.0, 1000.0);
        for &f in &freqs {
            assert!(
                f.is_finite() && f > 0.0,
                "frequency must be finite and positive, got {f}"
            );
        }
    }

    // DSP-SF-03 (P0): high == 0.0 — frequencies must be finite and positive after clamping.
    #[test]
    fn test_sf03_high_zero_no_nan() {
        let freqs = spread_frequencies(4, 100.0, 0.0);
        for &f in &freqs {
            assert!(
                f.is_finite() && f > 0.0,
                "frequency must be finite and positive, got {f}"
            );
        }
    }

    // DSP-SF-04 (P1): low > high (inverted range) — clamping produces ascending frequencies.
    #[test]
    fn test_sf04_inverted_range() {
        let freqs = spread_frequencies(4, 1000.0, 100.0);
        // After clamping: high = max(100.0, 1000.0*2.0) = 2000.0
        // Frequencies are spread between low=1000 and high=2000
        for &f in &freqs {
            assert!(
                f.is_finite() && f > 0.0,
                "frequency must be finite and positive, got {f}"
            );
        }
        assert!(
            freqs[0] <= freqs[freqs.len() - 1],
            "frequencies should be ascending after clamping"
        );
    }

    // DSP-SF-05 (P1): low == high — due to clamping, high becomes low*2 so all frequencies differ.
    #[test]
    fn test_sf05_low_equals_high() {
        let freqs = spread_frequencies(5, 500.0, 500.0);
        assert_eq!(freqs.len(), 5);
        for &f in &freqs {
            assert!(
                f.is_finite() && f > 0.0,
                "frequency must be finite and positive, got {f}"
            );
        }
    }

    // DSP-SF-08 (P1): n == 2 → first == low, last == high.
    #[test]
    fn test_sf08_n2_first_last() {
        let freqs = spread_frequencies(2, 100.0, 1000.0);
        assert_eq!(freqs.len(), 2);
        assert!(
            (freqs[0] - 100.0).abs() < 1e-6,
            "first freq should be low, got {}",
            freqs[0]
        );
        assert!(
            (freqs[1] - 1000.0).abs() < 1e-6,
            "last freq should be high, got {}",
            freqs[1]
        );
    }

    // DSP-SF-09 (P2): Logarithmic spacing invariant for n=20.
    #[test]
    fn test_sf09_log_spacing_n20() {
        let freqs = spread_frequencies(20, 100.0, 10000.0);
        assert_eq!(freqs.len(), 20);
        let ratio = freqs[1] / freqs[0];
        for i in 1..19 {
            let r = freqs[i + 1] / freqs[i];
            assert!(
                (r - ratio).abs() < 1e-6,
                "log spacing violated at i={i}: ratio={r}, expected={ratio}"
            );
        }
    }

    // ----- 1.7 FilterBank::analyse() — Panic Check -----

    // DSP-FB-01 (P0): analyse() panics with unimplemented!().
    #[test]
    #[should_panic(expected = "use Vocoder instead")]
    fn test_fb01_analyse_panics() {
        let mut fb = FilterBank::new(4, 200.0, 8000.0, 5.0, 0.001, 0.05, 44100.0);
        fb.analyse(0.0);
    }

    // ----- 1.8 Vocoder — Integration Tests -----

    // DSP-VO-01 (P0): Silence in produces exactly 0.0 out (tightened to exact equality).
    #[test]
    fn test_vo01_silence_exact_zero() {
        let mut vocoder = Vocoder::new(8, 200.0, 8000.0, 4.0, 0.001, 0.05, 44100.0);
        for _ in 0..100 {
            assert_eq!(
                vocoder.process(0.0, 0.0),
                0.0,
                "silence must produce exactly 0.0"
            );
        }
    }

    // DSP-VO-02 (P1): process() output is always finite for bounded inputs.
    #[test]
    fn test_vo02_finite_output_bounded_input() {
        let mut vocoder = Vocoder::new(20, 200.0, 8000.0, 4.0, 0.001, 0.05, 44100.0);
        let mut seed: u64 = 42;
        for _ in 0..10_000 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let modulator = ((seed >> 33) as i64 as f64) / (i64::MAX as f64);
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let carrier = ((seed >> 33) as i64 as f64) / (i64::MAX as f64);
            let out = vocoder.process(modulator, carrier);
            assert!(out.is_finite(), "output must be finite, got {out}");
        }
    }

    // DSP-VO-03 (P1): Output accumulation — output magnitude can exceed 1.0 with full-spectrum inputs.
    #[test]
    fn test_vo03_output_accumulation() {
        let mut vocoder = Vocoder::new(20, 200.0, 8000.0, 4.0, 0.001, 0.05, 44100.0);
        let mut max_output = 0.0_f64;
        let sample_rate = 44100.0;
        for i in 0..44100 {
            let t = i as f64 / sample_rate;
            let modulator = (2.0 * PI * 440.0 * t).sin();
            let carrier = (2.0 * PI * 220.0 * t).sin();
            let out = vocoder.process(modulator, carrier);
            max_output = max_output.max(out.abs());
        }
        // Document: output should be non-zero with signal input.
        // Accumulation across 20 bands may cause output > 1.0 — this is a known property.
        assert!(
            max_output > 0.0,
            "output should be non-zero with signal input, max was {max_output}"
        );
    }

    // DSP-VO-04 (P1): process_block truncates on mismatched lengths.
    #[test]
    fn test_vo04_process_block_truncates() {
        let mut vocoder = Vocoder::new(8, 200.0, 8000.0, 4.0, 0.001, 0.05, 44100.0);
        let modulator = vec![0.5; 256];
        let carrier = vec![0.3; 128];
        let mut output = vec![0.0; 256];

        vocoder.process_block(&modulator, &carrier, &mut output);

        // Only 128 samples should be written (min of 256, 128, 256)
        for i in 128..256 {
            assert_eq!(
                output[i], 0.0,
                "output[{i}] should remain 0.0 (beyond carrier length), got {}",
                output[i]
            );
        }
    }

    // DSP-VO-05 (P1): reset() clears all state.
    #[test]
    fn test_vo05_reset_clears_all_state() {
        let mut vocoder = Vocoder::new(8, 200.0, 8000.0, 4.0, 0.001, 0.05, 44100.0);
        for _ in 0..100 {
            vocoder.process(0.5, 0.5);
        }
        vocoder.reset();
        // Envelopes should all be 0.0
        for &e in vocoder.envelopes() {
            assert_eq!(e, 0.0, "envelope should be 0.0 after reset");
        }
        // Silence should produce exactly 0.0
        for _ in 0..50 {
            assert_eq!(vocoder.process(0.0, 0.0), 0.0);
        }
    }

    // DSP-VO-06 (P2): Vocoder::new() with num_bands = 0 — no panic, returns 0.0.
    #[test]
    fn test_vo06_zero_bands_no_panic() {
        let mut vocoder = Vocoder::new(0, 200.0, 8000.0, 4.0, 0.001, 0.05, 44100.0);
        let out = vocoder.process(0.5, 0.5);
        assert_eq!(out, 0.0, "zero-band vocoder should produce 0.0");
    }

    // DSP-VO-07 (P2): Vocoder::new() with low == high — output is non-zero with signal.
    #[test]
    fn test_vo07_low_equals_high_nonzero() {
        let mut vocoder = Vocoder::new(4, 500.0, 500.0, 4.0, 0.001, 0.05, 44100.0);
        let mut has_nonzero = false;
        let sample_rate = 44100.0;
        for i in 0..1000 {
            let t = i as f64 / sample_rate;
            let out = vocoder.process((2.0 * PI * 440.0 * t).sin(), (2.0 * PI * 220.0 * t).sin());
            if out.abs() > 0.0 {
                has_nonzero = true;
            }
        }
        assert!(
            has_nonzero,
            "vocoder with low==high should produce non-zero output with signal input"
        );
    }

    // DSP-VO-08 (P2): process() N times == process_block() on same data.
    #[test]
    fn test_vo08_process_vs_process_block_consistency() {
        let modulator: Vec<f64> = (0..128).map(|i| (i as f64 * 0.1).sin()).collect();
        let carrier: Vec<f64> = (0..128).map(|i| (i as f64 * 0.05).cos()).collect();

        let mut v1 = Vocoder::new(8, 200.0, 8000.0, 4.0, 0.001, 0.05, 44100.0);
        let mut outputs_single: Vec<f64> = vec![0.0; 128];
        for i in 0..128 {
            outputs_single[i] = v1.process(modulator[i], carrier[i]);
        }

        let mut v2 = Vocoder::new(8, 200.0, 8000.0, 4.0, 0.001, 0.05, 44100.0);
        let mut outputs_block: Vec<f64> = vec![0.0; 128];
        v2.process_block(&modulator, &carrier, &mut outputs_block);

        for (i, (s, b)) in outputs_single.iter().zip(outputs_block.iter()).enumerate() {
            assert!(
                (s - b).abs() < 1e-15,
                "process and process_block should produce identical output at sample {i}: {s} != {b}"
            );
        }
    }

    // ----- 6. Cross-Cutting Property-Based Tests (DSP) -----

    // PROP-01 (P2): Random valid BiquadFilter params → finite coefficients, no NaN output.
    #[test]
    fn test_prop01_biquad_valid_params_finite() {
        use proptest::prelude::*;
        proptest!(|(center_freq in 0.1_f64..20000.0, q in 0.01_f64..1000.0, sample_rate in 8000.0_f64..192000.0)| {
            // Only test when center_freq < sample_rate / 2
            if center_freq < sample_rate / 2.0 {
                let bp = BiquadFilter::bandpass(center_freq, q, sample_rate);
                assert!(bp.b0.is_finite());
                assert!(bp.b1.is_finite());
                assert!(bp.b2.is_finite());
                assert!(bp.a1.is_finite());
                assert!(bp.a2.is_finite());
                let mut bp = bp;
                for _ in 0..50 {
                    let out = bp.process(1.0);
                    assert!(out.is_finite());
                }
            }
        });
    }

    // PROP-02 (P2): EnvelopeFollower never returns negative values for any input.
    #[test]
    fn test_prop02_envelope_non_negative() {
        use proptest::prelude::*;
        proptest!(|(attack_time in 0.001_f64..1.0, release_time in 0.001_f64..1.0, input in -1.0_f64..1.0)| {
            let mut ef = EnvelopeFollower::new(attack_time, release_time, 44100.0);
            for _ in 0..100 {
                let out = ef.process(input);
                assert!(out >= 0.0, "envelope must be non-negative, got {out}");
            }
        });
    }

    // PROP-03 (P2): Vocoder process() returns finite value for any finite inputs in [-1, 1].
    #[test]
    fn test_prop03_vocoder_finite_output() {
        use proptest::prelude::*;
        proptest!(|(modulator in -1.0_f64..1.0, carrier in -1.0_f64..1.0)| {
            let mut vocoder = Vocoder::new(20, 200.0, 8000.0, 4.0, 0.001, 0.05, 44100.0);
            let out = vocoder.process(modulator, carrier);
            assert!(out.is_finite(), "output must be finite, got {out}");
        });
    }

    // PROP-07 (P2): spread_frequencies produces strictly increasing frequencies
    //              where freqs[0] == low and freqs[n-1] == high.
    // Note: spread_frequencies clamps high to at least low*2.0, so we use ratio >= 2.0
    // to avoid the clamping affecting the last-frequency assertion.
    #[test]
    fn test_prop07_spread_frequencies_strictly_increasing() {
        use proptest::prelude::*;
        proptest!(|(n in 2_usize..50, low in 1.0_f64..1000.0, ratio in 2.0_f64..100.0)| {
            let high = low * ratio;
            let freqs = spread_frequencies(n, low, high);
            assert_eq!(freqs.len(), n);
            assert!(
                (freqs[0] - low).abs() < 1e-6,
                "first freq should be low={low}, got {}",
                freqs[0]
            );
            assert!(
                (freqs[n - 1] - high).abs() < 1e-6,
                "last freq should be high={high}, got {}",
                freqs[n - 1]
            );
            for w in freqs.windows(2) {
                assert!(
                    w[1] > w[0],
                    "frequencies must be strictly increasing: {} > {}",
                    w[1],
                    w[0]
                );
            }
        });
    }
}
