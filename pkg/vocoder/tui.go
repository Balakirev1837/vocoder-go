package vocoder

import (
	"fmt"
	"math"
	"strings"
	"sync"
	"time"

	"github.com/charmbracelet/bubbles/progress"
	"github.com/charmbracelet/bubbles/spinner"
	"github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
)

// midiFreqs is a precomputed lookup table mapping MIDI note numbers
// (0–127) to their corresponding frequencies in Hz.
var midiFreqs [128]float64

func init() {
	for i := 0; i < 128; i++ {
		midiFreqs[i] = 440.0 * math.Pow(2.0, (float64(i)-69.0)/12.0)
	}
}

// softClip applies a fast, branchless soft-clipping function:
// x / (1 + |x|), which maps real numbers into (-1, 1).
func softClip(x float64) float64 {
	return x / (1.0 + math.Abs(x))
}

// ── Configurable parameters for the vocoder ────────────────────────

// Config holds all configurable parameters for the vocoder, mirroring
// the Rust TUI's Config struct.
type Config struct {
	AudioInputDevice  *string
	AudioOutputDevice *string
	MidiInputPort     *string
	SampleRate        uint32
	BufferSize        uint32
	MidiChannel       uint8
	FormantShift      float32
	PitchShift        float32
	Gain              float32
	// Number of vocoder filter bands.
	Bands int
	// When true, bypasses vocoder DSP and outputs the raw carrier signal
	// multiplied by gain (Keyboard mode). When false, normal vocoder
	// processing is applied (Vocoder mode).
	KeyboardMode bool
	// Carrier waveform: 0=Sine, 1=Sawtooth, 2=Square.
	Waveform int
	// Envelope follower speed: 0=Fast, 1=Medium, 2=Slow.
	EnvelopeSpeed int
	// When true, sets up a virtual microphone device for output.
	VirtualMic bool
}

// DefaultConfig returns a Config populated with sensible defaults,
// matching the Rust version's Default implementation.
func DefaultConfig() Config {
	return Config{
		AudioInputDevice:  nil,
		AudioOutputDevice: nil,
		MidiInputPort:     nil,
		SampleRate:        44100,
		BufferSize:        512,
		MidiChannel:       1,
		FormantShift:      1.0,
		PitchShift:        0.0,
		Gain:              1.0,
		Bands:             20,
		KeyboardMode:      false,
		Waveform:          0,
		EnvelopeSpeed:     1,
		VirtualMic:        false,
	}
}

// ── Shared state between TUI and audio/MIDI ────────────────────────

// SharedState holds the state shared between the TUI goroutine and the
// audio/MIDI callback goroutines. Access to mutable fields is protected
// by mu.
type SharedState struct {
	mu sync.Mutex

	// Config is modified by the TUI goroutine and read by the audio
	// callback (only Gain and KeyboardMode are read at audio-time).
	Config Config

	// Live status updated by the audio callback, read by the TUI.
	InputLevel  float32
	OutputLevel float32
	AudioActive bool
	MIDIActive  bool

	// Resources managed by the TUI goroutine.
	AudioStreams *AudioStreams
	MIDIStop     func()

	// DSP state – only accessed from the audio callback thread.
	// These are safe because the audio stream is stopped (and the
	// callback quiesced) before these fields are modified during a
	// restart.
	ActiveNotes *ActiveNotes
	Vocoder     *Vocoder
	Phases      map[uint8]float64
	SampleRate  float64
}

// NewSharedState creates a SharedState with default config and DSP.
func NewSharedState() *SharedState {
	return &SharedState{
		Config:      DefaultConfig(),
		ActiveNotes: NewActiveNotes(),
		Vocoder:     NewVocoder(20, 200.0, 8000.0, 4.0, 0.001, 0.05, 44100.0),
		Phases:      make(map[uint8]float64),
		SampleRate:  44100.0,
	}
}

// ProcessAudio is the audio callback invoked for each audio block.
//
// It reads the active MIDI notes, generates a carrier (sum of sines
// normalised by 1/sqrt(N)), applies vocoder DSP (or bypasses it in
// Keyboard mode), applies soft clipping, and applies makeup gain.
func (s *SharedState) ProcessAudio(modulator []float32, output []float32, channels uint32) {
	s.mu.Lock()
	gain := s.Config.Gain
	keyboardMode := s.Config.KeyboardMode
	waveform := s.Config.Waveform
	s.mu.Unlock()

	notes := s.ActiveNotes.List()
	n := len(notes)
	sr := s.SampleRate

	frames := len(output) / int(channels)
	var maxInput, maxOutput float32

	for i := 0; i < frames; i++ {
		idx := i * int(channels)

		// Read modulator (first channel).
		var mod float64
		if idx < len(modulator) {
			mod = float64(modulator[idx])
		}
		absIn := float32(math.Abs(mod))
		if absIn > maxInput {
			maxInput = absIn
		}

		// Generate carrier based on waveform selection.
		var carrier float64
		if n > 0 {
			invSqrtN := 1.0 / math.Sqrt(float64(n))
			for _, note := range notes {
				freq := midiFreqs[note]
				s.Phases[note] += freq / sr
				// Wrap phase to [0, 1).
				s.Phases[note] -= math.Floor(s.Phases[note])
				var sample float64
				switch waveform {
				case 1: // Sawtooth
					sample = 2.0*s.Phases[note] - 1.0
				case 2: // Square
					if s.Phases[note] < 0.5 {
						sample = 1.0
					} else {
						sample = -1.0
					}
				default: // Sine
					sample = math.Sin(2.0 * math.Pi * s.Phases[note])
				}
				carrier += sample * invSqrtN
			}
		}

		// Apply vocoder DSP or bypass.
		var sample float64
		if keyboardMode {
			sample = carrier
		} else {
			sample = s.Vocoder.Process(mod, carrier)
		}

		// Soft clip.
		sample = softClip(sample)

		// Makeup gain.
		sample *= float64(gain)

		out := float32(sample)
		absOut := float32(math.Abs(float64(out)))
		if absOut > maxOutput {
			maxOutput = absOut
		}

		// Fill all output channels.
		for ch := 0; ch < int(channels); ch++ {
			if idx+ch < len(output) {
				output[idx+ch] = out
			}
		}
	}

	// Update status for TUI.
	s.mu.Lock()
	s.InputLevel = maxInput
	s.OutputLevel = maxOutput
	s.mu.Unlock()
}

// ── Config field definitions ───────────────────────────────────────

// configField represents one selectable row in the configuration panel.
type configField int

const (
	fieldAudioInputDevice configField = iota
	fieldAudioOutputDevice
	fieldMidiInputPort
	fieldSampleRate
	fieldBufferSize
	fieldMidiChannel
	fieldFormantShift
	fieldPitchShift
	fieldGain
	fieldBands
	fieldWaveform
	fieldEnvelopeSpeed
	fieldVirtualMic
)

var configFields = []configField{
	fieldAudioInputDevice,
	fieldAudioOutputDevice,
	fieldMidiInputPort,
	fieldSampleRate,
	fieldBufferSize,
	fieldMidiChannel,
	fieldFormantShift,
	fieldPitchShift,
	fieldGain,
	fieldBands,
	fieldWaveform,
	fieldEnvelopeSpeed,
	fieldVirtualMic,
}

func (f configField) label() string {
	switch f {
	case fieldAudioInputDevice:
		return "Audio Input"
	case fieldAudioOutputDevice:
		return "Audio Output"
	case fieldMidiInputPort:
		return "MIDI Input"
	case fieldSampleRate:
		return "Sample Rate"
	case fieldBufferSize:
		return "Buffer Size"
	case fieldMidiChannel:
		return "MIDI Channel"
	case fieldFormantShift:
		return "Formant Shift"
	case fieldPitchShift:
		return "Pitch Shift"
	case fieldGain:
		return "Gain"
	case fieldBands:
		return "Bands"
	case fieldWaveform:
		return "Waveform"
	case fieldEnvelopeSpeed:
		return "Env Speed"
	case fieldVirtualMic:
		return "Virtual Mic"
	default:
		return ""
	}
}

func (f configField) displayValue(cfg Config) string {
	switch f {
	case fieldAudioInputDevice:
		if cfg.AudioInputDevice != nil {
			return *cfg.AudioInputDevice
		}
		return "(default)"
	case fieldAudioOutputDevice:
		if cfg.AudioOutputDevice != nil {
			return *cfg.AudioOutputDevice
		}
		return "(default)"
	case fieldMidiInputPort:
		if cfg.MidiInputPort != nil {
			return *cfg.MidiInputPort
		}
		return "(first available)"
	case fieldSampleRate:
		return fmt.Sprintf("%d Hz", cfg.SampleRate)
	case fieldBufferSize:
		return fmt.Sprintf("%d samples", cfg.BufferSize)
	case fieldMidiChannel:
		return fmt.Sprintf("Channel %d", cfg.MidiChannel)
	case fieldFormantShift:
		return fmt.Sprintf("%.2fx", cfg.FormantShift)
	case fieldPitchShift:
		return fmt.Sprintf("%+.1f semitones", cfg.PitchShift)
	case fieldGain:
		return fmt.Sprintf("%.0f%%", cfg.Gain*100.0)
	case fieldBands:
		return fmt.Sprintf("%d", cfg.Bands)
	case fieldWaveform:
		switch cfg.Waveform {
		case 0:
			return "Sine"
		case 1:
			return "Sawtooth"
		case 2:
			return "Square"
		default:
			return "Sine"
		}
	case fieldEnvelopeSpeed:
		switch cfg.EnvelopeSpeed {
		case 0:
			return "Fast"
		case 1:
			return "Medium"
		case 2:
			return "Slow"
		default:
			return "Medium"
		}
	case fieldVirtualMic:
		if cfg.VirtualMic {
			return "On"
		}
		return "Off"
	default:
		return ""
	}
}

// adjust modifies the config value for numeric/mode fields by delta.
// Device fields are handled by cycleDevice instead.
func (f configField) adjust(cfg *Config, delta int) {
	switch f {
	case fieldAudioInputDevice, fieldAudioOutputDevice, fieldMidiInputPort:
		// Device fields handled by cycleDevice
	case fieldSampleRate:
		opts := []uint32{22050, 44100, 48000, 96000}
		idx := sliceIndex(opts, cfg.SampleRate)
		if idx < 0 {
			idx = 1
		}
		newIdx := clamp(idx+delta, 0, len(opts)-1)
		cfg.SampleRate = opts[newIdx]
	case fieldBufferSize:
		opts := []uint32{128, 256, 512, 1024, 2048}
		idx := sliceIndex(opts, cfg.BufferSize)
		if idx < 0 {
			idx = 2
		}
		newIdx := clamp(idx+delta, 0, len(opts)-1)
		cfg.BufferSize = opts[newIdx]
	case fieldMidiChannel:
		cfg.MidiChannel = uint8(clamp(int(cfg.MidiChannel)+delta, 1, 16))
	case fieldFormantShift:
		cfg.FormantShift = clampFloat(cfg.FormantShift+float32(delta)*0.05, 0.25, 4.0)
	case fieldPitchShift:
		cfg.PitchShift = clampFloat(cfg.PitchShift+float32(delta)*0.5, -24.0, 24.0)
	case fieldGain:
		cfg.Gain = clampFloat(cfg.Gain+float32(delta)*0.05, 0.0, 10.0)
	case fieldBands:
		opts := []int{8, 12, 16, 20, 24, 32}
		idx := sliceIndex(opts, cfg.Bands)
		if idx < 0 {
			idx = 3 // default 20
		}
		newIdx := clamp(idx+delta, 0, len(opts)-1)
		cfg.Bands = opts[newIdx]
	case fieldWaveform:
		cfg.Waveform = ((cfg.Waveform+delta)%3 + 3) % 3
	case fieldEnvelopeSpeed:
		cfg.EnvelopeSpeed = ((cfg.EnvelopeSpeed+delta)%3 + 3) % 3
	case fieldVirtualMic:
		cfg.VirtualMic = !cfg.VirtualMic
	}
}

// needsRestart returns true if changing this field requires restarting
// the audio/MIDI streams.
func (f configField) needsRestart() bool {
	switch f {
	case fieldAudioInputDevice, fieldAudioOutputDevice,
		fieldMidiInputPort, fieldSampleRate, fieldBufferSize,
		fieldBands, fieldEnvelopeSpeed, fieldVirtualMic:
		return true
	default:
		return false
	}
}

// ── Helpers ────────────────────────────────────────────────────────

// envelopeParams returns the attack and release times (in seconds)
// based on the EnvelopeSpeed setting: 0=Fast, 1=Medium, 2=Slow.
func envelopeParams(speed int) (attack, release float64) {
	switch speed {
	case 0: // Fast
		return 0.001, 0.01
	case 1: // Medium
		return 0.005, 0.05
	case 2: // Slow
		return 0.02, 0.2
	default:
		return 0.005, 0.05
	}
}

func clamp(val, min, max int) int {
	if val < min {
		return min
	}
	if val > max {
		return max
	}
	return val
}

func clampFloat(val, min, max float32) float32 {
	if val < min {
		return min
	}
	if val > max {
		return max
	}
	return val
}

func sliceIndex[T comparable](s []T, target T) int {
	for i, v := range s {
		if v == target {
			return i
		}
	}
	return -1
}

// cycleDevice cycles through a device list, wrapping at the ends.
func cycleDevice(current **string, devices []string, delta int) {
	if len(devices) == 0 {
		return
	}
	idx := 0
	if *current != nil {
		idx = sliceIndex(devices, **current)
		if idx < 0 {
			idx = 0
		}
	}
	newIdx := ((idx+delta)%len(devices) + len(devices)) % len(devices)
	*current = &devices[newIdx]
}

// noteName converts a MIDI note number to a human-readable name (e.g. 60 → "C4").
func noteName(n uint8) string {
	names := []string{"C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"}
	octave := int(n)/12 - 1
	return fmt.Sprintf("%s%d", names[int(n)%12], octave)
}

// ── Styles ─────────────────────────────────────────────────────────

var (
	// Title bar styles
	accentStyle = lipgloss.NewStyle().
			Foreground(lipgloss.Color("#FF00FF")). // Magenta
			Bold(true)

	titleStyle = lipgloss.NewStyle().
			Foreground(lipgloss.Color("#00FFFF")). // Cyan
			Bold(true)

	modeVocoderStyle = lipgloss.NewStyle().
				Foreground(lipgloss.Color("#00FF00")). // Green
				Bold(true)

	modeKeyboardStyle = lipgloss.NewStyle().
				Foreground(lipgloss.Color("#FFFF00")). // Yellow
				Bold(true)

	// Config panel styles
	configTitleStyle = lipgloss.NewStyle().
				Foreground(lipgloss.Color("#00FFFF")). // Cyan
				Bold(true)

	labelStyle = lipgloss.NewStyle().
			Foreground(lipgloss.Color("#FFFFFF")) // White

	valueStyle = lipgloss.NewStyle().
			Foreground(lipgloss.Color("#FFFF00")). // Yellow
			Bold(true)

	selectedStyle = lipgloss.NewStyle().
			Foreground(lipgloss.Color("#FFFFFF")).
			Background(lipgloss.Color("#333333")).
			Bold(true)

	cursorStyle = lipgloss.NewStyle().
			Foreground(lipgloss.Color("#FF00FF")). // Magenta
			Bold(true)

	// Status panel styles
	statusTitleStyle = lipgloss.NewStyle().
				Foreground(lipgloss.Color("#00FFFF")). // Cyan
				Bold(true)

	statusOKStyle = lipgloss.NewStyle().
			Foreground(lipgloss.Color("#00FF00")) // Green

	statusFailStyle = lipgloss.NewStyle().
			Foreground(lipgloss.Color("#FF0000")) // Red

	// Level meter styles
	inputLevelStyle = lipgloss.NewStyle().
			Foreground(lipgloss.Color("#00FF00")). // Green
			Bold(true)

	outputLevelStyle = lipgloss.NewStyle().
				Foreground(lipgloss.Color("#FF00FF")). // Magenta
				Bold(true)

	// Help bar styles
	helpKeyStyle = lipgloss.NewStyle().
			Foreground(lipgloss.Color("#555555"))

	helpDescStyle = lipgloss.NewStyle().
			Foreground(lipgloss.Color("#888888"))

	// Border styles
	borderStyle = lipgloss.NewStyle().
			BorderForeground(lipgloss.Color("#555555"))
)

// ── Bubble Tea messages ────────────────────────────────────────────

// tickMsg is sent periodically to refresh the status display.
type tickMsg time.Time

// ── Bubble Tea Model ───────────────────────────────────────────────

// model is the top-level Bubble Tea model implementing tea.Model.
type model struct {
	state  *SharedState
	cursor int

	// Device lists for cycling
	audioInputDevices  []string
	audioOutputDevices []string
	midiInputPorts     []string

	// Progress bars for level meters
	inputProgress  progress.Model
	outputProgress progress.Model

	// Spinner animation shown when audio is active
	spinner spinner.Model

	// Virtual mic management
	virtualMicModuleID string
}

// newModel creates a new TUI model with the given shared state and device lists.
func newModel(state *SharedState, audioIn, audioOut, midiIn []string) model {
	cfg := &state.Config

	// Pre-select the first available device for each category.
	if len(audioIn) > 0 && cfg.AudioInputDevice == nil {
		cfg.AudioInputDevice = &audioIn[0]
	}
	if len(audioOut) > 0 && cfg.AudioOutputDevice == nil {
		cfg.AudioOutputDevice = &audioOut[0]
	}
	if len(midiIn) > 0 && cfg.MidiInputPort == nil {
		cfg.MidiInputPort = &midiIn[0]
	}

	// Initialize progress bars with gradients
	inProg := progress.New(
		progress.WithGradient("#00FF00", "#FFFF00"),
		progress.WithoutPercentage(),
	)
	inProg.Width = 20

	outProg := progress.New(
		progress.WithGradient("#FF00FF", "#00FFFF"),
		progress.WithoutPercentage(),
	)
	outProg.Width = 20

	s := spinner.New()
	s.Spinner = spinner.Dot
	s.Style = lipgloss.NewStyle().Foreground(lipgloss.Color("205"))

	return model{
		state:              state,
		cursor:             0,
		audioInputDevices:  audioIn,
		audioOutputDevices: audioOut,
		midiInputPorts:     midiIn,
		inputProgress:      inProg,
		outputProgress:     outProg,
		spinner:            s,
	}
}

// Init satisfies tea.Model. Starts the periodic status tick and spinner.
func (m model) Init() tea.Cmd {
	return tea.Batch(
		m.spinner.Tick,
		tea.Tick(50*time.Millisecond, func(t time.Time) tea.Msg {
			return tickMsg(t)
		}),
	)
}

// Update handles incoming messages and returns an updated model + command.
func (m model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tickMsg:
		// Periodic status refresh — just re-render by requesting the next tick.
		return m, tea.Tick(50*time.Millisecond, func(t time.Time) tea.Msg {
			return tickMsg(t)
		})

	case spinner.TickMsg:
		var cmd tea.Cmd
		m.spinner, cmd = m.spinner.Update(msg)
		return m, cmd

	case tea.KeyMsg:
		switch msg.String() {
		case "q", "esc":
			m.stopStreams()
			return m, tea.Quit

		case "tab":
			m.state.mu.Lock()
			m.state.Config.KeyboardMode = !m.state.Config.KeyboardMode
			m.state.mu.Unlock()

		case "up", "k":
			if m.cursor > 0 {
				m.cursor--
			}

		case "down", "j":
			if m.cursor < len(configFields)-1 {
				m.cursor++
			}

		case "right", "l":
			field := configFields[m.cursor]
			m.state.mu.Lock()
			switch field {
			case fieldAudioInputDevice:
				cycleDevice(&m.state.Config.AudioInputDevice, m.audioInputDevices, 1)
			case fieldAudioOutputDevice:
				cycleDevice(&m.state.Config.AudioOutputDevice, m.audioOutputDevices, 1)
			case fieldMidiInputPort:
				cycleDevice(&m.state.Config.MidiInputPort, m.midiInputPorts, 1)
			default:
				field.adjust(&m.state.Config, 1)
			}
			m.state.mu.Unlock()
			if field.needsRestart() {
				m.restartStreams()
			}

		case "left", "h":
			field := configFields[m.cursor]
			m.state.mu.Lock()
			switch field {
			case fieldAudioInputDevice:
				cycleDevice(&m.state.Config.AudioInputDevice, m.audioInputDevices, -1)
			case fieldAudioOutputDevice:
				cycleDevice(&m.state.Config.AudioOutputDevice, m.audioOutputDevices, -1)
			case fieldMidiInputPort:
				cycleDevice(&m.state.Config.MidiInputPort, m.midiInputPorts, -1)
			default:
				field.adjust(&m.state.Config, -1)
			}
			m.state.mu.Unlock()
			if field.needsRestart() {
				m.restartStreams()
			}
		}
	}

	return m, nil
}

// View renders the TUI as a string.
func (m model) View() string {
	var b strings.Builder

	// ── Title bar ──
	b.WriteString(m.renderTitle())
	b.WriteString("\n")

	// ── Body: two columns ──
	configPanel := m.renderConfigPanel()
	statusPanel := m.renderStatusPanel()

	body := lipgloss.JoinHorizontal(lipgloss.Top, configPanel, statusPanel)
	b.WriteString(body)
	b.WriteString("\n")

	// ── Help bar ──
	b.WriteString(m.renderHelp())

	return b.String()
}

// renderTitle renders the top title bar with mode indicator.
func (m model) renderTitle() string {
	m.state.mu.Lock()
	keyboardMode := m.state.Config.KeyboardMode
	m.state.mu.Unlock()

	modeLabel := "VOCODER"
	modeStyle := modeVocoderStyle
	if keyboardMode {
		modeLabel = "KEYBOARD"
		modeStyle = modeKeyboardStyle
	}

	title := fmt.Sprintf(
		"%s %s %s %s",
		accentStyle.Render(" ✦ "),
		titleStyle.Render("vocoder"),
		accentStyle.Render(" ✦ "),
		modeStyle.Render(fmt.Sprintf(" [%s] ", modeLabel)),
	)

	box := lipgloss.NewStyle().
		Border(lipgloss.RoundedBorder()).
		BorderForeground(lipgloss.Color("#FF00FF")). // Magenta
		Width(60).
		Render(title)

	return lipgloss.NewStyle().Align(lipgloss.Center).Render(box)
}

// renderConfigPanel renders the left column with config fields.
func (m model) renderConfigPanel() string {
	m.state.mu.Lock()
	cfg := m.state.Config
	m.state.mu.Unlock()

	var b strings.Builder

	header := configTitleStyle.Render(" ⚙  Configuration ")
	b.WriteString(header)
	b.WriteString("\n")

	for i, field := range configFields {
		label := field.label()
		value := field.displayValue(cfg)

		if i == m.cursor {
			cursor := cursorStyle.Render("▶")
			row := fmt.Sprintf(" %s %s  %s", cursor,
				selectedStyle.Render(fmt.Sprintf("%-16s", label)),
				selectedStyle.Render(fmt.Sprintf("◄ %s ►", value)))
			b.WriteString(row)
		} else {
			row := fmt.Sprintf("   %s  %s",
				labelStyle.Render(fmt.Sprintf("%-16s", label)),
				valueStyle.Render(fmt.Sprintf("◄ %s ►", value)))
			b.WriteString(row)
		}
		b.WriteString("\n")
	}

	box := lipgloss.NewStyle().
		Border(lipgloss.RoundedBorder()).
		BorderForeground(lipgloss.Color("#555555")).
		Width(45).
		Height(len(configFields) + 3).
		Render(b.String())

	return box
}

// renderStatusPanel renders the right column with live status info.
func (m model) renderStatusPanel() string {
	m.state.mu.Lock()
	audioActive := m.state.AudioActive
	midiActive := m.state.MIDIActive
	inputLevel := m.state.InputLevel
	outputLevel := m.state.OutputLevel
	m.state.mu.Unlock()

	noteCount := m.state.ActiveNotes.Count()
	activeNotes := m.state.ActiveNotes.List()

	var b strings.Builder

	header := statusTitleStyle.Render(" ♪  Status ")
	b.WriteString(header)
	b.WriteString("\n")

	// Audio status
	if audioActive {
		audioIcon := statusOKStyle.Render("●")
		audioText := statusOKStyle.Render("Running")
		b.WriteString(fmt.Sprintf(" %s Audio   %s %s\n", audioIcon, audioText, m.spinner.View()))
	} else {
		audioIcon := statusFailStyle.Render("○")
		audioText := statusFailStyle.Render("Stopped")
		b.WriteString(fmt.Sprintf(" %s Audio   %s\n", audioIcon, audioText))
	}

	// MIDI status
	if midiActive {
		midiIcon := statusOKStyle.Render("●")
		midiText := statusOKStyle.Render("Connected")
		b.WriteString(fmt.Sprintf(" %s MIDI    %s\n", midiIcon, midiText))
	} else {
		midiIcon := statusFailStyle.Render("○")
		midiText := statusFailStyle.Render("Disconnected")
		b.WriteString(fmt.Sprintf(" %s MIDI    %s\n", midiIcon, midiText))
	}

	// Levels
	b.WriteString("\n")
	inPct := int(clampFloat(inputLevel*100, 0, 100))
	b.WriteString(inputLevelStyle.Render("  In  "))
	b.WriteString(fmt.Sprintf("%s %d%%\n", m.inputProgress.ViewAs(float64(clampFloat(inputLevel, 0.0, 1.0))), inPct))

	outPct := int(clampFloat(outputLevel*100, 0, 100))
	b.WriteString(outputLevelStyle.Render(" Out "))
	b.WriteString(fmt.Sprintf("%s %d%%\n", m.outputProgress.ViewAs(float64(clampFloat(outputLevel, 0.0, 1.0))), outPct))

	// Note display
	b.WriteString("\n")
	if noteCount > 0 {
		names := make([]string, 0, len(activeNotes))
		for _, n := range activeNotes {
			names = append(names, noteName(n))
		}
		noteStr := strings.Join(names, " ")
		if len(noteStr) > 30 {
			noteStr = noteStr[:30] + "…"
		}
		b.WriteString(fmt.Sprintf(" Notes  ♫ %s", noteStr))
	} else {
		b.WriteString(" Notes  ♫ ---")
	}

	box := lipgloss.NewStyle().
		Border(lipgloss.RoundedBorder()).
		BorderForeground(lipgloss.Color("#555555")).
		Width(40).
		Height(len(configFields) + 3).
		Render(b.String())

	return box
}

// renderHelp renders the bottom help bar.
func (m model) renderHelp() string {
	keys := []struct {
		key  string
		desc string
	}{
		{" ↑/k ↓/j ", "navigate  "},
		{"←/h →/l ", "adjust  "},
		{"Tab ", "mode  "},
		{"q/Esc ", "quit"},
	}

	var parts []string
	for _, k := range keys {
		parts = append(parts,
			helpKeyStyle.Render(k.key),
			helpDescStyle.Render(k.desc),
		)
	}

	help := strings.Join(parts, "")
	box := lipgloss.NewStyle().
		Border(lipgloss.RoundedBorder()).
		BorderForeground(lipgloss.Color("#555555")).
		Width(60).
		Render(lipgloss.NewStyle().Align(lipgloss.Center).Render(help))

	return box
}

// ── Stream lifecycle ───────────────────────────────────────────────

// restartStreams stops existing audio and MIDI streams and restarts them
// with the current config.
func (m model) restartStreams() {
	m.state.mu.Lock()
	cfg := m.state.Config
	m.state.mu.Unlock()

	// Stop existing streams first (synchronous — callback will quiesce).
	m.stopStreams()

	// Virtual mic setup/teardown.
	if cfg.VirtualMic && m.virtualMicModuleID == "" {
		id, err := SetupVirtualMic()
		if err == nil {
			m.virtualMicModuleID = id
			if devs, err := ListOutputDevices(); err == nil {
				names := make([]string, len(devs))
				for i, d := range devs {
					names[i] = d.Name
				}
				m.audioOutputDevices = names
			}
			vmName := "Vocoder_Virtual_Mic"
			cfg.AudioOutputDevice = &vmName
			m.state.mu.Lock()
			m.state.Config.AudioOutputDevice = &vmName
			m.state.mu.Unlock()
		}
	} else if !cfg.VirtualMic && m.virtualMicModuleID != "" {
		_ = TeardownVirtualMic(m.virtualMicModuleID)
		m.virtualMicModuleID = ""
		if devs, err := ListOutputDevices(); err == nil {
			names := make([]string, len(devs))
			for i, d := range devs {
				names[i] = d.Name
			}
			m.audioOutputDevices = names
		}
		if len(m.audioOutputDevices) > 0 {
			first := m.audioOutputDevices[0]
			cfg.AudioOutputDevice = &first
			m.state.mu.Lock()
			m.state.Config.AudioOutputDevice = &first
			m.state.mu.Unlock()
		}
	}

	// Reset DSP state for the new sample rate.
	m.state.mu.Lock()
	sr := float64(cfg.SampleRate)
	attack, release := envelopeParams(cfg.EnvelopeSpeed)
	m.state.SampleRate = sr
	m.state.Vocoder = NewVocoder(cfg.Bands, 200.0, 8000.0, 4.0, attack, release, sr)
	m.state.Phases = make(map[uint8]float64)
	m.state.mu.Unlock()

	// Start audio duplex stream.
	streams, err := StartAudio(AudioConfig{
		SampleRate: cfg.SampleRate,
		BufferSize: cfg.BufferSize,
	}, m.state.ProcessAudio)
	m.state.mu.Lock()
	if err != nil {
		m.state.AudioActive = false
		m.state.AudioStreams = nil
	} else {
		m.state.AudioStreams = streams
		m.state.AudioActive = true
	}
	m.state.mu.Unlock()

	// Start MIDI input.
	if cfg.MidiInputPort != nil {
		stop, err := ListenToPort(*cfg.MidiInputPort, m.state.ActiveNotes)
		m.state.mu.Lock()
		if err != nil {
			m.state.MIDIActive = false
			m.state.MIDIStop = nil
		} else {
			m.state.MIDIStop = stop
			m.state.MIDIActive = true
		}
		m.state.mu.Unlock()
	}
}

// stopStreams stops and releases all audio and MIDI resources.
func (m model) stopStreams() {
	m.state.mu.Lock()
	audio := m.state.AudioStreams
	midiStop := m.state.MIDIStop
	m.state.AudioStreams = nil
	m.state.MIDIStop = nil
	m.state.AudioActive = false
	m.state.MIDIActive = false
	m.state.mu.Unlock()

	if audio != nil {
		audio.Close()
	}
	if midiStop != nil {
		midiStop()
	}
}

// ── Public entry point ─────────────────────────────────────────────

// RunTUI launches the Bubble Tea program and blocks until the user quits.
// It initialises audio/MIDI streams from the shared state, runs the TUI,
// and cleans up on exit. Returns the final model state.
func RunTUI(state *SharedState, audioIn, audioOut, midiIn []string) (model, error) {
	m := newModel(state, audioIn, audioOut, midiIn)

	// Start initial audio and MIDI streams.
	m.restartStreams()

	p := tea.NewProgram(m, tea.WithAltScreen())
	if _, err := p.Run(); err != nil {
		m.stopStreams()
		return m, fmt.Errorf("TUI error: %w", err)
	}
	return m, nil
}
