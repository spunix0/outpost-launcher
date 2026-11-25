//! Installed-mod scan, and installing / removing mods downloaded through The Forge.
//! Installs only into BepInEx\plugins|patchers|config and SPT_Runtime\user\mods. Every overwritten file is backed up first,
//! every install is recorded in <SPT>\OutpostLauncher\installed.json so it can be removed again.

use crate::{forge, game, spt, AppState};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, State};
use tokio::io::AsyncWriteExt;

const MAX_DOWNLOAD: u64 = 2 * 1024 * 1024 * 1024;
const DOC_EXT: [&str; 9] = ["txt", "md", "pdf", "png", "jpg", "jpeg", "url", "html", "htm"];

fn data_dir(game: &Path) -> PathBuf {
    game.join("OutpostLauncher")
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ---------- tracked installs ----------

#[derive(Serialize, Deserialize, Clone)]
pub struct Tracked {
    pub mod_id: u64,
    pub guid: String,
    pub name: String,
    pub version: String,
    pub kind: String,
    pub installed_at: u64,
    pub files: Vec<String>,
    pub backup: String,
}

fn load_tracked(game: &Path) -> Vec<Tracked> {
    game::read_json(&data_dir(game).join("installed.json")).and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

fn save_tracked(game: &Path, t: &[Tracked]) -> Result<(), String> {
    let d = data_dir(game);
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    std::fs::write(d.join("installed.json"), serde_json::to_vec_pretty(t).unwrap_or_default()).map_err(|e| e.to_string())
}

// ---------- scan ----------

#[derive(Serialize)]
struct Dep {
    guid: String,
    hard: bool,
}

#[derive(Serialize)]
struct Plugin {
    guid: String,
    name: String,
    version: String,
    file: String,
    deps: Vec<Dep>,
}

#[derive(Serialize)]
struct LogMissing {
    name: String,
    missing: Vec<String>,
}

fn guid_like(s: &str) -> bool {
    s.len() >= 5 && s.len() <= 120 && s.contains('.') && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// [BepInDependency(guid, flags)] / [BepInDependency(guid, minVersion)] custom-attribute blobs:
/// 01 00, SerString guid, then int32 flags (1 = hard, 2 = soft) or a SerString version (hard), then 00 00.
pub fn bepindeps_from_dll(b: &[u8]) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    let mut i = 0;
    while i + 12 < b.len() {
        if b[i] == 0x01 && b[i + 1] == 0x00 {
            let len = b[i + 2] as usize;
            if (5..0x80).contains(&len) {
                if let Some(g) = b.get(i + 3..i + 3 + len).and_then(|x| std::str::from_utf8(x).ok()).filter(|g| guid_like(g)) {
                    let p = i + 3 + len;
                    let hard = match b.get(p..p + 6) {
                        Some([f, 0, 0, 0, 0, 0]) if *f == 1 => Some(true),
                        Some([f, 0, 0, 0, 0, 0]) if *f == 2 => Some(false),
                        _ => {
                            let vl = b.get(p).copied().unwrap_or(0) as usize;
                            let ver = b.get(p + 1..p + 1 + vl).and_then(|x| std::str::from_utf8(x).ok());
                            let ends = b.get(p + 1 + vl..p + 3 + vl) == Some(&[0, 0]);
                            (vl > 0 && ends && ver.is_some_and(|v| v.chars().all(|c| c.is_ascii_digit() || c == '.'))).then_some(true)
                        }
                    };
                    if let Some(h) = hard {
                        if !out.iter().any(|(x, _)| x.eq_ignore_ascii_case(g)) {
                            out.push((g.to_string(), h));
                        }
                    }
                }
            }
        }
        i += 1;
    }
    out
}

/// "Could not load [Name 1.0.0] because it has missing dependencies: a.b.c, d.e.f (v1.0 or newer)" lines of BepInEx's log.
fn log_missing(game: &Path) -> Vec<LogMissing> {
    let Ok(text) = std::fs::read(game.join("BepInEx").join("LogOutput.log")) else { return Vec::new() };
    let text = String::from_utf8_lossy(&text[text.len().saturating_sub(4_000_000)..]).to_string();
    let mut out: Vec<LogMissing> = Vec::new();
    for line in text.lines() {
        let Some(at) = line.find("because it has missing dependencies:") else { continue };
        let name = line.find("Could not load [").map(|s| &line[s + 16..at]).and_then(|s| s.rfind(']').map(|e| s[..e].to_string())).unwrap_or_default();
        let missing: Vec<String> = line[at + 36..]
            .split(',')
            .filter_map(|p| p.trim().split([' ', '(']).next().map(str::to_string))
            .filter(|g| guid_like(g))
            .collect();
        if !missing.is_empty() && !out.iter().any(|o| o.name == name && o.missing == missing) {
            out.push(LogMissing { name, missing });
        }
    }
    out
}

/// BepInPlugin(guid, name, version) is stored as a custom-attribute blob: 01 00, three SerStrings, 00 00.
pub fn bepinplugin_from_dll(b: &[u8]) -> Option<(String, String, String)> {
    let mut i = 0;
    while i + 8 < b.len() {
        if b[i] == 0x01 && b[i + 1] == 0x00 {
            let mut p = i + 2;
            let mut parts = Vec::with_capacity(3);
            for _ in 0..3 {
                let len = *b.get(p)? as usize;
                if len == 0 || len >= 0x80 {
                    break;
                }
                let s = b.get(p + 1..p + 1 + len).and_then(|x| std::str::from_utf8(x).ok());
                match s {
                    Some(s) => parts.push(s.to_string()),
                    None => break,
                }
                p += 1 + len;
            }
            if parts.len() == 3 && b.get(p) == Some(&0) && b.get(p + 1) == Some(&0) && looks_like_plugin(&parts) {
                return Some((parts[0].clone(), parts[1].clone(), parts[2].clone()));
            }
        }
        i += 1;
    }
    None
}

fn looks_like_plugin(p: &[String]) -> bool {
    let guid_ok = p[0].len() >= 3 && !p[0].contains(char::is_whitespace) && p[0].chars().all(|c| c.is_ascii_graphic());
    let name_ok = p[1].chars().all(|c| !c.is_control());
    let v = &p[2];
    let core = v.split(['-', '+']).next().unwrap_or("");
    let ver_ok = !core.is_empty() && core.split('.').count() <= 4 && core.split('.').all(|x| !x.is_empty() && x.chars().all(|c| c.is_ascii_digit()));
    guid_ok && name_ok && ver_ok
}

fn dlls(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 4 {
        return;
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for p in rd.flatten().map(|e| e.path()) {
            if p.is_dir() {
                dlls(&p, depth + 1, out);
            } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("dll")) {
                out.push(p);
            }
        }
    }
}

#[tauri::command]
pub async fn mods_scan(state: State<'_, AppState>) -> Result<Value, String> {
    let game = game::game_dir(&state)?;
    let addr = game::server_addr(&game);
    let spt_version = spt::file_version(&game.join("BepInEx").join("plugins").join("spt").join("spt-core.dll"))
        .map(|(a, b, c)| format!("{a}.{b}.{c}"))
        .unwrap_or_default();

    let g = game.clone();
    let plugins = tokio::task::spawn_blocking(move || {
        let root = g.join("BepInEx").join("plugins");
        let mut files = Vec::new();
        dlls(&root, 0, &mut files);
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for f in files {
            let Ok(md) = std::fs::metadata(&f) else { continue };
            if md.len() > 64 * 1024 * 1024 {
                continue;
            }
            let Ok(b) = std::fs::read(&f) else { continue };
            if let Some((guid, name, version)) = bepinplugin_from_dll(&b) {
                if seen.insert(guid.to_lowercase()) {
                    let file = f.strip_prefix(&g).unwrap_or(&f).display().to_string();
                    let deps = bepindeps_from_dll(&b).into_iter().filter(|(d, _)| !d.eq_ignore_ascii_case(&guid)).map(|(guid, hard)| Dep { guid, hard }).collect();
                    out.push(Plugin { guid, name, version, file, deps });
                }
            }
        }
        out
    })
    .await
    .map_err(|e| e.to_string())?;

    let server_folders: Vec<String> = std::fs::read_dir(game.join("SPT_Runtime").join("user").join("mods"))
        .map(|rd| rd.flatten().filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().to_string()).collect())
        .unwrap_or_default();

    let s = spt::Spt::new(&addr);
    let server_mods = if s.ping().await { s.get_value("/launcher/v2/mods").await.ok() } else { None };

    Ok(json!({
        "spt_version": spt_version,
        "plugins": plugins,
        "server_folders": server_folders,
        "server_mods": server_mods,
        "log_missing": log_missing(&game),
        "tracked": load_tracked(&game),
    }))
}

#[tauri::command]
pub fn spt_version(state: State<AppState>) -> Result<String, String> {
    let game = game::game_dir(&state)?;
    spt::file_version(&game.join("BepInEx").join("plugins").join("spt").join("spt-core.dll"))
        .map(|(a, b, c)| format!("{a}.{b}.{c}"))
        .ok_or_else(|| "spt-core.dll not found.".into())
}

// ---------- install ----------

#[derive(Deserialize)]
pub struct InstallItem {
    mod_id: u64,
    #[serde(default)]
    guid: String,
    name: String,
    version: String,
    link: String,
    #[serde(default)]
    kind: String,
}

fn ensure_files_free(state: &AppState) -> Result<PathBuf, String> {
    let game = game::game_dir(state)?;
    if spt::game_running() {
        return Err("Close the game first.".into());
    }
    Ok(game)
}

#[tauri::command]
pub async fn mod_install(app: AppHandle, state: State<'_, AppState>, items: Vec<InstallItem>) -> Result<Vec<String>, String> {
    let game = ensure_files_free(&state)?;
    if spt::Spt::new(&game::server_addr(&game)).ping().await {
        return Err("Stop the SPT server first: it keeps server mod files locked.".into());
    }
    let http = reqwest::Client::builder()
        .user_agent(forge::UA)
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(1800))
        .build()
        .map_err(|e| e.to_string())?;
    let mut notes = Vec::new();
    for (n, it) in items.iter().enumerate() {
        if !it.link.starts_with(&format!("{}/", forge::BASE)) {
            return Err(format!("{}: not a Forge download link.", it.name));
        }
        let label = format!("{} {}", it.name, it.version);
        let _ = app.emit("mod-progress", format!("Downloading {label} ({}/{})", n + 1, items.len()));
        let file = download(&http, &it.link, &data_dir(&game).join("downloads"), it.mod_id, &it.version).await.map_err(|e| format!("{label}: {e}"))?;
        let _ = app.emit("mod-progress", format!("Installing {label}"));
        let g = game.clone();
        let it2 = (it.mod_id, it.guid.clone(), it.name.clone(), it.version.clone(), it.kind.clone());
        let res = tokio::task::spawn_blocking(move || install_archive(&g, &file, it2)).await.map_err(|e| e.to_string())?;
        let count = res.map_err(|e| format!("{label}: {e}"))?;
        notes.push(format!("Installed {label} ({count} files)."));
    }
    Ok(notes)
}

/// Follows the Forge link to the author's host. Archives are saved; a web page means the host needs a browser.
async fn download(http: &reqwest::Client, link: &str, dir: &Path, mod_id: u64, version: &str) -> Result<PathBuf, String> {
    let mut r = http.get(link).send().await.map_err(|e| format!("download failed: {e}"))?;
    let final_url = r.url().to_string();
    if !r.status().is_success() {
        return Err(format!("download failed: {} from {}", r.status(), host(&final_url)));
    }
    let ctype = r.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_lowercase();
    if ctype.starts_with("text/html") {
        return Err(format!("MANUAL|{final_url}"));
    }
    if r.content_length().is_some_and(|l| l > MAX_DOWNLOAD) {
        return Err("the file is larger than 2 GB.".into());
    }
    tokio::fs::create_dir_all(dir).await.map_err(|e| e.to_string())?;
    let safe_ver: String = version.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' { c } else { '_' }).collect();
    let dest = dir.join(format!("{mod_id}-{safe_ver}.bin"));
    let mut f = tokio::fs::File::create(&dest).await.map_err(|e| e.to_string())?;
    let mut total = 0u64;
    while let Some(chunk) = r.chunk().await.map_err(|e| format!("download broke off: {e}"))? {
        total += chunk.len() as u64;
        if total > MAX_DOWNLOAD {
            drop(f);
            let _ = tokio::fs::remove_file(&dest).await;
            return Err("the file is larger than 2 GB.".into());
        }
        f.write_all(&chunk).await.map_err(|e| e.to_string())?;
    }
    f.flush().await.map_err(|e| e.to_string())?;
    drop(f);
    let mut head = [0u8; 8];
    let n = std::fs::File::open(&dest).and_then(|mut x| x.read(&mut head)).unwrap_or(0);
    if n >= 1 && (head[0] == b'<' || head[..n].starts_with(b"\xEF\xBB\xBF<")) {
        let _ = std::fs::remove_file(&dest);
        return Err(format!("MANUAL|{final_url}"));
    }
    let name = final_url.split(['?', '#']).next().unwrap_or("").rsplit('/').next().unwrap_or("").to_lowercase();
    let ext = if head.starts_with(b"PK\x03\x04") {
        "zip"
    } else if head.starts_with(b"7z\xBC\xAF\x27\x1C") {
        "7z"
    } else if head.starts_with(b"MZ") && name.ends_with(".dll") {
        "dll"
    } else if head.starts_with(b"Rar!") {
        let _ = std::fs::remove_file(&dest);
        return Err("RAR archives are not supported yet. Install this one manually from its Forge page.".into());
    } else {
        let _ = std::fs::remove_file(&dest);
        return Err(format!("unknown file type from {}. Install it manually from its Forge page.", host(&final_url)));
    };
    let typed = if ext == "dll" { dir.join(sanitize_file_name(&name)) } else { dest.with_extension(ext) };
    std::fs::rename(&dest, &typed).map_err(|e| e.to_string())?;
    Ok(typed)
}

fn host(url: &str) -> String {
    url.split("://").nth(1).and_then(|r| r.split('/').next()).unwrap_or(url).to_string()
}

fn sanitize_file_name(n: &str) -> String {
    n.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')).collect()
}

/// Archive entry name → path relative to the SPT folder, or None for a root-level doc file to skip.
fn map_entry(name: &str) -> Result<Option<PathBuf>, String> {
    let n = name.replace('\\', "/");
    let n = n.trim_start_matches("./").trim_start_matches('/');
    let parts: Vec<&str> = n.split('/').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return Ok(None);
    }
    if parts.iter().any(|p| *p == ".." || p.contains(':')) {
        return Err(format!("unsafe path in archive: {name}"));
    }
    let first = parts[0].to_lowercase();
    let rest = &parts[1..];
    let (root, sub): (Vec<&str>, &[&str]) = match first.as_str() {
        "bepinex" => (vec!["BepInEx"], rest),
        "spt_runtime" | "spt" => (vec!["SPT_Runtime"], rest),
        "user" => (vec!["SPT_Runtime", "user"], rest),
        _ if parts.len() == 1 => {
            let ext = Path::new(parts[0]).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            return if DOC_EXT.contains(&ext.as_str()) || parts[0].eq_ignore_ascii_case("license") { Ok(None) } else { Err(format!("unexpected file at the archive root: {name}")) };
        }
        _ => return Err(format!("files outside BepInEx and SPT_Runtime: {name}")),
    };
    let mut p: PathBuf = root.iter().collect();
    for s in sub {
        p.push(s);
    }
    let rel = p.display().to_string().replace('/', "\\").to_lowercase();
    let allowed = ["bepinex\\plugins\\", "bepinex\\patchers\\", "bepinex\\config\\", "spt_runtime\\user\\mods\\"];
    if !allowed.iter().any(|a| rel.starts_with(a)) {
        return Err(format!("the archive writes outside the mod folders ({}).", p.display()));
    }
    Ok(Some(p))
}

/// Strips one or two wrapper folders (e.g. "SAIN-4.5.1/BepInEx/...") when every entry shares them.
fn strip_wrappers(names: &[String]) -> usize {
    let known = ["bepinex", "spt_runtime", "spt", "user"];
    for depth in 0..3 {
        let firsts: HashSet<String> = names
            .iter()
            .filter_map(|n| n.replace('\\', "/").split('/').filter(|p| !p.is_empty()).nth(depth).map(|s| s.to_lowercase()))
            .collect();
        if firsts.iter().any(|f| known.contains(&f.as_str())) {
            return depth;
        }
        let wrappers: HashSet<String> = names.iter().filter_map(|n| n.replace('\\', "/").split('/').filter(|p| !p.is_empty()).take(depth + 1).last().map(str::to_lowercase)).collect();
        if wrappers.len() != 1 || names.iter().any(|n| n.replace('\\', "/").split('/').filter(|p| !p.is_empty()).count() <= depth + 1) {
            return depth;
        }
    }
    0
}

fn plan(names: &[String]) -> Result<HashMap<String, PathBuf>, String> {
    let skip = strip_wrappers(names);
    let mut out = HashMap::new();
    for n in names {
        let trimmed: Vec<&str> = n.split(['/', '\\']).filter(|p| !p.is_empty()).skip(skip).collect();
        if let Some(p) = map_entry(&trimmed.join("/"))? {
            out.insert(n.clone(), p);
        }
    }
    if out.is_empty() {
        return Err("nothing to install was found in the archive.".into());
    }
    Ok(out)
}

fn install_archive(game: &Path, file: &Path, it: (u64, String, String, String, String)) -> Result<usize, String> {
    let (mod_id, guid, name, version, kind) = it;
    let ext = file.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let slug: String = name.chars().filter(|c| c.is_ascii_alphanumeric()).take(32).collect();
    let backup_rel = format!("backups\\{}-{}", now_secs(), slug);
    let backup = data_dir(game).join(&backup_rel);
    let mut written: Vec<String> = Vec::new();

    let mut put = |rel: &Path, data: &mut dyn Read| -> Result<(), String> {
        let target = game.join(rel);
        if target.is_file() {
            let b = backup.join(rel);
            if let Some(d) = b.parent() {
                std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
            }
            std::fs::copy(&target, &b).map_err(|e| format!("backup of {} failed: {e}", rel.display()))?;
        }
        if let Some(d) = target.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let mut out = std::fs::File::create(&target).map_err(|e| format!("cannot write {}: {e}", rel.display()))?;
        std::io::copy(data, &mut out).map_err(|e| format!("cannot write {}: {e}", rel.display()))?;
        written.push(rel.display().to_string());
        Ok(())
    };

    match ext.as_str() {
        "zip" => {
            let mut z = zip::ZipArchive::new(std::fs::File::open(file).map_err(|e| e.to_string())?).map_err(|e| format!("broken zip: {e}"))?;
            let names: Vec<String> = (0..z.len()).filter_map(|i| z.by_index(i).ok().filter(|f| !f.is_dir()).map(|f| f.name().to_string())).collect();
            let map = plan(&names)?;
            for i in 0..z.len() {
                let mut f = z.by_index(i).map_err(|e| e.to_string())?;
                if f.is_dir() {
                    continue;
                }
                if let Some(rel) = map.get(f.name()).cloned() {
                    put(&rel, &mut f)?;
                }
            }
        }
        "7z" => {
            let mut r = sevenz_rust::SevenZReader::open(file, sevenz_rust::Password::empty()).map_err(|e| format!("broken 7z: {e}"))?;
            let names: Vec<String> = r.archive().files.iter().filter(|f| !f.is_directory() && f.has_stream()).map(|f| f.name().to_string()).collect();
            let map = plan(&names)?;
            let mut err: Option<String> = None;
            r.for_each_entries(|entry, reader| {
                if entry.is_directory() {
                    return Ok(true);
                }
                match map.get(entry.name()) {
                    Some(rel) => {
                        if let Err(e) = put(rel, reader) {
                            err = Some(e);
                            return Ok(false);
                        }
                    }
                    None => {
                        let _ = std::io::copy(reader, &mut std::io::sink());
                    }
                }
                Ok(true)
            })
            .map_err(|e| format!("7z extraction failed: {e}"))?;
            if let Some(e) = err {
                return Err(e);
            }
        }
        "dll" => {
            let fname = file.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
            let rel = PathBuf::from("BepInEx").join("plugins").join(fname);
            let mut f = std::fs::File::open(file).map_err(|e| e.to_string())?;
            put(&rel, &mut f)?;
        }
        _ => return Err("unsupported file type.".into()),
    }
    let _ = std::fs::remove_file(file);

    // replace the previous tracked install of the same mod: files it had that the new version does not are moved to the backup
    let mut tracked = load_tracked(game);
    let new_set: HashSet<String> = written.iter().map(|w| w.to_lowercase()).collect();
    if let Some(pos) = tracked.iter().position(|t| t.mod_id == mod_id && t.kind == kind) {
        let old = tracked.remove(pos);
        for f in old.files.iter().filter(|f| !new_set.contains(&f.to_lowercase())) {
            move_to(game, f, &backup);
        }
    }
    let count = written.len();
    tracked.push(Tracked { mod_id, guid, name, version, kind, installed_at: now_secs(), files: written, backup: backup_rel });
    save_tracked(game, &tracked)?;
    Ok(count)
}

fn move_to(game: &Path, rel: &str, backup: &Path) {
    let src = game.join(rel);
    if !src.is_file() {
        return;
    }
    let dst = backup.join(rel);
    if let Some(d) = dst.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if std::fs::rename(&src, &dst).is_err() && std::fs::copy(&src, &dst).is_ok() {
        let _ = std::fs::remove_file(&src);
    }
    // remove now-empty folders up to (not including) the mod roots
    let mut dir = src.parent().map(Path::to_path_buf);
    while let Some(d) = dir {
        let rel = d.strip_prefix(game).map(|p| p.display().to_string().to_lowercase()).unwrap_or_default();
        if ["bepinex\\plugins", "bepinex\\patchers", "bepinex\\config", "spt_runtime\\user\\mods"].contains(&rel.as_str()) || rel.is_empty() {
            break;
        }
        if std::fs::remove_dir(&d).is_err() {
            break;
        }
        dir = d.parent().map(Path::to_path_buf);
    }
}

/// Removes a mod installed by this launcher: its files are moved to a backup folder, not deleted.
#[tauri::command]
pub async fn mod_uninstall(state: State<'_, AppState>, mod_id: u64, kind: String) -> Result<String, String> {
    let game = ensure_files_free(&state)?;
    if spt::Spt::new(&game::server_addr(&game)).ping().await {
        return Err("Stop the SPT server first: it keeps server mod files locked.".into());
    }
    let mut tracked = load_tracked(&game);
    let pos = tracked.iter().position(|t| t.mod_id == mod_id && t.kind == kind).ok_or("This mod was not installed by Outpost Launcher.")?;
    let t = tracked.remove(pos);
    let slug: String = t.name.chars().filter(|c| c.is_ascii_alphanumeric()).take(32).collect();
    let rel = format!("backups\\{}-removed-{}", now_secs(), slug);
    let backup = data_dir(&game).join(&rel);
    for f in &t.files {
        move_to(&game, f, &backup);
    }
    save_tracked(&game, &tracked)?;
    Ok(format!("Removed {} {}. Its files are in OutpostLauncher\\{rel}.", t.name, t.version))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_forge_layouts() {
        let names: Vec<String> = ["BepInEx/plugins/SAIN/SAIN.dll", "SPT_Runtime/user/mods/SAIN-Server/mod.dll", "README.md"].iter().map(|s| s.to_string()).collect();
        let m = plan(&names).unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m["BepInEx/plugins/SAIN/SAIN.dll"], PathBuf::from("BepInEx\\plugins\\SAIN\\SAIN.dll"));
        let wrapped: Vec<String> = ["SAIN-4.5.1/BepInEx/plugins/a.dll", "SAIN-4.5.1/user/mods/x/b.dll"].iter().map(|s| s.to_string()).collect();
        let m = plan(&wrapped).unwrap();
        assert_eq!(m["SAIN-4.5.1/user/mods/x/b.dll"], PathBuf::from("SPT_Runtime\\user\\mods\\x\\b.dll"));
    }

    #[test]
    fn refuses_unsafe_or_foreign_paths() {
        assert!(plan(&["BepInEx/plugins/../../evil.dll".to_string()]).is_err());
        assert!(plan(&["EscapeFromTarkov_Data/Managed/Assembly-CSharp.dll".to_string()]).is_err());
        assert!(plan(&["BepInEx/core/BepInEx.dll".to_string()]).is_err());
        assert!(plan(&["C:/Windows/x.dll".to_string()]).is_err());
    }

    /// Manual check against a real install: cargo test --release -- --ignored --nocapture scan_real
    #[test]
    #[ignore]
    fn scan_real_install() {
        let root = Path::new(r"C:\Single Player Tarkov\BepInEx\plugins");
        let mut files = Vec::new();
        dlls(root, 0, &mut files);
        let (mut hit, mut miss) = (0, Vec::new());
        for f in &files {
            match std::fs::read(f).ok().and_then(|b| bepinplugin_from_dll(&b)) {
                Some((g, n, v)) => {
                    hit += 1;
                    let deps: Vec<String> = std::fs::read(f).map(|b| bepindeps_from_dll(&b)).unwrap_or_default().into_iter().map(|(d, h)| format!("{d}{}", if h { "" } else { " (soft)" })).collect();
                    println!("{g} | {n} | {v} | deps: {}", if deps.is_empty() { "-".into() } else { deps.join(", ") });
                }
                None => miss.push(f.display().to_string()),
            }
        }
        println!("--- {hit} plugins found, {} DLLs without BepInPlugin:", miss.len());
        for m in miss { println!("  {m}"); }
    }

    #[test]
    fn finds_bepindependency_blobs() {
        let mut b = vec![0u8; 8];
        // [BepInDependency("xyz.drakia.bigbrain", Hard)]
        b.extend([0x01, 0x00, 19]);
        b.extend(b"xyz.drakia.bigbrain");
        b.extend([1, 0, 0, 0, 0, 0, 0xAA]);
        // [BepInDependency("com.x.soft", Soft)]
        b.extend([0x01, 0x00, 10]);
        b.extend(b"com.x.soft");
        b.extend([2, 0, 0, 0, 0, 0, 0xAA]);
        // [BepInDependency("com.x.minver", "1.2.0")]
        b.extend([0x01, 0x00, 12]);
        b.extend(b"com.x.minver");
        b.extend([5]);
        b.extend(b"1.2.0");
        b.extend([0, 0, 0xAA]);
        let d = bepindeps_from_dll(&b);
        assert_eq!(d, vec![("xyz.drakia.bigbrain".to_string(), true), ("com.x.soft".to_string(), false), ("com.x.minver".to_string(), true)]);
        // a BepInPlugin blob must not look like a dependency
        let mut p = vec![0x01, 0x00, 11];
        p.extend(b"me.sol.sain");
        p.extend([4]);
        p.extend(b"SAIN");
        p.extend([5]);
        p.extend(b"4.5.1");
        p.extend([0, 0]);
        assert!(bepindeps_from_dll(&p).is_empty());
    }

    #[test]
    fn parses_log_missing_dependency_line() {
        let dir = std::env::temp_dir().join("outpost-test-log");
        std::fs::create_dir_all(dir.join("BepInEx")).unwrap();
        std::fs::write(
            dir.join("BepInEx").join("LogOutput.log"),
            "[Info   :   BepInEx] Loading [X]\n[Error  :   BepInEx] Could not load [SPT DX12 Support 0.1.0] because it has missing dependencies: com.spt.core, xyz.drakia.bigbrain (v1.0.0 or newer)\n",
        )
        .unwrap();
        let m = log_missing(&dir);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].name, "SPT DX12 Support 0.1.0");
        assert_eq!(m[0].missing, vec!["com.spt.core".to_string(), "xyz.drakia.bigbrain".to_string()]);
    }

    #[test]
    fn finds_bepinplugin_blob() {
        let mut b = vec![0u8; 16];
        b.extend([0x01, 0x00]);
        for s in ["me.sol.sain", "SAIN", "4.5.1"] {
            b.push(s.len() as u8);
            b.extend(s.as_bytes());
        }
        b.extend([0x00, 0x00, 0xFF]);
        assert_eq!(bepinplugin_from_dll(&b), Some(("me.sol.sain".into(), "SAIN".into(), "4.5.1".into())));
    }
}
