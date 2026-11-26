'use strict';
// SPT versions tab: your installs (switch / remove), the official releases, and the installer with progress.
// Uses invoke / listen / $ / mk / toast / ask from app.js.
(() => {
  const S = { installs: [], root: '', releases: [], totalReleases: 0, loadedReleases: false, busy: false };
  const PHASES = [['download', 'Download and verify the SPT release'], ['clone', 'Set up the game files'], ['extract', 'Unpack SPT']];
  const gb = (b) => (b / 1073741824).toFixed(1);
  const mb = (b) => Math.round(b / 1048576) + ' MB';
  const pill = (t, cls) => mk('span', 'vpill ' + (cls || ''), t);

  async function loadInstalls() {
    const r = await invoke('installs_list');
    S.installs = r.installs || [];
    S.root = r.root || '';
    $('#ver-root').value = S.root;
  }

  async function loadReleases() {
    $('#ver-note').textContent = 'Loading the release list from GitHub…';
    try {
      const r = await invoke('versions_available');
      S.releases = r.releases || [];
      S.totalReleases = r.total_releases || 0;
      S.loadedReleases = true;
      $('#ver-note').textContent = `${S.releases.length} of ${S.totalReleases} SPT releases have a downloadable archive; the others are not listed. ` +
        'An SPT release contains only SPT: the Tarkov client (the game itself) must come from files you already have, and it must be the exact build the release needs.';
    } catch (e) {
      S.loadedReleases = false;
      $('#ver-note').textContent = String(e);
    }
  }

  // ---------- downloaded archives ----------
  async function loadDownloads() {
    try {
      const d = await invoke('downloads_info');
      $('#ver-dl-info').textContent = d.count
        ? `${d.count} ${d.count === 1 ? 'file' : 'files'}, ${mb(d.bytes)} in ${d.path}`
        : `Nothing downloaded (${d.path}).`;
      $('#ver-dl-clear').disabled = !d.count;
      $('#ver-dl-clear').textContent = d.count ? `Clear downloads (${mb(d.bytes)})` : 'Clear downloads';
      $('#ver-dl-keep').checked = !!d.keep;
    } catch { /* leave the card as it was */ }
  }
  $('#ver-dl-clear').onclick = async () => {
    if (!(await ask('Delete the downloaded archives?', 'This only deletes the cached SPT release files. Installs you already made are not affected; installing that version again would download it again.', 'Delete'))) return;
    try {
      const r = await invoke('downloads_clear');
      toast(`Deleted ${r.count} ${r.count === 1 ? 'file' : 'files'} (${mb(r.bytes)}).`, 'ok');
    } catch (e) { toast(e, 'error'); }
    loadDownloads();
  };
  $('#ver-dl-keep').onchange = () => invoke('downloads_set_keep', { keep: $('#ver-dl-keep').checked }).catch((e) => toast(e, 'error'));

  async function open() {
    loadDownloads();
    try { await loadInstalls(); } catch (e) { toast(e, 'error'); }
    render();
    await loadReleases();
    render();
  }

  // ---------- installs ----------
  function render() { renderInstalls(); renderReleases(); }

  function renderInstalls() {
    const box = $('#ver-installs');
    box.replaceChildren();
    if (!S.installs.length) box.appendChild(mk('div', 'card empty', 'No installs yet. Set your SPT folder in Settings first.'));
    for (const i of S.installs) {
      const c = mk('div', 'card ver-card' + (i.active ? ' active' : ''));
      const head = mk('div', 'card-row');
      const left = mk('div', 'ver-title');
      left.append(mk('div', 'ver-name', i.name), mk('div', 'muted small', [i.spt_version ? 'SPT ' + i.spt_version : 'SPT version unknown', i.eft_version ? 'Tarkov ' + i.eft_version : ''].filter(Boolean).join('  ·  ')));
      const badges = mk('div', 'ver-badges');
      if (i.active) badges.appendChild(pill('Active', 'green'));
      if (!i.exists) badges.appendChild(pill('Folder missing', 'red'));
      else if (i.generation === 3) badges.appendChild(pill('Older SPT (3.x)', 'amber'));
      head.append(left, badges);
      c.appendChild(head);
      c.appendChild(mk('div', 'muted small mono ver-path', i.path));
      if (i.exists && i.generation === 3) c.appendChild(mk('div', 'f-warn-box', 'This is an older SPT generation with a different server and launcher. Start and manage it with its own launcher (button below); this launcher cannot run it.'));
      if (i.exists && i.generation === 0) c.appendChild(mk('div', 'f-warn-box', 'This folder does not look like an SPT install.'));
      const btns = mk('div', 'btns ver-btns');
      if (!i.active && i.switchable) { const b = mk('button', 'btn primary', 'Switch to this'); b.onclick = () => doSwitch(i); btns.appendChild(b); }
      if (i.exists) {
        const o = mk('button', 'btn', 'Open folder'); o.onclick = () => invoke('install_open_folder', { id: i.id }).catch((e) => toast(e, 'error')); btns.appendChild(o);
      }
      if (i.exists && i.generation && !i.switchable) {
        const l = mk('button', 'btn', 'Start its own launcher'); l.onclick = () => invoke('install_open_official', { id: i.id }).catch((e) => toast(e, 'error')); btns.appendChild(l);
      }
      if (i.own && !i.active) { const r = mk('button', 'btn danger-outline', 'Remove'); r.onclick = () => doRemove(i); btns.appendChild(r); }
      if (!i.exists) { const r = mk('button', 'btn danger-outline', 'Remove from list'); r.onclick = () => doRemove(i); btns.appendChild(r); }
      c.appendChild(btns);
      box.appendChild(c);
    }
  }

  async function doSwitch(i) {
    try {
      await invoke('install_switch', { id: i.id });
      toast(`Switched to ${i.name}. Reloading…`, 'ok');
      setTimeout(() => location.reload(), 700);
    } catch (e) {
      const s = String(e);
      if (s.startsWith('STOP_SERVER|')) {
        if (await ask('Stop the SPT server?', s.slice(12) + '\n\nClose the game first: stopping the server loses progress that was not saved yet.', 'Stop and switch')) {
          try { await invoke('server_stop'); } catch (x) { toast(x, 'error'); return; }
          setTimeout(() => doSwitch(i), 1200);
        }
      } else toast(s, 'error');
    }
  }

  // asks the user to type a word before something destructive happens
  function typed(title, text, word, yes) {
    return new Promise((resolve) => {
      $('#td-title').textContent = title;
      $('#td-text').textContent = text;
      $('#td-label').textContent = `Type ${word} to confirm`;
      $('#td-go').textContent = yes;
      const inp = $('#td-input');
      inp.value = '';
      $('#td-go').disabled = true;
      inp.oninput = () => { $('#td-go').disabled = inp.value.trim() !== word; };
      const done = (v) => { $('#typed-dlg').hidden = true; resolve(v); };
      inp.onkeydown = (e) => { if (e.key === 'Escape') done(false); };
      $('#td-cancel').onclick = () => done(false);
      $('#td-go').onclick = () => done(true);
      $('#typed-dlg').hidden = false;
      inp.focus();
    });
  }

  async function doRemove(i) {
    const text = i.exists
      ? `This deletes the folder ${i.path} with everything in it: its mods, profiles and logs.\n\nYour saved profiles are copied to ${S.root}\\_removed first. Game files shared with another install stay as long as that install uses them. Close the game before removing.`
      : `The folder ${i.path} no longer exists. This only removes the entry from the list.`;
    if (!(await typed(i.exists ? 'Remove this install?' : 'Remove from the list?', text, i.name, i.exists ? 'Yes, delete it' : 'Remove from list'))) return;
    $('#ver-run').hidden = true;
    toast('Removing… this can take a minute for a large folder.');
    try { toast(await invoke('install_remove', { id: i.id }), 'ok'); } catch (e) { toast(e, 'error'); }
    try { await loadInstalls(); } catch { /* keep the old list */ }
    render();
  }

  $('#ver-root-save').onclick = async () => {
    try { await invoke('installs_set_root', { path: $('#ver-root').value }); toast('Installs folder saved.', 'ok'); await loadInstalls(); } catch (e) { toast(e, 'error'); }
  };
  $('#ver-refresh').onclick = () => open();

  // ---------- releases ----------
  function renderReleases() {
    const box = $('#ver-releases');
    box.replaceChildren();
    if (!S.loadedReleases) return;
    for (const r of S.releases) {
      const have = S.installs.some((i) => i.spt_version === r.version);
      const donors = S.installs.filter((i) => i.exists && i.eft_build && i.eft_build === r.build);
      const row = mk('div', 'card ver-rel');
      const info = mk('div', 'ver-rel-info');
      info.append(mk('div', 'ver-name', 'SPT ' + r.version),
        mk('div', 'muted small', `${r.published}  ·  ${mb(r.size)}  ·  needs Tarkov client build ${r.build || 'unknown'}${r.requires ? ' (' + r.requires + ')' : ''}`));
      const side = mk('div', 'ver-rel-side');
      if (have) side.appendChild(pill('Installed', 'green'));
      if (!r.build) side.appendChild(pill('Client unknown', 'amber'));
      else if (donors.length) side.appendChild(pill('Your game files fit', 'cyan'));
      else side.appendChild(pill('Needs other game files', 'amber'));
      const b = mk('button', 'btn' + (donors.length ? ' primary' : ''), 'Install…');
      b.disabled = !r.build || S.busy;
      b.onclick = () => openInstall(r, donors);
      side.appendChild(b);
      row.append(info, side);
      box.appendChild(row);
    }
  }

  // ---------- installer dialog ----------
  let current = null;
  function openInstall(r, donors) {
    current = { r, donors };
    $('#vd-title').textContent = `Install SPT ${r.version}`;
    $('#vd-sub').textContent = `Downloads ${r.asset_name} (${mb(r.size)}) from the official SPT release on GitHub and checks it against GitHub's checksum.`;
    $('#vd-name').value = r.version;
    const sel = $('#vd-source');
    sel.replaceChildren();
    for (const d of donors) sel.appendChild(new Option(`${d.name}  (Tarkov ${d.eft_version})`, 'id:' + d.id));
    sel.appendChild(new Option('Another folder with my own game files…', 'path'));
    sel.value = donors.length ? 'id:' + donors[0].id : 'path';
    const upd = () => {
      const custom = sel.value === 'path';
      $('#vd-path').hidden = !custom;
      $('#vd-source-note').textContent = custom
        ? `Type the folder of a Tarkov client that is exactly build ${r.build}${r.requires ? ' (' + r.requires + ')' : ''}. The launcher checks the build and refuses a different one. Nothing in that folder is changed.`
        : donors.length ? 'This install has the exact client build this SPT release needs. Nothing in it is changed.' : '';
      $('#vd-warn').textContent = donors.length ? '' : `None of your installs has client build ${r.build}, so you must point to a folder with your own copy of it. This launcher cannot download Tarkov game files.`;
      $('#vd-share').closest('label').style.display = custom || donors.length ? '' : 'none';
    };
    sel.onchange = upd;
    upd();
    $('#vd-path').value = '';
    const target = () => { $('#vd-target').textContent = `${S.root}\\${$('#vd-name').value.trim() || '…'}`; };
    $('#vd-name').oninput = target;
    target();
    if (r.version && /^3\./.test(r.version)) $('#vd-warn').textContent += ' SPT 3.x is an older generation: it will be installed, but started with its own launcher.';
    $('#ver-dlg').hidden = false;
    $('#vd-name').focus();
  }
  $('#vd-cancel').onclick = () => { $('#ver-dlg').hidden = true; };
  $('#vd-go').onclick = async () => {
    const { r } = current;
    const sel = $('#vd-source').value;
    const args = {
      tag: r.tag, url: r.url, asset_name: r.asset_name, size: r.size, sha256: r.sha256 || '', build: r.build,
      name: $('#vd-name').value.trim(),
      source_id: sel.startsWith('id:') ? sel.slice(3) : null,
      source_path: sel === 'path' ? $('#vd-path').value.trim() : null,
      share: $('#vd-share').checked,
    };
    if (!args.name) { toast('Enter a folder name.', 'error'); return; }
    if (sel === 'path' && !args.source_path) { toast('Enter the folder with your game files.', 'error'); return; }
    $('#ver-dlg').hidden = true;
    await runInstall(r, args);
  };

  // ---------- progress ----------
  function showRun(r) {
    $('#vr-title').textContent = `Installing SPT ${r.version}`;
    const steps = $('#vr-steps');
    steps.replaceChildren();
    for (const [key, label] of PHASES) { const s = mk('div', 'ver-step', label); s.dataset.phase = key; steps.appendChild(s); }
    $('#vr-bar').style.width = '0%';
    $('#vr-note').textContent = 'Starting';
    $('#vr-cancel').disabled = false;
    $('#ver-run').hidden = false;
  }

  listen('ver-progress', (e) => {
    const p = e.payload;
    const order = PHASES.map((x) => x[0]);
    const at = order.indexOf(p.phase);
    document.querySelectorAll('#vr-steps .ver-step').forEach((s, i) => {
      s.classList.toggle('done', p.phase === 'finish' || i < at);
      s.classList.toggle('now', i === at && p.phase !== 'finish');
    });
    const pct = p.total ? Math.min(100, (p.done / p.total) * 100) : 0;
    $('#vr-bar').style.width = pct.toFixed(1) + '%';
    let note = p.note || '';
    if (p.phase === 'download' && p.total) note = (p.total < 1073741824 ? `${Math.round(p.done / 1048576)} / ${Math.round(p.total / 1048576)} MB` : `${gb(p.done)} / ${gb(p.total)} GB`) + `  ·  ${note}`;
    else if (p.phase === 'extract' && p.total) note = `${Math.round(p.done / 1048576)} / ${Math.round(p.total / 1048576)} MB unpacked`;
    else if (p.phase === 'clone' && p.total) note = `${p.done} / ${p.total} files  ·  ${note}`;
    $('#vr-note').textContent = note;
  });

  $('#vr-cancel').onclick = () => { invoke('install_cancel'); $('#vr-cancel').disabled = true; $('#vr-note').textContent = 'Cancelling…'; };

  async function runInstall(r, args) {
    S.busy = true;
    showRun(r);
    try {
      toast(await invoke('install_version', { args }), 'ok');
    } catch (e) {
      const s = String(e);
      toast(s, /^Cancelled/.test(s) ? '' : 'error');
    } finally {
      S.busy = false;
      $('#ver-run').hidden = true;
    }
    try { await loadInstalls(); } catch { /* keep the old list */ }
    loadDownloads();
    render();
  }

  window.Versions = { open };
})();
