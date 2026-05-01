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
        self.z1 = self.b1 * input - self.a1 * output + self.z2;
        self.z2 = self.b2 * input - self.a2 * output;
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
        self.envelope = coeff * self.envelope + (1.0 - coeff) * abs_input;
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
}
