//! `gwm` — the terminal front-end.
//!
//! Keeps the terminal lifecycle tiny: initialise, run the app, always restore.

mod app;
mod count_job;
mod download_job;
mod file_browser;
mod gotek_job;
mod install_job;
mod net_job;
mod read_job;
mod rpm_job;
mod text_input;
mod theme;
mod ui;
mod version_job;
mod write_job;

use anyhow::{Context, Result};

use app::App;
use gwm_core::Core;

fn main() -> Result<()> {
    // Make sure ~/.local/bin is on PATH so tools installed by pipx and our build
    // recipes are found and runnable without the user first fixing their shell.
    gwm_core::tools::ensure_user_path();

    let core = Core::init().context("failed to initialise the gwm core")?;

    let mut terminal = ratatui::init();
    let result = App::new(core).run(&mut terminal);
    ratatui::restore();

    result
}
