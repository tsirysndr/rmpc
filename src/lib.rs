#![deny(clippy::unwrap_used, clippy::pedantic)]
#![allow(
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
use std::{ops::Sub, sync::mpsc::TryRecvError, time::Duration};

use anyhow::{bail, Result};
use cli::run_external;
use config::ConfigFile;
use crossterm::event::{Event, KeyEvent};
use deps::DEPENDENCIES;
use itertools::Itertools;
use log::{error, info, trace, warn};
use mpd::{client::Client, commands::idle::IdleEvent};
use ratatui::{prelude::Backend, Terminal};
use ui::{Level, UiEvent};
use utils::{
    macros::{status_error, status_info, try_cont, try_skip},
    mouse_event::{MouseEvent, MouseEventTracker},
    DurationExt, ErrorExt,
};
use ytdlp::YtDlp;

use crate::{
    config::Config,
    mpd::mpd_client::MpdClient,
    ui::Ui,
    utils::macros::{status_warn, try_ret},
};

#[cfg(test)]
mod tests {
    pub mod fixtures;
}

mod cli;
mod config;
mod context;
mod deps;
mod geometry;
mod logging;
mod mpd;
mod tmux;
mod ui;
mod utils;
mod ytdlp;

#[derive(Debug)]
pub enum WorkRequest {
    DownloadYoutube { url: String },
}

#[derive(Debug)]
pub enum WorkDone {
    YoutubeDowloaded { file_path: String },
}

#[derive(Debug)]
pub enum AppEvent {
    UserKeyInput(KeyEvent),
    UserMouseInput(MouseEvent),
    Status(String, Level),
    Log(Vec<u8>),
    IdleEvent(IdleEvent),
    RequestStatusUpdate,
    RequestRender(bool),
    Resized { columns: u16, rows: u16 },
    WorkDone(Result<WorkDone>),
}

pub fn main_tui() -> Result<()> {
    let (tx, rx) = std::sync::mpsc::channel::<AppEvent>();
    logging::init(tx.clone()).expect("Logger to initialize");
    log::debug!(rev = env!("VERGEN_GIT_DESCRIBE"); "rmpc started");
    std::thread::spawn(|| DEPENDENCIES.iter().for_each(|d| d.log()));

    let (worker_tx, worker_rx) = std::sync::mpsc::channel::<WorkRequest>();

    let mut address = Some(std::env::var("MPD_HOST").unwrap_or("127.0.0.1:6600".to_string()));
    let mut password = match std::env::var("MPD_PASSWORD") {
        Ok(password) => Some(password),
        Err(_) => None,
    };

    let config =
        ConfigFile::default().into_config(None, std::mem::take(&mut address), std::mem::take(&mut password), false)?;

    try_ret!(tx.send(AppEvent::RequestRender(false)), "Failed to render first frame");

    let mut client = try_ret!(
        Client::init(config.address, config.password, "command", true),
        "Failed to connect to mpd"
    );

    let terminal = try_ret!(ui::setup_terminal(config.enable_mouse), "Failed to setup terminal");
    let tx_clone = tx.clone();

    let context = try_ret!(
        context::AppContext::try_new(&mut client, config, tx_clone, worker_tx),
        "Failed to create app context"
    );

    let mut render_loop = RenderLoop::new(tx.clone(), context.config);
    if context.status.state == mpd::commands::status::State::Play {
        render_loop.start()?;
    }

    let tx_clone = tx.clone();
    std::thread::Builder::new()
        .name("worker task".to_owned())
        .spawn(|| worker_task(worker_rx, tx_clone, context.config))?;

    let tx_clone = tx.clone();

    std::thread::Builder::new()
        .name("input poll".to_owned())
        .spawn(|| input_poll_task(tx_clone))?;

    let mut idle_client = try_ret!(
        Client::init(context.config.address, context.config.password, "idle", true),
        "Failed to connect to mpd with idle client"
    );

    let main_task = std::thread::Builder::new().name("main task".to_owned()).spawn(|| {
        main_task(context, rx, client, render_loop, terminal);
    })?;

    idle_client.set_read_timeout(None)?;
    std::thread::Builder::new()
        .name("idle task".to_owned())
        .spawn(|| idle_task(idle_client, tx))?;

    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        crossterm::terminal::disable_raw_mode().expect("Disabling of raw mode to succeed");
        crossterm::execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen)
            .expect("Exit from alternate screen to succeed");
        original_hook(panic);
    }));

    info!("Application initialized successfully");

    main_task.join().expect("Main task to not fail");

    Ok(())
}

fn handle_work_request(request: WorkRequest, config: &Config) -> Result<WorkDone> {
    match request {
        WorkRequest::DownloadYoutube { url } => {
            let Some(cache_dir) = config.cache_dir else {
                bail!("Youtube support requires 'cache_dir' to be configured")
            };

            if let Err(unsupported_list) = crate::deps::is_youtube_supported(config.address) {
                status_warn!(
                    "Youtube support requires the following and may thus not work properly: {}",
                    unsupported_list.join(", ")
                );
            } else {
                status_info!("Downloading '{url}'");
            }

            let ytdlp = YtDlp::new(cache_dir)?;
            let file_path = ytdlp.download(&url)?;

            Ok(WorkDone::YoutubeDowloaded { file_path })
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
fn worker_task(
    work_request_receiver: std::sync::mpsc::Receiver<WorkRequest>,
    work_result_sender: std::sync::mpsc::Sender<AppEvent>,
    config: &Config,
) {
    while let Ok(request) = work_request_receiver.recv() {
        match handle_work_request(request, config) {
            Ok(result) => {
                try_cont!(
                    work_result_sender.send(AppEvent::WorkDone(Ok(result))),
                    "Failed to send work done success event"
                );
            }
            Err(err) => {
                try_cont!(
                    work_result_sender.send(AppEvent::WorkDone(Err(err))),
                    "Failed to send work done error event"
                );
            }
        }
    }
}

fn main_task<B: Backend + std::io::Write>(
    mut context: context::AppContext,
    event_receiver: std::sync::mpsc::Receiver<AppEvent>,
    mut client: Client<'_>,
    mut render_loop: RenderLoop,
    mut terminal: Terminal<B>,
) {
    let mut ui = Ui::new(&context).expect("UI to be created correctly");
    let event_receiver = event_receiver;
    let mut render_wanted = false;
    let mut full_rerender_wanted = false;
    let max_fps = 30f64;
    let min_frame_duration = Duration::from_secs_f64(1f64 / max_fps);
    let mut last_render = std::time::Instant::now().sub(Duration::from_secs(10));
    ui.before_show(&mut context, &mut client)
        .expect("Initial render init to succeed");

    loop {
        let now = std::time::Instant::now();

        let event = if render_wanted {
            match event_receiver.recv_timeout(
                min_frame_duration
                    .checked_sub(now - last_render)
                    .unwrap_or(Duration::ZERO),
            ) {
                Ok(v) => Some(v),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => None,
            }
        } else {
            event_receiver.recv().ok()
        };

        if let Some(event) = event {
            match event {
                AppEvent::UserKeyInput(key) => match ui.handle_key(key, &mut context, &mut client) {
                    Ok(ui::KeyHandleResult::SkipRender) => continue,
                    Ok(ui::KeyHandleResult::Quit) => {
                        if let Err(err) = ui.on_event(UiEvent::Exit, &mut context, &mut client) {
                            error!(error:? = err, event:?; "Ui failed to handle quit event");
                        }
                        break;
                    }
                    Ok(ui::KeyHandleResult::RenderRequested) => {
                        render_wanted = true;
                    }
                    Ok(ui::KeyHandleResult::FullRenderRequested) => {
                        render_wanted = true;
                        full_rerender_wanted = true;
                    }
                    Err(err) => {
                        status_error!(err:?; "Error: {}", err.to_status());
                        render_wanted = true;
                    }
                },
                AppEvent::UserMouseInput(ev) => match ui.handle_mouse_event(ev, &mut client, &mut context) {
                    Ok(ui::KeyHandleResult::SkipRender) => continue,
                    Ok(ui::KeyHandleResult::Quit) => {
                        if let Err(err) = ui.on_event(UiEvent::Exit, &mut context, &mut client) {
                            error!(error:? = err, event:?; "Ui failed to handle quit event");
                        }
                        break;
                    }
                    Ok(ui::KeyHandleResult::RenderRequested) => {
                        render_wanted = true;
                    }
                    Ok(ui::KeyHandleResult::FullRenderRequested) => {
                        render_wanted = true;
                        full_rerender_wanted = true;
                    }
                    Err(err) => {
                        status_error!(err:?; "Error: {}", err.to_status());
                        render_wanted = true;
                    }
                },
                AppEvent::Status(message, level) => {
                    ui.display_message(message, level);
                    render_wanted = true;
                }
                AppEvent::Log(msg) => {
                    if let Err(err) = ui.on_event(UiEvent::LogAdded(msg), &mut context, &mut client) {
                        error!(error:? = err; "Ui failed to handle log event");
                    }
                }
                AppEvent::IdleEvent(event) => {
                    if let Err(err) = handle_idle_event(event, &mut context, &mut client, &mut render_loop) {
                        status_error!(error:? = err, event:?; "Failed handle idle event, event: '{:?}', error: '{}'", event, err.to_status());
                    }
                    if let Ok(ev) = event.try_into() {
                        if let Err(err) = ui.on_event(ev, &mut context, &mut client) {
                            status_error!(error:? = err, event:?; "Ui failed to handle idle event, event: '{:?}', error: '{}'", event, err.to_status());
                        }
                    }
                    render_wanted = true;
                }
                AppEvent::RequestStatusUpdate => {
                    match client.get_status() {
                        Ok(status) => context.status = status,
                        Err(err) => {
                            error!(err:?; "Unable to update status requested by render loop");
                        }
                    };
                    render_wanted = true;
                }
                AppEvent::RequestRender(wanted) => {
                    render_wanted = true;
                    full_rerender_wanted = wanted;
                }
                AppEvent::WorkDone(Ok(result)) => match result {
                    WorkDone::YoutubeDowloaded { file_path } => {
                        match client.add(&file_path) {
                            Ok(()) => {
                                status_info!("File '{file_path}' added to the queue");
                            }
                            Err(err) => {
                                status_error!(err:?; "Failed to add '{file_path}' to the queue");
                            }
                        };
                    }
                },
                AppEvent::WorkDone(Err(err)) => {
                    status_error!("{}", err);
                }
                AppEvent::Resized { columns, rows } => {
                    if let Err(err) = ui.on_event(UiEvent::Resized { columns, rows }, &mut context, &mut client) {
                        error!(error:? = err, event:?; "Ui failed to resize event");
                    }
                    full_rerender_wanted = true;
                    render_wanted = true;
                }
            }
        }
        if render_wanted {
            let till_next_frame = min_frame_duration.saturating_sub(now.duration_since(last_render));
            if till_next_frame != Duration::ZERO {
                continue;
            }
            if full_rerender_wanted {
                terminal.swap_buffers();
                terminal.swap_buffers();
                terminal.clear().expect("Terminal clear after full rerender to succeed");
                full_rerender_wanted = false;
            }
            terminal
                .draw(|frame| {
                    if let Err(err) = ui.render(frame, &mut context) {
                        error!(error:? = err; "Failed to render a frame");
                    };
                })
                .expect("Expected render to succeed");
            if let Err(err) = ui.post_render(&mut terminal.get_frame(), &mut context) {
                error!(error:? = err; "Failed handle post render phase");
            };
            last_render = now;
            render_wanted = false;
        }
    }

    ui::restore_terminal(&mut terminal, context.config.enable_mouse).expect("Terminal restore to succeed");
}

fn handle_idle_event(
    event: IdleEvent,
    context: &mut context::AppContext,
    client: &mut Client<'_>,
    render_loop: &mut RenderLoop,
) -> Result<()> {
    match event {
        IdleEvent::Mixer => {
            if context.supported_commands.contains("getvol") {
                context.status.volume = try_ret!(client.get_volume(), "Failed to get volume");
            } else {
                context.status = try_ret!(client.get_status(), "Failed to get status");
            }
        }
        IdleEvent::Options => context.status = try_ret!(client.get_status(), "Failed to get status"),
        IdleEvent::Player => {
            let current_song_id = context.status.song;

            context.status = try_ret!(client.get_status(), "Failed get status");

            if context.status.state == mpd::commands::status::State::Play {
                try_skip!(render_loop.start(), "Failed to start render loop");
            } else {
                try_skip!(render_loop.stop(), "Failed to stop render loop");
            }

            if context.status.song.is_some_and(|id| Some(id) != current_song_id) {
                if let Some(command) = context.config.on_song_change {
                    let env = match context.get_current_song(client) {
                        Ok(Some(song)) => song
                            .metadata
                            .into_iter()
                            .map(|(mut k, v)| {
                                k.make_ascii_uppercase();
                                (k, v)
                            })
                            .chain(std::iter::once(("FILE".to_owned(), song.file)))
                            .chain(std::iter::once((
                                "DURATION".to_owned(),
                                song.duration.map_or_else(String::new, |d| d.to_string()),
                            )))
                            .collect_vec(),
                        Ok(None) => {
                            status_error!("No song found when executing on_song_change");
                            Vec::new()
                        }
                        Err(err) => {
                            status_error!("Unexpected error when crating env for on_song_change: {:?}", err);
                            Vec::new()
                        }
                    };

                    run_external(command, env);
                };
            }
        }
        IdleEvent::Playlist => {
            let queue = client.playlist_info()?;
            context.queue = queue.unwrap_or_default();
        }
        IdleEvent::StoredPlaylist => {}
        IdleEvent::Database => {}
        IdleEvent::Update => {}
        // TODO: handle these events eventually ?
        IdleEvent::Output
        | IdleEvent::Partition
        | IdleEvent::Sticker
        | IdleEvent::Subscription
        | IdleEvent::Message
        | IdleEvent::Neighbor
        | IdleEvent::Mount => {
            warn!(event:?; "Received unhandled event");
        }
    };
    Ok(())
}

fn idle_task(mut idle_client: Client<'_>, sender: std::sync::mpsc::Sender<AppEvent>) {
    let mut error_count = 0;
    let sender = sender;
    loop {
        let events = match idle_client.idle(None) {
            Ok(val) => val,
            Err(err) => {
                if error_count > 5 {
                    error!(err:?; "Unexpected error when receiving idle events");
                    break;
                }
                warn!(err:?; "Unexpected error when receiving idle events");
                error_count += 1;
                std::thread::sleep(Duration::from_secs(error_count));
                continue;
            }
        };

        for event in events {
            trace!(idle_event:? = event; "Received idle event");
            if let Err(err) = sender.send(AppEvent::IdleEvent(event)) {
                error!(error:? = err; "Failed to send app event");
            }
        }
    }
}

fn input_poll_task(user_input_tx: std::sync::mpsc::Sender<AppEvent>) {
    let user_input_tx = user_input_tx;
    let mut mouse_event_tracker = MouseEventTracker::default();
    loop {
        match crossterm::event::poll(Duration::from_millis(250)) {
            Ok(true) => match crossterm::event::read() {
                Ok(Event::Mouse(mouse)) => {
                    if let Some(ev) = mouse_event_tracker.track_and_get(mouse) {
                        if let Err(err) = user_input_tx.send(AppEvent::UserMouseInput(ev)) {
                            error!(error:? = err; "Failed to send user mouse input");
                        }
                    }
                }
                Ok(Event::Key(key)) => {
                    if let Err(err) = user_input_tx.send(AppEvent::UserKeyInput(key)) {
                        error!(error:? = err; "Failed to send user input");
                    }
                }
                Ok(Event::Resize(columns, rows)) => {
                    if let Err(err) = user_input_tx.send(AppEvent::Resized { columns, rows }) {
                        error!(error:? = err; "Failed to render request after resize");
                    }
                }
                Ok(ev) => {
                    log::warn!(ev:?; "Unexpected event");
                }
                Err(err) => {
                    warn!(error:? = err; "Failed to read input event");
                    continue;
                }
            },
            Ok(_) => {}
            Err(e) => warn!(error:? = e; "Error when polling for event"),
        }
    }
}

enum LoopEvent {
    Start,
    Stop,
}

#[derive(Debug)]
struct RenderLoop {
    event_tx: Option<std::sync::mpsc::Sender<LoopEvent>>,
}

impl RenderLoop {
    fn new(render_sender: std::sync::mpsc::Sender<AppEvent>, config: &Config) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<LoopEvent>();

        // send stop event at the start to not start the loop immedietally
        if let Err(err) = tx.send(LoopEvent::Stop) {
            error!(error:? = err; "Failed to properly initialize status update loop");
        }

        let Some(update_interval) = config.status_update_interval_ms.map(Duration::from_millis) else {
            return Self { event_tx: None };
        };
        std::thread::spawn(move || {
            loop {
                match rx.try_recv() {
                    Ok(LoopEvent::Stop) => loop {
                        if let Ok(LoopEvent::Start) = rx.recv() {
                            break;
                        }
                    },
                    Err(TryRecvError::Disconnected) => {
                        error!("Render loop channel is disconnected");
                    }
                    Ok(LoopEvent::Start) | Err(TryRecvError::Empty) => {} // continue with the update loop
                }

                std::thread::sleep(update_interval);
                if let Err(err) = render_sender.send(AppEvent::RequestStatusUpdate) {
                    error!(error:? = err; "Failed to send status update request");
                }
            }
        });
        Self { event_tx: Some(tx) }
    }

    fn start(&mut self) -> Result<()> {
        if let Some(tx) = &self.event_tx {
            Ok(tx.send(LoopEvent::Start)?)
        } else {
            Ok(())
        }
    }

    fn stop(&mut self) -> Result<()> {
        if let Some(tx) = &self.event_tx {
            Ok(tx.send(LoopEvent::Stop)?)
        } else {
            Ok(())
        }
    }
}