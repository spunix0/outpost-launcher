'use strict';
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const $ = (s) => document.querySelector(s);

let status = null;        // { online, owned, address }
let profiles = [];        // MiniProfiles from the running server
let selected = null;      // username of the active profile
let activeName = null;    // remembered even while the server is offline (settings.last_profile)
let profilesLoaded = false;
let lastFetch = 0;
let statTab = 'overall';

// ---------- helpers ----------
function mk(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text != null) e.textContent = text;
  return e;
}

function toast(msg, kind = '') {
  const t = document.createElement('div');
  t.className = 'toast ' + kind;
  t.textContent = String(msg);
  $('#toasts').appendChild(t);
  if (kind === 'error') {
    // errors stay until clicked, so they cannot be missed
    t.title = 'Click to dismiss';
    t.style.cursor = 'pointer';
    t.onclick = () => t.remove();
  } else {
    setTimeout(() => t.remove(), Math.min(20000, 5000 + String(msg).length * 40));
  }
}

function ask(title, text, yes = 'OK', style = 'danger') {
  return new Promise((resolve) => {
    $('#ask-title').textContent = title;
    $('#ask-text').textContent = text;
    $('#ask-yes').textContent = yes;
    $('#ask-yes').className = 'btn ' + style;
    $('#ask').hidden = false;
    const done = (v) => { $('#ask').hidden = true; resolve(v); };
    $('#ask-yes').onclick = () => done(true);
    $('#ask-no').onclick = () => done(false);
  });
}

function show(page) {
  document.querySelectorAll('.page').forEach((p) => (p.hidden = p.id !== 'page-' + page));
  document.querySelectorAll('.rail button[data-page]').forEach((b) => b.classList.toggle('active', b.dataset.page === page));
  $('#profile-flyout').hidden = true;
  if (page === 'profiles') fetchProfiles();
  if (page === 'mods' && window.Mods) window.Mods.open();
}
document.querySelectorAll('.rail button[data-page]').forEach((b) => (b.onclick = () => show(b.dataset.page)));

// ---------- server ----------
async function refreshStatus() {
  try {
    status = await invoke('server_status');
  } catch (e) {
    status = { online: false, owned: false, address: '', error: String(e) };
  }
  // a server this launcher started that is not answering yet is still starting (also covers auto-start)
  const starting = !!(status.owned && !status.online);
  const el = $('#srv-status');
  const state = status.online ? 'online' : starting ? 'starting' : 'offline';
  el.className = 'status ' + state;
  el.querySelector('.label').textContent = status.online ? 'Online' : starting ? 'Starting' : 'Offline';
  $('#srv-address').textContent = status.address || '-';
  $('#btn-start').disabled = status.online || status.owned || !!status.error;
  $('#btn-stop').disabled = !status.owned;
  $('#srv-note').textContent = status.error
    ? status.error
    : status.online && !status.owned
      ? 'Running in its own window (started outside this launcher).'
      : '';
  // load the profiles as soon as the server answers; forget them when it goes away
  if (status.online && !profilesLoaded) { profilesLoaded = true; fetchProfiles(true); }
  if (!status.online && profilesLoaded) { profilesLoaded = false; profiles = []; renderAll(); }
}

const con = $('#console');
let conEmpty = true;
function addLine(line) {
  if (conEmpty) { con.textContent = ''; conEmpty = false; }
  const d = document.createElement('div');
  d.textContent = line;
  if (/\b(error|exception|fail)/i.test(line)) d.className = 'err';
  else if (/\bwarn/i.test(line)) d.className = 'warn';
  else if (/server is running|happy playing/i.test(line)) d.className = 'ok';
  con.appendChild(d);
  while (con.childElementCount > 1500) con.firstChild.remove();
  if ($('#autoscroll').checked) con.scrollTop = con.scrollHeight;
}

$('#btn-start').onclick = async () => {
  try {
    await invoke('server_start');
    refreshStatus();
  } catch (e) { toast(e, 'error'); }
};

$('#btn-stop').onclick = async () => {
  const ok = await ask('Stop the SPT server?', 'Close the game first. Stopping the server while you play loses progress that was not saved yet.', 'Stop');
  if (!ok) return;
  try { await invoke('server_stop'); toast('Server stopped.'); } catch (e) { toast(e, 'error'); }
  refreshStatus();
};

// ---------- profiles ----------
function fmt(n) { return typeof n === 'number' ? n.toLocaleString('en-US') : '-'; }
const displayName = (p) => (p.nickname && p.nickname !== 'unknown' ? p.nickname : p.username);
const hasCharacter = (p) => !!p.nickname && p.nickname !== 'unknown';

function initials(name) {
  const words = String(name || '?').split(/[^A-Za-z0-9]+/).filter(Boolean);
  return (words.length ? words.slice(0, 2).map((w) => w[0]).join('') : '?').toUpperCase();
}
function tile(name, cls) {
  const d = mk('div', cls || 'ptile', initials(name));
  let hsh = 0;
  for (const ch of String(name || '')) hsh = (hsh * 31 + ch.charCodeAt(0)) >>> 0;
  d.style.background = `linear-gradient(160deg, hsl(${hsh % 360} 45% 32%), hsl(${(hsh % 360 + 30) % 360} 50% 18%))`;
  return d;
}

async function fetchProfiles(quiet) {
  if (!status || !status.online) { profiles = []; renderAll(); return; }
  try {
    profiles = (await invoke('list_profiles')) || [];
  } catch (e) { if (!quiet) toast(e, 'error'); return; }
  lastFetch = Date.now();
  if (!selected) selected = activeName;
  // the remembered profile may be gone (deleted, or a different server): fall back to the first usable one
  if (!profiles.some((p) => p.username === selected && !p.invalidOrUnloadableProfile)) selected = (profiles.find((p) => !p.invalidOrUnloadableProfile) || {}).username || null;
  if (selected) activeName = selected;
  renderAll();
}

function setActive(name) {
  selected = name;
  activeName = name;
  invoke('set_last_profile', { username: name }).catch(() => {});
  renderAll();
}

function renderAll() {
  renderRail();
  if (!$('#profile-flyout').hidden) renderFlyout();
  if (!$('#page-profiles').hidden) renderProfiles();
}

// --- sidebar tile + switcher flyout ---
function renderRail() {
  const p = profiles.find((x) => x.username === selected);
  const name = p ? displayName(p) : activeName || '?';
  const t = $('#rail-profile-tile');
  const c = tile(name);
  t.textContent = c.textContent;
  t.style.background = c.style.background;
  $('#rail-profile').title = (activeName ? 'Profile: ' + name + '. ' : '') + 'Click to switch profile';
}

function renderFlyout() {
  const fl = $('#profile-flyout');
  fl.replaceChildren(mk('div', 'card-meta', 'Switch profile'));
  if (!status || !status.online) {
    fl.appendChild(mk('div', 'muted small fly-note', status && status.owned ? 'The SPT server is starting…' : 'Start the SPT server to switch profiles.'));
  }
  for (const p of profiles) {
    const it = mk('button', 'fly-item' + (p.username === selected ? ' on' : ''));
    it.disabled = !!p.invalidOrUnloadableProfile;
    const txt = mk('div', 'fly-text');
    txt.append(mk('div', 'fly-name', displayName(p)), mk('div', 'fly-sub', hasCharacter(p) ? `${(p.side || '').toUpperCase()} · Level ${p.currlvl ?? 0}` : 'No character yet'));
    it.append(tile(displayName(p), 'ptile sm'), txt);
    it.onclick = () => { setActive(p.username); fl.hidden = true; };
    fl.appendChild(it);
  }
  fl.appendChild(mk('div', 'fly-sep'));
  const create = mk('button', 'fly-item fly-action', '+  Create profile…');
  create.onclick = () => { fl.hidden = true; openCreate(); };
  const manage = mk('button', 'fly-item fly-action', 'Manage profiles');
  manage.onclick = () => show('profiles');
  fl.append(create, manage);
}

$('#rail-profile').onclick = (e) => {
  e.stopPropagation();
  const fl = $('#profile-flyout');
  fl.hidden = !fl.hidden;
  if (!fl.hidden) renderFlyout();
};
document.addEventListener('click', (e) => {
  const fl = $('#profile-flyout');
  if (!fl.hidden && !fl.contains(e.target) && !$('#rail-profile').contains(e.target)) fl.hidden = true;
});

// --- profiles page: list + preview ---
function renderProfiles() {
  const box = $('#profiles');
  box.replaceChildren();
  if (!status || !status.online) {
    box.appendChild(mk('div', 'card empty', status && status.owned ? 'The SPT server is starting. Your profiles appear when the Server page shows Online.' : 'Start the SPT server to see your profiles.'));
    $('#profile-detail').replaceChildren();
    $('#playbar').hidden = true;
    $('#btn-new-profile').disabled = true;
    return;
  }
  $('#btn-new-profile').disabled = false;
  if (!profiles.length) box.appendChild(mk('div', 'card empty', 'No profiles on this server yet. Press "Create profile".'));
  for (const p of profiles) {
    const invalid = !!p.invalidOrUnloadableProfile;
    const c = mk('div', 'card interactive profile' + (p.username === selected ? ' selected' : '') + (invalid ? ' invalid' : ''));
    const span = (p.nextlvl ?? 0) - (p.prevexp ?? 0);
    const pct = span > 0 ? Math.max(0, Math.min(100, (((p.currexp ?? 0) - (p.prevexp ?? 0)) / span) * 100)) : 0;
    const row = mk('div', 'card-row');
    const left = mk('div'); left.style.minWidth = '0';
    left.append(mk('div', 'name', displayName(p)),
      mk('div', 'sub', invalid ? 'Profile cannot be loaded' : hasCharacter(p) ? `${(p.side || '').toUpperCase()} · Level ${p.currlvl ?? 0}` : 'No character created yet'));
    row.append(tile(displayName(p), 'ptile sm'), left, mk('span', 'edition', p.edition || ''));
    const xp = mk('div', 'xp'); const bar = mk('div'); bar.style.width = pct + '%'; xp.appendChild(bar);
    c.append(row, xp);
    if (!invalid) c.onclick = () => setActive(p.username);
    box.appendChild(c);
  }
  const sel = profiles.find((p) => p.username === selected);
  renderDetail(sel);
  $('#playbar').hidden = !sel;
  if (sel) $('#play-name').textContent = displayName(sel) + ' (' + sel.username + ')';
}

const STAT_ROWS = [['Raids', 'Raids'], ['RunThrough', 'Run Through'], ['Survived', 'Survived'], ['AWOL', 'AWOL'], ['KIA', 'KIA'], ['MIA', 'MIA'], ['Kills', 'Kills'],
  ['KD', 'KD'], ['SurvivalRate', 'Survival Rate'], ['LeaveRate', 'Leave Rate'], ['AverageLifeSpan', 'Average Life Span'], ['SurvivalsInARow', 'Survivals in a row'],
  ['Online', 'Online'], ['AccountLifetime', 'Account Lifetime']];

async function copyText(t) {
  try { await navigator.clipboard.writeText(t); return true; } catch { /* fall back below */ }
  const ta = document.createElement('textarea');
  ta.value = t; document.body.appendChild(ta); ta.select();
  let ok = false;
  try { ok = document.execCommand('copy'); } catch { ok = false; }
  ta.remove();
  return ok;
}

// the preview of the selected profile, like the profile page of the SPT launcher
function renderDetail(p) {
  const box = $('#profile-detail');
  box.replaceChildren();
  if (!p) { if (profiles.length) box.appendChild(mk('div', 'card empty', 'Select a profile to preview it.')); return; }
  const invalid = !!p.invalidOrUnloadableProfile;
  const char = hasCharacter(p);

  // header
  const head = mk('div', 'card pd-head');
  const info = mk('div', 'pd-info');
  info.append(mk('div', 'pd-name', char ? p.nickname : 'No character created yet'),
    mk('div', 'muted', [char ? (p.side || '').toUpperCase() : '', p.edition || ''].filter(Boolean).join(' · ')),
    mk('div', 'muted small mono', `${p.username}  ·  ID ${p.profileId || '-'}`));
  const acts = mk('div', 'btns');
  const copy = mk('button', 'btn', 'Copy ID');
  copy.onclick = async () => toast((await copyText(p.profileId || '')) ? 'Copied the profile ID.' : 'Could not copy to the clipboard.', 'ok');
  acts.appendChild(copy);
  if (!p.wipe && !invalid) {
    const wipe = mk('button', 'btn danger-outline', 'Wipe profile');
    wipe.onclick = () => openWipe(p);
    acts.appendChild(wipe);
  }
  head.append(tile(displayName(p), 'ptile lg'), info, acts);
  box.appendChild(head);

  if (invalid) box.appendChild(mk('div', 'f-warn-box', 'The server marked this profile as invalid or unloadable.'));
  else if (p.wipe && char) box.appendChild(mk('div', 'f-warn-box', 'A wipe is pending: this profile is reset to a new character the next time you launch the game.'));

  // level
  if (char) {
    const lvl = p.currlvl ?? 0, prev = p.prevexp ?? 0, next = p.nextlvl, max = p.maxlvl ?? 79;
    const card = mk('div', 'card pd-level');
    const top = mk('div', 'card-row');
    top.appendChild(mk('div', 'pd-big', `Level ${lvl}`));
    if (lvl >= max || next == null) top.appendChild(mk('div', 'muted', 'Max level'));
    else top.appendChild(mk('div', 'muted', `${fmt((p.currexp ?? 0) - prev)} / ${fmt(next - prev)} EXP  ·  next level ${lvl + 1}`));
    card.appendChild(top);
    if (next != null && lvl < max) {
      const span = next - prev;
      const pct = span > 0 ? Math.max(0, Math.min(100, (((p.currexp ?? 0) - prev) / span) * 100)) : 0;
      const xp = mk('div', 'xp thick'); const bar = mk('div'); bar.style.width = pct + '%'; xp.appendChild(bar);
      card.appendChild(xp);
    }
    box.appendChild(card);
  }

  // currencies
  const cur = p.profileCurrency || {};
  const money = mk('div', 'card pd-money');
  for (const [sym, key] of [['₽', 'roubles'], ['$', 'dollars'], ['€', 'euros'], ['GP', 'gp']]) {
    if (key === 'gp' && !cur.gp) continue;
    const m = mk('div', 'pd-cur'); m.append(mk('span', 'muted', sym), mk('b', '', fmt(cur[key])));
    money.appendChild(m);
  }
  box.appendChild(money);

  // stats
  const stats = mk('div', 'card pd-stats');
  const tabs = mk('div', 'f-sub-tabs pd-tabs');
  for (const [key, label] of [['overall', 'Overall'], ['pmc', 'PMC'], ['scav', 'Scav']]) {
    const b = mk('button', statTab === key ? 'active' : '', label);
    b.onclick = () => { statTab = key; renderDetail(p); };
    tabs.appendChild(b);
  }
  stats.appendChild(tabs);
  const data = (p.profileStats || {})[statTab] || {};
  const grid = mk('div', 'stat-grid');
  for (const [key, label] of STAT_ROWS) {
    const cell = mk('div', 'stat');
    cell.append(mk('div', 'stat-label', label), mk('div', 'stat-value', data[key] != null && data[key] !== '' ? String(data[key]) : '-'));
    grid.appendChild(cell);
  }
  stats.appendChild(grid);
  box.appendChild(stats);
}

$('#btn-refresh').onclick = () => { refreshStatus().then(() => fetchProfiles()); };
window.addEventListener('focus', () => { if (status && status.online && Date.now() - lastFetch > 5000) fetchProfiles(true); });

// --- create profile ---
let profileTypes = {};
let chosenEdition = '';
async function openCreate() {
  if (!status || !status.online) { toast('Start the SPT server first: profiles are created on the running server.', 'warn'); return; }
  try { profileTypes = (await invoke('profile_types')) || {}; } catch (e) { toast(e, 'error'); return; }
  // every edition the server offers, listed with its description (the server returns them in no particular order)
  const ORDER = ['Standard', 'Left Behind', 'Prepare To Escape', 'Edge Of Darkness', 'Unheard', 'Tournament'];
  const names = Object.keys(profileTypes).sort((a, b) => {
    const ia = ORDER.indexOf(a), ib = ORDER.indexOf(b);
    return (ia < 0 ? 99 : ia) - (ib < 0 ? 99 : ib) || a.localeCompare(b);
  });
  const list = $('#cp-editions');
  list.replaceChildren();
  chosenEdition = names.includes('Standard') ? 'Standard' : names[0] || '';
  $('#cp-ed-label').textContent = `Edition (${names.length} available)`;
  for (const k of names) {
    const it = mk('button', 'ed-item' + (k === chosenEdition ? ' on' : ''));
    it.type = 'button';
    it.setAttribute('role', 'option');
    it.dataset.edition = k;
    it.append(mk('div', 'ed-name', k), mk('div', 'ed-desc', profileTypes[k] || ''));
    it.onclick = () => {
      chosenEdition = k;
      list.querySelectorAll('.ed-item').forEach((x) => x.classList.toggle('on', x === it));
    };
    list.appendChild(it);
  }
  $('#cp-name').value = '';
  $('#cp-go').disabled = true;
  $('#create-dlg').hidden = false;
  $('#cp-name').focus();
}
$('#btn-new-profile').onclick = openCreate;
$('#cp-name').addEventListener('input', () => { $('#cp-go').disabled = !$('#cp-name').value.trim(); });
$('#cp-name').addEventListener('keydown', (e) => {
  if (e.key === 'Enter' && !$('#cp-go').disabled) $('#cp-go').click();
  if (e.key === 'Escape') $('#create-dlg').hidden = true;
});
$('#cp-cancel').onclick = () => { $('#create-dlg').hidden = true; };
$('#cp-go').onclick = async () => {
  const name = $('#cp-name').value.trim();
  const edition = chosenEdition;
  if (!name || !edition) return;
  $('#cp-go').disabled = true;
  try {
    await invoke('profile_create', { username: name, edition });
    $('#create-dlg').hidden = true;
    toast(`Created profile ${name}.`, 'ok');
    await fetchProfiles();
    if (profiles.some((p) => p.username === name)) setActive(name);
    show('profiles');
  } catch (e) {
    toast(e, 'error');
    $('#cp-go').disabled = false;
  }
};

// --- wipe profile ---
function openWipe(p) {
  $('#wp-text').textContent = `This permanently deletes the progress of "${displayName(p)}" (${p.username}) and cannot be undone. The wipe runs the next time you launch the game.`;
  $('#wp-label').textContent = `Type ${p.username} to confirm`;
  const inp = $('#wp-confirm');
  inp.value = '';
  $('#wp-go').disabled = true;
  inp.oninput = () => { $('#wp-go').disabled = inp.value.trim() !== p.username; };
  inp.onkeydown = (e) => { if (e.key === 'Escape') $('#wipe-dlg').hidden = true; };
  $('#wp-cancel').onclick = () => { $('#wipe-dlg').hidden = true; };
  $('#wp-go').onclick = async () => {
    $('#wp-go').disabled = true;
    try {
      toast(await invoke('profile_wipe', { username: p.username }), 'ok');
    } catch (e) { toast(e, 'error'); }
    $('#wipe-dlg').hidden = true;
    await fetchProfiles();
  };
  $('#wipe-dlg').hidden = false;
  inp.focus();
}

$('#btn-play').onclick = async () => {
  if (!selected) return;
  $('#launch-step').textContent = 'Starting';
  $('#launch-overlay').hidden = false;
  try {
    const notes = await invoke('launch_game', { username: selected });
    toast('Game started.', 'ok');
    (notes || []).forEach((n) => toast(n, 'warn'));
  } catch (e) {
    toast(e, 'error');
  } finally {
    $('#launch-overlay').hidden = true;
  }
};

// ---------- settings ----------
async function loadSettings() {
  const s = await invoke('get_settings');
  $('#set-path').value = s.game_path || '';
  $('#set-minimize').checked = !!s.minimize_on_launch;
  $('#set-autostart').checked = s.auto_start_server !== false;
  activeName = s.last_profile || null;
  renderRail();
}

$('#btn-save').onclick = async () => {
  const current = await invoke('get_settings');
  const settings = { ...current, game_path: $('#set-path').value.trim(), minimize_on_launch: $('#set-minimize').checked, auto_start_server: $('#set-autostart').checked };
  try {
    await invoke('save_settings', { settings });
    toast('Settings saved.', 'ok');
    refreshStatus();
  } catch (e) { toast(e, 'error'); }
};

// ---------- start ----------
(async () => {
  listen('server-log', (e) => addLine(e.payload));
  listen('launch-step', (e) => ($('#launch-step').textContent = e.payload));
  listen('confirm-close', async () => {
    const ok = await ask('Close the launcher?', 'The SPT server was started from this launcher and stops when it closes. Close the game first.', 'Close');
    if (ok) invoke('quit_app');
  });
  for (const l of await invoke('server_log')) addLine(l);
  await loadSettings();
  await refreshStatus();
  setInterval(refreshStatus, 3000);
})();
