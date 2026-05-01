mod audio;
mod dsp;
mod midi;
mod tui;

use tui::{App, Status};

fn main() -> anyhow::Result<()> {
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
    Ok(())
}
