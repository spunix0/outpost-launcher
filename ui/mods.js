'use strict';
// Mod browser for The Forge (sp-mod.com/api/v0). Uses invoke/$/toast/ask from app.js.
(() => {
  const F = (path) => invoke('forge_get', { path });
  const S = {
    inited: false, spt: '', page: 1, seq: 0, category: null, cats: new Map(),
    local: new Map(),        // lower guid -> { guid, version, source }
    trackedIds: new Set(),   // Forge mod ids installed by this launcher
    scan: null, forgeByGuid: new Map(), updates: [], updatesChecked: false,
  };

  // ---------- small helpers ----------
  function el(tag, cls, text) { const e = document.createElement(tag); if (cls) e.className = cls; if (text != null) e.textContent = text; return e; }
  function num(n) { return typeof n !== 'number' ? '-' : n >= 1e6 ? (n / 1e6).toFixed(1) + 'M' : n >= 1e3 ? (n / 1e3).toFixed(1) + 'k' : String(n); }
  function size(b) { return !b ? '' : b >= 1048576 ? (b / 1048576).toFixed(1) + ' MB' : Math.max(1, Math.round(b / 1024)) + ' KB'; }
  function ago(iso) {
    const t = Date.parse(iso); if (!t) return '';
    const d = (Date.now() - t) / 1000;
    for (const [s, n] of [[31536000, 'year'], [2592000, 'month'], [86400, 'day'], [3600, 'hour'], [60, 'minute']]) {
      if (d >= s) { const v = Math.floor(d / s); return v + ' ' + n + (v > 1 ? 's' : '') + ' ago'; }
    }
    return 'just now';
  }
  function initials(n) {
    const words = (n || '?').split(/[^A-Za-z0-9]+/).filter(Boolean);
    return (words.length ? words.slice(0, 2).map((w) => w[0]).join('') : '?').toUpperCase();
  }
  // Forge's file server only serves its thumbnails to sp-mod.com pages (hotlink protection), so every mod gets an icon for its
  // category on a tile coloured by that category.
  const CAT_ICON = {
    audio: ['🔊', 195], bots: ['🤖', 150], clothing: ['👕', 285], equipment: ['🎒', 35], hideout: ['🏠', 25], items: ['📦', 45],
    locales: ['🌐', 205], locations: ['🗺️', 120], models: ['🧊', 175], other: ['✨', 260], overhauls: ['🧩', 330],
    quests: ['📜', 55], retextures: ['🎨', 310], scripting: ['📝', 215], tools: ['🔧', 0], traders: ['🛒', 90], ui: ['🖥️', 235], weapons: ['🔫', 10],
  };
  function thumb(mod, name, cls = 'f-thumb') {
    const title = mod && ((mod.category && mod.category.title) || S.cats.get(mod.category_id));
    const [glyph, hue] = CAT_ICON[(title || '').toLowerCase()] || ['🔌', 220];
    const d = el('div', cls, glyph);
    d.title = title || name || '';
    d.style.background = `linear-gradient(160deg, hsl(${hue} 45% 30%), hsl(${(hue + 25) % 360} 50% 15%))`;
    return d;
  }
  function pill(text, color = '') { return el('span', 'f-pill ' + color, text); }
  function button(text, cls, fn) { const b = el('button', 'btn ' + (cls || ''), text); b.onclick = (e) => { e.stopPropagation(); fn(); }; return b; }
  function authors(m) { return [m.owner && m.owner.name, ...(m.additional_authors || []).map((a) => a.name)].filter(Boolean).join(', '); }
  function openUrl(url) { invoke('open_url', { url }).catch((e) => toast(e, 'error')); }
  function verText(v) {
    if (v == null) return '';
    if (typeof v === 'string') return v;
    const ma = v.Major ?? v.major, mi = v.Minor ?? v.minor, pa = v.Patch ?? v.patch;
    return ma != null ? `${ma}.${mi ?? 0}.${pa ?? 0}` : String(v);
  }
  const installedVer = (m) => (m && m.guid && S.local.get(m.guid.toLowerCase())) ? S.local.get(m.guid.toLowerCase()).version : null;
  const isInstalled = (m) => S.trackedIds.has(m.id) || installedVer(m) != null;

  // ---------- safe HTML for descriptions / changelogs ----------
  const KEEP = new Set(['P', 'BR', 'STRONG', 'B', 'EM', 'I', 'U', 'S', 'DEL', 'INS', 'MARK', 'SMALL', 'UL', 'OL', 'LI', 'A', 'CODE', 'PRE', 'BLOCKQUOTE',
    'H1', 'H2', 'H3', 'H4', 'H5', 'H6', 'IMG', 'TABLE', 'THEAD', 'TBODY', 'TR', 'TH', 'TD', 'HR', 'SPAN', 'DIV', 'DETAILS', 'SUMMARY', 'SUP', 'SUB']);
  const DROP = new Set(['SCRIPT', 'STYLE', 'IFRAME', 'OBJECT', 'EMBED', 'FORM', 'INPUT', 'BUTTON', 'TEXTAREA', 'SELECT', 'LINK', 'META', 'SVG', 'VIDEO', 'AUDIO', 'BASE']);
  function clean(node) {
    for (const c of [...node.childNodes]) {
      if (c.nodeType === 8) { c.remove(); continue; }
      if (c.nodeType !== 1) continue;
      if (DROP.has(c.tagName)) { c.remove(); continue; }
      let n = c;
      if (!KEEP.has(c.tagName)) { n = document.createElement('span'); while (c.firstChild) n.appendChild(c.firstChild); c.replaceWith(n); }
      for (const a of [...n.attributes]) {
        const k = a.name.toLowerCase();
        const keep = (k === 'href' && n.tagName === 'A') || (k === 'src' && n.tagName === 'IMG') || ['alt', 'title', 'colspan', 'rowspan'].includes(k);
        if (!keep) n.removeAttribute(a.name);
      }
      if (n.tagName === 'A' && !/^https?:\/\//i.test(n.getAttribute('href') || '')) n.removeAttribute('href');
      if (n.tagName === 'IMG') {
        const src = n.getAttribute('src') || '';
        // files.sp-mod.com refuses other sites (hotlink protection): leave those out instead of showing broken images
        if (!/^https:\/\//i.test(src) || /^https:\/\/files\.sp-mod\.com\//i.test(src)) { n.remove(); continue; }
        n.setAttribute('loading', 'lazy');
        n.setAttribute('referrerpolicy', 'no-referrer');
      }
      clean(n);
    }
  }
  function setHtml(box, raw) {
    const doc = new DOMParser().parseFromString(raw || '', 'text/html');
    clean(doc.body);
    box.replaceChildren(...doc.body.childNodes);
    box.querySelectorAll('a[href]').forEach((a) => (a.onclick = (e) => { e.preventDefault(); openUrl(a.href); }));
  }

  // ---------- tabs ----------
  function tab(name) {
    for (const t of ['browse', 'detail', 'installed', 'updates']) $('#tab-' + t).hidden = t !== name;
    document.querySelectorAll('.f-tabs button').forEach((b) => b.classList.toggle('active', b.dataset.tab === name || (name === 'detail' && b.dataset.tab === 'browse')));
    if (name === 'installed') renderInstalled();
    if (name === 'updates' && !S.updatesChecked) checkUpdates();
  }
  document.querySelectorAll('.f-tabs button').forEach((b) => (b.onclick = () => tab(b.dataset.tab)));

  // ---------- local scan ----------
  async function rescan() {
    try { S.scan = await invoke('mods_scan'); } catch (e) { toast(e, 'error'); return; }
    S.local.clear(); S.trackedIds.clear();
    if (S.scan.spt_version) S.spt = S.scan.spt_version;
    for (const p of S.scan.plugins || []) S.local.set(p.guid.toLowerCase(), { guid: p.guid, version: p.version, source: 'client' });
    for (const m of Object.values(S.scan.server_mods || {})) {
      const guid = m.ModGuid ?? m.modGuid, version = verText(m.Version ?? m.version);
      if (guid && !S.local.has(guid.toLowerCase())) S.local.set(guid.toLowerCase(), { guid, version, source: 'server' });
    }
    for (const t of S.scan.tracked || []) {
      S.trackedIds.add(t.mod_id);
      if (t.guid && !S.local.has(t.guid.toLowerCase())) S.local.set(t.guid.toLowerCase(), { guid: t.guid, version: t.version, source: 'tracked' });
    }
    computeMissing();
  }

  // ---------- missing dependencies ----------
  // Sources: [BepInDependency] attributes read from each plugin DLL, server mods' declared dependencies, and the
  // "missing dependencies" lines BepInEx wrote to its log. Anything already installed is dropped, so stale log lines vanish.
  function computeMissing() {
    const miss = new Map(); // lower guid -> { guid, by: Set }
    const add = (guid, by) => {
      const k = guid.toLowerCase();
      if (S.local.has(k)) return;
      if (!miss.has(k)) miss.set(k, { guid, by: new Set() });
      miss.get(k).by.add(by);
    };
    for (const p of S.scan.plugins || []) for (const d of p.deps || []) if (d.hard) add(d.guid, p.name);
    for (const m of Object.values(S.scan.server_mods || {})) {
      const deps = m.ModDependencies ?? m.modDependencies;
      if (deps && typeof deps === 'object') for (const g of Object.keys(deps)) add(g, m.Name ?? m.name ?? 'a server mod');
    }
    for (const l of S.scan.log_missing || []) for (const g of l.missing) add(g, l.name || 'a mod');
    S.missing = miss;
    const b = $('#inst-badge');
    b.hidden = !miss.size;
    b.textContent = String(miss.size);
  }

  function missingText() {
    return [...S.missing.values()].map((x) => `${x.guid} (needed by ${[...x.by].join(', ')})`).join('\n');
  }

  async function installMissing() {
    if (!S.missing || !S.missing.size) return;
    const need = [...S.missing.values()];
    const items = [];
    const unresolved = [];
    $('#mod-step').textContent = 'Looking up missing dependencies on The Forge';
    $('#mod-overlay').hidden = false;
    try {
      const found = new Map();
      for (let i = 0; i < need.length; i += 40) {
        const r = await F('/api/v0/mods?per_page=50&filter[include_legacy]=1&filter[guid]=' + encodeURIComponent(need.slice(i, i + 40).map((x) => x.guid).join(',')));
        for (const m of r.data || []) if (m.guid) found.set(m.guid.toLowerCase(), m);
      }
      for (const n of need) {
        const m = found.get(n.guid.toLowerCase());
        const by = [...n.by].join(', ');
        if (!m) { unresolved.push(`${n.guid} (needed by ${by}): not on The Forge`); continue; }
        const q = new URLSearchParams({ per_page: '1', sort: '-published_at' });
        if (S.spt) q.set('filter[spt_version]', S.spt);
        const v = ((await F(`/api/v0/mod/${m.id}/versions?` + q.toString())).data || [])[0];
        if (!v) { unresolved.push(`${m.name} (needed by ${by}): no version for SPT ${S.spt}`); continue; }
        items.push({ mod_id: m.id, guid: m.guid, name: m.name, version: v.version, link: v.link, kind: 'mod', size: v.content_length, dep: true, have: null, by });
      }
    } catch (e) {
      toast(e, 'error');
      return;
    } finally {
      $('#mod-overlay').hidden = true;
    }
    if (!items.length) {
      toast('None of the missing dependencies can be installed from The Forge:\n' + unresolved.join('\n'), 'warn');
      return;
    }
    const note = unresolved.length ? 'Not installable from The Forge, add these manually: ' + unresolved.join('; ') + '.' : '';
    const chosen = await confirmInstall({ name: 'missing dependencies' }, items, note);
    if (chosen && chosen.length) await runInstall(chosen, null);
  }

  let askedMissing = '';
  async function askAboutMissing() {
    if (!S.missing || !S.missing.size) return;
    const key = [...S.missing.keys()].sort().join('|');
    if (key === askedMissing) return;
    askedMissing = key;
    const ok = await ask(`${S.missing.size} missing ${S.missing.size === 1 ? 'dependency' : 'dependencies'}`,
      'These mods will not load until the following are installed:\n\n' + missingText() + '\n\nLook them up on The Forge and install them?', 'Install…', 'primary');
    if (ok) installMissing();
  }

  // ---------- browse ----------
  async function open() {
    if (S.inited) return;
    S.inited = true;
    try { S.spt = await invoke('spt_version'); } catch { S.spt = ''; }
    await Promise.all([loadSptVersions(), loadCats(), rescan()]);
    search(1);
    checkUpdates(true);
    askAboutMissing();
  }

  async function loadSptVersions() {
    const sel = $('#f-spt');
    sel.replaceChildren(new Option('Any version', ''));
    try {
      const r = await F('/api/v0/spt/versions?per_page=30&sort=-version');
      for (const v of r.data || []) sel.appendChild(new Option(`${v.version}${v.version === S.spt ? ' (yours)' : ''}`, v.version));
    } catch { /* offline: keep "any" */ }
    if (S.spt && [...sel.options].some((o) => o.value === S.spt)) sel.value = S.spt;
  }

  async function loadCats() {
    const box = $('#f-cats');
    box.replaceChildren();
    const all = el('button', 'active', 'All categories');
    all.onclick = () => pickCat(null, all);
    box.appendChild(all);
    try {
      const r = await F('/api/v0/mod-categories?per_page=100&sort=title');
      for (const c of r.data || []) {
        S.cats.set(c.id, c.title);
        const b = el('button', '', c.title);
        b.onclick = () => pickCat(c.id, b);
        box.appendChild(b);
      }
    } catch { /* ignore */ }
  }
  function pickCat(id, btn) {
    S.category = id;
    $('#f-cats').querySelectorAll('button').forEach((b) => b.classList.toggle('active', b === btn));
    search(1);
  }

  async function search(page) {
    S.page = page;
    const p = new URLSearchParams({ per_page: '24', page: String(page), sort: $('#f-sort').value });
    const q = $('#f-query').value.trim();
    if (q) p.set('query', q);
    if ($('#f-spt').value) p.set('filter[spt_version]', $('#f-spt').value);
    if (S.category) p.set('filter[category_id]', String(S.category));
    if ($('#f-featured').checked) p.set('filter[featured]', '1');
    if ($('#f-fika').checked) p.set('filter[fika_compatibility]', '1');
    if ($('#f-legacy').checked) p.set('filter[include_legacy]', '1');
    const seq = ++S.seq;
    $('#f-grid').replaceChildren(el('div', 'f-empty', 'Loading…'));
    try {
      const r = await F('/api/v0/mods?' + p.toString());
      if (seq === S.seq) renderGrid(r);
    } catch (e) {
      if (seq === S.seq) $('#f-grid').replaceChildren(el('div', 'f-empty', String(e)));
    }
  }

  function renderGrid(r) {
    const hide = $('#f-hide-installed').checked;
    const mods = (r.data || []).filter((m) => !(hide && isInstalled(m)));
    const hidden = (r.data || []).length - mods.length;
    const total = r.meta ? r.meta.total : mods.length;
    $('#f-count').textContent = `${num(total)} mods` + (hidden ? ` · ${hidden} installed hidden on this page` : '');
    const grid = $('#f-grid');
    grid.replaceChildren();
    if (!mods.length) grid.appendChild(el('div', 'f-empty', hidden ? 'Everything on this page is already installed.' : 'No mods match these filters.'));
    for (const m of mods) grid.appendChild(card(m));
    const pager = $('#f-pager');
    pager.replaceChildren();
    const last = r.meta ? r.meta.last_page : 1;
    if (last > 1) {
      const first = button('«', '', () => search(1)); first.disabled = S.page <= 1; first.title = 'First page';
      const prev = button('‹', '', () => search(S.page - 1)); prev.disabled = S.page <= 1; prev.title = 'Previous page';
      const next = button('›', '', () => search(S.page + 1)); next.disabled = S.page >= last; next.title = 'Next page';
      const end = button('»', '', () => search(last)); end.disabled = S.page >= last; end.title = 'Last page';
      const box = document.createElement('input');
      box.type = 'number'; box.min = '1'; box.max = String(last); box.value = String(S.page); box.className = 'f-page-input';
      box.title = 'Type a page number and press Enter';
      const go = () => { const n = Math.max(1, Math.min(last, parseInt(box.value, 10) || S.page)); if (n !== S.page) search(n); else box.value = String(S.page); };
      box.addEventListener('keydown', (e) => { if (e.key === 'Enter') go(); });
      box.addEventListener('change', go);
      const mid = el('span', 'muted small f-page-mid');
      mid.append(document.createTextNode('Page '), box, document.createTextNode(` of ${last}`));
      pager.append(first, prev, mid, next, end);
    }
  }

  function card(m) {
    const c = el('div', 'f-card');
    const body = el('div', 'f-body');
    body.append(el('div', 'f-name', m.name), el('div', 'f-by', 'by ' + (authors(m) || 'unknown')), el('div', 'f-teaser', m.teaser || ''));
    const meta = el('div', 'f-meta');
    meta.append(el('span', '', '⬇ ' + num(m.downloads)), el('span', '', ago(m.updated_at)));
    const cat = (m.category && m.category.title) || S.cats.get(m.category_id);
    if (cat) meta.appendChild(pill(cat));
    if (m.featured) meta.appendChild(pill('Featured', 'cyan'));
    const iv = installedVer(m);
    if (iv != null || S.trackedIds.has(m.id)) meta.appendChild(pill('Installed' + (iv ? ' ' + iv : ''), 'green'));
    if (m.contains_ads) meta.appendChild(pill('Ads', 'amber'));
    if (m.cheat_notice) meta.appendChild(pill('Cheat notice', 'red'));
    body.appendChild(meta);
    c.append(thumb(m, m.name), body);
    c.onclick = () => showMod(m.id);
    return c;
  }

  function setView(v) {
    $('#f-grid').classList.toggle('compact', v === 'compact');
    document.querySelectorAll('#f-view button').forEach((b) => b.classList.toggle('active', b.dataset.view === v));
    try { localStorage.setItem('outpost.modsView', v); } catch { /* not persisted */ }
  }
  document.querySelectorAll('#f-view button').forEach((b) => (b.onclick = () => setView(b.dataset.view)));
  let savedView = 'cards';
  try { savedView = localStorage.getItem('outpost.modsView') || 'cards'; } catch { /* default */ }
  setView(savedView);

  let typing;
  $('#f-query').addEventListener('input', () => { clearTimeout(typing); typing = setTimeout(() => search(1), 450); });
  for (const id of ['#f-sort', '#f-spt', '#f-featured', '#f-fika', '#f-legacy']) $(id).addEventListener('change', () => search(1));
  $('#f-hide-installed').addEventListener('change', () => search(S.page));

  // ---------- detail ----------
  async function versionsOf(id) {
    const base = `/api/v0/mod/${id}/versions?include=dependencies,virus_total_links&per_page=50`;
    try { return (await F(base + '&sort=-published_at')).data || []; } catch { return (await F(base)).data || []; }
  }

  async function showMod(id) {
    tab('detail');
    const box = $('#tab-detail');
    box.replaceChildren(el('div', 'f-empty', 'Loading…'));
    try {
      const [m, vs, addons] = await Promise.all([
        F(`/api/v0/mod/${id}?include=license,category,source_code_links`),
        versionsOf(id),
        F(`/api/v0/addons?filter[mod_id]=${id}&include=versions&per_page=50`).catch(() => null),
      ]);
      renderDetail(m.data, vs, (addons && addons.data) || []);
    } catch (e) {
      box.replaceChildren(button('‹ Back', '', () => tab('browse')), el('div', 'f-empty', String(e)));
    }
  }

  function renderDetail(m, vs, addons) {
    const box = $('#tab-detail');
    const wrap = el('div', 'f-detail');
    const top = el('div', 'f-bar');
    top.append(button('‹ Back to results', '', () => tab('browse')), el('span'));
    wrap.appendChild(top);

    const head = el('div', 'f-head');
    const info = el('div', 'f-body');
    info.append(el('h2', '', m.name), el('div', 'f-by', 'by ' + (authors(m) || 'unknown')), el('div', 'f-teaser', m.teaser || ''));
    const stats = el('div', 'f-stats');
    const stat = (label, v) => { const s = el('span'); const b = el('b', '', v); s.append(b, document.createTextNode(' ' + label)); stats.appendChild(s); };
    stat('downloads', num(m.downloads)); stat('favourites', num(m.favourites_count)); stat('endorsements', num(m.endorsements_count));
    stats.appendChild(el('span', '', 'Updated ' + ago(m.updated_at)));
    const cat = (m.category && m.category.title) || S.cats.get(m.category_id);
    if (cat) stats.appendChild(pill(cat));
    if (m.license && m.license.name) stats.appendChild(pill(m.license.name));
    const iv = installedVer(m);
    if (iv != null || S.trackedIds.has(m.id)) stats.appendChild(pill('Installed' + (iv ? ' ' + iv : ''), 'green'));
    info.appendChild(stats);
    const links = el('div', 'f-links');
    if (m.detail_url) links.appendChild(button('Open on The Forge', '', () => openUrl(m.detail_url)));
    for (const s of m.source_code_links || []) if (s.url) links.appendChild(button(s.label || 'Source code', '', () => openUrl(s.url)));
    info.appendChild(links);
    head.append(thumb(m, m.name), info);
    wrap.appendChild(head);

    if (m.cheat_notice) wrap.appendChild(el('div', 'f-warn', 'The Forge marks this mod with a cheat notice: it gives gameplay advantages.'));
    if (m.contains_ads) wrap.appendChild(el('div', 'f-warn', 'The author says this mod or its download contains advertising.'));
    if (m.shows_profile_binding_notice) wrap.appendChild(el('div', 'f-warn', 'Profile binding: this mod adds content to your profile. Removing it later can break that profile, so back up your profile first.'));

    const tabs = el('div', 'f-sub-tabs');
    const panel = el('div');
    const subs = [['Description', () => { const d = el('div', 'f-desc'); setHtml(d, m.description || '<p>No description.</p>'); return d; }],
      [`Versions (${vs.length})`, () => versionList(m, vs)],
      [`Addons (${addons.length})`, () => addonList(m, addons)]];
    subs.forEach(([label, make], i) => {
      const b = el('button', i === 0 ? 'active' : '', label);
      b.onclick = () => { tabs.querySelectorAll('button').forEach((x) => x.classList.toggle('active', x === b)); panel.replaceChildren(make()); };
      tabs.appendChild(b);
    });
    panel.appendChild(subs[0][1]());
    wrap.append(tabs, panel);
    box.replaceChildren(wrap);
    $('main').scrollTop = 0;
  }

  function versionList(m, vs) {
    const list = el('div', 'f-list');
    if (!vs.length) list.appendChild(el('div', 'f-empty', 'No versions published.'));
    const iv = installedVer(m);
    vs.forEach((v, i) => {
      const box = el('div', 'f-version');
      const head = el('div', 'f-version-head');
      const left = el('div', 'f-meta');
      left.style.marginTop = '0';
      left.append(el('span', 'v', v.version));
      if (i === 0) left.appendChild(pill('Latest', 'cyan'));
      if (v.spt_version_constraint) left.appendChild(pill('SPT ' + v.spt_version_constraint, 'green'));
      if (v.fika_compatibility && v.fika_compatibility !== 'unknown') left.appendChild(pill('Fika ' + v.fika_compatibility, v.fika_compatibility === 'compatible' ? 'cyan' : 'amber'));
      left.append(el('span', '', size(v.content_length)), el('span', '', '⬇ ' + num(v.downloads)), el('span', '', ago(v.published_at)));
      const label = iv == null ? 'Install' : iv === v.version ? 'Reinstall' : 'Install this version';
      head.append(left, button(label, i === 0 ? 'primary' : '', () => install(m, v, 'mod')));
      box.appendChild(head);
      const deps = v.dependencies || [];
      if (deps.length) {
        const d = el('div', 'f-deps', 'Requires: ');
        deps.forEach((x, k) => {
          const a = el('a', '', x.name);
          a.onclick = () => showMod(x.id);
          d.appendChild(a);
          if (k < deps.length - 1) d.appendChild(document.createTextNode(', '));
        });
        box.appendChild(d);
      }
      const vt = v.virus_total_links || [];
      if (vt.length) {
        const d = el('div', 'f-deps', 'VirusTotal: ');
        vt.forEach((x, k) => {
          const a = el('a', '', x.label || ('scan ' + (k + 1)));
          a.onclick = () => openUrl(x.url);
          d.appendChild(a);
          if (k < vt.length - 1) d.appendChild(document.createTextNode(', '));
        });
        box.appendChild(d);
      }
      if (v.description) { const d = el('div', 'f-desc'); setHtml(d, v.description); box.appendChild(d); }
      list.appendChild(box);
    });
    return list;
  }

  function addonList(m, addons) {
    const list = el('div', 'f-list');
    if (!addons.length) list.appendChild(el('div', 'f-empty', 'No addons for this mod.'));
    for (const a of addons) {
      const row = el('div', 'f-row');
      const body = el('div', 'f-body');
      body.append(el('div', 'f-name', a.name), el('div', 'f-teaser', a.teaser || ''));
      const v = (a.versions || [])[0];
      row.append(thumb(m, a.name), body);
      if (v) {
        const btns = el('div', 'btns');
        btns.append(el('span', 'muted small', v.version), button('Install', 'primary', () => install({ id: a.id, guid: '', name: a.name, detail_url: a.detail_url || m.detail_url }, v, 'addon')));
        row.appendChild(btns);
      }
      list.appendChild(row);
    }
    return list;
  }

  // ---------- install ----------
  async function install(m, v, kind) {
    const main = { mod_id: m.id, guid: m.guid || '', name: m.name, version: v.version, link: v.link, kind, size: v.content_length };
    const deps = [];
    let note = '';
    if (kind === 'mod') {
      try {
        const key = (m.guid || m.id) + ':' + v.version;
        const r = await F(`/api/v0/mods/dependencies?mods=${encodeURIComponent(key)}&spt_version=${encodeURIComponent(S.spt)}`);
        const tree = (r.data && (r.data[key] || Object.values(r.data)[0])) || [];
        const flat = [];
        const walk = (list) => { for (const d of list) { walk(d.dependencies || []); if (!flat.some((x) => x.id === d.id)) flat.push(d); } };
        walk(tree);
        for (const d of flat) {
          const lv = d.latest_compatible_version;
          if (!lv) { note += `No version of ${d.name} fits SPT ${S.spt}. `; continue; }
          deps.push({ mod_id: d.id, guid: d.guid || '', name: d.name, version: lv.version, link: lv.link, kind: 'mod', size: lv.content_length,
            dep: true, conflict: d.conflict, have: installedVer(d) });
        }
      } catch (e) { note += 'Dependencies could not be checked: ' + e + ' '; }
    }
    const chosen = await confirmInstall(m, [...deps, main], note);
    if (chosen && chosen.length) await runInstall(chosen, m.detail_url);
  }

  function confirmInstall(m, items, note) {
    return new Promise((resolve) => {
      $('#inst-title').textContent = 'Install ' + m.name;
      const box = $('#inst-items');
      box.replaceChildren();
      const checks = [];
      for (const it of items) {
        const row = el('label');
        const cb = document.createElement('input');
        cb.type = 'checkbox';
        cb.checked = !it.dep || it.have == null;
        if (!it.dep) cb.disabled = true;
        checks.push([cb, it]);
        const g = el('div', 'grow');
        g.append(el('div', '', `${it.name} ${it.version}`));
        const sub = [it.by ? 'needed by ' + it.by : it.dep ? 'dependency' : 'selected mod', size(it.size)];
        if (it.have != null) sub.push('installed: ' + it.have);
        if (it.conflict) sub.push('conflicts with an installed mod');
        g.appendChild(el('div', 'sub', sub.filter(Boolean).join(' · ')));
        row.append(cb, g);
        box.appendChild(row);
      }
      const warn = [];
      if (m.cheat_notice) warn.push('Cheat notice on The Forge.');
      if (m.contains_ads) warn.push('Contains advertising.');
      $('#inst-note').textContent = [note, ...warn,
        'Files come from each author\'s own host (for example GitHub) and unpack into BepInEx\\plugins and SPT_Runtime\\user\\mods.',
        'Overwritten files are backed up to OutpostLauncher\\backups. Close the game and stop the SPT server first. Only install mods you trust.'].filter(Boolean).join(' ');
      $('#install-dlg').hidden = false;
      const done = (v) => { $('#install-dlg').hidden = true; resolve(v); };
      $('#inst-cancel').onclick = () => done(null);
      $('#inst-go').onclick = () => done(checks.filter(([cb]) => cb.checked).map(([, it]) => it));
    });
  }

  async function runInstall(items, pageUrl) {
    $('#mod-step').textContent = 'Starting';
    $('#mod-overlay').hidden = false;
    try {
      const payload = items.map(({ mod_id, guid, name, version, link, kind }) => ({ mod_id, guid, name, version, link, kind }));
      const notes = await invoke('mod_install', { items: payload });
      notes.forEach((n) => toast(n, 'ok'));
      return true;
    } catch (e) {
      const s = String(e);
      const man = s.match(/MANUAL\|(\S+)/);
      $('#mod-overlay').hidden = true;
      if (man) {
        let host = man[1]; try { host = new URL(man[1]).host; } catch { /* keep */ }
        const what = s.split(': MANUAL')[0];
        if (await ask('Manual download needed', `${what} is offered on a web page (${host}), not as a direct file. Open its Forge page to download it yourself?`, 'Open page')) openUrl(pageUrl || man[1]);
      } else toast(s, 'error');
      return false;
    } finally {
      $('#mod-overlay').hidden = true;
      await rescan();
      if (!$('#tab-installed').hidden) renderInstalled();
    }
  }

  // ---------- installed ----------
  async function forgeMatch(guids) {
    const need = guids.filter((g) => !S.forgeByGuid.has(g.toLowerCase()));
    for (let i = 0; i < need.length; i += 40) {
      const part = need.slice(i, i + 40);
      try {
        const r = await F('/api/v0/mods?per_page=50&filter[include_legacy]=1&filter[guid]=' + encodeURIComponent(part.join(',')));
        for (const m of r.data || []) if (m.guid) S.forgeByGuid.set(m.guid.toLowerCase(), m);
      } catch { /* offline */ }
      for (const g of part) if (!S.forgeByGuid.has(g.toLowerCase())) S.forgeByGuid.set(g.toLowerCase(), null);
    }
  }

  function row(name, sub, fm, btns) {
    const r = el('div', 'f-row');
    const body = el('div', 'f-body');
    body.append(el('div', 'f-name', name), el('div', 'f-by', sub));
    r.append(thumb(fm, name), body);
    const b = el('div', 'btns');
    if (fm) b.appendChild(button('View', '', () => showMod(fm.id)));
    for (const x of btns || []) b.appendChild(x);
    r.appendChild(b);
    return r;
  }

  async function renderInstalled() {
    const list = $('#inst-list');
    if (!S.scan) await rescan();
    if (!S.scan) return;
    list.replaceChildren(el('div', 'f-empty', 'Matching with The Forge…'));
    const plugins = S.scan.plugins || [];
    const serverMods = Object.values(S.scan.server_mods || {});
    await forgeMatch([...plugins.map((p) => p.guid), ...serverMods.map((m) => m.ModGuid ?? m.modGuid).filter(Boolean)]);
    list.replaceChildren();
    const tracked = S.scan.tracked || [];
    $('#inst-summary').textContent = `${plugins.length} client plugins · ${serverMods.length || (S.scan.server_folders || []).length} server mods · ${tracked.length} installed with this launcher`;

    if (S.missing && S.missing.size) {
      const warn = el('div', 'f-warn f-missing');
      const t = el('div');
      t.append(el('b', '', `${S.missing.size} missing ${S.missing.size === 1 ? 'dependency' : 'dependencies'}: `), document.createTextNode('these mods will not load without them.'));
      warn.appendChild(t);
      const ul = el('ul');
      for (const x of S.missing.values()) ul.appendChild(el('li', '', `${x.guid} - needed by ${[...x.by].join(', ')}`));
      warn.appendChild(ul);
      warn.appendChild(button('Install missing dependencies', 'primary', installMissing));
      list.appendChild(warn);
    }

    if (tracked.length) {
      list.appendChild(el('div', 'f-section', 'Installed with Outpost Launcher'));
      for (const t of tracked) {
        const fm = t.guid ? S.forgeByGuid.get(t.guid.toLowerCase()) : null;
        const rm = button('Remove', '', async () => {
          if (!(await ask('Remove ' + t.name + '?', `Its ${t.files.length} files are moved to OutpostLauncher\\backups (not deleted). Close the game and stop the SPT server first.`, 'Remove'))) return;
          try { toast(await invoke('mod_uninstall', { modId: t.mod_id, kind: t.kind }), 'ok'); } catch (e) { toast(e, 'error'); }
          await rescan(); renderInstalled();
        });
        list.appendChild(row(t.name, `${t.version} · ${t.files.length} files · ${t.kind}`, fm || { id: t.mod_id, thumbnail: '' }, [rm]));
      }
    }
    list.appendChild(el('div', 'f-section', 'Client plugins (BepInEx)'));
    for (const p of plugins.sort((a, b) => a.name.localeCompare(b.name))) {
      const fm = S.forgeByGuid.get(p.guid.toLowerCase());
      const lack = (p.deps || []).filter((d) => d.hard && !S.local.has(d.guid.toLowerCase())).map((d) => d.guid);
      list.appendChild(row(p.name, `${p.version} · ${p.guid}` + (fm ? '' : ' · not on The Forge') + (lack.length ? ' · MISSING: ' + lack.join(', ') : ''), fm, []));
    }
    list.appendChild(el('div', 'f-section', 'Server mods'));
    if (serverMods.length) {
      for (const m of serverMods.sort((a, b) => String(a.Name ?? a.name).localeCompare(String(b.Name ?? b.name)))) {
        const guid = m.ModGuid ?? m.modGuid ?? '';
        const fm = guid ? S.forgeByGuid.get(guid.toLowerCase()) : null;
        list.appendChild(row(m.Name ?? m.name ?? guid, `${verText(m.Version ?? m.version)} · ${m.Author ?? m.author ?? ''}`, fm, []));
      }
    } else {
      for (const f of S.scan.server_folders || []) list.appendChild(row(f, 'Start the SPT server to see its version', null, []));
    }
  }
  $('#btn-rescan').onclick = async () => { await rescan(); renderInstalled(); };

  // ---------- updates ----------
  async function checkUpdates(silent) {
    if (!silent) { $('#upd-summary').textContent = 'Checking…'; $('#upd-list').replaceChildren(); }
    await rescan();
    const pairs = [...S.local.values()].filter((x) => x.guid && x.version);
    if (!pairs.length) { if (!silent) $('#upd-summary').textContent = 'No mods with a GUID and version were found.'; return; }
    const res = { updates: [], blocked_updates: [], up_to_date: [], incompatible_with_spt: [] };
    try {
      for (let i = 0; i < pairs.length; i += 30) {
        const q = pairs.slice(i, i + 30).map((x) => x.guid + ':' + x.version).join(',');
        const r = await F(`/api/v0/mods/updates?mods=${encodeURIComponent(q)}&spt_version=${encodeURIComponent(S.spt)}`);
        for (const k of Object.keys(res)) res[k].push(...((r.data && r.data[k]) || []));
      }
    } catch (e) { if (!silent) { $('#upd-summary').textContent = String(e); } return; }
    S.updatesChecked = true;
    S.updates = res.updates;
    const badge = $('#upd-badge');
    badge.hidden = !res.updates.length;
    badge.textContent = String(res.updates.length);
    renderUpdates(res);
  }

  const cur = (u) => u.current_version || {};
  // updates carry "recommended_version" (with link); blocked updates carry "latest_version"
  const lat = (u) => u.recommended_version || u.latest_version || {};

  async function updateItem(u) {
    const c = cur(u), l = lat(u);
    let link = l.link;
    const modId = c.mod_id ?? u.mod_id;
    if (!link) {
      const r = await F(`/api/v0/mod/${modId}/versions?filter[version]=${encodeURIComponent(l.version)}&per_page=1`);
      link = r.data && r.data[0] && r.data[0].link;
    }
    if (!link) throw new Error('No download link for ' + (c.name || modId));
    return { mod_id: modId, guid: c.guid || '', name: c.name || String(modId), version: l.version, link, kind: 'mod' };
  }

  async function runUpdates(list) {
    try {
      const items = [];
      for (const u of list) items.push(await updateItem(u));
      const page = list.length === 1 ? (S.forgeByGuid.get((cur(list[0]).guid || '').toLowerCase()) || {}).detail_url : null;
      if (await runInstall(items, page)) { S.updatesChecked = false; checkUpdates(); }
    } catch (e) { toast(e, 'error'); }
  }

  // a row needs a mod-like object only for its category icon; unknown mods get the generic plug icon
  const modStub = () => null;

  function cmpVer(a, b) {
    const p = (v) => String(v || '').split(/[.\-+]/).map((x) => parseInt(x, 10) || 0);
    const x = p(a), y = p(b);
    for (let i = 0; i < Math.max(x.length, y.length); i++) {
      const d = (x[i] || 0) - (y[i] || 0);
      if (d) return d;
    }
    return 0;
  }

  // Changelogs of every version after the installed one, up to the offered version (newest first, max 10).
  async function toggleNotes(u, box, btn) {
    if (box.childElementCount) { box.replaceChildren(); btn.textContent = 'Patch notes'; return; }
    btn.textContent = 'Hide notes';
    box.appendChild(el('div', 'f-empty', 'Loading patch notes…'));
    try {
      const c = cur(u), l = lat(u);
      const all = await versionsOf(c.mod_id ?? u.mod_id);
      const newer = all.filter((v) => cmpVer(v.version, c.version) > 0 && cmpVer(v.version, l.version) <= 0).sort((a, b) => cmpVer(b.version, a.version));
      box.replaceChildren();
      const wrap = el('div', 'f-notes');
      if (!newer.length) wrap.appendChild(el('div', 'muted small', 'The Forge has no patch notes for these versions.'));
      for (const v of newer.slice(0, 10)) {
        const h = el('div', 'f-notes-head');
        h.append(el('span', 'v', v.version), el('span', 'muted small', ago(v.published_at)));
        if (v.spt_version_constraint) h.appendChild(pill('SPT ' + v.spt_version_constraint, 'green'));
        const d = el('div', 'f-desc');
        setHtml(d, v.description || '<p class="muted">No notes for this version.</p>');
        wrap.append(h, d);
      }
      if (newer.length > 10) wrap.appendChild(el('div', 'muted small', `${newer.length - 10} older versions not shown.`));
      box.appendChild(wrap);
    } catch (e) {
      box.replaceChildren(el('div', 'f-empty', String(e)));
      btn.textContent = 'Patch notes';
    }
  }

  function renderUpdates(res) {
    const list = $('#upd-list');
    list.replaceChildren();
    $('#upd-summary').textContent = `${res.updates.length} updates · ${res.blocked_updates.length} blocked · ${res.up_to_date.length} up to date` +
      (res.incompatible_with_spt.length ? ` · ${res.incompatible_with_spt.length} without a version for SPT ${S.spt}` : '');
    $('#btn-update-all').disabled = !res.updates.length;
    $('#btn-update-all').onclick = async () => {
      if (await ask(`Update ${res.updates.length} mods?`, 'Each one is downloaded from its author\'s host; overwritten files are backed up. Close the game and stop the SPT server first.', 'Update all')) runUpdates(res.updates);
    };
    if (res.updates.length) {
      list.appendChild(el('div', 'f-section', 'Updates available'));
      for (const u of res.updates) {
        const c = cur(u), l = lat(u);
        const fm = S.forgeByGuid.get((c.guid || '').toLowerCase());
        const wrap = el('div', 'f-upd');
        const notes = el('div');
        const nb = button('Patch notes', '', () => toggleNotes(u, notes, nb));
        wrap.append(row(c.name || c.guid, `${c.version} → ${l.version}`, fm || modStub(c), [nb, button('Update', 'primary', () => runUpdates([u]))]), notes);
        list.appendChild(wrap);
      }
    }
    if (res.blocked_updates.length) {
      list.appendChild(el('div', 'f-section', 'Blocked updates'));
      for (const u of res.blocked_updates) {
        const c = cur(u), l = lat(u);
        const by = (u.blocking_mods || []).map((b) => `${b.mod_name} ${b.current_version} needs ${b.constraint}`).join('; ');
        const why = u.block_reason === 'dependency_constraint_violation' ? 'another mod needs the current version' + (by ? ': ' + by : '')
          : u.block_reason === 'chain_dependency_conflict' ? 'its new dependencies conflict with your other mods' : (u.block_reason || 'blocked');
        const wrap = el('div', 'f-upd');
        const notes = el('div');
        const nb = button('Patch notes', '', () => toggleNotes(u, notes, nb));
        wrap.append(row(c.name || c.guid, `${c.version} → ${l.version} · blocked: ${why}`, S.forgeByGuid.get((c.guid || '').toLowerCase()), [nb]), notes);
        list.appendChild(wrap);
      }
    }
    if (res.incompatible_with_spt.length) {
      list.appendChild(el('div', 'f-section', `No version for SPT ${S.spt}`));
      for (const u of res.incompatible_with_spt) { const c = cur(u); list.appendChild(row(c.name || c.guid || u.guid || '?', c.version || '', null, [])); }
    }
    if (!list.childElementCount) list.appendChild(el('div', 'f-empty', 'Everything is up to date.'));
  }
  $('#btn-check-updates').onclick = () => { S.updatesChecked = false; checkUpdates(); };

  listen('mod-progress', (e) => ($('#mod-step').textContent = e.payload));
  window.Mods = { open };
})();
