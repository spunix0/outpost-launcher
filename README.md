# Outpost Launcher

An alternative launcher for **SPT 4.1.6**, written in Rust with [Tauri 2](https://tauri.app).
Start the SPT server, pick a profile, launch the game, and browse, install and update mods from
[The Forge](https://sp-mod.com), all in one window.

> Unofficial. Not made by, endorsed by, or affiliated with the SPT team, The Forge, or Battlestate Games.

## Features

**Server**
- Start and stop the SPT server from the launcher, with its live console built in
- Online / offline status; a server started elsewhere is detected and never stopped by the launcher
- Optional auto-start: the server starts when the launcher opens (Settings; skipped if one is already running)

**Play**
- Profile cards (name, side, level, edition, roubles, XP progress)
- Launches exactly like the official SPT launcher: login, version check, file cleanup, mod bundle sync, then the game
  with the same arguments

**Mods (The Forge)**
- Search, sort (downloads, updated, newest, favourites, endorsements, name) and filter (SPT version, category, featured,
  Fika compatible, legacy)
- Card view or compact grid; **Hide installed** toggle; installed mods are badged with their version
- Mod pages: description, every version with changelog, SPT/Fika compatibility, size, dependencies, VirusTotal scans,
  license, source code links and addons
- One-click install with automatic dependency resolution and a confirmation list
- **Check for updates**, update one mod or **Update all**; blocked updates explain why; **Patch notes** for every version
  between yours and the update, right in the list
- **Missing dependencies:** the launcher reads each plugin's declared dependencies (and BepInEx's "missing dependencies" log
  lines), tells you exactly what is missing and which mod needs it, and offers to install it from The Forge
- Blocked updates say exactly why (which mod needs which version, or which dependency has no version for your SPT), and an
  **Install missing dependencies** button appears when the missing mods can be installed from The Forge; normal updates
  offer to install any new dependencies together with the update
- Installed view: your client plugins and server mods matched to The Forge, with **Uninstall** for any of them. Uninstall
  works out which files belong to the mod (its plugin folder and its server folder), warns if other mods need it, and moves
  everything to a backup folder instead of deleting it. SPT's own parts are protected
- Page navigation with first/last arrows and a jump-to-page box; each mod gets an initials tile

## Install

1. Download `Outpost-Launcher.exe` from the [latest release](../../releases/latest).
2. Put it in your SPT folder (the one with `EscapeFromTarkov.exe` and `SPT_Runtime`).
3. Run it. Windows 10/11 with WebView2 (included in Windows 11) is required.

The launcher only ever works on an SPT folder. It refuses any folder under `steamapps` and never touches a live
Escape from Tarkov install.

## Q&A

**Windows SmartScreen says "unknown publisher". Is it safe?**
The exe is not code-signed (a certificate costs money), so Windows warns about any new unsigned program. You do not have to
trust the download: every release lists its SHA-256 so you can check the file, and the full source is here to read or build
yourself (see "Build from source").

**Does it replace the official SPT launcher?**
It does the same launch steps, so day to day you can use either. Creating new profiles still needs the official launcher
for now.

**Is it safe for my profile?**
The launcher does not edit profiles. Mod installs can affect a profile the same way manual installs do: mods that add
items to your profile ("profile binding" on The Forge, shown as a warning on the mod page) can break that profile if you
remove them later. Back up `SPT_Runtime\user\profiles` before big changes.

**Where do mods get installed? Can I undo an install?**
Almost always into `BepInEx\plugins` and `SPT_Runtime\user\mods`. Some mods also ship files for the game folder itself
(for example DLLs in `EscapeFromTarkov_Data\Managed`); those are installed too, and the install message lists them. The
launcher only ever writes **inside the SPT folder you selected**: paths that try to leave it (`..`, drive letters, links or
junctions pointing elsewhere) are refused, and so are programs and scripts (`.exe`, `.bat`, `.cmd`, `.ps1`, ...). Every
file an install overwrites is copied to `OutpostLauncher\backups\` first. **Uninstall** moves a mod's files to that backup
folder instead of deleting them. Shared game files a mod added (such as a DLL in `Managed`) are left in place, because the
game or another mod may rely on them.

**Why does an install say "Stop the SPT server first"?**
The running server keeps server-mod files locked, and the game locks client plugins. Close both, install, then start again.

**Why do some mods open their Forge page instead of installing?**
Downloads come from each author's own host. GitHub and direct links install automatically; hosts that show a web page
first (Google Drive, Mega, etc.) and `.rar` archives need a manual download.

**Why do mods show letters instead of their own pictures?**
The Forge's image server only serves its thumbnails to sp-mod.com itself (hotlink protection). The launcher respects that
instead of faking a referrer, so each mod shows its initials on a colour taken from its name. Mod descriptions still show
images hosted elsewhere (imgur, GitHub, ...).

**It says a mod has a missing dependency. What does that mean?**
The mod declares that it needs another mod (for example BigBrain) which is not in your install, so BepInEx will refuse to
load it. The launcher lists what is missing and who needs it, then offers to install it. Anything that is not on The Forge
(or has no version for your SPT version) is listed so you can add it by hand.

**Does it slow down The Forge?**
No. It uses the public read-only API, stays under its rate limits, honours `Retry-After`, and caches answers for 5 minutes.

**How does "Check for updates" know my mod versions?**
Client plugins: it reads the BepInEx plugin ID and version from each DLL in `BepInEx\plugins`. Server mods: it asks the
running SPT server. Mods installed through the launcher are also recorded in `OutpostLauncher\installed.json`.

**Will it work with other SPT versions?**
It is built and tested against SPT 4.1.6. It reads the server's protocol from the same endpoints the official launcher
uses, so nearby versions likely work, but nothing is guaranteed.

**Where are its settings?**
`%APPDATA%\dev.outpost.launcher\settings.json`. Mod records and backups are in `<SPT folder>\OutpostLauncher\`.

**Something went wrong. What do I send?**
The error toast text, `SPT_Runtime\user\logs\` (server) and `BepInEx\LogOutput.log` (game). Open an issue on this
repository.

## How a launch works

Mirrors SPT.Launcher 4.1.6:
1. `GET /launcher/v2/ping` → `"Pong!"`
2. `PUT /launcher/v2/login` and `/launcher/v2/profile` with `{"username": ...}` (zlib body) → profile id
3. `GET /launcher/v2/version` compared with `BepInEx/plugins/spt/spt-core.dll` (warning only, like SPT)
4. Deletes `BattlEye`, `Logs`, `ConsistencyInfo`, `EscapeFromTarkov_BE.exe`, `Uninstall.exe`, `UnityCrashHandler64.exe`,
   `WinPixEventRuntime.dll`, `hwecho.dll` from the SPT folder, as the official launcher does (respects `ExcludeFromCleanup`)
5. Binary patches: none ship with 4.1.6; if a later version ships some, run the official launcher once
6. `GET /singleplayer/bundles`; missing bundles from `/files/bundle/<name>` into `SPT_Runtime/user/cache/bundles/<CRC>/`
7. `EscapeFromTarkov.exe -force-gfx-jobs native -token=<profileId> -config={'BackendUrl':'https://<ip:port>','Version':'live','MatchingVersion':'live'}`

## Build from source

Needs Rust (stable, MSVC toolchain). No Node.js.

```
cd src-tauri
cargo build --release --features tauri/custom-protocol
cargo test --release --features tauri/custom-protocol
```

Output: `src-tauri/target/release/outpost-launcher.exe`. The UI is plain HTML/CSS/JS in `ui/`.

## Roadmap
- Profile editor
- Create and wipe profiles
- Mod thumbnails, if The Forge allows it

## License
MIT, see [LICENSE](LICENSE).
