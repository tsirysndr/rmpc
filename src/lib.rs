#![deny(clippy::unwrap_used, clippy::pedantic)]
#![allow(
    clippy::single_match,
    clippy::type_complexity,
    clippy::module_name_repetitions,
    clippy::unused_self,
    clippy::unnested_or_patterns,
    clippy::match_same_arms,
    clippy::manual_let_else,
    clippy::needless_return,
    clippy::zero_sized_map_values,
    clippy::too_many_lines,
    clippy::match_single_binding,
    clippy::struct_field_names,
    clippy::redundant_closure_for_method_calls,
    unused_macros
)]
use anyhow::Result;
use config::ConfigFile;
use context::AppContext;
use core::update_loop::UpdateLoop;
use crossbeam::channel::unbounded;
use log::info;
use mpd::{client::Client, commands::State};
use shared::{
    dependencies::DEPENDENCIES,
    events::{AppEvent, ClientRequest, WorkRequest},
    mpd_query::{MpdCommand, MpdQuery, MpdQueryResult},
};
use shared::{logging, tmux};

use crate::shared::macros::try_ret;

#[cfg(test)]
mod tests {
    pub mod fixtures;
}

mod config;
mod context;
mod core;
mod mpd;
mod shared;
mod ui;

pub fn main_tui() -> Result<()> {
    let (worker_tx, worker_rx) = unbounded::<WorkRequest>();
    let (client_tx, client_rx) = unbounded::<ClientRequest>();
    let (event_tx, event_rx) = unbounded::<AppEvent>();
    logging::init(event_tx.clone()).expect("Logger to initialize");

    log::debug!(rev = env!("VERGEN_GIT_DESCRIBE"); "rmpc started");
    std::thread::Builder::new()
        .name("dependency_check".to_string())
        .spawn(|| DEPENDENCIES.iter().for_each(|d| d.log()))?;

    try_ret!(event_tx.send(AppEvent::RequestRender), "Failed to render first frame");

    let mut address = Some(std::env::var("MPD_HOST").unwrap_or("localhost:6600".to_string()));
    let mut password = match std::env::var("MPD_PASSWORD") {
        Ok(password) => Some(password),
        Err(_) => None,
    };

    let config =
        ConfigFile::default().into_config(None, std::mem::take(&mut address), std::mem::take(&mut password), false)?;

    try_ret!(event_tx.send(AppEvent::RequestRender), "Failed to render first frame");

    let mut client = try_ret!(
        Client::init(config.address, config.password, "command"),
        "Failed to connect to MPD"
    );
    client.set_read_timeout(None)?;

    let enable_mouse = true;
    let terminal = try_ret!(ui::setup_terminal(enable_mouse), "Failed to setup terminal");
    let tx_clone = event_tx.clone();

    let context = try_ret!(
        AppContext::try_new(&mut client, config, tx_clone, worker_tx.clone(), client_tx.clone()),
        "Failed to create app context"
    );

    let mut render_loop = UpdateLoop::try_new(client_tx.clone(), context.config.status_update_interval_ms)?;
    if context.status.state == State::Play {
        render_loop.start()?;
    }

    core::client::init(client_rx.clone(), event_tx.clone(), client)?;
    core::work::init(worker_rx.clone(), client_tx.clone(), event_tx.clone(), context.config)?;
    core::input::init(event_tx.clone())?;
    let event_loop_handle = core::event_loop::init(context, event_rx, render_loop, terminal)?;

    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        crossterm::terminal::disable_raw_mode().expect("Disabling of raw mode to succeed");
        crossterm::execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen)
            .expect("Exit from alternate screen to succeed");
        original_hook(panic);
    }));

    info!("Application initialized successfully");

    let mut terminal = event_loop_handle.join().expect("event loop to not panic");
    try_ret!(
        ui::restore_terminal(&mut terminal, enable_mouse),
        "Terminal restore to succeed"
    );

    Ok(())
}
