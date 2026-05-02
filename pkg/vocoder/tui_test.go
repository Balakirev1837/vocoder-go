package vocoder

import (
	"math"
	"testing"
)

// ── SharedState tests ──────────────────────────────────────────────

func TestNewSharedStateDefaults(t *testing.T) {
	state := NewSharedState()
	if state.Config.SampleRate != 44100 {
		t.Errorf("expected default sample rate 44100, got %d", state.Config.SampleRate)
	}
	if state.Config.BufferSize != 512 {
		t.Errorf("expected default buffer size 512, got %d", state.Config.BufferSize)
	}
	if state.Config.Gain != 1.0 {
		t.Errorf("expected default gain 1.0, got %f", state.Config.Gain)
	}
	if state.Config.KeyboardMode {
		t.Error("expected KeyboardMode to be false by default")
	}
	if state.Vocoder == nil {
		t.Error("expected Vocoder to be initialised")
	}
	if state.ActiveNotes == nil {
		t.Error("expected ActiveNotes to be initialised")
	}
	if state.Phases == nil {
		t.Error("expected Phases map to be initialised")
	}
}

// ── ProcessAudio tests ─────────────────────────────────────────────

func TestProcessAudioSilenceNoNotes(t *testing.T) {
	state := NewSharedState()
	output := make([]float32, 256)
	modulator := make([]float32, 256)

	state.ProcessAudio(modulator, output, 2)

	for i, s := range output {
		if s != 0.0 {
			t.Errorf("expected silence with no notes, got %f at sample %d", s, i)
		}
	}
}

func TestProcessAudioWithNotesProducesSound(t *testing.T) {
	state := NewSharedState()
	state.mu.Lock()
	state.Config.KeyboardMode = true
	state.mu.Unlock()
	state.ActiveNotes.On(60) // Middle C

	output := make([]float32, 512)
	modulator := make([]float32, 512)

	state.ProcessAudio(modulator, output, 2)

	hasNonZero := false
	for _, s := range output {
		if s != 0.0 {
			hasNonZero = true
			break
		}
	}
	if !hasNonZero {
		t.Error("expected non-zero output with active notes in keyboard mode")
	}
}

func TestProcessAudioKeyboardModeBypass(t *testing.T) {
	state := NewSharedState()
	state.ActiveNotes.On(69) // A4 = 440 Hz
	// Keyboard mode is false by default (vocoder mode). Switch to keyboard.
	state.mu.Lock()
	state.Config.KeyboardMode = true
	state.mu.Unlock()

	output := make([]float32, 1024)
	modulator := make([]float32, 1024)

	state.ProcessAudio(modulator, output, 2)

	// In keyboard mode with a single note, output should be close to
	// sin(2π * phase) * tanh(1/sqrt(1)) * gain = sin * tanh(1) * 1.0
	// which is non-zero for most frames.
	nonZero := 0
	for _, s := range output {
		if math.Abs(float64(s)) > 1e-10 {
			nonZero++
		}
	}
	if nonZero == 0 {
		t.Error("expected non-zero samples in keyboard mode")
	}
}

func TestProcessAudioVocoderMode(t *testing.T) {
	state := NewSharedState()
	state.ActiveNotes.On(60)
	// Default is vocoder mode (KeyboardMode = false).

	output := make([]float32, 512)
	modulator := make([]float32, 512)
	// Feed a 440 Hz modulator signal.
	sr := float64(state.SampleRate)
	for i := range modulator {
		modulator[i] = float32(math.Sin(2.0 * math.Pi * 440.0 * float64(i) / sr))
	}

	state.ProcessAudio(modulator, output, 2)

	// Vocoder output with both modulator and carrier should be non-zero.
	nonZero := 0
	for _, s := range output {
		if math.Abs(float64(s)) > 1e-10 {
			nonZero++
		}
	}
	if nonZero == 0 {
		t.Error("expected non-zero samples in vocoder mode with modulator signal")
	}
}

func TestProcessAudioSoftClipping(t *testing.T) {
	state := NewSharedState()
	// Use multiple notes to push the carrier amplitude high, but keep gain=1
	// so that tanh soft clipping keeps output within [-1, 1].
	state.mu.Lock()
	state.Config.Gain = 1.0
	state.Config.KeyboardMode = true
	state.mu.Unlock()

	for n := uint8(60); n < 72; n++ {
		state.ActiveNotes.On(n)
	}

	output := make([]float32, 4096)
	modulator := make([]float32, 4096)

	state.ProcessAudio(modulator, output, 2)

	// Due to tanh soft clipping with gain=1, no sample should exceed ±1.0.
	for i, s := range output {
		if math.Abs(float64(s)) > 1.0+1e-6 {
			t.Errorf("expected soft-clipped output (|sample| <= 1.0), got %f at sample %d", s, i)
		}
	}
	// And with 12 notes there should be non-trivial output.
	maxAbs := 0.0
	for _, s := range output {
		v := math.Abs(float64(s))
		if v > maxAbs {
			maxAbs = v
		}
	}
	if maxAbs < 0.5 {
		t.Errorf("expected substantial output with 12 notes, max abs=%f", maxAbs)
	}
}

func TestProcessAudioGainAppliedAfterClip(t *testing.T) {
	state := NewSharedState()
	// With high gain, tanh clips to [-1,1] first, then gain amplifies.
	state.mu.Lock()
	state.Config.Gain = 5.0
	state.Config.KeyboardMode = true
	state.mu.Unlock()
	state.ActiveNotes.On(60)

	output := make([]float32, 4096)
	modulator := make([]float32, 4096)

	state.ProcessAudio(modulator, output, 2)

	// Output should be in [-5, 5] range (tanh(-1..1) * 5).
	for i, s := range output {
		v := math.Abs(float64(s))
		if v > 5.0+1e-6 {
			t.Errorf("expected |sample| <= 5.0 with gain=5, got %f at sample %d", s, i)
		}
	}
}

func TestProcessAudioGainZero(t *testing.T) {
	state := NewSharedState()
	state.mu.Lock()
	state.Config.Gain = 0.0
	state.Config.KeyboardMode = true
	state.mu.Unlock()

	state.ActiveNotes.On(60)

	output := make([]float32, 256)
	modulator := make([]float32, 256)

	state.ProcessAudio(modulator, output, 2)

	for i, s := range output {
		if s != 0.0 {
			t.Errorf("expected silence with zero gain, got %f at sample %d", s, i)
		}
	}
}

func TestProcessAudioUpdatesLevels(t *testing.T) {
	state := NewSharedState()
	state.mu.Lock()
	state.Config.KeyboardMode = true
	state.mu.Unlock()

	state.ActiveNotes.On(60)

	output := make([]float32, 512)
	modulator := make([]float32, 512)
	// Feed a strong input signal.
	for i := range modulator {
		modulator[i] = 0.8
	}

	state.ProcessAudio(modulator, output, 2)

	state.mu.Lock()
	inLevel := state.InputLevel
	outLevel := state.OutputLevel
	state.mu.Unlock()

	if inLevel < 0.7 {
		t.Errorf("expected input level >= 0.7, got %f", inLevel)
	}
	if outLevel <= 0.0 {
		t.Errorf("expected positive output level, got %f", outLevel)
	}
}

// ── Helper function tests ──────────────────────────────────────────

func TestNoteName(t *testing.T) {
	tests := []struct {
		note uint8
		want string
	}{
		{0, "C-1"},
		{60, "C4"},
		{69, "A4"},
		{127, "G9"},
		{62, "D4"},
	}
	for _, tt := range tests {
		got := noteName(tt.note)
		if got != tt.want {
			t.Errorf("noteName(%d) = %q, want %q", tt.note, got, tt.want)
		}
	}
}

// ── configField.needsRestart tests ─────────────────────────────────

func TestConfigFieldNeedsRestart(t *testing.T) {
	restartFields := []configField{
		fieldAudioInputDevice,
		fieldAudioOutputDevice,
		fieldMidiInputPort,
		fieldSampleRate,
		fieldBufferSize,
		fieldBands,
	}
	for _, f := range restartFields {
		if !f.needsRestart() {
			t.Errorf("expected field %d to need restart", f)
		}
	}

	noRestartFields := []configField{
		fieldMidiChannel,
		fieldFormantShift,
		fieldPitchShift,
		fieldGain,
	}
	for _, f := range noRestartFields {
		if f.needsRestart() {
			t.Errorf("expected field %d to NOT need restart", f)
		}
	}
}
