mod agents;
mod app;
mod cli;
mod command;
mod ghostty;
mod layout;
mod model;
mod pi;
mod storage;
mod ui;
mod watch;

use std::ffi::OsString;

use anyhow::Result;

pub use app::App;

pub fn run(args: impl IntoIterator<Item = OsString>) -> Result<()> {
    App::from_env()?.run(args)
}
