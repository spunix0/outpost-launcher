#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod forge;
mod game;
mod mods;
mod server;
mod spt;

use std::sync::Mutex;
use tauri::{Emitter, Manager, WindowEvent};

pub struct AppState {
    pub settings: Mutex<game::Settings>,
    pub server: server::ServerProc,
    pub forge: forge::Forge,
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let settings = game::Settings::load(app.handle());
            app.manage(AppState { settings: Mutex::new(settings), server: server::ServerProc::default(), forge: forge::Forge::default() });

            // auto-start: only when enabled, the SPT folder is valid, and no server is answering yet
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let state = handle.state::<AppState>();
                let enabled = state.settings.lock().unwrap().auto_start_server;
                if !enabled {
                    return;
                }
                if let Err(e) = server::start(&handle, &state).await {
                    if !e.contains("already") && !e.contains("Choose your SPT folder") {
                        state.server.note(format!("Auto-start failed: {e}"));
                    }
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // A server we started writes into our pipes; it must not outlive the launcher unnoticed.
            if let WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<AppState>();
                if state.server.owned_running() {
                    api.prevent_close();
                    let _ = window.emit("confirm-close", ());
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            game::get_settings,
            game::save_settings,
            server::server_status,
            server::server_start,
            server::server_stop,
            server::server_log,
            spt::list_profiles,
            spt::launch_game,
            spt::profile_types,
            spt::profile_create,
            spt::profile_wipe,
            game::set_last_profile,
            forge::forge_get,
            mods::mods_scan,
            mods::spt_version,
            mods::mod_install,
            mods::mod_uninstall,
            mods::mod_uninstall_plan,
            mods::mod_uninstall_local,
            open_url,
            quit_app
        ])
        .run(tauri::generate_context!())
        .expect("error while running Outpost Launcher");
}

#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("Only web links can be opened.".into());
    }
    std::process::Command::new("explorer").arg(&url).spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// Stops the server this launcher started (if any) and exits.
#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    app.state::<AppState>().server.kill_owned();
    app.exit(0);
}
