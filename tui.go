package vocoder

import (
	"fmt"
	"strings"

	"github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
)

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
	// When true, bypasses vocoder DSP and outputs the raw carrier signal
	// multiplied by gain (Keyboard mode). When false, normal vocoder
	// processing is applied (Vocoder mode).
	KeyboardMode bool
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
		KeyboardMode:      false,
	}
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
	}
}

// ── Helpers ────────────────────────────────────────────────────────

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

// ── Bubble Tea Model ───────────────────────────────────────────────

// model is the top-level Bubble Tea model implementing tea.Model.
type model struct {
	config     Config
	cursor     int
	shouldQuit bool

	// Device lists for cycling
	audioInputDevices  []string
	audioOutputDevices []string
	midiInputPorts     []string
}

// newModel creates a new TUI model with the given device lists.
func newModel(audioIn, audioOut, midiIn []string) model {
	cfg := DefaultConfig()

	// Pre-select the first available device for each category.
	if len(audioIn) > 0 {
		cfg.AudioInputDevice = &audioIn[0]
	}
	if len(audioOut) > 0 {
		cfg.AudioOutputDevice = &audioOut[0]
	}
	if len(midiIn) > 0 {
		cfg.MidiInputPort = &midiIn[0]
	}

	return model{
		config:             cfg,
		cursor:             0,
		shouldQuit:         false,
		audioInputDevices:  audioIn,
		audioOutputDevices: audioOut,
		midiInputPorts:     midiIn,
	}
}

// Init satisfies tea.Model. No initial I/O needed.
func (m model) Init() tea.Cmd {
	return nil
}

// Update handles incoming messages and returns an updated model + command.
func (m model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.KeyMsg:
		switch msg.String() {
		case "q", "esc":
			m.shouldQuit = true
			return m, tea.Quit

		case "tab":
			m.config.KeyboardMode = !m.config.KeyboardMode

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
			switch field {
			case fieldAudioInputDevice:
				cycleDevice(&m.config.AudioInputDevice, m.audioInputDevices, 1)
			case fieldAudioOutputDevice:
				cycleDevice(&m.config.AudioOutputDevice, m.audioOutputDevices, 1)
			case fieldMidiInputPort:
				cycleDevice(&m.config.MidiInputPort, m.midiInputPorts, 1)
			default:
				field.adjust(&m.config, 1)
			}

		case "left", "h":
			field := configFields[m.cursor]
			switch field {
			case fieldAudioInputDevice:
				cycleDevice(&m.config.AudioInputDevice, m.audioInputDevices, -1)
			case fieldAudioOutputDevice:
				cycleDevice(&m.config.AudioOutputDevice, m.audioOutputDevices, -1)
			case fieldMidiInputPort:
				cycleDevice(&m.config.MidiInputPort, m.midiInputPorts, -1)
			default:
				field.adjust(&m.config, -1)
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
	modeLabel := "VOCODER"
	modeStyle := modeVocoderStyle
	if m.config.KeyboardMode {
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
	var b strings.Builder

	header := configTitleStyle.Render(" ⚙  Configuration ")
	b.WriteString(header)
	b.WriteString("\n")

	for i, field := range configFields {
		label := field.label()
		value := field.displayValue(m.config)

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

// renderStatusPanel renders the right column with status info.
func (m model) renderStatusPanel() string {
	var b strings.Builder

	header := statusTitleStyle.Render(" ♪  Status ")
	b.WriteString(header)
	b.WriteString("\n")

	// Audio status
	audioIcon := statusFailStyle.Render("○")
	audioText := statusFailStyle.Render("Stopped")
	b.WriteString(fmt.Sprintf(" %s Audio   %s\n", audioIcon, audioText))

	// MIDI status
	midiIcon := statusFailStyle.Render("○")
	midiText := statusFailStyle.Render("Disconnected")
	b.WriteString(fmt.Sprintf(" %s MIDI    %s\n", midiIcon, midiText))

	// Levels
	b.WriteString("\n")
	b.WriteString(inputLevelStyle.Render("  In  "))
	b.WriteString(fmt.Sprintf("[%-20s] 0%%\n", strings.Repeat("░", 20)))
	b.WriteString(outputLevelStyle.Render(" Out "))
	b.WriteString(fmt.Sprintf("[%-20s] 0%%\n", strings.Repeat("░", 20)))

	// CPU + Note
	b.WriteString("\n")
	b.WriteString(" CPU   0% ")
	b.WriteString(strings.Repeat("░", 20))
	b.WriteString("\n")
	b.WriteString(" Note  ♫ ---")

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

// ── Public entry point ─────────────────────────────────────────────

// RunTUI launches the Bubble Tea program and blocks until the user quits.
// Returns the final model state.
func RunTUI(audioIn, audioOut, midiIn []string) (model, error) {
	m := newModel(audioIn, audioOut, midiIn)
	p := tea.NewProgram(m, tea.WithAltScreen())
	if _, err := p.Run(); err != nil {
		return m, fmt.Errorf("TUI error: %w", err)
	}
	return m, nil
}
