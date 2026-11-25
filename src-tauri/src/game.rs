use crate::AppState;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager, State};

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Settings {
    pub game_path: String,
    pub minimize_on_launch: bool,
    pub last_profile: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { game_path: detect_game_path(), minimize_on_launch: true, last_profile: None }
    }
}

fn settings_file(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("settings.json"))
}

impl Settings {
    pub fn load(app: &AppHandle) -> Settings {
        settings_file(app)
            .and_then(|f| std::fs::read(f).ok())
            .and_then(|b| serde_json::from_slice(strip_bom(&b)).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, app: &AppHandle) -> Result<(), String> {
        let f = settings_file(app).ok_or("No settings folder available.")?;
        if let Some(d) = f.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        std::fs::write(f, serde_json::to_vec_pretty(self).unwrap_or_default()).map_err(|e| e.to_string())
    }
}

pub fn strip_bom(b: &[u8]) -> &[u8] {
    b.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(b)
}

pub fn read_json(p: &Path) -> Option<serde_json::Value> {
    std::fs::read(p).ok().and_then(|b| serde_json::from_slice(strip_bom(&b)).ok())
}

fn detect_game_path() -> String {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        candidates.extend(exe.ancestors().skip(1).take(3).map(Path::to_path_buf));
    }
    candidates.push(PathBuf::from(r"C:\Single Player Tarkov"));
    candidates.into_iter().find(|p| check_spt_folder(p).is_ok()).map(|p| p.display().to_string()).unwrap_or_default()
}

/// An SPT install = the game exe plus SPT_Runtime with the server. Never a Steam / live EFT folder.
pub fn check_spt_folder(p: &Path) -> Result<(), String> {
    if p.as_os_str().is_empty() {
        return Err("Choose your SPT folder in Settings.".into());
    }
    if p.display().to_string().to_lowercase().contains("steamapps") {
        return Err("That is a Steam game folder. Choose your separate SPT folder; live EFT is never touched.".into());
    }
    if !p.join("EscapeFromTarkov.exe").is_file() {
        return Err("EscapeFromTarkov.exe was not found in the chosen folder.".into());
    }
    if !p.join("SPT_Runtime").join("SPT.Server.exe").is_file() {
        return Err("SPT_Runtime\\SPT.Server.exe was not found: this is not an SPT folder.".into());
    }
    Ok(())
}

pub fn game_dir(state: &AppState) -> Result<PathBuf, String> {
    let p = PathBuf::from(state.settings.lock().unwrap().game_path.clone());
    check_spt_folder(&p)?;
    Ok(p)
}

/// "ip:port" from SPT_Runtime/SPT_Data/configs/http.json (what the SPT launcher connects to).
pub fn server_addr(game: &Path) -> String {
    let v = read_json(&game.join("SPT_Runtime").join("SPT_Data").join("configs").join("http.json")).unwrap_or_default();
    let ip = v.get("ip").and_then(|x| x.as_str()).unwrap_or("127.0.0.1");
    let ip = if ip == "0.0.0.0" { "127.0.0.1" } else { ip };
    let port = v.get("port").and_then(|x| x.as_u64()).unwrap_or(6969);
    format!("{ip}:{port}")
}

#[tauri::command]
pub fn get_settings(state: State<AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
pub fn save_settings(app: AppHandle, state: State<AppState>, settings: Settings) -> Result<(), String> {
    check_spt_folder(Path::new(&settings.game_path))?;
    settings.save(&app)?;
    *state.settings.lock().unwrap() = settings;
    Ok(())
}
