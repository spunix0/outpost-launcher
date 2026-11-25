use crate::{game, spt::Spt, AppState};
use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, State};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

const LOG_LINES: usize = 3000;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The SPT server process, only when this launcher started it. A server started elsewhere is never stopped from here.
#[derive(Default)]
pub struct ServerProc {
    child: Mutex<Option<tokio::process::Child>>,
    log: Arc<Mutex<VecDeque<String>>>,
}

impl ServerProc {
    pub fn owned_running(&self) -> bool {
        let mut g = self.child.lock().unwrap();
        let alive = matches!(g.as_mut().map(|c| c.try_wait()), Some(Ok(None)));
        if !alive {
            *g = None;
        }
        alive
    }

    pub fn kill_owned(&self) {
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.start_kill();
        }
    }
}

#[derive(serde::Serialize)]
pub struct ServerStatus {
    online: bool,
    owned: bool,
    address: String,
}

#[tauri::command]
pub async fn server_status(state: State<'_, AppState>) -> Result<ServerStatus, String> {
    let game = game::game_dir(&state)?;
    let address = game::server_addr(&game);
    let online = Spt::new(&address).ping().await;
    Ok(ServerStatus { online, owned: state.server.owned_running(), address })
}

#[tauri::command]
pub async fn server_start(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let game = game::game_dir(&state)?;
    if Spt::new(&game::server_addr(&game)).ping().await {
        return Err("The SPT server is already running.".into());
    }
    if state.server.owned_running() {
        return Err("The server is already starting.".into());
    }
    let runtime = game.join("SPT_Runtime");
    let mut cmd = tokio::process::Command::new(runtime.join("SPT.Server.exe"));
    cmd.current_dir(&runtime).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(false);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let mut child = cmd.spawn().map_err(|e| format!("Could not start SPT.Server.exe: {e}"))?;
    state.server.log.lock().unwrap().clear();
    if let Some(out) = child.stdout.take() {
        pump(app.clone(), state.server.log.clone(), out);
    }
    if let Some(err) = child.stderr.take() {
        pump(app.clone(), state.server.log.clone(), err);
    }
    *state.server.child.lock().unwrap() = Some(child);
    Ok(())
}

#[tauri::command]
pub fn server_stop(state: State<AppState>) -> Result<(), String> {
    if !state.server.owned_running() {
        return Err("This launcher did not start the running server. Close it in its own window.".into());
    }
    state.server.kill_owned();
    Ok(())
}

#[tauri::command]
pub fn server_log(state: State<AppState>) -> Vec<String> {
    state.server.log.lock().unwrap().iter().cloned().collect()
}

fn pump<R: AsyncRead + Unpin + Send + 'static>(app: AppHandle, log: Arc<Mutex<VecDeque<String>>>, r: R) {
    tauri::async_runtime::spawn(async move {
        let mut rd = BufReader::new(r);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match rd.read_until(b'\n', &mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let line = strip_ansi(&String::from_utf8_lossy(&buf)).trim_end().to_string();
            if line.is_empty() {
                continue;
            }
            {
                let mut l = log.lock().unwrap();
                if l.len() >= LOG_LINES {
                    l.pop_front();
                }
                l.push_back(line.clone());
            }
            let _ = app.emit("server-log", line);
        }
    });
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\u{1b}' {
            if it.peek() == Some(&'[') {
                it.next();
                while let Some(n) = it.next() {
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn strips_colour_codes() {
        assert_eq!(super::strip_ansi("\u{1b}[32mServer is running\u{1b}[0m"), "Server is running");
        assert_eq!(super::strip_ansi("plain"), "plain");
    }
}
