use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph};
use ratatui::{backend::CrosstermBackend, symbols, Frame, Terminal};
use std::io;
use std::time::Duration;

// ── Application state ──────────────────────────────────────────────

/// Configurable parameters for the vocoder.
#[derive(Debug, Clone)]
pub struct Config {
    pub sample_rate: u32,
    pub buffer_size: u32,
    pub midi_channel: u8,
    pub formant_shift: f32,
    pub pitch_shift: f32,
    pub gain: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sample_rate: 44_100,
            buffer_size: 512,
            midi_channel: 1,
            formant_shift: 1.0,
            pitch_shift: 0.0,
            gain: 0.8,
        }
    }
}

/// Runtime status displayed by the TUI.
#[derive(Debug, Clone, Default)]
pub struct Status {
    pub audio_running: bool,
    pub midi_connected: bool,
    pub current_note: Option<String>,
    pub cpu_usage: f32,
    pub input_level: f32,
    pub output_level: f32,
}

/// Which config field is currently selected for editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigField {
    SampleRate,
    BufferSize,
    MidiChannel,
    FormantShift,
    PitchShift,
    Gain,
}

const CONFIG_FIELDS: [ConfigField; 6] = [
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
                cfg.gain = (cfg.gain + delta as f32 * 0.05).clamp(0.0, 1.5);
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
    should_quit: bool,
}

impl App {
    pub fn new() -> Self {
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        Self {
            config: Config::default(),
            status: Status::default(),
            selected_field: 0,
            list_state,
            should_quit: false,
        }
    }

    pub fn with_status(mut self, status: Status) -> Self {
        self.status = status;
        self
    }

    fn handle_event(&mut self, evt: &Event) {
        if let Event::Key(key) = evt {
            if key.kind != KeyEventKind::Press {
                return;
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => {
                    self.should_quit = true;
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
                    field.adjust(&mut self.config, 1);
                }
                KeyCode::Left | KeyCode::Char('h') => {
                    let field = CONFIG_FIELDS[self.selected_field];
                    field.adjust(&mut self.config, -1);
                }
                _ => {}
            }
        }
    }
}

// ── Rendering ──────────────────────────────────────────────────────

fn draw(f: &mut Frame, app: &App) {
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

    let note_display = app.status.current_note.as_deref().unwrap_or("---");

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
