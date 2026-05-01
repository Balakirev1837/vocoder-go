#[cfg(feature = "audio")]
mod audio;
#[cfg(feature = "midi")]
mod midi;
#[cfg(feature = "tui")]
mod tui;

#[cfg(feature = "tui")]
use tui::{App, Status};
use vocoder::dsp;

fn main() -> anyhow::Result<()> {
    #[cfg(feature = "tui")]
    {
        let status = Status {
            audio_running: true,
            midi_connected: false,
            current_note: None,
            cpu_usage: 0.12,
            input_level: 0.3,
            output_level: 0.25,
        };

        let app = App::new().with_status(status);
        let returned_app = tui::run(app)?;

        println!("Final config: {:?}", returned_app.config);
    }

    #[cfg(not(feature = "tui"))]
    {
        println!("vocoder: DSP core ready (no TUI)");
    }

    Ok(())
}
