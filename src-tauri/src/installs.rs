//! Several SPT installs side by side: a registry of install folders, switching the active one, the list of official SPT
//! releases, and an installer (download + verify, clone the Tarkov client, extract the SPT archive).
//!
//! An SPT release is only the SPT layer (about 150 MB). The game itself (the Tarkov client, ~57 GB) must come from the user's
//! own files: a new install clones the client of an existing install that has the same client build number. Big, never-changing
//! asset files are hardlinked (no extra space); everything small or mutable is copied.

use crate::{forge, game, server, spt, AppState};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, State};

const RELEASE_REPOS: [&str; 2] = ["SP-Tushonka/build", "sp-tarkov/build"];
const DOWNLOAD_PREFIXES: [&str; 2] = ["https://github.com/sp-tarkov/build/releases/download/", "https://github.com/sp-tushonka/build/releases/download/"];
/// What belongs to the Tarkov client itself (the rest of an SPT folder is SPT, mods or logs).
const CLIENT_ITEMS: [&str; 5] = ["EscapeFromTarkov.exe", "UnityPlayer.dll", "EscapeFromTarkov_Data", "MonoBleedingEdge", "NLog"];
/// Files at least this big under EscapeFromTarkov_Data (outside Managed and Plugins) are shared by hardlink.
const SHARE_MIN: u64 = 1 << 20;

// ---------- version detection ----------

/// The ProductVersion string of a PE file, e.g. "0.16.9.5-40743-f137e819" for EscapeFromTarkov.exe.
pub fn product_version(exe: &Path) -> Option<String> {
    let b = std::fs::read(exe).ok()?;
    let key: Vec<u8> = "ProductVersion".encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
    let at = b.windows(key.len()).position(|w| w == key.as_slice())?;
    let mut p = at + key.len() + 2; // the key's terminating NUL
    while p + 1 < b.len() && b[p] == 0 && b[p + 1] == 0 {
        p += 1;
    }
    while p < b.len() && b[p] == 0 {
        p += 1;
    }
    let mut e = p;
    while e + 1 < b.len() && !(b[e] == 0 && b[e + 1] == 0) {
        e += 2;
    }
    let units: Vec<u16> = b[p..e].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let s = String::from_utf16(&units).ok()?;
    (!s.trim().is_empty()).then(|| s.trim().to_string())
}

/// The client build number (the only part that identifies a client): "0.16.9.5-40743-f137e819" -> "40743".
pub fn eft_build(s: &str) -> String {
    s.split(|c: char| matches!(c, '-' | '.' | ' ' | '`')).filter(|t| t.len() >= 4 && t.chars().all(|c| c.is_ascii_digit())).last().unwrap_or("").to_string()
}

/// 4 = SPT 4.x layout (SPT_Runtime\SPT.Server.exe), 3 = SPT 3.x layout (SPT.Server.exe at the root), 0 = unknown.
pub fn generation(path: &Path) -> u32 {
    if path.join("SPT_Runtime").join("SPT.Server.exe").is_file() {
        4
    } else if path.join("SPT.Server.exe").is_file() {
        3
    } else {
        0
    }
}

fn spt_version_of(path: &Path) -> String {
    spt::file_version(&path.join("BepInEx").join("plugins").join("spt").join("spt-core.dll")).map(|(a, b, c)| format!("{a}.{b}.{c}")).unwrap_or_default()
}

fn same_path(a: &str, b: &str) -> bool {
    let n = |s: &str| s.trim().trim_end_matches(['\\', '/']).replace('/', "\\").to_lowercase();
    !a.trim().is_empty() && n(a) == n(b)
}

fn slug(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    let s = s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    if s.is_empty() { "install".into() } else { s }
}

fn unique_id(existing: &[game::Install], name: &str) -> String {
    let base = slug(name);
    let mut id = base.clone();
    let mut n = 2;
    while existing.iter().any(|i| i.id == id) {
        id = format!("{base}-{n}");
        n += 1;
    }
    id
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ---------- registry ----------

#[derive(Serialize)]
pub struct InstallInfo {
    id: String,
    name: String,
    path: String,
    active: bool,
    exists: bool,
    generation: u32,
    spt_version: String,
    eft_version: String,
    eft_build: String,
    /// created by this launcher (inside the installs folder), so it may also be removed by it
    own: bool,
    switchable: bool,
}

fn info_of(i: &game::Install, active_path: &str, root: &str) -> InstallInfo {
    let p = PathBuf::from(&i.path);
    let exists = p.join("EscapeFromTarkov.exe").is_file();
    let generation = if exists { generation(&p) } else { 0 };
    let eft_version = if exists { product_version(&p.join("EscapeFromTarkov.exe")).unwrap_or_default() } else { String::new() };
    InstallInfo {
        id: i.id.clone(),
        name: i.name.clone(),
        path: i.path.clone(),
        active: same_path(&i.path, active_path),
        exists,
        generation,
        spt_version: if exists { spt_version_of(&p) } else { String::new() },
        eft_build: eft_build(&eft_version),
        eft_version,
        own: !root.is_empty() && std::fs::canonicalize(&p).ok().zip(std::fs::canonicalize(root).ok()).is_some_and(|(a, r)| a.starts_with(&r) && a != r),
        switchable: generation == 4 && game::check_spt_folder(&p).is_ok(),
    }
}

/// The installs this launcher knows. The folder currently in use is added automatically the first time.
#[tauri::command]
pub async fn installs_list(app: AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    let (installs, root, active) = {
        let mut s = state.settings.lock().unwrap();
        let cur = s.game_path.clone();
        if !cur.is_empty() && game::check_spt_folder(Path::new(&cur)).is_ok() && !s.installs.iter().any(|i| same_path(&i.path, &cur)) {
            let v = spt_version_of(Path::new(&cur));
            let name = if v.is_empty() { "My SPT install".to_string() } else { format!("SPT {v}") };
            let id = unique_id(&s.installs, &name);
            s.installs.push(game::Install { id, name, path: cur.clone() });
            let _ = s.save(&app);
        }
        (s.installs.clone(), s.installs_root.clone(), s.game_path.clone())
    };
    let infos = tokio::task::spawn_blocking(move || installs.iter().map(|i| info_of(i, &active, &root)).collect::<Vec<_>>()).await.map_err(|e| e.to_string())?;
    let root = state.settings.lock().unwrap().installs_root.clone();
    Ok(json!({ "installs": infos, "root": root }))
}

/// Where new installs are created (default C:\SPT).
#[tauri::command]
pub fn installs_set_root(app: AppHandle, state: State<AppState>, path: String) -> Result<(), String> {
    let p = path.trim().trim_end_matches(['\\', '/']).to_string();
    let low = p.to_lowercase();
    if p.len() < 4 || !p.chars().nth(1).is_some_and(|c| c == ':') {
        return Err("Enter a full folder path such as C:\\SPT.".into());
    }
    if low.contains("steamapps") || low.contains("program files") || low.contains("\\downloads") || low.contains("\\desktop") || low.contains("\\windows") {
        return Err("Pick a plain folder such as C:\\SPT (not Steam, Program Files, Downloads, Desktop or Windows).".into());
    }
    std::fs::create_dir_all(&p).map_err(|e| format!("Could not create {p}: {e}"))?;
    let mut s = state.settings.lock().unwrap();
    s.installs_root = p;
    s.save(&app)
}

/// Makes another install the one the whole launcher works on (server, profiles, mods). The UI reloads afterwards.
#[tauri::command]
pub async fn install_switch(app: AppHandle, state: State<'_, AppState>, id: String) -> Result<(), String> {
    if spt::game_running() {
        return Err("Close the game first.".into());
    }
    if let Ok(g) = game::game_dir(&state) {
        if spt::Spt::new(&game::server_addr(&g)).ping().await {
            return Err(if state.server.owned_running() {
                "STOP_SERVER|The SPT server is running. Stop it before switching (both versions use port 6969).".to_string()
            } else {
                "An SPT server is running that this launcher did not start. Close it first (both versions use port 6969).".to_string()
            });
        }
    }
    let target = state.settings.lock().unwrap().installs.iter().find(|i| i.id == id).cloned().ok_or("That install is not in the list.")?;
    game::check_spt_folder(Path::new(&target.path)).map_err(|e| format!("{} cannot be used by this launcher: {e}", target.name))?;
    let auto = {
        let mut s = state.settings.lock().unwrap();
        s.game_path = target.path.clone();
        s.save(&app)?;
        s.auto_start_server
    };
    if auto {
        let _ = server::start(&app, &state).await;
    }
    Ok(())
}

fn find_install(state: &AppState, id: &str) -> Result<game::Install, String> {
    state.settings.lock().unwrap().installs.iter().find(|i| i.id == id).cloned().ok_or_else(|| "That install is not in the list.".to_string())
}

#[tauri::command]
pub fn install_open_folder(state: State<AppState>, id: String) -> Result<(), String> {
    let i = find_install(&state, &id)?;
    if !Path::new(&i.path).is_dir() {
        return Err("The folder no longer exists.".into());
    }
    std::process::Command::new("explorer").arg(&i.path).spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// Starts the install's own SPT launcher (for installs this launcher cannot operate itself, such as SPT 3.x).
#[tauri::command]
pub fn install_open_official(state: State<AppState>, id: String) -> Result<(), String> {
    let i = find_install(&state, &id)?;
    let base = PathBuf::from(&i.path);
    let exe = [base.join("SPT.Launcher.exe"), base.join("SPT_Runtime").join("SPT.Launcher.exe")].into_iter().find(|p| p.is_file()).ok_or("SPT.Launcher.exe was not found in that install.")?;
    let dir = exe.parent().unwrap_or(&base).to_path_buf();
    std::process::Command::new(&exe).current_dir(dir).spawn().map(|_| ()).map_err(|e| format!("Could not start it: {e}"))
}

/// Deletes an install this launcher created (inside the installs folder). Its saved profiles are copied to
/// <installs folder>\_removed first. Shared game files stay as long as another install still uses them.
#[tauri::command]
pub async fn install_remove(app: AppHandle, state: State<'_, AppState>, id: String) -> Result<String, String> {
    if state.job.lock().unwrap().is_some() {
        return Err("An install is running. Wait for it to finish.".into());
    }
    if spt::game_running() {
        return Err("Close the game first.".into());
    }
    let (inst, root, active) = {
        let s = state.settings.lock().unwrap();
        (s.installs.iter().find(|i| i.id == id).cloned().ok_or("That install is not in the list.")?, s.installs_root.clone(), s.game_path.clone())
    };
    if same_path(&inst.path, &active) {
        return Err("This is the active install. Switch to another one first.".into());
    }
    let p = PathBuf::from(&inst.path);
    let canon = std::fs::canonicalize(&p).map_err(|_| "The folder no longer exists; it was only removed from the list.".to_string());
    let (p2, root2) = match (canon, std::fs::canonicalize(&root)) {
        (Ok(a), Ok(r)) => (a, r),
        _ => {
            // already gone: just forget it
            let mut s = state.settings.lock().unwrap();
            s.installs.retain(|i| i.id != id);
            s.save(&app)?;
            return Ok(format!("{} was already deleted and is removed from the list.", inst.name));
        }
    };
    if !p2.starts_with(&root2) || p2 == root2 {
        return Err("Only installs created inside the installs folder can be removed here. Delete other folders yourself.".into());
    }
    let name = inst.name.clone();
    let res = tokio::task::spawn_blocking(move || -> Result<String, String> {
        let stamp = format!("{}-{}", slug(&inst.name), now_secs());
        let kept = [p2.join("SPT_Runtime").join("user").join("profiles"), p2.join("user").join("profiles")].into_iter().find(|d| d.is_dir());
        let mut note = String::new();
        if let Some(d) = kept {
            let dst = root2.join("_removed").join(&stamp).join("profiles");
            crate::mods::copy_dir(&d, &dst).map_err(|e| format!("Could not save the profiles first ({e}); nothing was deleted."))?;
            note = format!(" Its saved profiles are in {}.", dst.display());
        }
        std::fs::remove_dir_all(&p2).map_err(|e| format!("Could not delete the folder completely ({e}). Close anything using it and try again."))?;
        Ok(note)
    })
    .await
    .map_err(|e| e.to_string())??;
    let mut s = state.settings.lock().unwrap();
    s.installs.retain(|i| i.id != id);
    s.save(&app)?;
    Ok(format!("Removed {name}.{res}"))
}

// ---------- the list of SPT releases ----------

static RELEASES: OnceLock<Mutex<Option<(Instant, Value)>>> = OnceLock::new();

fn str_of<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// Releases that have a downloadable SPT archive, with the Tarkov client build each one needs.
pub fn parse_releases(v: &Value, repo: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for r in v.as_array().into_iter().flatten() {
        let Some(a) = r.get("assets").and_then(Value::as_array).into_iter().flatten().find(|a| str_of(a, "name").to_lowercase().ends_with(".7z") && str_of(a, "name").starts_with("SPT-")) else { continue };
        let body = str_of(r, "body");
        let requires = body.find("Requires EFT `").map(|i| body[i + 14..].split('`').next().unwrap_or("").to_string()).unwrap_or_default();
        let tag = str_of(r, "tag_name");
        out.push(json!({
            "tag": tag,
            "version": tag.trim_start_matches('v'),
            "published": str_of(r, "published_at").chars().take(10).collect::<String>(),
            "prerelease": r.get("prerelease").and_then(Value::as_bool).unwrap_or(false),
            "asset_name": str_of(a, "name"),
            "url": str_of(a, "browser_download_url"),
            "size": a.get("size").and_then(Value::as_u64).unwrap_or(0),
            "sha256": str_of(a, "digest").trim_start_matches("sha256:"),
            "requires": requires,
            "build": eft_build(&requires),
            "repo": repo,
        }));
    }
    out
}

#[tauri::command]
pub async fn versions_available() -> Result<Value, String> {
    let cell = RELEASES.get_or_init(|| Mutex::new(None));
    if let Some((at, v)) = cell.lock().unwrap().as_ref() {
        if at.elapsed() < Duration::from_secs(600) {
            return Ok(v.clone());
        }
    }
    let http = reqwest::Client::builder().user_agent(forge::UA).connect_timeout(Duration::from_secs(10)).timeout(Duration::from_secs(30)).build().map_err(|e| e.to_string())?;
    let mut all: Vec<Value> = Vec::new();
    let mut total = 0usize;
    for repo in RELEASE_REPOS {
        for page in 1..=3 {
            let r = http
                .get(format!("https://api.github.com/repos/{repo}/releases?per_page=100&page={page}"))
                .header("Accept", "application/vnd.github+json")
                .send()
                .await
                .map_err(|e| format!("Could not reach GitHub: {e}"))?;
            if r.status().as_u16() == 403 || r.status().as_u16() == 429 {
                return Err("GitHub's request limit was reached. Try again in a few minutes.".into());
            }
            if !r.status().is_success() {
                break;
            }
            let bytes = r.bytes().await.map_err(|e| format!("GitHub's answer broke off: {e}"))?;
            let v: Value = serde_json::from_slice(&bytes).map_err(|e| format!("GitHub sent something unexpected: {e}"))?;
            let n = v.as_array().map(Vec::len).unwrap_or(0);
            if n == 0 {
                break;
            }
            total += n;
            for rel in parse_releases(&v, repo) {
                if !all.iter().any(|x| x["tag"] == rel["tag"]) {
                    all.push(rel);
                }
            }
            if n < 100 {
                break;
            }
        }
    }
    all.sort_by(|a, b| str_of(b, "published").cmp(str_of(a, "published")));
    let out = json!({ "releases": all, "total_releases": total });
    *cell.lock().unwrap() = Some((Instant::now(), out.clone()));
    Ok(out)
}

// ---------- installer ----------

#[derive(Deserialize)]
pub struct InstallArgs {
    tag: String,
    url: String,
    asset_name: String,
    size: u64,
    sha256: String,
    build: String,
    name: String,
    source_id: Option<String>,
    source_path: Option<String>,
    share: bool,
}

#[derive(Serialize, Clone)]
struct Progress {
    phase: &'static str,
    done: u64,
    total: u64,
    note: String,
}

struct Reporter {
    app: AppHandle,
    last: Instant,
}

impl Reporter {
    fn new(app: &AppHandle) -> Reporter {
        Reporter { app: app.clone(), last: Instant::now() - Duration::from_secs(1) }
    }
    /// Throttled to a few updates a second; `force` always sends.
    fn send(&mut self, phase: &'static str, done: u64, total: u64, note: &str, force: bool) {
        if force || self.last.elapsed() >= Duration::from_millis(150) {
            self.last = Instant::now();
            let _ = self.app.emit("ver-progress", Progress { phase, done, total, note: note.to_string() });
        }
    }
}

struct JobGuard<'a>(&'a Mutex<Option<Arc<AtomicBool>>>);
impl Drop for JobGuard<'_> {
    fn drop(&mut self) {
        *self.0.lock().unwrap() = None;
    }
}

#[tauri::command]
pub fn install_cancel(state: State<AppState>) {
    if let Some(f) = state.job.lock().unwrap().as_ref() {
        f.store(true, Ordering::SeqCst);
    }
}

// ---------- downloaded archives ----------

fn downloads_dir(state: &AppState) -> PathBuf {
    PathBuf::from(&state.settings.lock().unwrap().installs_root).join("_downloads")
}

/// (files, bytes) of the regular files directly inside the downloads folder.
fn dir_files(dir: &Path) -> (usize, u64) {
    let mut n = (0usize, 0u64);
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                n.0 += 1;
                n.1 += e.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    n
}

/// Deletes the regular files directly inside `dir` (never folders, never links). Returns (files, bytes) removed.
fn clear_dir_files(dir: &Path) -> Result<(usize, u64), String> {
    let mut n = (0usize, 0u64);
    let Ok(rd) = std::fs::read_dir(dir) else { return Ok(n) };
    for e in rd.flatten() {
        if e.file_type().map(|t| t.is_file()).unwrap_or(false) {
            let len = e.metadata().map(|m| m.len()).unwrap_or(0);
            std::fs::remove_file(e.path()).map_err(|x| format!("Could not delete {}: {x}", e.path().display()))?;
            n.0 += 1;
            n.1 += len;
        }
    }
    Ok(n)
}

#[tauri::command]
pub fn downloads_info(state: State<AppState>) -> Value {
    let dir = downloads_dir(&state);
    let (count, bytes) = dir_files(&dir);
    json!({ "count": count, "bytes": bytes, "path": dir.display().to_string(), "keep": state.settings.lock().unwrap().keep_downloads })
}

#[tauri::command]
pub fn downloads_clear(state: State<AppState>) -> Result<Value, String> {
    if state.job.lock().unwrap().is_some() {
        return Err("An install is running: its download is in use. Try again when it has finished.".into());
    }
    let (count, bytes) = clear_dir_files(&downloads_dir(&state))?;
    Ok(json!({ "count": count, "bytes": bytes }))
}

#[tauri::command]
pub fn downloads_set_keep(app: AppHandle, state: State<AppState>, keep: bool) -> Result<(), String> {
    let mut s = state.settings.lock().unwrap();
    s.keep_downloads = keep;
    s.save(&app)
}

fn folder_name_ok(n: &str) -> Result<String, String> {
    let n = n.trim().trim_end_matches('.').to_string();
    if n.is_empty() || n.chars().count() > 60 || !n.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '-' | '_')) || n.starts_with('_') || n.starts_with('.') {
        return Err("The folder name may contain letters, numbers, spaces, dots, dashes and underscores (1 to 60 characters, not starting with _ or a dot).".into());
    }
    Ok(n)
}

fn free_space(p: &Path) -> Option<u64> {
    let mut c = p.to_path_buf();
    while !c.exists() {
        if !c.pop() {
            return None;
        }
    }
    fs2::available_space(&c).ok()
}

fn same_drive(a: &Path, b: &Path) -> bool {
    let d = |p: &Path| p.display().to_string().chars().next().map(|c| c.to_ascii_lowercase());
    d(a) == d(b)
}

async fn download(app: &AppHandle, url: &str, dest: &Path, sha: &str, size: u64, cancel: &AtomicBool) -> Result<(), String> {
    let lower = url.to_lowercase();
    if !DOWNLOAD_PREFIXES.iter().any(|p| lower.starts_with(p)) {
        return Err("Only the official SPT release downloads on GitHub are allowed.".into());
    }
    if let Some(d) = dest.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let check = |p: &Path| -> bool {
        let Ok(mut f) = std::fs::File::open(p) else { return false };
        let mut h = Sha256::new();
        std::io::copy(&mut f, &mut h).is_ok() && (sha.is_empty() || format!("{:x}", h.finalize()).eq_ignore_ascii_case(sha))
    };
    let mut rep = Reporter::new(app);
    if dest.is_file() && (size == 0 || std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0) == size) && !sha.is_empty() && check(dest) {
        rep.send("download", size, size, "Already downloaded and verified", true);
        return Ok(());
    }
    let http = reqwest::Client::builder().user_agent(forge::UA).connect_timeout(Duration::from_secs(15)).timeout(Duration::from_secs(3600)).build().map_err(|e| e.to_string())?;
    let mut r = http.get(url).send().await.map_err(|e| format!("Download failed: {e}"))?.error_for_status().map_err(|e| format!("Download failed: {e}"))?;
    let total = r.content_length().unwrap_or(size);
    let part = PathBuf::from(format!("{}.part", dest.display()));
    let mut f = std::fs::File::create(&part).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let (mut done, started) = (0u64, Instant::now());
    let res: Result<(), String> = async {
        while let Some(chunk) = r.chunk().await.map_err(|e| format!("Download broke off: {e}"))? {
            if cancel.load(Ordering::SeqCst) {
                return Err("cancelled".into());
            }
            f.write_all(&chunk).map_err(|e| e.to_string())?;
            hasher.update(&chunk);
            done += chunk.len() as u64;
            let speed = done as f64 / started.elapsed().as_secs_f64().max(0.1) / 1048576.0;
            rep.send("download", done, total, &format!("{speed:.1} MB/s"), false);
        }
        Ok(())
    }
    .await;
    drop(f);
    if let Err(e) = res {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    if !sha.is_empty() && !format!("{:x}", hasher.finalize()).eq_ignore_ascii_case(sha) {
        let _ = std::fs::remove_file(&part);
        return Err("The downloaded file does not match the checksum GitHub published for it, so it was discarded.".into());
    }
    std::fs::rename(&part, dest).map_err(|e| e.to_string())?;
    rep.send("download", total, total, "Verified", true);
    Ok(())
}

fn walk(base: &Path, rel: &Path, out: &mut Vec<(PathBuf, u64)>) {
    let Ok(rd) = std::fs::read_dir(base.join(rel)) else { return };
    for e in rd.flatten() {
        let r = rel.join(e.file_name());
        match e.file_type() {
            Ok(t) if t.is_dir() => walk(base, &r, out),
            Ok(t) if t.is_file() => out.push((r, e.metadata().map(|m| m.len()).unwrap_or(0))),
            _ => {}
        }
    }
}

fn client_files(src: &Path) -> Vec<(PathBuf, u64)> {
    let mut v = Vec::new();
    for item in CLIENT_ITEMS {
        let p = src.join(item);
        if p.is_dir() {
            walk(src, Path::new(item), &mut v);
        } else if p.is_file() {
            v.push((PathBuf::from(item), std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0)));
        }
    }
    v
}

/// Big asset files under EscapeFromTarkov_Data (outside Managed and Plugins, where mods and patchers write) can be shared.
fn shareable(rel: &Path, size: u64) -> bool {
    let r = rel.display().to_string().replace('/', "\\").to_lowercase();
    size >= SHARE_MIN && r.starts_with("escapefromtarkov_data\\") && !r.starts_with("escapefromtarkov_data\\managed\\") && !r.starts_with("escapefromtarkov_data\\plugins\\")
}

pub struct CloneStats {
    pub files: usize,
    pub linked: usize,
    pub copied: usize,
    pub linked_bytes: u64,
    pub copied_bytes: u64,
}

/// Copies the Tarkov client from `src` into `dst`; big asset files are hardlinked when `share` is true.
pub fn clone_client(src: &Path, dst: &Path, share: bool, cancel: &AtomicBool, mut progress: impl FnMut(u64, u64)) -> Result<CloneStats, String> {
    let files = client_files(src);
    if !files.iter().any(|(r, _)| r.starts_with("EscapeFromTarkov_Data")) || !src.join("EscapeFromTarkov.exe").is_file() {
        return Err("Those game files are incomplete (EscapeFromTarkov.exe or EscapeFromTarkov_Data is missing).".into());
    }
    let mut st = CloneStats { files: files.len(), linked: 0, copied: 0, linked_bytes: 0, copied_bytes: 0 };
    let total = files.len() as u64;
    let mut can_link = share;
    for (i, (rel, size)) in files.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            return Err("cancelled".into());
        }
        let (from, to) = (src.join(rel), dst.join(rel));
        if let Some(d) = to.parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("Could not create {}: {e}", d.display()))?;
        }
        if can_link && shareable(rel, *size) {
            if std::fs::hard_link(&from, &to).is_ok() {
                st.linked += 1;
                st.linked_bytes += size;
                if i % 20 == 0 {
                    progress(i as u64, total);
                }
                continue;
            }
            can_link = false; // not possible here (another drive, or not NTFS): copy everything from now on
        }
        std::fs::copy(&from, &to).map_err(|e| format!("Could not copy {}: {e}", rel.display()))?;
        st.copied += 1;
        st.copied_bytes += size;
        if i % 20 == 0 || *size > SHARE_MIN {
            progress(i as u64, total);
        }
    }
    progress(total, total);
    Ok(st)
}

/// Joins an archive path onto `dst`, refusing anything that could leave it.
pub fn safe_join(dst: &Path, name: &str) -> Result<PathBuf, String> {
    let n = name.replace('\\', "/");
    let parts: Vec<&str> = n.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    if parts.is_empty() || parts.iter().any(|p| *p == ".." || p.contains(':') || p.contains(['<', '>', '"', '|', '?', '*'])) {
        return Err(format!("unsafe path in the archive: {name}"));
    }
    let mut p = dst.to_path_buf();
    for s in parts {
        p.push(s);
    }
    Ok(p)
}

/// Extracts a .7z into `dst`. `progress(bytes_done, bytes_total)`.
pub fn extract_7z(archive: &Path, dst: &Path, cancel: &AtomicBool, mut progress: impl FnMut(u64, u64)) -> Result<usize, String> {
    let mut r = sevenz_rust::SevenZReader::open(archive, sevenz_rust::Password::empty()).map_err(|e| format!("The archive is broken: {e}"))?;
    let total: u64 = r.archive().files.iter().filter(|f| !f.is_directory()).map(|f| f.size()).sum();
    // refuse the whole archive before writing anything if one path is unsafe
    for f in &r.archive().files {
        safe_join(dst, f.name())?;
    }
    let (mut done, mut files) = (0u64, 0usize);
    let mut err: Option<String> = None;
    r.for_each_entries(|entry, reader| {
        if cancel.load(Ordering::SeqCst) {
            err = Some("cancelled".into());
            return Ok(false);
        }
        let target = match safe_join(dst, entry.name()) {
            Ok(t) => t,
            Err(e) => {
                err = Some(e);
                return Ok(false);
            }
        };
        if entry.is_directory() {
            let _ = std::fs::create_dir_all(&target);
            return Ok(true);
        }
        if let Some(d) = target.parent() {
            if let Err(e) = std::fs::create_dir_all(d) {
                err = Some(e.to_string());
                return Ok(false);
            }
        }
        match std::fs::File::create(&target).and_then(|mut out| std::io::copy(reader, &mut out)) {
            Ok(n) => {
                done += n;
                files += 1;
                progress(done, total);
                Ok(true)
            }
            Err(e) => {
                err = Some(format!("Could not write {}: {e}", target.display()));
                Ok(false)
            }
        }
    })
    .map_err(|e| format!("Extracting failed: {e}"))?;
    match err {
        Some(e) => Err(e),
        None => Ok(files),
    }
}

/// Downloads an SPT release, clones a matching Tarkov client and extracts the release into a new folder under the installs folder.
#[tauri::command]
pub async fn install_version(app: AppHandle, state: State<'_, AppState>, args: InstallArgs) -> Result<String, String> {
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut j = state.job.lock().unwrap();
        if j.is_some() {
            return Err("Another install is already running.".into());
        }
        *j = Some(cancel.clone());
    }
    let _guard = JobGuard(&state.job);
    let mut rep = Reporter::new(&app);

    let name = folder_name_ok(&args.name)?;
    let (root, donor) = {
        let s = state.settings.lock().unwrap();
        let donor = match (&args.source_id, &args.source_path) {
            (Some(id), _) => s.installs.iter().find(|i| &i.id == id).map(|i| PathBuf::from(&i.path)).ok_or("The chosen game-files source is not in the list.")?,
            (None, Some(p)) if !p.trim().is_empty() => PathBuf::from(p.trim()),
            _ => return Err("Choose where the game files come from.".into()),
        };
        (PathBuf::from(&s.installs_root), donor)
    };
    if args.build.is_empty() {
        return Err("This release does not state which Tarkov client it needs, so it cannot be installed safely.".into());
    }
    let donor_ver = product_version(&donor.join("EscapeFromTarkov.exe")).ok_or("EscapeFromTarkov.exe was not found in the chosen game files.")?;
    if eft_build(&donor_ver) != args.build {
        return Err(format!("SPT {} needs Tarkov client build {}, but the chosen game files are build {} ({donor_ver}).", args.tag, args.build, eft_build(&donor_ver)));
    }
    let target = root.join(&name);
    if target.exists() && std::fs::read_dir(&target).map(|mut r| r.next().is_some()).unwrap_or(true) {
        return Err(format!("{} already exists and is not empty. Choose another name.", target.display()));
    }
    // hardlinks only work on one drive, and never into a Steam folder (Steam updates files in place)
    let mut share = args.share && same_drive(&donor, &root) && !donor.display().to_string().to_lowercase().contains("steamapps");
    let client_size: u64 = client_files(&donor).iter().map(|(_, s)| *s).sum();
    let need = if share { 4u64 << 30 } else { client_size + (4u64 << 30) };
    if let Some(free) = free_space(&root) {
        if free < need {
            if share {
                share = false;
            }
            let need = client_size + (4u64 << 30);
            if free < need {
                return Err(format!("Not enough free space on that drive: {:.1} GB needed, {:.1} GB free.", need as f64 / 1073741824.0, free as f64 / 1073741824.0));
            }
        }
    }

    let archive = root.join("_downloads").join(&args.asset_name);
    download(&app, &args.url, &archive, &args.sha256, args.size, &cancel).await?;

    // only a folder this install created (it did not exist, or was empty) is ever cleaned up after a failure or cancel
    let cleanup = |t: &Path| {
        if t.starts_with(&root) && t != root {
            let _ = std::fs::remove_dir_all(t);
        }
    };
    std::fs::create_dir_all(&target).map_err(|e| format!("Could not create {}: {e}", target.display()))?;

    rep.send("clone", 0, 1, if share { "Sharing the game files" } else { "Copying the game files" }, true);
    let (d2, t2, c2, app2) = (donor.clone(), target.clone(), cancel.clone(), app.clone());
    let stats = tokio::task::spawn_blocking(move || {
        let mut r = Reporter::new(&app2);
        clone_client(&d2, &t2, share, &c2, |d, t| r.send("clone", d, t, "", false))
    })
    .await
    .map_err(|e| e.to_string())?;
    let stats = match stats {
        Ok(s) => s,
        Err(e) => {
            cleanup(&target);
            return Err(if e == "cancelled" { "Cancelled. The partly created folder was removed.".into() } else { e });
        }
    };

    rep.send("extract", 0, 1, "Unpacking SPT", true);
    let (a2, t3, c3, app3) = (archive.clone(), target.clone(), cancel.clone(), app.clone());
    let ex = tokio::task::spawn_blocking(move || {
        let mut r = Reporter::new(&app3);
        extract_7z(&a2, &t3, &c3, |d, t| r.send("extract", d, t, "", false))
    })
    .await
    .map_err(|e| e.to_string())?;
    let files = match ex {
        Ok(n) => n,
        Err(e) => {
            cleanup(&target);
            return Err(if e == "cancelled" { "Cancelled. The partly created folder was removed.".into() } else { e });
        }
    };

    let gen = generation(&target);
    let found = spt_version_of(&target);
    {
        let mut s = state.settings.lock().unwrap();
        let id = unique_id(&s.installs, &name);
        s.installs.push(game::Install { id, name: name.clone(), path: target.display().to_string() });
        s.save(&app)?;
    }
    rep.send("finish", 1, 1, "Done", true);
    // the archive is only a cache: after a successful install it is deleted (unless the user chose to keep it)
    let keep = state.settings.lock().unwrap().keep_downloads;
    let freed = if keep { 0 } else { std::fs::metadata(&archive).map(|m| m.len()).unwrap_or(0) };
    let deleted = !keep && std::fs::remove_file(&archive).is_ok();
    let mut msg = format!(
        "Installed SPT {} in {}: {} SPT files unpacked, {} game files shared by hardlink ({:.1} GB) and {} copied ({:.1} GB).",
        if found.is_empty() { args.tag.clone() } else { found.clone() },
        target.display(),
        files,
        stats.linked,
        stats.linked_bytes as f64 / 1073741824.0,
        stats.copied,
        stats.copied_bytes as f64 / 1073741824.0
    );
    if gen == 3 {
        msg.push_str(" This is an older SPT generation: this launcher cannot start it, use its own launcher (button on its card).");
    }
    if stats.files == 0 {
        msg.push_str(" No game files were found to copy.");
    }
    if deleted {
        msg.push_str(&format!(" The downloaded archive ({:.0} MB) was deleted.", freed as f64 / 1048576.0));
    }
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_numbers_identify_clients() {
        assert_eq!(eft_build("0.16.9.5-40743-f137e819"), "40743");
        assert_eq!(eft_build("0.16.9.5.40743"), "40743");
        assert_eq!(eft_build("0.16.1-35392"), "35392");
        assert_eq!(eft_build("0.16.9"), "");
        assert_eq!(eft_build(""), "");
    }

    #[test]
    fn reads_product_version_from_a_pe_string_table() {
        let mut b = vec![0u8; 40];
        b.extend("ProductVersion".encode_utf16().flat_map(|c| c.to_le_bytes()));
        b.extend([0, 0, 0, 0]); // terminator + padding
        b.extend("0.16.9.5-40743-f137e819".encode_utf16().flat_map(|c| c.to_le_bytes()));
        b.extend([0, 0, 9, 9]);
        let dir = std::env::temp_dir().join("outpost-test-pv");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("x.exe");
        std::fs::write(&f, b).unwrap();
        assert_eq!(product_version(&f).as_deref(), Some("0.16.9.5-40743-f137e819"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn archive_paths_cannot_escape() {
        let d = Path::new("C:\\SPT\\x");
        assert_eq!(safe_join(d, "SPT_Runtime/SPT.Server.exe").unwrap(), PathBuf::from("C:\\SPT\\x\\SPT_Runtime\\SPT.Server.exe"));
        assert!(safe_join(d, "../evil.dll").is_err());
        assert!(safe_join(d, "a/../../evil.dll").is_err());
        assert!(safe_join(d, "C:/Windows/x.dll").is_err());
        assert!(safe_join(d, "").is_err());
    }

    #[test]
    fn clearing_downloads_removes_files_only() {
        let dir = std::env::temp_dir().join("outpost-test-downloads");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("keepme")).unwrap();
        std::fs::write(dir.join("keepme").join("inner.txt"), b"x").unwrap();
        std::fs::write(dir.join("SPT-4.1.2-40743-cf04a11.7z"), vec![1u8; 3000]).unwrap();
        std::fs::write(dir.join("SPT-4.1.5-x.7z.part"), vec![1u8; 500]).unwrap();
        assert_eq!(dir_files(&dir), (2, 3500));
        assert_eq!(clear_dir_files(&dir).unwrap(), (2, 3500));
        assert_eq!(dir_files(&dir), (0, 0));
        assert!(dir.join("keepme").join("inner.txt").is_file(), "folders are never touched");
        // a missing folder is simply empty
        assert_eq!(clear_dir_files(&dir.join("nope")).unwrap(), (0, 0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn folder_names_and_slugs() {
        assert!(folder_name_ok("4.1.5").is_ok());
        assert!(folder_name_ok("SPT 4.1.5 test").is_ok());
        assert!(folder_name_ok("..").is_err());
        assert!(folder_name_ok("a\\b").is_err());
        assert!(folder_name_ok("_downloads").is_err());
        assert!(folder_name_ok("").is_err());
        assert_eq!(slug("SPT 4.1.6"), "spt-4-1-6");
        let existing = vec![game::Install { id: "spt-4-1-6".into(), name: "x".into(), path: "p".into() }];
        assert_eq!(unique_id(&existing, "SPT 4.1.6"), "spt-4-1-6-2");
        assert!(same_path("C:\\SPT\\a\\", "c:/spt/A"));
        assert!(!same_path("", ""));
    }

    #[test]
    fn parses_releases_with_archives_and_required_client() {
        let v = json!([
            { "tag_name": "3.11.4", "published_at": "2025-09-01T17:49:34Z", "prerelease": false, "body": "#### Requires EFT `0.16.1-35392` (released 5th March 2025)\nmore",
              "assets": [{ "name": "SPT-3.11.4-35392-96e5b73.7z", "size": 108200000u64, "digest": "sha256:b06c523b", "browser_download_url": "https://github.com/sp-tarkov/build/releases/download/3.11.4/SPT-3.11.4-35392-96e5b73.7z" }] },
            { "tag_name": "4.0.1", "published_at": "2025-10-17T00:00:00Z", "body": "Requires EFT `0.16.9`", "assets": [] }
        ]);
        let r = parse_releases(&v, "sp-tarkov/build");
        assert_eq!(r.len(), 1, "a release without an archive is not listed");
        assert_eq!(r[0]["version"], "3.11.4");
        assert_eq!(r[0]["build"], "35392");
        assert_eq!(r[0]["sha256"], "b06c523b");
        assert_eq!(r[0]["published"], "2025-09-01");
    }

    #[test]
    fn clone_shares_big_asset_files_and_copies_the_rest() {
        let base = std::env::temp_dir().join("outpost-test-clone");
        let _ = std::fs::remove_dir_all(&base);
        let (src, dst) = (base.join("src"), base.join("dst"));
        let data = src.join("EscapeFromTarkov_Data");
        std::fs::create_dir_all(data.join("Managed")).unwrap();
        std::fs::create_dir_all(data.join("StreamingAssets")).unwrap();
        std::fs::write(src.join("EscapeFromTarkov.exe"), b"exe").unwrap();
        std::fs::write(src.join("UnityPlayer.dll"), vec![1u8; 2 << 20]).unwrap();
        std::fs::write(data.join("StreamingAssets").join("big.bundle"), vec![2u8; 2 << 20]).unwrap();
        std::fs::write(data.join("Managed").join("Assembly-CSharp.dll"), vec![3u8; 2 << 20]).unwrap();
        std::fs::write(data.join("globalgamemanagers"), b"small").unwrap();
        std::fs::write(src.join("BepInEx.txt"), b"not part of the client").unwrap();
        let cancel = AtomicBool::new(false);
        let st = clone_client(&src, &dst, true, &cancel, |_, _| {}).unwrap();
        assert_eq!((st.linked, st.copied), (1, 4), "only the big asset is linked");
        assert!(!dst.join("BepInEx.txt").exists(), "SPT/mod files are not part of the client");
        // linked = same file: a change through the source shows in the clone; copies stay independent
        std::fs::write(src.join("EscapeFromTarkov_Data").join("StreamingAssets").join("big.bundle"), vec![9u8; 2 << 20]).unwrap();
        assert_eq!(std::fs::read(dst.join("EscapeFromTarkov_Data").join("StreamingAssets").join("big.bundle")).unwrap()[0], 9);
        std::fs::write(src.join("EscapeFromTarkov_Data").join("Managed").join("Assembly-CSharp.dll"), vec![8u8; 2 << 20]).unwrap();
        assert_eq!(std::fs::read(dst.join("EscapeFromTarkov_Data").join("Managed").join("Assembly-CSharp.dll")).unwrap()[0], 3);
        // full copy mode links nothing
        let dst2 = base.join("dst2");
        let st2 = clone_client(&src, &dst2, false, &cancel, |_, _| {}).unwrap();
        assert_eq!((st2.linked, st2.copied), (0, 5));
        // incomplete game files are refused, and a cancel stops the copy
        assert!(clone_client(&base.join("nothing"), &base.join("d3"), true, &cancel, |_, _| {}).is_err());
        cancel.store(true, Ordering::SeqCst);
        assert_eq!(clone_client(&src, &base.join("d4"), true, &cancel, |_, _| {}).err().as_deref(), Some("cancelled"));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Read-only look at a real install (changes nothing): what a clone would share and what it would copy.
    /// cargo test --release -- --ignored --nocapture plan_real_clone
    #[test]
    #[ignore]
    fn plan_real_clone() {
        let src = Path::new(r"C:\Single Player Tarkov");
        let pv = product_version(&src.join("EscapeFromTarkov.exe")).unwrap_or_default();
        println!("client: {pv}  build {}  generation {}  spt {}", eft_build(&pv), generation(src), spt_version_of(src));
        let files = client_files(src);
        let (mut l, mut lb, mut c, mut cb) = (0usize, 0u64, 0usize, 0u64);
        for (rel, size) in &files {
            if shareable(rel, *size) { l += 1; lb += size } else { c += 1; cb += size }
        }
        println!("client files: {}  total {:.2} GB", files.len(), (lb + cb) as f64 / 1073741824.0);
        println!("would be hardlinked (shared): {l} files, {:.2} GB", lb as f64 / 1073741824.0);
        println!("would be copied:              {c} files, {:.2} GB", cb as f64 / 1073741824.0);
        let big_copied: Vec<_> = files.iter().filter(|(r, s)| !shareable(r, *s) && *s > (50 << 20)).collect();
        println!("copied files over 50 MB: {}", big_copied.len());
        for (r, s) in big_copied.iter().take(8) {
            println!("   {:>8.1} MB  {}", *s as f64 / 1048576.0, r.display());
        }
    }

    /// Real archive: cargo test --release -- --ignored --nocapture extract_real SPT_ARCHIVE=<path to a .7z>
    #[test]
    #[ignore]
    fn extract_real_archive() {
        let archive = std::env::var("SPT_ARCHIVE").expect("set SPT_ARCHIVE to an SPT .7z");
        let dst = std::env::temp_dir().join("outpost-test-extract");
        let _ = std::fs::remove_dir_all(&dst);
        let cancel = AtomicBool::new(false);
        let mut last = (0, 0);
        let n = extract_7z(Path::new(&archive), &dst, &cancel, |d, t| last = (d, t)).unwrap();
        println!("extracted {n} files, {} of {} bytes", last.0, last.1);
        println!("generation: {}, spt version: {}", generation(&dst), spt_version_of(&dst));
        assert!(n > 100 && last.0 == last.1);
        let _ = std::fs::remove_dir_all(&dst);
    }
}
