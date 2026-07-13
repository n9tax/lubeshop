//! `gwm` — the terminal front-end.
//!
//! Keeps the terminal lifecycle tiny: initialise, run the app, always restore.

mod app;
mod count_job;
mod download_job;
mod file_browser;
mod install_job;
mod net_job;
mod read_job;
mod text_input;
mod theme;
mod ui;
mod write_job;

use anyhow::{Context, Result};

use app::App;
use gwm_core::Core;

fn main() -> Result<()> {
    let core = Core::init().context("failed to initialise the gwm core")?;

    let mut terminal = ratatui::init();
    let result = App::new(core).run(&mut terminal);
    ratatui::restore();

    result
}
