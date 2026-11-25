//! SPT 4.1.6 launcher protocol, mirrored from SPT.Launcher (SPTarkov.Core: SessionHelper, GameHelper, BundleHelper, HttpHelper).
//! Requests: HTTPS to the local server (self-signed cert), PUT bodies zlib-compressed JSON, every answer zlib-compressed JSON.

use crate::{game, AppState};
use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::io::AsyncWriteExt;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// .NET DateTime ticks at 1970-01-01.
const UNIX_EPOCH_TICKS: i64 = 621_355_968_000_000_000;
/// Files and folders SPT.Launcher deletes from the SPT game folder before every start (GameHelper.SetupGameFiles).
const CLEANUP: [&str; 7] =
    ["BattlEye", "Logs", "ConsistencyInfo", "EscapeFromTarkov_BE.exe", "Uninstall.exe", "UnityCrashHandler64.exe", "WinPixEventRuntime.dll"];

pub struct Spt {
    http: reqwest::Client,
    base: String,
}

#[derive(Deserialize)]
struct Resp<T> {
    #[serde(rename = "Response", alias = "response")]
    response: T,
}

impl Spt {
    pub fn new(addr: &str) -> Spt {
        let http = reqwest::Client::builder()
            .danger_accept_invalid_certs(true) // the SPT server uses a self-signed certificate, as SPT.Launcher accepts
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(15))
            .build()
            .expect("http client");
        Spt { http, base: format!("https://{addr}") }
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, String> {
        let r = self.http.get(format!("{}{}", self.base, path)).send().await.map_err(net)?;
        parse(&r.error_for_status().map_err(net)?.bytes().await.map_err(net)?)
    }

    async fn put<T: DeserializeOwned>(&self, path: &str, body: &Value) -> Result<T, String> {
        let mut z = ZlibEncoder::new(Vec::new(), Compression::best());
        z.write_all(body.to_string().as_bytes()).map_err(|e| e.to_string())?;
        let data = z.finish().map_err(|e| e.to_string())?;
        let r = self.http.put(format!("{}{}", self.base, path)).body(data).send().await.map_err(net)?;
        parse(&r.error_for_status().map_err(net)?.bytes().await.map_err(net)?)
    }

    pub async fn get_value(&self, path: &str) -> Result<Value, String> {
        let r: Resp<Value> = self.get(path).await?;
        Ok(r.response)
    }

    pub async fn ping(&self) -> bool {
        matches!(self.get::<Resp<String>>("/launcher/v2/ping").await, Ok(r) if r.response == "Pong!")
    }
}

fn net(e: reqwest::Error) -> String {
    format!("Server request failed: {e}")
}

fn parse<T: DeserializeOwned>(b: &[u8]) -> Result<T, String> {
    let mut out = Vec::new();
    let data: &[u8] = if ZlibDecoder::new(b).read_to_end(&mut out).is_ok() { &out } else { b };
    serde_json::from_slice(data).map_err(|e| format!("Unexpected answer from the server: {e}"))
}

fn server_for(state: &AppState) -> Result<(PathBuf, String, Spt), String> {
    let game = game::game_dir(state)?;
    let addr = game::server_addr(&game);
    let spt = Spt::new(&addr);
    Ok((game, addr, spt))
}

#[tauri::command]
pub async fn list_profiles(state: State<'_, AppState>) -> Result<Value, String> {
    let (_, _, spt) = server_for(&state)?;
    let r: Resp<Value> = spt.get("/launcher/v2/profiles").await?;
    Ok(r.response)
}

/// The same sequence as SPT.Launcher: ping, login, version check, file cleanup, patches, bundles, start.
#[tauri::command]
pub async fn launch_game(app: AppHandle, state: State<'_, AppState>, username: String) -> Result<Vec<String>, String> {
    let (game, addr, spt) = server_for(&state)?;
    let step = |t: &str| {
        let _ = app.emit("launch-step", t.to_string());
    };
    if game_running() {
        return Err("The game is already running.".into());
    }

    step("Connecting to the SPT server");
    if !spt.ping().await {
        return Err(format!("The SPT server at {addr} is not running. Start it on the Server page."));
    }

    step("Logging in");
    let body = json!({ "username": username });
    let ok: Resp<bool> = spt.put("/launcher/v2/login", &body).await?;
    if !ok.response {
        return Err(format!("The server has no profile named \"{username}\"."));
    }
    let profile: Resp<Value> = spt.put("/launcher/v2/profile", &body).await?;
    let pid = profile.response.get("profileId").and_then(Value::as_str).unwrap_or_default().to_string();
    if pid.is_empty() || !pid.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("The server returned no valid profile id.".into());
    }

    step("Checking versions");
    let mut warnings = Vec::new();
    if let Some(w) = version_warning(&spt, &game).await {
        warnings.push(w);
    }

    step("Preparing game files");
    cleanup(&game);
    check_patches(&game)?;

    step("Checking mod bundles");
    let downloaded = sync_bundles(&app, &spt, &game).await?;
    if downloaded > 0 {
        warnings.push(format!("Downloaded {downloaded} missing mod bundle(s)."));
    }

    step("Starting the game");
    start_game(&game, &addr, &pid)?;

    let minimize = {
        let mut s = state.settings.lock().unwrap();
        s.last_profile = Some(username.clone());
        let _ = s.save(&app);
        s.minimize_on_launch
    };
    if minimize {
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.minimize();
        }
    }
    Ok(warnings)
}

pub fn game_running() -> bool {
    let mut c = std::process::Command::new("tasklist");
    c.args(["/FI", "IMAGENAME eq EscapeFromTarkov.exe", "/NH"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c.output().map(|o| String::from_utf8_lossy(&o.stdout).contains("EscapeFromTarkov.exe")).unwrap_or(false)
}

/// SPT.Launcher only logs a mismatch and continues; so do we, but the user sees it.
async fn version_warning(spt: &Spt, game: &Path) -> Option<String> {
    let v: Resp<String> = spt.get("/launcher/v2/version").await.ok()?;
    let server = v.response.split('-').next()?.trim().to_string();
    let (a, b, c) = file_version(&game.join("BepInEx").join("plugins").join("spt").join("spt-core.dll"))?;
    let client = format!("{a}.{b}.{c}");
    (client != server).then(|| format!("Version mismatch: SPT server {server}, client plugin spt-core.dll {client}."))
}

/// Major.minor.build from a PE file's VS_FIXEDFILEINFO.
pub fn file_version(p: &Path) -> Option<(u32, u32, u32)> {
    let b = std::fs::read(p).ok()?;
    let i = b.windows(4).position(|w| w == [0xBD, 0x04, 0xEF, 0xFE])?;
    let rd = |o: usize| -> Option<u32> { Some(u32::from_le_bytes(b.get(i + o..i + o + 4)?.try_into().ok()?)) };
    let (ms, ls) = (rd(8)?, rd(12)?);
    Some((ms >> 16, ms & 0xFFFF, ls >> 16))
}

fn cleanup(game: &Path) {
    let settings = game::read_json(&game.join("SPT_Runtime").join("user").join("Launcher").join("LauncherSettings.json"));
    let excluded: Vec<String> = settings
        .as_ref()
        .and_then(|v| v.get("ExcludeFromCleanup"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let mut targets: Vec<PathBuf> = CLEANUP.iter().filter(|n| !excluded.iter().any(|e| e == *n)).map(|n| game.join(n)).collect();
    targets.push(game.join("EscapeFromTarkov_Data").join("Plugins").join("x86_64").join("hwecho.dll"));
    for t in targets {
        force_remove(&t);
    }
}

fn force_remove(p: &Path) {
    let Ok(md) = std::fs::symlink_metadata(p) else { return };
    if md.is_dir() {
        if let Ok(rd) = std::fs::read_dir(p) {
            for e in rd.flatten() {
                force_remove(&e.path());
            }
        }
        let _ = std::fs::remove_dir(p);
    } else {
        let mut perm = md.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        let _ = std::fs::set_permissions(p, perm);
        let _ = std::fs::remove_file(p);
    }
}

/// SPT.Launcher re-applies binary patches from SPT_Data/Launcher/Patches on every start (empty in 4.1.6).
/// We do not apply them; if any are shipped and not yet applied (no .spt-bak), the official launcher must run once.
fn check_patches(game: &Path) -> Result<(), String> {
    let dir = game.join("SPT_Runtime").join("SPT_Data").join("Launcher").join("Patches");
    let Ok(patches) = std::fs::read_dir(&dir) else { return Ok(()) };
    for patch in patches.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
        for delta in files_with_ext(&patch, "delta") {
            let rel = delta.strip_prefix(&patch).unwrap_or(&delta).with_extension("");
            let backup = PathBuf::from(format!("{}.spt-bak", game.join(rel).display()));
            if !backup.is_file() {
                return Err("This SPT version ships game patches that are not applied yet. Start once with the official SPT launcher, then use this one.".into());
            }
        }
    }
    Ok(())
}

fn files_with_ext(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for p in rd.flatten().map(|e| e.path()) {
            if p.is_dir() {
                out.extend(files_with_ext(&p, ext));
            } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext)) {
                out.push(p);
            }
        }
    }
    out
}

#[derive(Deserialize)]
struct Bundle {
    #[serde(rename = "FileName", alias = "fileName")]
    file_name: String,
    #[serde(rename = "ModPath", alias = "modPath", default)]
    mod_path: String,
    #[serde(rename = "Crc", alias = "crc")]
    crc: u32,
    #[serde(rename = "Size", alias = "size")]
    size: u64,
    #[serde(rename = "ModifiedUtcTicks", alias = "modifiedUtcTicks", default)]
    ticks: i64,
}

async fn sync_bundles(app: &AppHandle, spt: &Spt, game: &Path) -> Result<usize, String> {
    let list: Vec<Bundle> = spt.get("/singleplayer/bundles").await?;
    let runtime = game.join("SPT_Runtime");
    let cache = runtime.join("user").join("cache").join("bundles");
    let missing: Vec<&Bundle> = list.iter().filter(|b| !bundle_available(b, &cache, &runtime)).collect();
    for (i, b) in missing.iter().enumerate() {
        let _ = app.emit("launch-step", format!("Downloading bundle {}/{}: {}", i + 1, missing.len(), b.file_name));
        let dest = cache.join(format!("{:08X}", b.crc)).join(&b.file_name);
        download(spt, &format!("/files/bundle/{}", b.file_name), &dest).await.map_err(|e| format!("Bundle {} failed: {e}", b.file_name))?;
    }
    Ok(missing.len())
}

fn bundle_available(b: &Bundle, cache: &Path, runtime: &Path) -> bool {
    let cached = cache.join(format!("{:08X}", b.crc)).join(&b.file_name);
    if std::fs::metadata(&cached).is_ok_and(|m| m.len() == b.size) {
        return true;
    }
    let modfile = runtime.join(&b.mod_path).join("bundles").join(&b.file_name);
    let Ok(md) = std::fs::metadata(&modfile) else { return false };
    if md.len() != b.size {
        return false;
    }
    if let Some(ticks) = md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| (d.as_nanos() / 100) as i64 + UNIX_EPOCH_TICKS) {
        if ticks == b.ticks {
            return true;
        }
    }
    crc_file(&modfile) == Some(b.crc)
}

fn crc_file(p: &Path) -> Option<u32> {
    let mut f = std::fs::File::open(p).ok()?;
    let mut h = crc32fast::Hasher::new();
    let mut buf = vec![0u8; 81920];
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Some(h.finalize())
}

async fn download(spt: &Spt, path: &str, dest: &Path) -> Result<(), String> {
    if let Some(d) = dest.parent() {
        tokio::fs::create_dir_all(d).await.map_err(|e| e.to_string())?;
    }
    let part = PathBuf::from(format!("{}.part", dest.display()));
    let mut r = spt.http.get(format!("{}{}", spt.base, path)).timeout(Duration::from_secs(600)).send().await.map_err(net)?.error_for_status().map_err(net)?;
    let mut f = tokio::fs::File::create(&part).await.map_err(|e| e.to_string())?;
    let res = async {
        while let Some(chunk) = r.chunk().await.map_err(net)? {
            f.write_all(&chunk).await.map_err(|e| e.to_string())?;
        }
        f.flush().await.map_err(|e| e.to_string())
    }
    .await;
    drop(f);
    if let Err(e) = res {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(e);
    }
    tokio::fs::rename(&part, dest).await.map_err(|e| e.to_string())
}

fn start_game(game: &Path, addr: &str, pid: &str) -> Result<(), String> {
    // exactly SPT.Launcher's LaunchGameWindows argument line
    let args = format!("-force-gfx-jobs native -token={pid} -config={{'BackendUrl':'https://{addr}','Version':'live','MatchingVersion':'live'}}");
    let mut c = std::process::Command::new(game.join("EscapeFromTarkov.exe"));
    c.current_dir(game);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.raw_arg(&args);
    }
    c.spawn().map(|_| ()).map_err(|e| format!("Could not start the game: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_zlib_and_plain_answers() {
        let mut z = ZlibEncoder::new(Vec::new(), Compression::best());
        z.write_all(br#"{"Response":"Pong!"}"#).unwrap();
        let r: Resp<String> = parse(&z.finish().unwrap()).unwrap();
        assert_eq!(r.response, "Pong!");
        let r: Resp<bool> = parse(br#"{"response":true}"#).unwrap();
        assert!(r.response);
    }

    #[test]
    fn reads_bundle_manifest_casing() {
        let v: Vec<Bundle> = serde_json::from_str(r#"[{"FileName":"a/b.bundle","ModPath":"user/mods/x","Crc":3735928559,"Size":5,"ModifiedUtcTicks":1}]"#).unwrap();
        assert_eq!(v[0].crc, 0xDEADBEEF);
        assert_eq!(format!("{:08X}", v[0].crc), "DEADBEEF");
    }
}
