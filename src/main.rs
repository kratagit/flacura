#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod aurora;
mod config;
mod decode;
mod ipc;
mod motion;
mod platform;
mod ui;
mod waveform;

use std::path::PathBuf;

fn main() {
    let arg = std::env::args_os().nth(1);
    match arg.as_ref().and_then(|a| a.to_str()) {
        Some("--register") => return exit_with(platform::register()),
        Some("--unregister") => return exit_with(platform::unregister()),
        _ => {}
    }

    let path = arg
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| std::path::absolute(&p).unwrap_or(p));

    let listener = match ipc::acquire(path.as_ref()) {
        ipc::Role::Forwarded => return,
        ipc::Role::Primary(l) => Some(l),
        ipc::Role::Standalone => None,
    };

    // Start decoding and opening the audio device before the window exists,
    // so sound starts while the GPU context is still being created.
    let config = config::Config::load();
    let player = audio::Player::new();
    player.set_volume(ui::volume_gain(config.volume, config.muted));
    if let Some(p) = &path {
        player.load(p.clone());
    }

    if let Err(e) = ui::run(player, listener, config) {
        eprintln!("flacura: {e}");
        std::process::exit(1);
    }
}

fn exit_with(r: Result<(), String>) {
    if let Err(e) = r {
        eprintln!("flacura: {e}");
        std::process::exit(1);
    }
}
