package vocoder

import (
	"math"
	"testing"
)

// ---------------------------------------------------------------------------
// spreadFrequencies tests
// ---------------------------------------------------------------------------

func TestSpreadFrequencies(t *testing.T) {
	freqs := spreadFrequencies(4, 100.0, 1000.0)
	if len(freqs) != 4 {
		t.Fatalf("expected 4 frequencies, got %d", len(freqs))
	}
	if math.Abs(freqs[0]-100.0) > 1e-6 {
		t.Errorf("first freq should be 100, got %f", freqs[0])
	}
	if math.Abs(freqs[3]-1000.0) > 1e-6 {
		t.Errorf("last freq should be 1000, got %f", freqs[3])
	}
	ratio := freqs[1] / freqs[0]
	if math.Abs(freqs[2]/freqs[1]-ratio) > 1e-6 {
		t.Errorf("log spacing violated between bands 1-2")
	}
	if math.Abs(freqs[3]/freqs[2]-ratio) > 1e-6 {
		t.Errorf("log spacing violated between bands 2-3")
	}
}

func TestSpreadFrequenciesSingle(t *testing.T) {
	freqs := spreadFrequencies(1, 200.0, 800.0)
	if len(freqs) != 1 {
		t.Fatalf("expected 1 frequency, got %d", len(freqs))
	}
	expected := math.Sqrt(200.0 * 800.0)
	if math.Abs(freqs[0]-expected) > 1e-6 {
		t.Errorf("expected %f, got %f", expected, freqs[0])
	}
}

func TestSpreadFrequenciesEmpty(t *testing.T) {
	freqs := spreadFrequencies(0, 100.0, 1000.0)
	if len(freqs) != 0 {
		t.Fatalf("expected 0 frequencies, got %d", len(freqs))
	}
}

func TestSpreadFrequenciesLowZero(t *testing.T) {
	freqs := spreadFrequencies(4, 0.0, 1000.0)
	for _, f := range freqs {
		if !math.IsFinite(f) || f <= 0.0 {
			t.Errorf("frequency must be finite and positive, got %f", f)
		}
	}
}

func TestSpreadFrequenciesNegativeBounds(t *testing.T) {
	freqs := spreadFrequencies(4, -100.0, -10.0)
	for _, f := range freqs {
		if !math.IsFinite(f) || f <= 0.0 {
			t.Errorf("frequency must be finite and positive, got %f", f)
		}
	}
}

func TestSpreadFrequenciesN2(t *testing.T) {
	freqs := spreadFrequencies(2, 100.0, 1000.0)
	if len(freqs) != 2 {
		t.Fatalf("expected 2, got %d", len(freqs))
	}
	if math.Abs(freqs[0]-100.0) > 1e-6 {
		t.Errorf("first should be 100, got %f", freqs[0])
	}
	if math.Abs(freqs[1]-1000.0) > 1e-6 {
		t.Errorf("last should be 1000, got %f", freqs[1])
	}
}

// ---------------------------------------------------------------------------
// BiquadFilter tests
// ---------------------------------------------------------------------------

func TestBiquadBandpassSilence(t *testing.T) {
	bp := NewBandpass(1000.0, 5.0, 44100.0)
	for i := 0; i < 100; i++ {
		out := bp.Process(0.0)
		if out != 0.0 {
			t.Fatalf("zero input must produce zero output at sample %d, got %f", i, out)
		}
	}
}

func TestBiquadBandpassResonatesAtCenter(t *testing.T) {
	sampleRate := 44100.0
	center := 1000.0
	bp := NewBandpass(center, 10.0, sampleRate)

	for i := 0; i < 64; i++ {
		bp.Process(1.0)
	}
	maxVal := 0.0
	for i := 0; i < 200; i++ {
		out := bp.Process(0.0)
		if math.Abs(out) > maxVal {
			maxVal = math.Abs(out)
		}
	}
	if maxVal <= 0.01 {
		t.Errorf("bandpass should ring after impulse, max=%f", maxVal)
	}
}

func TestBiquadReset(t *testing.T) {
	bp := NewBandpass(1000.0, 5.0, 44100.0)
	bp.Process(1.0)
	bp.Process(0.5)
	bp.Reset()
	out := bp.Process(0.0)
	if out != 0.0 {
		t.Errorf("after reset, process(0) should be 0, got %f", out)
	}
}

func TestBiquadQZeroNoPanic(t *testing.T) {
	bp := NewBandpass(1000.0, 0.0, 44100.0)
	out := bp.Process(1.0)
	if !math.IsFinite(out) {
		t.Errorf("output must be finite with q=0, got %f", out)
	}
}

func TestBiquadNegativeFrequencyNoNaN(t *testing.T) {
	bp := NewBandpass(-500.0, 5.0, 44100.0)
	out := bp.Process(1.0)
	if !math.IsFinite(out) {
		t.Errorf("output must be finite with negative freq, got %f", out)
	}
}

func TestBiquadZeroFrequencyNoNaN(t *testing.T) {
	bp := NewBandpass(0.0, 5.0, 44100.0)
	out := bp.Process(1.0)
	if !math.IsFinite(out) {
		t.Errorf("output must be finite with zero freq, got %f", out)
	}
}

func TestBiquadZeroSampleRateNoNaN(t *testing.T) {
	bp := NewBandpass(1000.0, 5.0, 0.0)
	out := bp.Process(1.0)
	if !math.IsFinite(out) {
		t.Errorf("output must be finite with zero sample_rate, got %f", out)
	}
}

func TestBiquadAboveNyquistClamped(t *testing.T) {
	bp := NewBandpass(30000.0, 5.0, 44100.0)
	out := bp.Process(1.0)
	if !math.IsFinite(out) {
		t.Errorf("output must be finite with freq above Nyquist, got %f", out)
	}
}

func TestBiquadLinearity(t *testing.T) {
	bpScaled := NewBandpass(1000.0, 5.0, 44100.0)
	bpBase := NewBandpass(1000.0, 5.0, 44100.0)
	a := 2.0
	for i := 0; i < 100; i++ {
		x := 0.5
		scaled := bpScaled.Process(a * x)
		base := bpBase.Process(x)
		expected := a * base
		if math.Abs(scaled-expected) > 1e-10 {
			t.Fatalf("linearity violated at sample %d: process(%v*%v) = %v, but %v*process(%v) = %v",
				i, a, x, scaled, a, x, expected)
		}
	}
}

func TestBiquadTimeInvariance(t *testing.T) {
	input := make([]float64, 50)
	for i := range input {
		input[i] = math.Sin(float64(i) * 0.3)
	}
	bp1 := NewBandpass(1000.0, 5.0, 44100.0)
	outputs1 := make([]float64, 50)
	for i, x := range input {
		outputs1[i] = bp1.Process(x)
	}
	bp2 := NewBandpass(1000.0, 5.0, 44100.0)
	outputs2 := make([]float64, 50)
	for i, x := range input {
		outputs2[i] = bp2.Process(x)
	}
	for i := range outputs1 {
		if math.Abs(outputs1[i]-outputs2[i]) > 1e-15 {
			t.Fatalf("time-invariance violated at sample %d: %v != %v", i, outputs1[i], outputs2[i])
		}
	}
}

func TestDenormalsFlushedInBiquad(t *testing.T) {
	bp := NewBandpass(1000.0, 5.0, 44100.0)
	bp.Process(1.0)
	for i := 0; i < 100000; i++ {
		bp.Process(0.0)
	}
	out := bp.Process(0.0)
	if out != 0.0 && math.Abs(out) >= 1e-15 {
		t.Errorf("denormals should be flushed, got %f", out)
	}
}

// ---------------------------------------------------------------------------
// EnvelopeFollower tests
// ---------------------------------------------------------------------------

func TestEnvelopeFollowerAttackFasterThanRelease(t *testing.T) {
	sampleRate := 44100.0
	ef := NewEnvelopeFollower(0.0001, 0.1, sampleRate)

	riseCount := 0
	for i := 0; i < int(sampleRate); i++ {
		v := ef.Process(1.0)
		if v < 0.99 {
			riseCount++
		}
	}
	riseSamples := riseCount

	ef.Reset()
	for i := 0; i < int(sampleRate)*2; i++ {
		ef.Process(1.0)
	}
	fallCount := 0
	for i := 0; i < int(sampleRate); i++ {
		v := ef.Process(0.0)
		if v > 0.01 {
			fallCount++
		}
	}

	if riseSamples >= fallCount {
		t.Errorf("attack should be faster than release: rise=%d, fall=%d", riseSamples, fallCount)
	}
}

func TestEnvelopeFollowerValue(t *testing.T) {
	ef := NewEnvelopeFollower(0.001, 0.05, 44100.0)
	if ef.Value() != 0.0 {
		t.Errorf("initial value should be 0, got %f", ef.Value())
	}
	ef.Process(0.5)
	if ef.Value() <= 0.0 {
		t.Errorf("value should be positive after processing 0.5, got %f", ef.Value())
	}
}

func TestEnvelopeFollowerReset(t *testing.T) {
	ef := NewEnvelopeFollower(0.001, 0.05, 44100.0)
	ef.Process(1.0)
	if ef.Value() <= 0.0 {
		t.Fatalf("should be positive after input")
	}
	ef.Reset()
	if ef.Value() != 0.0 {
		t.Errorf("should be 0 after reset, got %f", ef.Value())
	}
}

func TestEnvelopeFollowerSteadyStateAccuracy(t *testing.T) {
	ef := NewEnvelopeFollower(0.001, 0.05, 44100.0)
	target := 0.75
	for i := 0; i < 100000; i++ {
		ef.Process(target)
	}
	val := ef.Value()
	if math.Abs(val-target) > 1e-6 {
		t.Errorf("envelope should converge to |x|, got %f vs target %f", val, target)
	}
}

func TestEnvelopeFollowerMonotonicNonDecreasing(t *testing.T) {
	ef := NewEnvelopeFollower(0.001, 0.05, 44100.0)
	prev := ef.Process(0.5)
	for i := 0; i < 1000; i++ {
		curr := ef.Process(0.5)
		if curr < prev-1e-15 {
			t.Fatalf("envelope must be monotonically non-decreasing for constant input, prev=%v, curr=%v", prev, curr)
		}
		prev = curr
	}
}

func TestEnvelopeFollowerNegativeAttackTime(t *testing.T) {
	ef := NewEnvelopeFollower(-0.001, 0.05, 44100.0)
	out := ef.Process(1.0)
	if !math.IsFinite(out) {
		t.Errorf("envelope must be finite, got %f", out)
	}
	if out <= 0.0 {
		t.Errorf("envelope should be positive after positive input, got %f", out)
	}
}

func TestEnvelopeFollowerZeroAttackTime(t *testing.T) {
	ef := NewEnvelopeFollower(0.0, 0.05, 44100.0)
	for i := 0; i < 10; i++ {
		ef.Process(0.5)
	}
	val := ef.Value()
	if math.Abs(val-0.5) >= 0.1 {
		t.Errorf("should converge to input value with zero attack, got %f", val)
	}
}

func TestDenormalsFlushedInEnvelopeFollower(t *testing.T) {
	ef := NewEnvelopeFollower(0.001, 0.001, 44100.0)
	ef.Process(1.0)
	for i := 0; i < 500000; i++ {
		ef.Process(0.0)
	}
	val := ef.Value()
	if val != 0.0 && val >= 1e-15 {
		t.Errorf("denormals should be flushed in envelope follower, got %f", val)
	}
}

// ---------------------------------------------------------------------------
// Vocoder tests
// ---------------------------------------------------------------------------

func TestVocoderProcessSilence(t *testing.T) {
	v := NewVocoder(8, 200.0, 8000.0, 4.0, 0.001, 0.05, 44100.0)
	for i := 0; i < 100; i++ {
		out := v.Process(0.0, 0.0)
		if math.Abs(out) >= 1e-12 {
			t.Fatalf("silence should produce silence, got %f", out)
		}
	}
}

func TestVocoderModulatedCarrierLouder(t *testing.T) {
	v := NewVocoder(16, 200.0, 8000.0, 4.0, 0.001, 0.05, 44100.0)

	// Feed carrier with no modulator.
	var unmodulatedEnergy float64
	for i := 0; i < 4096; i++ {
		carrier := math.Sin(0.5)
		out := v.Process(0.0, carrier)
		unmodulatedEnergy += out * out
	}

	v.Reset()

	// Now feed both modulator and carrier.
	var modulatedEnergy float64
	sampleRate := 44100.0
	for i := 0; i < 4096; i++ {
		t := float64(i) / sampleRate
		modulator := math.Sin(2.0 * math.Pi * 440.0 * t)
		carrier := math.Sin(2.0 * math.Pi * 220.0 * t)
		out := v.Process(modulator, carrier)
		modulatedEnergy += out * out
	}

	if modulatedEnergy <= unmodulatedEnergy {
		t.Errorf("modulated energy (%f) should exceed unmodulated (%f)", modulatedEnergy, unmodulatedEnergy)
	}
}

func TestVocoderProcessBlock(t *testing.T) {
	v := NewVocoder(8, 200.0, 8000.0, 4.0, 0.001, 0.05, 44100.0)
	length := 256
	modulator := make([]float64, length)
	carrier := make([]float64, length)
	output := make([]float64, length)
	for i := 0; i < length; i++ {
		modulator[i] = math.Sin(float64(i) * 0.1)
		carrier[i] = math.Cos(float64(i) * 0.05)
	}

	v.ProcessBlock(modulator, carrier, output)

	nonzero := 0
	for _, val := range output {
		if math.Abs(val) > 0.0 {
			nonzero++
		}
	}
	if nonzero == 0 {
		t.Error("block output should contain non-zero samples")
	}
}

func TestVocoderReset(t *testing.T) {
	v := NewVocoder(8, 200.0, 8000.0, 4.0, 0.001, 0.05, 44100.0)
	for i := 0; i < 100; i++ {
		v.Process(0.5, 0.5)
	}
	v.Reset()
	for _, e := range v.Envelopes() {
		if e != 0.0 {
			t.Errorf("envelope should be 0.0 after reset, got %f", e)
		}
	}
	for i := 0; i < 50; i++ {
		out := v.Process(0.0, 0.0)
		if math.Abs(out) >= 1e-12 {
			t.Fatalf("silence after reset should be silent, got %f", out)
		}
	}
}

func TestVocoderNumBandsAndFreqs(t *testing.T) {
	v := NewVocoder(20, 100.0, 10000.0, 5.0, 0.001, 0.05, 44100.0)
	if v.NumBands() != 20 {
		t.Errorf("expected 20 bands, got %d", v.NumBands())
	}
	freqs := v.CenterFrequencies()
	if len(freqs) != 20 {
		t.Errorf("expected 20 center frequencies, got %d", len(freqs))
	}
	if math.Abs(freqs[0]-100.0) > 1e-6 {
		t.Errorf("first freq should be 100, got %f", freqs[0])
	}
	if math.Abs(freqs[19]-10000.0) > 1e-6 {
		t.Errorf("last freq should be 10000, got %f", freqs[19])
	}
}

func TestVocoderFiniteOutput(t *testing.T) {
	v := NewVocoder(20, 200.0, 8000.0, 4.0, 0.001, 0.05, 44100.0)
	seed := uint64(42)
	for i := 0; i < 10000; i++ {
		seed = seed*6364136223846793005 + 1
		modulator := float64(int64(seed>>33)) / float64(int64(0x7FFFFFFFFFFFFFFF))
		seed = seed*6364136223846793005 + 1
		carrier := float64(int64(seed>>33)) / float64(int64(0x7FFFFFFFFFFFFFFF))
		out := v.Process(modulator, carrier)
		if !math.IsFinite(out) {
			t.Fatalf("output must be finite at sample %d, got %f", i, out)
		}
	}
}

func TestVocoderProcessBlockTruncates(t *testing.T) {
	v := NewVocoder(8, 200.0, 8000.0, 4.0, 0.001, 0.05, 44100.0)
	modulator := make([]float64, 256)
	carrier := make([]float64, 128)
	output := make([]float64, 256)
	for i := range modulator {
		modulator[i] = 0.5
	}
	for i := range carrier {
		carrier[i] = 0.3
	}

	v.ProcessBlock(modulator, carrier, output)

	for i := 128; i < 256; i++ {
		if output[i] != 0.0 {
			t.Errorf("output[%d] should remain 0.0 (beyond carrier length), got %f", i, output[i])
		}
	}
}

func TestVocoderZeroBandsNoPanic(t *testing.T) {
	v := NewVocoder(0, 200.0, 8000.0, 4.0, 0.001, 0.05, 44100.0)
	out := v.Process(0.5, 0.5)
	if out != 0.0 {
		t.Errorf("zero-band vocoder should produce 0.0, got %f", out)
	}
}

func TestVocoderProcessVsProcessBlock(t *testing.T) {
	modulator := make([]float64, 128)
	carrier := make([]float64, 128)
	for i := range modulator {
		modulator[i] = math.Sin(float64(i) * 0.1)
	}
	for i := range carrier {
		carrier[i] = math.Cos(float64(i) * 0.05)
	}

	v1 := NewVocoder(8, 200.0, 8000.0, 4.0, 0.001, 0.05, 44100.0)
	outputsSingle := make([]float64, 128)
	for i := range modulator {
		outputsSingle[i] = v1.Process(modulator[i], carrier[i])
	}

	v2 := NewVocoder(8, 200.0, 8000.0, 4.0, 0.001, 0.05, 44100.0)
	outputsBlock := make([]float64, 128)
	v2.ProcessBlock(modulator, carrier, outputsBlock)

	for i := range outputsSingle {
		if math.Abs(outputsSingle[i]-outputsBlock[i]) > 1e-15 {
			t.Fatalf("process and process_block should match at sample %d: %v != %v",
				i, outputsSingle[i], outputsBlock[i])
		}
	}
}
