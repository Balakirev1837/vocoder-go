// Package vocoder implements the core DSP for a channel vocoder.
//
// A modulator signal (e.g. voice) is analysed through a bank of bandpass
// filters whose envelopes are tracked.  A carrier signal (e.g. synthesizer)
// is passed through a matching bank of bandpass filters whose gains are
// modulated by the modulator envelopes.
//
// All filters are second-order IIR (biquad) sections.
package vocoder

import (
	"math"
)

// denormalThreshold is the smallest positive normalised float64 value we
// consider "normal".  Values with magnitude below this threshold are
// subnormal (denormal) floats that can cause severe CPU performance
// penalties on many architectures.  We flush them to zero.
const denormalThreshold = 1e-20

// flushDenormal flushes a denormal (subnormal) float to zero.  This is a
// cheap helper used in the inner DSP loops to prevent accumulation of
// denormals in filter state variables.
//
//go:nosplit
func flushDenormal(x float64) float64 {
	if math.Abs(x) < denormalThreshold {
		return 0.0
	}
	return x
}

// ---------------------------------------------------------------------------
// Biquad filter
// ---------------------------------------------------------------------------

// BiquadFilter holds the coefficients and state for a single second-order
// IIR (biquad) section.
//
// Transfer function:
//
//	H(z) = (b0 + b1*z^-1 + b2*z^-2) / (a0 + a1*z^-1 + a2*z^-2)
//
// Internally the coefficients are normalised by a0 so that a0 == 1.
// Direct-Form II Transposed structure is used for numerical stability.
type BiquadFilter struct {
	b0 float64
	b1 float64
	b2 float64
	a1 float64
	a2 float64
	// Direct-Form II transposed state
	z1 float64
	z2 float64
}

// NewBandpass creates a new bandpass filter with constant skirt gain
// (peak gain = Q).
//
//   - centerFreq – centre frequency in Hz
//   - q          – quality factor (bandwidth = centreFreq / q)
//   - sampleRate – sampling rate in Hz
func NewBandpass(centerFreq, q, sampleRate float64) BiquadFilter {
	// Guard against invalid parameters that would produce NaN / Inf or
	// cause division-by-zero in the coefficient calculation.
	sampleRate = math.Max(sampleRate, 1.0)
	nyquist := sampleRate / 2.0
	centerFreq = math.Max(1e-6, math.Min(centerFreq, nyquist*0.999))
	q = math.Max(q, 1e-6)

	w0 := 2.0 * math.Pi * centerFreq / sampleRate
	cosW0 := math.Cos(w0)
	sinW0 := math.Sin(w0)
	alpha := sinW0 / (2.0 * q)

	b0 := alpha
	b1 := 0.0
	b2 := -alpha
	a0 := 1.0 + alpha
	a1 := -2.0 * cosW0
	a2 := 1.0 - alpha

	return BiquadFilter{
		b0: b0 / a0,
		b1: b1 / a0,
		b2: b2 / a0,
		a1: a1 / a0,
		a2: a2 / a0,
		z1: 0.0,
		z2: 0.0,
	}
}

// Process processes a single sample through the filter.
// No heap allocations occur in this method.
//
//go:nosplit
func (bf *BiquadFilter) Process(input float64) float64 {
	output := bf.b0*input + bf.z1
	bf.z1 = flushDenormal(bf.b1*input - bf.a1*output + bf.z2)
	bf.z2 = flushDenormal(bf.b2*input - bf.a2*output)
	return output
}

// Reset resets internal state to zero.
func (bf *BiquadFilter) Reset() {
	bf.z1 = 0.0
	bf.z2 = 0.0
}

// ---------------------------------------------------------------------------
// Envelope follower
// ---------------------------------------------------------------------------

// EnvelopeFollower implements a simple attack / release envelope follower.
//
// When the input exceeds the current level the attack coefficient is used;
// otherwise the release coefficient is used.  Both coefficients are in the
// range [0, 1] where 1 means "infinite time constant" and 0 means
// "instantaneous".
type EnvelopeFollower struct {
	attack   float64
	release  float64
	envelope float64
}

// NewEnvelopeFollower creates a new envelope follower.
//
//   - attackTime  – rise time constant in seconds (e.g. 0.001)
//   - releaseTime – fall time constant in seconds (e.g. 0.05)
//   - sampleRate  – sampling rate in Hz
func NewEnvelopeFollower(attackTime, releaseTime, sampleRate float64) EnvelopeFollower {
	var attack, release float64
	if attackTime > 0.0 {
		attack = math.Exp(-1.0 / (attackTime * sampleRate))
	}
	if releaseTime > 0.0 {
		release = math.Exp(-1.0 / (releaseTime * sampleRate))
	}
	return EnvelopeFollower{
		attack:   attack,
		release:  release,
		envelope: 0.0,
	}
}

// Process processes a single sample and returns the current envelope value.
// No heap allocations occur in this method.
//
//go:nosplit
func (ef *EnvelopeFollower) Process(input float64) float64 {
	absInput := math.Abs(input)
	var coeff float64
	if absInput > ef.envelope {
		coeff = ef.attack
	} else {
		coeff = ef.release
	}
	ef.envelope = flushDenormal(coeff*ef.envelope + (1.0-coeff)*absInput)
	return ef.envelope
}

// Value returns the current envelope value without processing a new sample.
func (ef *EnvelopeFollower) Value() float64 {
	return ef.envelope
}

// Reset resets the envelope to zero.
func (ef *EnvelopeFollower) Reset() {
	ef.envelope = 0.0
}

// ---------------------------------------------------------------------------
// Vocoder
// ---------------------------------------------------------------------------

// Vocoder is a channel vocoder that imposes the spectral envelope of a
// modulator signal onto a carrier signal.
//
// Typical usage:
//
//	v := NewVocoder(20, 200.0, 8000.0, 4.0, 0.001, 0.05, 44100.0)
//	output := v.Process(modulatorSample, carrierSample)
//
// The Vocoder struct is designed so that Process makes zero heap allocations.
// All slices are allocated once at construction time and reused.
type Vocoder struct {
	// Analysis filter bank (modulator side).
	analysisFilters []BiquadFilter
	// Synthesis filter bank (carrier side).
	synthesisFilters []BiquadFilter
	// Envelope followers for each band.
	envelopeFollowers []EnvelopeFollower
	// Current envelope values for each band (cached between process calls).
	envelopes []float64
	// Centre frequencies (kept for introspection).
	centerFreqs []float64
	// Number of bands.
	numBands int
}

// NewVocoder creates a new vocoder.
//
//   - numBands    – number of frequency channels (e.g. 16–32)
//   - lowFreq     – lower frequency bound (Hz)
//   - highFreq    – upper frequency bound (Hz)
//   - q           – Q factor for each bandpass filter
//   - attackTime  – envelope follower attack time (seconds)
//   - releaseTime – envelope follower release time (seconds)
//   - sampleRate  – audio sampling rate (Hz)
func NewVocoder(numBands int, lowFreq, highFreq, q, attackTime, releaseTime, sampleRate float64) *Vocoder {
	centerFreqs := spreadFrequencies(numBands, lowFreq, highFreq)

	analysisFilters := make([]BiquadFilter, numBands)
	synthesisFilters := make([]BiquadFilter, numBands)
	envelopeFollowers := make([]EnvelopeFollower, numBands)
	envelopes := make([]float64, numBands)

	for i := 0; i < numBands; i++ {
		analysisFilters[i] = NewBandpass(centerFreqs[i], q, sampleRate)
		synthesisFilters[i] = NewBandpass(centerFreqs[i], q, sampleRate)
		envelopeFollowers[i] = NewEnvelopeFollower(attackTime, releaseTime, sampleRate)
		envelopes[i] = 0.0
	}

	return &Vocoder{
		analysisFilters:   analysisFilters,
		synthesisFilters:  synthesisFilters,
		envelopeFollowers: envelopeFollowers,
		envelopes:         envelopes,
		centerFreqs:       centerFreqs,
		numBands:          numBands,
	}
}

// Process processes one sample pair (modulator, carrier) and returns the
// vocoded output sample.
//
// This method performs ZERO heap allocations. All work is done in-place on
// pre-allocated slices.
//
//go:nosplit
func (v *Vocoder) Process(modulator, carrier float64) float64 {
	var output float64

	for i := 0; i < v.numBands; i++ {
		// Analyse the modulator through the analysis bandpass filter.
		filteredMod := v.analysisFilters[i].Process(modulator)

		// Track the envelope.
		v.envelopes[i] = v.envelopeFollowers[i].Process(filteredMod)

		// Synthesise: filter the carrier through the matching bandpass.
		filteredCarrier := v.synthesisFilters[i].Process(carrier)

		// Apply the modulator envelope to the filtered carrier.
		output += v.envelopes[i] * filteredCarrier
	}

	return output
}

// ProcessBlock processes a block of samples.
//
// modulator, carrier, and output must have the same length. The vocoded
// output is written into output.
func (v *Vocoder) ProcessBlock(modulator, carrier, output []float64) {
	length := len(modulator)
	if len(carrier) < length {
		length = len(carrier)
	}
	if len(output) < length {
		length = len(output)
	}
	for i := 0; i < length; i++ {
		output[i] = v.Process(modulator[i], carrier[i])
	}
}

// Envelopes returns the current envelope values for each band (read-only).
func (v *Vocoder) Envelopes() []float64 {
	return v.envelopes
}

// CenterFrequencies returns the centre frequencies of the bandpass filters.
func (v *Vocoder) CenterFrequencies() []float64 {
	return v.centerFreqs
}

// NumBands returns the number of frequency bands.
func (v *Vocoder) NumBands() int {
	return v.numBands
}

// Reset resets all internal state (filters and envelope followers).
func (v *Vocoder) Reset() {
	for i := range v.analysisFilters {
		v.analysisFilters[i].Reset()
	}
	for i := range v.synthesisFilters {
		v.synthesisFilters[i].Reset()
	}
	for i := range v.envelopeFollowers {
		v.envelopeFollowers[i].Reset()
	}
	for i := range v.envelopes {
		v.envelopes[i] = 0.0
	}
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

// spreadFrequencies generates n centre frequencies logarithmically spaced
// between low and high.
func spreadFrequencies(n int, low, high float64) []float64 {
	if n == 0 {
		return []float64{}
	}
	// Clamp to safe positive values so ln() is well-defined.
	low = math.Max(low, 1e-6)
	high = math.Max(high, low*2.0)
	if n == 1 {
		return []float64{math.Sqrt(low * high)}
	}
	logLow := math.Log(low)
	logHigh := math.Log(high)

	freqs := make([]float64, n)
	for i := 0; i < n; i++ {
		t := float64(i) / float64(n-1)
		freqs[i] = math.Exp(logLow + t*(logHigh-logLow))
	}
	return freqs
}
