use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph};
use ratatui::{backend::CrosstermBackend, Frame, Terminal};
use std::io;
use std::time::Duration;

// ── Application state ──────────────────────────────────────────────

/// Configurable parameters for the vocoder.
#[derive(Debug, Clone)]
pub struct Config {
    pub audio_input_device: Option<String>,
    pub audio_output_device: Option<String>,
    pub midi_input_port: Option<String>,
    pub sample_rate: u32,
    pub buffer_size: u32,
    pub midi_channel: u8,
    pub formant_shift: f32,
    pub pitch_shift: f32,
    pub gain: f32,
    /// When `true`, the audio callback bypasses the vocoder DSP and outputs
    /// the raw carrier signal multiplied by gain (Keyboard mode).
    /// When `false`, normal vocoder processing is applied (Vocoder mode).
    pub keyboard_mode: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            audio_input_device: None,
            audio_output_device: None,
            midi_input_port: None,
            sample_rate: 44_100,
            buffer_size: 512,
            midi_channel: 1,
            formant_shift: 1.0,
            pitch_shift: 0.0,
            gain: 3.0,
            keyboard_mode: false,
        }
    }
}

/// Runtime status displayed by the TUI.
#[derive(Debug, Clone, Default)]
pub struct Status {
    pub audio_running: bool,
    pub midi_connected: bool,
    pub active_notes: Vec<String>,
    pub cpu_usage: f32,
    pub input_level: f32,
    pub output_level: f32,
}

/// Which config field is currently selected for editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigField {
    AudioInputDevice,
    AudioOutputDevice,
    MidiInputPort,
    SampleRate,
    BufferSize,
    MidiChannel,
    FormantShift,
    PitchShift,
    Gain,
}

const CONFIG_FIELDS: [ConfigField; 9] = [
    ConfigField::AudioInputDevice,
    ConfigField::AudioOutputDevice,
    ConfigField::MidiInputPort,
    ConfigField::SampleRate,
    ConfigField::BufferSize,
    ConfigField::MidiChannel,
    ConfigField::FormantShift,
    ConfigField::PitchShift,
    ConfigField::Gain,
];

impl ConfigField {
    fn label(&self) -> &'static str {
        match self {
            Self::AudioInputDevice => "Audio Input",
            Self::AudioOutputDevice => "Audio Output",
            Self::MidiInputPort => "MIDI Input",
            Self::SampleRate => "Sample Rate",
            Self::BufferSize => "Buffer Size",
            Self::MidiChannel => "MIDI Channel",
            Self::FormantShift => "Formant Shift",
            Self::PitchShift => "Pitch Shift",
            Self::Gain => "Gain",
        }
    }

    fn display_value(&self, cfg: &Config) -> String {
        match self {
            Self::AudioInputDevice => cfg
                .audio_input_device
                .as_deref()
                .unwrap_or("(default)")
                .to_string(),
            Self::AudioOutputDevice => cfg
                .audio_output_device
                .as_deref()
                .unwrap_or("(default)")
                .to_string(),
            Self::MidiInputPort => cfg
                .midi_input_port
                .as_deref()
                .unwrap_or("(first available)")
                .to_string(),
            Self::SampleRate => format!("{} Hz", cfg.sample_rate),
            Self::BufferSize => format!("{} samples", cfg.buffer_size),
            Self::MidiChannel => format!("Channel {}", cfg.midi_channel),
            Self::FormantShift => format!("{:.2}x", cfg.formant_shift),
            Self::PitchShift => format!("{:+.1} semitones", cfg.pitch_shift),
            Self::Gain => format!("{:.0}%", cfg.gain * 100.0),
        }
    }

    fn adjust(&self, cfg: &mut Config, delta: i32) {
        match self {
            // Device fields are handled by App::cycle_device instead.
            Self::AudioInputDevice | Self::AudioOutputDevice | Self::MidiInputPort => {}
            Self::SampleRate => {
                let opts = [22_050, 44_100, 48_000, 96_000];
                let idx = opts.iter().position(|&r| r == cfg.sample_rate).unwrap_or(1);
                let new = (idx as i32 + delta).clamp(0, (opts.len() - 1) as i32) as usize;
                cfg.sample_rate = opts[new];
            }
            Self::BufferSize => {
                let opts = [128, 256, 512, 1024, 2048];
                let idx = opts.iter().position(|&r| r == cfg.buffer_size).unwrap_or(2);
                let new = (idx as i32 + delta).clamp(0, (opts.len() - 1) as i32) as usize;
                cfg.buffer_size = opts[new];
            }
            Self::MidiChannel => {
                cfg.midi_channel = (cfg.midi_channel as i32 + delta).clamp(1, 16) as u8;
            }
            Self::FormantShift => {
                cfg.formant_shift = (cfg.formant_shift + delta as f32 * 0.05).clamp(0.25, 4.0);
            }
            Self::PitchShift => {
                cfg.pitch_shift = (cfg.pitch_shift + delta as f32 * 0.5).clamp(-24.0, 24.0);
            }
            Self::Gain => {
                cfg.gain = (cfg.gain + delta as f32 * 0.05).clamp(0.0, 10.0);
            }
        }
    }
}

/// Top-level application state for the TUI.
pub struct App {
    pub config: Config,
    pub status: Status,
    selected_field: usize,
    list_state: ListState,
    pub should_quit: bool,
    audio_input_devices: Vec<String>,
    audio_output_devices: Vec<String>,
    midi_input_ports: Vec<String>,
}

impl App {
    pub fn new(
        audio_input_devices: Vec<String>,
        audio_output_devices: Vec<String>,
        midi_input_ports: Vec<String>,
    ) -> Self {
        let mut list_state = ListState::default();
        list_state.select(Some(0));

        let mut config = Config::default();

        // Pre-select the first available device for each category.
        if let Some(first) = audio_input_devices.first() {
            config.audio_input_device = Some(first.clone());
        }
        if let Some(first) = audio_output_devices.first() {
            config.audio_output_device = Some(first.clone());
        }
        if let Some(first) = midi_input_ports.first() {
            config.midi_input_port = Some(first.clone());
        }

        Self {
            config,
            status: Status::default(),
            selected_field: 0,
            list_state,
            should_quit: false,
            audio_input_devices,
            audio_output_devices,
            midi_input_ports,
        }
    }

    /// Cycle through a device list, wrapping around at the ends.
    fn cycle_device(current: &mut Option<String>, devices: &[String], delta: i32) {
        if devices.is_empty() {
            return;
        }
        let idx = match current.as_deref() {
            Some(name) => devices.iter().position(|d| d == name).unwrap_or(0),
            None => 0,
        };
        let len = devices.len() as i32;
        let new_idx = ((idx as i32 + delta).rem_euclid(len)) as usize;
        *current = Some(devices[new_idx].clone());
    }

    pub fn with_status(mut self, status: Status) -> Self {
        self.status = status;
        self
    }

    pub fn handle_event(&mut self, evt: &Event) {
        if let Event::Key(key) = evt {
            if key.kind != KeyEventKind::Press {
                return;
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => {
                    self.should_quit = true;
                }
                KeyCode::Tab => {
                    self.config.keyboard_mode = !self.config.keyboard_mode;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if self.selected_field > 0 {
                        self.selected_field -= 1;
                        self.list_state.select(Some(self.selected_field));
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if self.selected_field < CONFIG_FIELDS.len() - 1 {
                        self.selected_field += 1;
                        self.list_state.select(Some(self.selected_field));
                    }
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    let field = CONFIG_FIELDS[self.selected_field];
                    match field {
                        ConfigField::AudioInputDevice => {
                            Self::cycle_device(
                                &mut self.config.audio_input_device,
                                &self.audio_input_devices,
                                1,
                            );
                        }
                        ConfigField::AudioOutputDevice => {
                            Self::cycle_device(
                                &mut self.config.audio_output_device,
                                &self.audio_output_devices,
                                1,
                            );
                        }
                        ConfigField::MidiInputPort => {
                            Self::cycle_device(
                                &mut self.config.midi_input_port,
                                &self.midi_input_ports,
                                1,
                            );
                        }
                        _ => field.adjust(&mut self.config, 1),
                    }
                }
                KeyCode::Left | KeyCode::Char('h') => {
                    let field = CONFIG_FIELDS[self.selected_field];
                    match field {
                        ConfigField::AudioInputDevice => {
                            Self::cycle_device(
                                &mut self.config.audio_input_device,
                                &self.audio_input_devices,
                                -1,
                            );
                        }
                        ConfigField::AudioOutputDevice => {
                            Self::cycle_device(
                                &mut self.config.audio_output_device,
                                &self.audio_output_devices,
                                -1,
                            );
                        }
                        ConfigField::MidiInputPort => {
                            Self::cycle_device(
                                &mut self.config.midi_input_port,
                                &self.midi_input_ports,
                                -1,
                            );
                        }
                        _ => field.adjust(&mut self.config, -1),
                    }
                }
                _ => {}
            }
        }
    }
}

// ── Rendering ──────────────────────────────────────────────────────

pub fn draw(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // title
            Constraint::Min(0),    // body
            Constraint::Length(3), // footer / help
        ])
        .split(f.area());

    render_title(f, chunks[0]);
    render_body(f, app, chunks[1]);
    render_help(f, chunks[2]);
}

fn render_title(f: &mut Frame, area: Rect) {
    let title = Paragraph::new(Line::from(vec![
        Span::styled(
            " ✦ ",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "vocoder",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " ✦ ",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Magenta)),
    )
    .centered();
    f.render_widget(title, area);
}

fn render_body(f: &mut Frame, app: &App, area: Rect) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    render_config(f, app, columns[0]);
    render_status(f, app, columns[1]);
}

fn render_config(f: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = CONFIG_FIELDS
        .iter()
        .map(|field| {
            let label = field.label();
            let value = field.display_value(&app.config);
            let line = Line::from(vec![
                Span::styled(format!(" {label:<16}"), Style::default().fg(Color::White)),
                Span::styled(format!("◄ {value} ►"), Style::default().fg(Color::Yellow)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let config_list = List::new(items)
        .block(
            Block::default()
                .title(" ⚙  Configuration ")
                .title_style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    f.render_stateful_widget(config_list, area, &mut app.list_state.clone());
}

fn render_status(f: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4), // connection status
            Constraint::Length(4), // audio levels
            Constraint::Min(0),    // cpu + note
        ])
        .split(area);

    render_connection_status(f, app, chunks[0]);
    render_levels(f, app, chunks[1]);
    render_cpu_and_note(f, app, chunks[2]);
}

fn render_connection_status(f: &mut Frame, app: &App, area: Rect) {
    let audio_icon = if app.status.audio_running {
        ("●", Color::Green)
    } else {
        ("○", Color::Red)
    };
    let midi_icon = if app.status.midi_connected {
        ("●", Color::Green)
    } else {
        ("○", Color::Red)
    };

    let lines = vec![
        Line::from(vec![
            Span::styled(
                format!(" {} Audio  ", audio_icon.0),
                Style::default().fg(audio_icon.1),
            ),
            Span::styled(
                if app.status.audio_running {
                    "Running"
                } else {
                    "Stopped"
                },
                Style::default().fg(audio_icon.1),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                format!(" {} MIDI   ", midi_icon.0),
                Style::default().fg(midi_icon.1),
            ),
            Span::styled(
                if app.status.midi_connected {
                    "Connected"
                } else {
                    "Disconnected"
                },
                Style::default().fg(midi_icon.1),
            ),
        ]),
    ];

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .title(" ♪  Status ")
            .title_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(paragraph, area);
}

fn render_levels(f: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area.inner(ratatui::layout::Margin {
            horizontal: 1,
            vertical: 1,
        }));

    let input_gauge = Gauge::default()
        .block(
            Block::default()
                .title("  In ")
                .title_style(Style::default().fg(Color::Green))
                .borders(Borders::NONE),
        )
        .gauge_style(
            Style::default()
                .fg(Color::Green)
                .bg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .ratio((app.status.input_level as f64).clamp(0.0, 1.0))
        .label(format!("{:.0}%", app.status.input_level * 100.0));

    let output_gauge = Gauge::default()
        .block(
            Block::default()
                .title(" Out ")
                .title_style(Style::default().fg(Color::Magenta))
                .borders(Borders::NONE),
        )
        .gauge_style(
            Style::default()
                .fg(Color::Magenta)
                .bg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .ratio((app.status.output_level as f64).clamp(0.0, 1.0))
        .label(format!("{:.0}%", app.status.output_level * 100.0));

    let block = Block::default()
        .title(" 🎚  Levels ")
        .title_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    f.render_widget(Clear, area);
    f.render_widget(block, area);
    f.render_widget(input_gauge, chunks[0]);
    f.render_widget(output_gauge, chunks[1]);
}

fn render_cpu_and_note(f: &mut Frame, app: &App, area: Rect) {
    let cpu_pct = (app.status.cpu_usage * 100.0) as u16;
    let cpu_color = if app.status.cpu_usage < 0.5 {
        Color::Green
    } else if app.status.cpu_usage < 0.8 {
        Color::Yellow
    } else {
        Color::Red
    };

    let note_display = if app.status.active_notes.is_empty() {
        "---".to_string()
    } else {
        app.status.active_notes.join(", ")
    };

    let lines = vec![
        Line::from(vec![
            Span::styled(" CPU ", Style::default().fg(Color::White)),
            Span::styled(
                format!("{cpu_pct:3}% "),
                Style::default().fg(cpu_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "▰".repeat((app.status.cpu_usage * 20.0) as usize),
                Style::default().fg(cpu_color),
            ),
            Span::styled(
                "▱".repeat(20 - (app.status.cpu_usage * 20.0) as usize),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(vec![
            Span::styled(" Note ", Style::default().fg(Color::White)),
            Span::styled(
                format!("♫ {note_display}"),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
    ];

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(paragraph, area);
}

fn render_help(f: &mut Frame, area: Rect) {
    let help = Paragraph::new(Line::from(vec![
        Span::styled(" ↑/k ↓/j ", Style::default().fg(Color::DarkGray)),
        Span::styled("navigate  ", Style::default().fg(Color::Gray)),
        Span::styled("←/h →/l ", Style::default().fg(Color::DarkGray)),
        Span::styled("adjust  ", Style::default().fg(Color::Gray)),
        Span::styled("q/Esc ", Style::default().fg(Color::DarkGray)),
        Span::styled("quit", Style::default().fg(Color::Gray)),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    )
    .centered();
    f.render_widget(help, area);
}

// ── Public entry point ─────────────────────────────────────────────

/// Run the TUI event loop. Blocks until the user presses `q` or `Esc`.
///
/// The caller can pre-populate `Status` before calling this, and read
/// back the final `Config` from the returned `App`.
#[allow(dead_code)]
pub fn run(app: App) -> Result<App> {
    // Set up terminal
    crossterm::execute!(io::stdout(), EnterAlternateScreen)?;
    enable_raw_mode()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut app = app;

    // Main loop
    while !app.should_quit {
        terminal.draw(|f| draw(f, &app))?;

        // Poll for events (100 ms timeout to keep the UI responsive)
        if event::poll(Duration::from_millis(100))? {
            let evt = event::read()?;
            app.handle_event(&evt);
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    crossterm::execute!(io::stdout(), LeaveAlternateScreen)?;

    Ok(app)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    // Helper: construct a key-press event.
    fn key_press(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::empty()))
    }

    // Helper: construct a key-release event (non-press).
    fn key_release(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new_with_kind(
            code,
            KeyModifiers::empty(),
            KeyEventKind::Release,
        ))
    }

    // --- TUI-CF: Config::default() tests ---

    /// TUI-CF-01: Default config values.
    #[test]
    fn config_default_values() {
        let cfg = Config::default();
        assert_eq!(cfg.sample_rate, 44_100);
        assert_eq!(cfg.buffer_size, 512);
        assert_eq!(cfg.midi_channel, 1);
        assert!((cfg.formant_shift - 1.0).abs() < 1e-6);
        assert!((cfg.pitch_shift - 0.0).abs() < 1e-6);
        assert!((cfg.gain - 3.0).abs() < 1e-6);
    }

    /// TUI-CF-02: Default devices are None.
    #[test]
    fn config_default_devices_none() {
        let cfg = Config::default();
        assert!(cfg.audio_input_device.is_none());
        assert!(cfg.audio_output_device.is_none());
        assert!(cfg.midi_input_port.is_none());
    }

    // --- TUI-AD: ConfigField::adjust() boundary tests ---

    /// TUI-AD-01: SampleRate clamps at minimum (22050).
    #[test]
    fn adjust_sample_rate_clamp_min() {
        let mut cfg = Config::default();
        cfg.sample_rate = 22_050;
        ConfigField::SampleRate.adjust(&mut cfg, -1);
        assert_eq!(cfg.sample_rate, 22_050);
    }

    /// TUI-AD-02: SampleRate clamps at maximum (96000).
    #[test]
    fn adjust_sample_rate_clamp_max() {
        let mut cfg = Config::default();
        cfg.sample_rate = 96_000;
        ConfigField::SampleRate.adjust(&mut cfg, 1);
        assert_eq!(cfg.sample_rate, 96_000);
    }

    /// TUI-AD-03: BufferSize clamps at 128 (min) and 2048 (max).
    #[test]
    fn adjust_buffer_size_clamps() {
        let mut cfg = Config::default();
        cfg.buffer_size = 128;
        ConfigField::BufferSize.adjust(&mut cfg, -1);
        assert_eq!(cfg.buffer_size, 128);

        cfg.buffer_size = 2048;
        ConfigField::BufferSize.adjust(&mut cfg, 1);
        assert_eq!(cfg.buffer_size, 2048);
    }

    /// TUI-AD-04: MidiChannel clamps at 1 (min) and 16 (max).
    #[test]
    fn adjust_midi_channel_clamps() {
        let mut cfg = Config::default();
        cfg.midi_channel = 1;
        ConfigField::MidiChannel.adjust(&mut cfg, -1);
        assert_eq!(cfg.midi_channel, 1);

        cfg.midi_channel = 16;
        ConfigField::MidiChannel.adjust(&mut cfg, 1);
        assert_eq!(cfg.midi_channel, 16);
    }

    /// TUI-AD-05: FormantShift clamps at 0.25 (min) and 4.0 (max).
    #[test]
    fn adjust_formant_shift_clamps() {
        let mut cfg = Config::default();
        cfg.formant_shift = 0.25;
        ConfigField::FormantShift.adjust(&mut cfg, -1);
        assert!((cfg.formant_shift - 0.25).abs() < 1e-6);

        cfg.formant_shift = 4.0;
        ConfigField::FormantShift.adjust(&mut cfg, 1);
        assert!((cfg.formant_shift - 4.0).abs() < 1e-6);
    }

    /// TUI-AD-06: PitchShift clamps at -24.0 (min) and 24.0 (max).
    #[test]
    fn adjust_pitch_shift_clamps() {
        let mut cfg = Config::default();
        cfg.pitch_shift = -24.0;
        ConfigField::PitchShift.adjust(&mut cfg, -1);
        assert!((cfg.pitch_shift - (-24.0)).abs() < 1e-6);

        cfg.pitch_shift = 24.0;
        ConfigField::PitchShift.adjust(&mut cfg, 1);
        assert!((cfg.pitch_shift - 24.0).abs() < 1e-6);
    }

    /// TUI-AD-07: Gain clamps at 0.0 (min) and 1.5 (max).
    #[test]
    fn adjust_gain_clamps() {
        let mut cfg = Config::default();
        cfg.gain = 0.0;
        ConfigField::Gain.adjust(&mut cfg, -1);
        assert!((cfg.gain - 0.0).abs() < 1e-6);

        cfg.gain = 10.0;
        ConfigField::Gain.adjust(&mut cfg, 1);
        assert!((cfg.gain - 10.0).abs() < 1e-6);
    }

    /// TUI-AD-08: FormantShift step size is 0.05.
    #[test]
    fn adjust_formant_shift_step_size() {
        let mut cfg = Config::default();
        cfg.formant_shift = 1.0;
        ConfigField::FormantShift.adjust(&mut cfg, 1);
        assert!((cfg.formant_shift - 1.05).abs() < 1e-6);
    }

    /// TUI-AD-09: PitchShift step size is 0.5.
    #[test]
    fn adjust_pitch_shift_step_size() {
        let mut cfg = Config::default();
        cfg.pitch_shift = 0.0;
        ConfigField::PitchShift.adjust(&mut cfg, 1);
        assert!((cfg.pitch_shift - 0.5).abs() < 1e-6);
    }

    // --- TUI-CD: cycle_device tests ---

    /// TUI-CD-01: Empty device list — no panic, current unchanged.
    #[test]
    fn cycle_device_empty_list() {
        let mut current = Some("device".to_string());
        App::cycle_device(&mut current, &[], 1);
        assert_eq!(current, Some("device".to_string()));
    }

    /// TUI-CD-02: Single device — cycling stays on same device.
    #[test]
    fn cycle_device_single() {
        let devices = vec!["only".to_string()];
        let mut current = Some("only".to_string());
        App::cycle_device(&mut current, &devices, 1);
        assert_eq!(current, Some("only".to_string()));
        App::cycle_device(&mut current, &devices, -1);
        assert_eq!(current, Some("only".to_string()));
    }

    /// TUI-CD-03: Wrap-around forward.
    #[test]
    fn cycle_device_wrap_forward() {
        let devices = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let mut current = Some("c".to_string());
        App::cycle_device(&mut current, &devices, 1);
        assert_eq!(current, Some("a".to_string()));
    }

    /// TUI-CD-04: Wrap-around backward.
    #[test]
    fn cycle_device_wrap_backward() {
        let devices = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let mut current = Some("a".to_string());
        App::cycle_device(&mut current, &devices, -1);
        assert_eq!(current, Some("c".to_string()));
    }

    /// TUI-CD-05: current is None and devices are available — idx starts at 0,
    /// then delta is applied, so delta=1 yields index 1 ("b").
    #[test]
    fn cycle_device_none_selects_device() {
        let devices = vec!["a".to_string(), "b".to_string()];
        let mut current = None;
        App::cycle_device(&mut current, &devices, 1);
        assert_eq!(current, Some("b".to_string()));
    }

    /// TUI-CD-06: current names a device not in the list — falls back to index 0.
    #[test]
    fn cycle_device_missing_falls_back() {
        let devices = vec!["a".to_string(), "b".to_string()];
        let mut current = Some("z".to_string());
        App::cycle_device(&mut current, &devices, 1);
        assert_eq!(current, Some("b".to_string()));
    }

    // --- TUI-HE: handle_event tests ---

    /// TUI-HE-01: 'q' sets should_quit = true.
    #[test]
    fn handle_event_quit_q() {
        let mut app = App::new(vec![], vec![], vec![]);
        assert!(!app.should_quit);
        app.handle_event(&key_press(KeyCode::Char('q')));
        assert!(app.should_quit);
    }

    /// TUI-HE-02: Esc sets should_quit = true.
    #[test]
    fn handle_event_quit_esc() {
        let mut app = App::new(vec![], vec![], vec![]);
        app.handle_event(&key_press(KeyCode::Esc));
        assert!(app.should_quit);
    }

    /// TUI-HE-03: Up decrements selected_field (bounded at 0).
    #[test]
    fn handle_event_up_decrements() {
        let mut app = App::new(vec![], vec![], vec![]);
        app.selected_field = 2;
        app.handle_event(&key_press(KeyCode::Up));
        assert_eq!(app.selected_field, 1);
        // Bounded at 0
        app.selected_field = 0;
        app.handle_event(&key_press(KeyCode::Up));
        assert_eq!(app.selected_field, 0);
    }

    /// TUI-HE-04: Down increments selected_field (bounded at last field).
    #[test]
    fn handle_event_down_increments() {
        let mut app = App::new(vec![], vec![], vec![]);
        assert_eq!(app.selected_field, 0);
        app.handle_event(&key_press(KeyCode::Down));
        assert_eq!(app.selected_field, 1);
        // Bounded at CONFIG_FIELDS.len() - 1
        app.selected_field = CONFIG_FIELDS.len() - 1;
        app.handle_event(&key_press(KeyCode::Down));
        assert_eq!(app.selected_field, CONFIG_FIELDS.len() - 1);
    }

    /// TUI-HE-05: 'k' acts like Up, 'j' acts like Down.
    #[test]
    fn handle_event_vim_up_down() {
        let mut app = App::new(vec![], vec![], vec![]);
        app.selected_field = 1;
        app.handle_event(&key_press(KeyCode::Char('k')));
        assert_eq!(app.selected_field, 0);

        app.handle_event(&key_press(KeyCode::Char('j')));
        assert_eq!(app.selected_field, 1);
    }

    /// TUI-HE-06: 'h' acts like Left, 'l' acts like Right.
    #[test]
    fn handle_event_vim_left_right() {
        let mut app = App::new(vec![], vec![], vec![]);
        // Select SampleRate field (index 3) to test value adjustment
        app.selected_field = 3;
        let initial_sr = app.config.sample_rate;
        app.handle_event(&key_press(KeyCode::Char('l')));
        assert_ne!(app.config.sample_rate, initial_sr);
        app.handle_event(&key_press(KeyCode::Char('h')));
        assert_eq!(app.config.sample_rate, initial_sr);
    }

    /// TUI-HE-07: Non-press key events are ignored.
    #[test]
    fn handle_event_release_ignored() {
        let mut app = App::new(vec![], vec![], vec![]);
        app.handle_event(&key_release(KeyCode::Char('q')));
        assert!(!app.should_quit);
    }

    /// TUI-HE-08: Unmapped keys are ignored without side effects.
    #[test]
    fn handle_event_unmapped_key() {
        let mut app = App::new(vec![], vec![], vec![]);
        let config_before = app.config.clone();
        app.handle_event(&key_press(KeyCode::Char('x')));
        assert_eq!(app.config.sample_rate, config_before.sample_rate);
        assert_eq!(app.selected_field, 0);
        assert!(!app.should_quit);
    }

    // --- TUI-DV: label() and display_value() tests ---

    /// TUI-DV-01: Every ConfigField variant has a non-empty label.
    #[test]
    fn all_fields_have_nonempty_label() {
        for field in &CONFIG_FIELDS {
            assert!(!field.label().is_empty(), "label for {:?} is empty", field);
        }
    }

    /// TUI-DV-02: display_value() produces a non-empty string for default config.
    #[test]
    fn display_value_nonempty_for_default() {
        let cfg = Config::default();
        for field in &CONFIG_FIELDS {
            let val = field.display_value(&cfg);
            assert!(!val.is_empty(), "display_value for {:?} is empty", field);
        }
    }

    /// TUI-DV-03: display_value() for device fields shows "(default)" when None.
    #[test]
    fn display_value_device_default() {
        let cfg = Config::default();
        assert_eq!(
            ConfigField::AudioInputDevice.display_value(&cfg),
            "(default)"
        );
        assert_eq!(
            ConfigField::AudioOutputDevice.display_value(&cfg),
            "(default)"
        );
        assert_eq!(
            ConfigField::MidiInputPort.display_value(&cfg),
            "(first available)"
        );
    }
}
