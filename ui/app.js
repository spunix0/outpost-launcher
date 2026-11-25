'use strict';
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const $ = (s) => document.querySelector(s);

let status = null;        // { online, owned, address }
let starting = false;     // start pressed, server not answering yet
let profiles = [];
let selected = null;      // username

// ---------- helpers ----------
function toast(msg, kind = '') {
  const t = document.createElement('div');
  t.className = 'toast ' + kind;
  t.textContent = String(msg);
  $('#toasts').appendChild(t);
  setTimeout(() => t.remove(), kind === 'error' ? 9000 : 5000);
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
  if (page === 'profiles') loadProfiles();
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
  if (status.online) starting = false;
  if (starting && !status.owned) starting = false; // process ended before it answered
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
    starting = true;
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

async function loadProfiles() {
  const box = $('#profiles');
  if (!status || !status.online) {
    box.innerHTML = '';
    const e = document.createElement('div');
    e.className = 'card empty';
    e.textContent = 'Start the SPT server to see your profiles.';
    box.appendChild(e);
    $('#playbar').hidden = true;
    return;
  }
  try {
    profiles = (await invoke('list_profiles')) || [];
  } catch (e) { toast(e, 'error'); return; }
  const settings = await invoke('get_settings');
  if (!selected) selected = settings.last_profile;
  if (!profiles.some((p) => p.username === selected)) selected = profiles.length === 1 ? profiles[0].username : null;
  renderProfiles();
}

function renderProfiles() {
  const box = $('#profiles');
  box.innerHTML = '';
  if (!profiles.length) {
    const e = document.createElement('div');
    e.className = 'card empty';
    e.textContent = 'No profiles on this server yet. Create one with the official SPT launcher for now.';
    box.appendChild(e);
  }
  for (const p of profiles) {
    const c = document.createElement('div');
    const invalid = !!p.invalidOrUnloadableProfile;
    c.className = 'card interactive profile' + (p.username === selected ? ' selected' : '') + (invalid ? ' invalid' : '');
    const side = (p.side || '').toUpperCase();
    const lvl = p.currlvl ?? 0;
    const span = (p.nextlvl ?? 0) - (p.prevexp ?? 0);
    const pct = span > 0 ? Math.max(0, Math.min(100, (((p.currexp ?? 0) - (p.prevexp ?? 0)) / span) * 100)) : 0;
    const roubles = p.profileCurrency ? p.profileCurrency.roubles : undefined;

    const mark = document.createElement('div'); mark.className = 'side-mark'; mark.textContent = side || 'SPT';
    const row = document.createElement('div'); row.className = 'card-row';
    const left = document.createElement('div'); left.style.minWidth = '0';
    const name = document.createElement('div'); name.className = 'name'; name.textContent = p.nickname || p.username;
    const sub = document.createElement('div'); sub.className = 'sub';
    sub.textContent = invalid ? 'Profile cannot be loaded' : (side ? side + ' · ' : '') + 'Level ' + lvl + (p.nickname && p.nickname !== p.username ? ' · ' + p.username : '');
    left.append(name, sub);
    const ed = document.createElement('span'); ed.className = 'edition'; ed.textContent = p.edition || '';
    row.append(left, ed);
    const xp = document.createElement('div'); xp.className = 'xp';
    const bar = document.createElement('div'); bar.style.width = pct + '%'; xp.appendChild(bar);
    const stats = document.createElement('div'); stats.className = 'stats';
    const s1 = document.createElement('span'); s1.textContent = '₽ ' + fmt(roubles);
    const s2 = document.createElement('span'); s2.textContent = p.wipe ? 'Fresh profile' : 'XP ' + fmt(p.currexp);
    stats.append(s1, s2);
    c.append(mark, row, xp, stats);
    if (!invalid) c.onclick = () => { selected = p.username; renderProfiles(); };
    box.appendChild(c);
  }
  const sel = profiles.find((p) => p.username === selected);
  $('#playbar').hidden = !sel;
  if (sel) $('#play-name').textContent = (sel.nickname || sel.username) + ' (' + sel.username + ')';
}

$('#btn-refresh').onclick = () => { refreshStatus().then(loadProfiles); };

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
}

$('#btn-save').onclick = async () => {
  const current = await invoke('get_settings');
  const settings = { ...current, game_path: $('#set-path').value.trim(), minimize_on_launch: $('#set-minimize').checked };
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
