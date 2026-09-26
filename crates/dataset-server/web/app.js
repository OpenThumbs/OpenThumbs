'use strict';
/* OpenThumbs web app — dependency-free SPA over the /api REST API.
 * All markup goes through the `html` tagged template, which escapes every
 * interpolated value unless it is itself `html`/`raw` output. */

// ======================================================================
// Utilities
// ======================================================================

class Raw { constructor(s) { this.s = s; } toString() { return this.s; } }
const raw = (s) => new Raw(String(s));
const ESC = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };
function esc(v) {
  if (v === null || v === undefined || v === false) return '';
  if (v instanceof Raw) return v.s;
  if (Array.isArray(v)) return v.map(esc).join('');
  return String(v).replace(/[&<>"']/g, (c) => ESC[c]);
}
function html(strings, ...vals) {
  let out = '';
  strings.forEach((s, i) => { out += s; if (i < vals.length) out += esc(vals[i]); });
  return new Raw(out);
}
const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => Array.from(root.querySelectorAll(sel));
const enc = encodeURIComponent;
const IS_MAC = /Mac|iPhone|iPad/.test(navigator.platform);

function fmtSize(n) {
  if (n === null || n === undefined || Number.isNaN(n)) return '—';
  const u = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'];
  let i = 0; let v = Number(n);
  while (v >= 1000 && i < u.length - 1) { v /= 1000; i++; }
  if (i === 0) return `${v} B`;
  return `${v < 10 ? v.toFixed(2) : v < 100 ? v.toFixed(1) : v.toFixed(0)} ${u[i]}`;
}
function fmtCompact(n) {
  if (n === null || n === undefined || n === '') return null;
  const v = Number(n);
  if (!Number.isFinite(v)) return String(n);
  if (Math.abs(v) >= 1e9) return `${+(v / 1e9).toFixed(2)}B`;
  if (Math.abs(v) >= 1e6) return `${+(v / 1e6).toFixed(2)}M`;
  if (Math.abs(v) >= 1e4) return `${+(v / 1e3).toFixed(1)}K`;
  return v.toLocaleString();
}
function fmtInt(n) { return n === null || n === undefined ? '—' : Number(n).toLocaleString(); }
function ago(ts) {
  if (!ts) return '';
  const t = new Date(ts).getTime();
  if (Number.isNaN(t)) return ts;
  const s = Math.max(0, (Date.now() - t) / 1000);
  if (s < 45) return 'just now';
  if (s < 3600) return `${Math.round(s / 60)}m ago`;
  if (s < 86400) return `${Math.round(s / 3600)}h ago`;
  if (s < 86400 * 14) return `${Math.round(s / 86400)}d ago`;
  const d = new Date(t);
  const opts = { month: 'short', day: 'numeric' };
  if (d.getFullYear() !== new Date().getFullYear()) opts.year = 'numeric';
  return d.toLocaleDateString(undefined, opts);
}
function fmtDate(ts) { return ts ? new Date(ts).toLocaleString() : '—'; }
function fmtDuration(sec) {
  if (!Number.isFinite(sec) || sec < 0) return '';
  if (sec < 60) return `${Math.ceil(sec)}s`;
  if (sec < 3600) return `${Math.floor(sec / 60)}m ${Math.round(sec % 60)}s`;
  return `${Math.floor(sec / 3600)}h ${Math.round((sec % 3600) / 60)}m`;
}
/** Short, distinctive form of a ULID (the random tail; the head is a timestamp). */
const shortId = (id) => (id ? String(id).slice(-7).toLowerCase() : '');
const shortHash = (h) => (h ? String(h).replace(/^sha256:/, '').slice(0, 10) : '');
const truncate = (s, n) => (s && s.length > n ? `${s.slice(0, n - 1)}…` : s || '');
const extOf = (path) => { const m = /\.([A-Za-z0-9]{1,8})$/.exec(path); return m ? m[1].toLowerCase() : ''; };

function meta(m, ...keys) {
  if (!m || typeof m !== 'object') return null;
  for (const k of keys) if (m[k] !== undefined && m[k] !== null && m[k] !== '') return m[k];
  return null;
}
const rowsOf = (m) => meta(m, 'rows', 'row_count', 'num_rows');
const colsOf = (m) => meta(m, 'columns', 'num_columns', 'n_columns', 'features');
const formatOf = (m) => meta(m, 'format');

/** Fuzzy subsequence score (higher is better; -1 = no match). */
function fuzzy(query, text) {
  query = query.toLowerCase(); text = text.toLowerCase();
  if (!query) return 0;
  const direct = text.indexOf(query);
  if (direct >= 0) return 1000 - direct;
  let score = 0; let ti = 0; let streak = 0;
  for (const ch of query) {
    const found = text.indexOf(ch, ti);
    if (found < 0) return -1;
    streak = found === ti ? streak + 1 : 0;
    score += 10 + streak * 5 - (found - ti);
    ti = found + 1;
  }
  return score;
}

async function copyText(text, label = 'Copied to clipboard') {
  try {
    if (navigator.clipboard && window.isSecureContext) await navigator.clipboard.writeText(text);
    else {
      const ta = document.createElement('textarea');
      ta.value = text; ta.setAttribute('readonly', ''); ta.className = 'sr-only';
      document.body.appendChild(ta); ta.select(); document.execCommand('copy'); ta.remove();
    }
    toast(label, 'success');
  } catch (e) { toast('Couldn’t copy to clipboard', 'error'); }
}

const store = {
  get(k, d) { try { const v = localStorage.getItem(`ot.${k}`); return v === null ? d : JSON.parse(v); } catch { return d; } },
  set(k, v) { try { localStorage.setItem(`ot.${k}`, JSON.stringify(v)); } catch { /* private mode */ } },
};

// ======================================================================
// Scoped action handlers (event delegation via data-fn)
// ======================================================================

const handlers = new Map();
let handlerSeq = 0;
/** Register a click handler for markup; cleared when its scope re-renders. */
function act(fn, scope = 'page') { const id = `${scope}:${++handlerSeq}`; handlers.set(id, fn); return id; }
function clearScope(scope) { for (const k of handlers.keys()) if (k.startsWith(`${scope}:`)) handlers.delete(k); }

// ======================================================================
// API
// ======================================================================

class ApiError extends Error {
  constructor(status, message, details, data) { super(message); this.status = status; this.details = details; this.data = data; }
}

async function api(path, { method = 'GET', body } = {}) {
  const headers = { 'x-ds-csrf': '1' };
  if (body !== undefined) headers['content-type'] = 'application/json';
  let res;
  try {
    res = await fetch(`/api${path}`, { method, headers, credentials: 'same-origin', body: body !== undefined ? JSON.stringify(body) : undefined });
  } catch (e) {
    setConnection('offline');
    throw new ApiError(0, 'network', `${method} /api${path}\n${e}`);
  }
  setConnection('ok');
  const text = await res.text();
  let data = null;
  if (text) {
    try { data = JSON.parse(text); } catch (e) {
      if (res.ok) throw new ApiError(res.status, 'invalid', `${method} /api${path}\nInvalid JSON: ${e.message}\n\n${text.slice(0, 800)}`);
    }
  }
  if (!res.ok) {
    if (res.status === 401 && !path.startsWith('/auth/')) onUnauthorized();
    throw new ApiError(res.status, (data && data.error) || res.statusText,
      `HTTP ${res.status} ${res.statusText}\n${method} /api${path}\n\n${text.slice(0, 2000)}`, data);
  }
  return data;
}

/** Map errors to user-facing copy; raw details stay behind a disclosure. */
function friendly(err, what) {
  const title = `Couldn’t ${what}`;
  const details = err instanceof ApiError ? err.details : String(err && err.stack || err);
  let message;
  if (!(err instanceof ApiError)) message = 'Something went wrong in the app.';
  else if (err.status === 0) message = 'The server can’t be reached. Check that it’s running and that you’re online.';
  else if (err.message === 'invalid') message = 'The server returned an invalid response.';
  else if (err.status === 401) message = 'You need to sign in to do this.';
  else if (err.status === 403) message = 'You don’t have permission to do this.';
  else if (err.status === 404) message = 'It may have been deleted or renamed.';
  else if (err.status === 400 || err.status === 409) message = sentence(err.message);
  else if (err.status >= 500) message = 'The server ran into a problem. Try again in a moment.';
  else message = sentence(err.message);
  return { title, message, details };
}
const sentence = (s) => { s = String(s || '').trim(); return s ? s[0].toUpperCase() + s.slice(1) + (/[.!?]$/.test(s) ? '' : '.') : ''; };

// ======================================================================
// State
// ======================================================================

const S = {
  me: null,
  publicRead: false,
  booted: false,
  connection: 'connecting',
  datasets: null,
  datasetsError: null,
  catalogQuery: '',
  catalogFilter: store.get('catalogFilter', 'all'),
  catalogView: store.get('catalogView', null),
  ds: {},        // name -> { info, versions, error, loading }
  files: {},     // name@versionId -> { files, total, error }
  lineage: {},   // uri|depth -> { graph, error }
  compare: {},   // name|a|b -> { data, error }
  collapsed: new Set(),
  transfers: [],
  route: { name: 'datasets', parts: [], q: new URLSearchParams() },
};

function setConnection(c) {
  if (S.connection !== c) { S.connection = c; renderStatusbar(); }
}

function onUnauthorized() {
  const wasSignedIn = !!S.me;
  S.me = null;
  renderAccount();
  if (wasSignedIn) toast('Your session expired. Sign in again.', 'info');
  if (!S.publicRead && S.route.name !== 'login') {
    location.hash = `#/login?next=${enc(location.hash.slice(1) || '/datasets')}`;
  }
}

// ======================================================================
// Theme
// ======================================================================

function applyTheme(mode) {
  document.documentElement.dataset.theme = mode;
  store.set('theme', mode);
}
function isDark() {
  const m = document.documentElement.dataset.theme;
  return m === 'dark' || (m === 'system' && matchMedia('(prefers-color-scheme: dark)').matches);
}

// ======================================================================
// Data loading
// ======================================================================

async function loadDatasets() {
  try {
    S.datasets = await api('/datasets');
    S.datasetsError = null;
  } catch (e) {
    if (!S.datasets) S.datasetsError = e;
    else toast(friendly(e, 'refresh datasets').message, 'error');
  }
  if (S.route.name === 'datasets') renderCatalogList();
  renderRecent();
}

async function loadDataset(name, { force = false } = {}) {
  const cur = S.ds[name];
  if (cur && cur.loading) return;
  if (cur && !force && !cur.error) return;
  S.ds[name] = { ...(cur || {}), loading: true };
  try {
    const [info, versions] = await Promise.all([
      api(`/datasets/${enc(name)}`),
      api(`/datasets/${enc(name)}/versions?limit=10000`),
    ]);
    S.ds[name] = { info, versions, loading: false, error: null };
  } catch (e) {
    S.ds[name] = { ...(S.ds[name] || {}), loading: false, error: e };
  }
  if (S.route.name === 'dataset' && S.route.parts[1] === name) renderPage();
}

function invalidateDataset(name) {
  delete S.ds[name];
  for (const k of Object.keys(S.compare)) if (k.startsWith(`${name}|`)) delete S.compare[k];
  loadDatasets();
}

async function loadFiles(name, versionId) {
  const key = `${name}@${versionId}`;
  if (S.files[key]) return;
  S.files[key] = { loading: true };
  try {
    const r = await api(`/datasets/${enc(name)}/versions/${enc(versionId)}/files?limit=100000`);
    S.files[key] = { files: r.files, total: r.total };
  } catch (e) { S.files[key] = { error: e }; }
  if (S.route.name === 'dataset') renderPage();
}

async function loadLineage(uri, depth, path) {
  const key = `${uri}|${depth}`;
  if (S.lineage[key] && !S.lineage[key].error) return;
  S.lineage[key] = { loading: true };
  try { S.lineage[key] = { graph: await api(path) }; } catch (e) { S.lineage[key] = { error: e }; }
  if (S.route.name === 'dataset' || S.route.name === 'lineage') renderPage();
}

async function loadCompare(name, a, b) {
  const key = `${name}|${a}|${b}`;
  if (S.compare[key]) return;
  S.compare[key] = { loading: true };
  try { S.compare[key] = { data: await api(`/compare/${enc(name)}/${enc(a)}/${enc(b)}`) }; } catch (e) { S.compare[key] = { error: e }; }
  if (S.route.name === 'dataset') renderPage();
}

// ======================================================================
// Dataset helpers
// ======================================================================

function versionIndex(ds) {
  const map = new Map();
  ds.versions.forEach((v, i) => map.set(v.id, ds.versions.length - i));
  return map;
}
const vLabel = (ds, id) => { const n = versionIndex(ds).get(id); return n ? `v${n}` : shortId(id); };
function defaultRef(ds) {
  const branches = (ds.info.refs || []).filter((r) => r.kind === 'branch').map((r) => r.name);
  for (const pref of ['main', 'production', 'training']) if (branches.includes(pref)) return pref;
  return branches[0] || 'latest';
}
function resolveRef(ds, ref) {
  if (!ds.versions.length) return null;
  if (!ref || ref === 'latest') return ds.versions[0];
  const r = (ds.info.refs || []).find((x) => x.name === ref);
  const id = r ? r.version_id : ref;
  return ds.versions.find((v) => v.id === id) || null;
}
const refsFor = (ds, id) => (ds.info.refs || []).filter((r) => r.version_id === id);
const versionTitle = (v) => meta(v.metadata, 'message', 'title') || `Version ${shortId(v.id)}`;

function rememberRecent(name) {
  const recent = store.get('recent', []).filter((n) => n !== name);
  recent.unshift(name);
  store.set('recent', recent.slice(0, 6));
  renderRecent();
}

// ======================================================================
// Shared components (markup)
// ======================================================================

function badge(text, kind = '', extra = '') { return html`<span class="badge ${kind} ${extra}">${text}</span>`; }
function statusBadge(versionCount) {
  return versionCount > 0 ? badge('Ready', 'success') : badge('Empty');
}
function emptyState({ icon = '◇', title, text, action }) {
  return html`<div class="empty" role="status">
    <div class="empty-icon" aria-hidden="true">${icon}</div>
    <div class="empty-title">${title}</div>
    ${text ? html`<p>${text}</p>` : ''}
    ${action || ''}
  </div>`;
}
function errorState(err, what, retry) {
  const f = friendly(err, what);
  return html`<div class="error-state" role="alert">
    <div class="empty-title">${f.title}</div>
    <p>${f.message}</p>
    <div class="row">
      ${retry ? html`<button class="btn btn-sm" data-fn="${act(retry)}">Retry</button>` : ''}
    </div>
    <details><summary>View details</summary><pre>${f.details}</pre></details>
  </div>`;
}
function skeletonRows(n = 6) {
  return html`<div aria-busy="true" aria-label="Loading">${Array.from({ length: n }, () => html`
    <div class="skeleton-row"><div class="sk w1"></div><div class="sk w4"></div><div class="sk w2"></div><div class="sk w3"></div></div>`)}</div>`;
}
function skeletonCards(n = 4) { return html`<div aria-busy="true" aria-label="Loading">${Array.from({ length: n }, () => html`<div class="sk sk-card"></div>`)}</div>`; }
function segmented(options, value, onChange, label) {
  return html`<div class="segmented" role="group" aria-label="${label}">${options.map(([v, text]) => html`
    <button type="button" aria-pressed="${v === value}" data-fn="${act(() => onChange(v))}">${text}</button>`)}</div>`;
}
function codeBlock(text) {
  return html`<div class="codeblock">${text}<button class="btn btn-sm btn-ghost" data-fn="${act(() => copyText(text))}" aria-label="Copy">Copy</button></div>`;
}
function versionSelect(ds, value, onChange, id = 'vsel', scope = 'page') {
  const idx = versionIndex(ds);
  const refs = ds.info.refs || [];
  const handlerId = act((el) => onChange(el.value), scope);
  return html`<span class="select-wrap"><select class="select" id="${id}" data-change="${handlerId}" aria-label="Version">
    ${refs.length ? html`<optgroup label="Branches & tags">${refs.map((r) => html`
      <option value="${r.name}" ${value === r.name ? raw('selected') : ''}>${r.kind === 'branch' ? '⎇' : '⌗'} ${r.name} → v${idx.get(r.version_id) || '?'}</option>`)}</optgroup>` : ''}
    <optgroup label="Versions">
      <option value="latest" ${value === 'latest' ? raw('selected') : ''}>latest → v${ds.versions.length}</option>
      ${ds.versions.map((v) => html`<option value="${v.id}" ${value === v.id ? raw('selected') : ''}>v${idx.get(v.id)} · ${shortId(v.id)} · ${truncate(versionTitle(v), 48)}</option>`)}
    </optgroup>
  </select></span>`;
}

// ======================================================================
// Shell rendering: nav, account, recent, status bar
// ======================================================================

function renderNav() {
  const active = { datasets: 'datasets', dataset: 'datasets', lineage: 'lineage', transfers: 'transfers', settings: 'settings' }[S.route.name];
  $$('.nav-item[data-nav]').forEach((a) => a.classList.toggle('active', a.dataset.nav === active));
  $$('#recent .nav-item').forEach((a) => a.classList.toggle('active', S.route.name === 'dataset' && a.dataset.name === S.route.parts[1]));
}

function renderRecent() {
  const known = new Set((S.datasets || []).map((d) => d.name));
  const recent = store.get('recent', []).filter((n) => !S.datasets || known.has(n));
  $('#recent-label').hidden = recent.length === 0;
  $('#recent').innerHTML = esc(recent.map((n) => html`
    <a class="nav-item" href="#/d/${enc(n)}" data-name="${n}"><span class="nav-icon" aria-hidden="true">·</span>${n}</a>`));
  renderNav();
}

function renderAccount() {
  clearScope('account');
  const el = $('#account');
  if (S.me) {
    el.innerHTML = esc(html`<button class="btn btn-ghost" data-fn="${act((b) => openMenu(b, [
      { label: 'Settings', fn: () => { location.hash = '#/settings'; } },
      { label: 'Sign out', fn: signOut },
    ]), 'account')}" aria-haspopup="menu">
      <span class="dot ok" aria-hidden="true"></span>${S.me.username}${S.me.admin ? html` <span class="muted">· admin</span>` : ''} ▾</button>`);
  } else if (S.booted) {
    el.innerHTML = esc(html`<a class="btn" href="#/login">Sign in</a>`);
  } else el.innerHTML = '';
}

function renderStatusbar() {
  const up = S.transfers.filter((t) => t.state === 'active').length;
  const failed = S.transfers.filter((t) => t.state === 'failed').length;
  const conn = {
    ok: html`<span><span class="dot ok" aria-hidden="true"></span>Connected · ${location.host}</span>`,
    offline: html`<span><span class="dot bad" aria-hidden="true"></span>Offline — server unreachable</span>`,
    connecting: html`<span><span class="dot busy" aria-hidden="true"></span>Connecting…</span>`,
  }[S.connection];
  const theme = { system: 'System theme', light: 'Light', dark: 'Dark' }[document.documentElement.dataset.theme];
  $('#statusbar').innerHTML = esc(html`${conn}
    ${S.me ? html`<span>${S.me.username}</span>` : S.publicRead ? html`<span>Read-only</span>` : ''}
    <span class="grow"></span>
    ${up || failed ? html`<button data-href="#/transfers">${up ? html`<span class="spinner" aria-hidden="true"></span> ↑ ${up} upload${up > 1 ? 's' : ''}` : ''}${failed ? html` <span class="dot bad" aria-hidden="true"></span>${failed} failed` : ''}</button>` : html`<span>No active transfers</span>`}
    <span>${theme}</span>`);
  $('#nav-transfers').textContent = up ? String(up) : '';
}

// ======================================================================
// Router
// ======================================================================

function parseRoute() {
  const h = location.hash.replace(/^#/, '') || '/datasets';
  const [p, qs] = h.split('?');
  const parts = p.split('/').filter(Boolean).map((x) => { try { return decodeURIComponent(x); } catch { return x; } });
  const q = new URLSearchParams(qs || '');
  const name = { datasets: 'datasets', d: 'dataset', lineage: 'lineage', transfers: 'transfers', settings: 'settings', login: 'login' }[parts[0]] || 'notfound';
  return { name, parts, q };
}
function go(hash) { if (location.hash !== hash) location.hash = hash; else renderPage(); }
function setQuery(updates) {
  const q = new URLSearchParams(S.route.q);
  for (const [k, v] of Object.entries(updates)) { if (v === null || v === undefined || v === '') q.delete(k); else q.set(k, v); }
  const base = location.hash.replace(/^#/, '').split('?')[0];
  const qs = q.toString();
  go(`#${base}${qs ? `?${qs}` : ''}`);
}

function onRoute() {
  S.route = parseRoute();
  closeInspector();
  closeMenu();
  renderNav();
  renderPage();
  $('#page').scrollTop = 0;
}

function renderPage() {
  clearScope('page');
  const page = $('#page');
  const r = S.route;
  if (!S.booted) { page.innerHTML = esc(skeletonCards(3)); return; }
  if (r.name !== 'login' && !S.me && !S.publicRead) { location.hash = `#/login?next=${enc(location.hash.slice(1))}`; return; }
  const renderers = { datasets: pageDatasets, dataset: pageDataset, lineage: pageLineage, transfers: pageTransfers, settings: pageSettings, login: pageLogin };
  const fn = renderers[r.name];
  const title = { datasets: 'Datasets', dataset: r.parts[1], lineage: 'Lineage', transfers: 'Transfers', settings: 'Settings', login: 'Sign in' }[r.name];
  document.title = title ? `${title} · OpenThumbs` : 'OpenThumbs';
  page.innerHTML = esc(fn ? fn() : emptyState({ icon: '?', title: 'Page not found', text: 'This link doesn’t match any page.', action: html`<a class="btn" href="#/datasets">Go to datasets</a>` }));
  const ver = $('#server-version');
  if (ver && S.serverVersion) ver.textContent = S.serverVersion;
  afterRender();
}

/** Hooks that need live DOM (graphs, focus) after a page render. */
let afterRenderHooks = [];
function afterRender() { const hooks = afterRenderHooks; afterRenderHooks = []; hooks.forEach((h) => h()); }

// ======================================================================
// Page: Login
// ======================================================================

function pageLogin() {
  const submit = act(async () => {
    const u = $('#login-user').value.trim();
    const p = $('#login-pass').value;
    const btn = $('#login-btn');
    const err = $('#login-error');
    if (!u || !p) { err.textContent = 'Enter your username and password.'; err.hidden = false; return; }
    btn.disabled = true; btn.textContent = 'Signing in…';
    try {
      S.me = await api('/auth/login', { method: 'POST', body: { username: u, password: p } });
      renderAccount(); renderStatusbar();
      toast(`Signed in as ${S.me.username}`, 'success');
      S.datasets = null; loadDatasets();
      go(`#${S.route.q.get('next') || '/datasets'}`);
    } catch (e) {
      btn.disabled = false; btn.textContent = 'Sign in';
      err.textContent = e.status === 401 ? 'Incorrect username or password.' : friendly(e, 'sign in').message;
      err.hidden = false;
    }
  });
  afterRenderHooks.push(() => $('#login-user') && $('#login-user').focus());
  return html`<div class="login">
    <div class="page-head"><h1 class="page-title">Sign in to OpenThumbs</h1></div>
    <form class="card" data-submit="${submit}">
      <div class="field"><label for="login-user">Username</label><input class="input" id="login-user" autocomplete="username"></div>
      <div class="field"><label for="login-pass">Password</label><input class="input" id="login-pass" type="password" autocomplete="current-password"></div>
      <p class="meta" id="login-error" role="alert" hidden></p>
      <button class="btn btn-primary" id="login-btn" type="submit">Sign in</button>
    </form>
    <p class="meta section">Accounts are created by an administrator: <code>dataset-server useradd &lt;name&gt;</code></p>
  </div>`;
}

async function signOut() {
  try { await api('/auth/logout', { method: 'POST' }); } catch { /* cookie is cleared either way */ }
  S.me = null;
  renderAccount(); renderStatusbar();
  toast('Signed out', 'info');
  go(S.publicRead ? '#/datasets' : '#/login');
}

// ======================================================================
// Page: Datasets catalog
// ======================================================================

function parseSize(expr) {
  const m = /^(>=|<=|>|<|=)?(\d+(?:\.\d+)?)\s*(b|kb|mb|gb|tb)?$/i.exec(expr);
  if (!m) return null;
  const mult = { b: 1, kb: 1e3, mb: 1e6, gb: 1e9, tb: 1e12 }[(m[3] || 'b').toLowerCase()];
  return { op: m[1] || '>=', bytes: parseFloat(m[2]) * mult };
}
function matchDataset(d, query) {
  const lm = d.latest && d.latest.metadata;
  let score = 0;
  for (const tok of query.trim().split(/\s+/).filter(Boolean)) {
    const [k, ...rest] = tok.split(':');
    const v = rest.join(':').toLowerCase();
    if (rest.length && v) {
      if (k === 'owner') { if (d.owner.toLowerCase() !== v) return -1; continue; }
      if (k === 'format') { if (String(formatOf(lm) || '').toLowerCase() !== v) return -1; continue; }
      if (k === 'size') {
        const s = parseSize(v); const size = d.latest ? d.latest.total_size : 0;
        if (!s) return -1;
        const ok = { '>': size > s.bytes, '>=': size >= s.bytes, '<': size < s.bytes, '<=': size <= s.bytes, '=': size === s.bytes }[s.op];
        if (!ok) return -1; continue;
      }
      if (k === 'name') { const f = fuzzy(v, d.name); if (f < 0) return -1; score += f; continue; }
    }
    const f = fuzzy(tok, `${d.name} ${d.description}`);
    if (f < 0) return -1;
    score += f;
  }
  return score;
}

function filteredDatasets() {
  let list = S.datasets || [];
  if (S.catalogFilter === 'mine' && S.me) list = list.filter((d) => d.owner === S.me.username);
  if (S.catalogFilter === 'recent') {
    const recent = store.get('recent', []);
    list = recent.map((n) => list.find((d) => d.name === n)).filter(Boolean);
  }
  if (S.catalogQuery.trim()) {
    list = list.map((d) => [d, matchDataset(d, S.catalogQuery)]).filter(([, s]) => s >= 0).sort((a, b) => b[1] - a[1]).map(([d]) => d);
  }
  return list;
}

function pageDatasets() {
  loadDatasets();
  const search = act((el) => { S.catalogQuery = el.value; renderCatalogList(); });
  afterRenderHooks.push(renderCatalogList);
  return html`
    <div class="page-head">
      <h1 class="page-title">Datasets</h1>
      ${S.me ? html`<button class="btn btn-primary" data-fn="${act(openCreateDataset)}">+ New dataset</button>` : ''}
    </div>
    <div class="catalog-tools">
      <div class="search-box"><input class="input" id="catalog-search" type="search" placeholder="Filter…  owner:rinat  format:parquet  size:>10GB" value="${S.catalogQuery}" data-input="${search}" aria-label="Filter datasets"></div>
      <span id="catalog-filter"></span>
      <span id="catalog-view"></span>
    </div>
    <div id="ds-list"></div>`;
}

function renderCatalogList() {
  const el = $('#ds-list');
  if (!el) return;
  clearScope('catalog');
  const scopeAct = (fn) => act(fn, 'catalog');
  const filters = [['all', 'All'], ...(S.me ? [['mine', 'Mine']] : []), ['recent', 'Recent']];
  $('#catalog-filter').innerHTML = esc(html`<div class="segmented" role="group" aria-label="Filter">${filters.map(([v, t]) => html`
    <button type="button" aria-pressed="${S.catalogFilter === v}" data-fn="${scopeAct(() => { S.catalogFilter = v; store.set('catalogFilter', v); renderCatalogList(); })}">${t}</button>`)}</div>`);
  const view = S.catalogView || ((S.datasets || []).length > 12 ? 'table' : 'cards');
  $('#catalog-view').innerHTML = esc(html`<div class="segmented" role="group" aria-label="View">${[['cards', 'Cards'], ['table', 'Table']].map(([v, t]) => html`
    <button type="button" aria-pressed="${view === v}" data-fn="${scopeAct(() => { S.catalogView = v; store.set('catalogView', v); renderCatalogList(); })}">${t}</button>`)}</div>`);

  if (S.datasetsError && !S.datasets) {
    el.innerHTML = esc(errorState(S.datasetsError, 'load datasets', () => { S.datasetsError = null; renderCatalogList(); loadDatasets(); }));
    return;
  }
  if (S.datasets === null) { el.innerHTML = esc(view === 'table' ? skeletonRows(8) : skeletonCards(4)); return; }
  if (S.datasets.length === 0) {
    el.innerHTML = esc(emptyState({
      icon: '▤', title: 'No datasets yet',
      text: 'Create a dataset, then add files from the browser or push a folder with the ds CLI.',
      action: S.me ? html`<button class="btn btn-primary" data-fn="${scopeAct(openCreateDataset)}">Create dataset</button>` : '',
    }));
    return;
  }
  const list = filteredDatasets();
  if (list.length === 0) {
    el.innerHTML = esc(emptyState({
      icon: '⌕', title: 'No datasets match this filter', text: 'Try a different search or clear the filters.',
      action: html`<button class="btn" data-fn="${scopeAct(() => { S.catalogQuery = ''; S.catalogFilter = 'all'; store.set('catalogFilter', 'all'); $('#catalog-search').value = ''; renderCatalogList(); })}">Clear filters</button>`,
    }));
    return;
  }
  el.innerHTML = esc(view === 'table' ? catalogTable(list) : catalogCards(list));
}

function catalogCards(list) {
  return html`<div class="cards">${list.map((d) => {
    const l = d.latest; const m = l && l.metadata;
    const stats = [
      rowsOf(m) !== null ? `${fmtCompact(rowsOf(m))} rows` : null,
      l ? fmtSize(l.total_size) : null,
      l ? `${fmtInt(l.file_count)} files` : null,
      colsOf(m) !== null ? `${colsOf(m)} columns` : null,
      formatOf(m) ? String(formatOf(m)).replace(/^./, (c) => c.toUpperCase()) : null,
    ].filter(Boolean);
    return html`<a class="ds-card" href="#/d/${enc(d.name)}">
      <div class="ds-card-head"><span class="ds-card-name">${d.name}</span>${statusBadge(d.version_count)}</div>
      ${d.description ? html`<div class="ds-card-desc">${d.description}</div>` : ''}
      ${stats.length ? html`<div class="ds-card-stats">${stats.map((s) => html`<span>${s}</span>`)}</div>` : ''}
      <div class="ds-card-foot meta">${l ? html`v${d.version_count} · updated ${ago(l.created_at)} · ${l.created_by}` : html`No versions yet · created by ${d.owner}`}</div>
    </a>`;
  })}</div>`;
}

function catalogTable(list) {
  return html`<table class="table">
    <thead><tr><th>Name</th><th>Status</th><th class="num">Versions</th><th class="num">Size</th><th class="num">Files</th><th class="num">Rows</th><th>Format</th><th>Updated</th><th>Owner</th></tr></thead>
    <tbody>${list.map((d) => {
      const l = d.latest; const m = l && l.metadata;
      return html`<tr tabindex="0" data-row data-href="#/d/${enc(d.name)}">
        <td><strong>${d.name}</strong>${d.description ? html`<div class="meta">${truncate(d.description, 80)}</div>` : ''}</td>
        <td>${statusBadge(d.version_count)}</td>
        <td class="num">${d.version_count}</td>
        <td class="num mono">${l ? fmtSize(l.total_size) : '—'}</td>
        <td class="num">${l ? fmtInt(l.file_count) : '—'}</td>
        <td class="num">${fmtCompact(rowsOf(m)) || '—'}</td>
        <td>${formatOf(m) ? html`<span class="format">${String(formatOf(m)).toUpperCase()}</span>` : '—'}</td>
        <td class="meta">${l ? ago(l.created_at) : '—'}</td>
        <td class="secondary">${d.owner}</td>
      </tr>`;
    })}</tbody></table>`;
}

// ======================================================================
// Page: Dataset
// ======================================================================

const TABS = [['overview', 'Overview'], ['files', 'Files'], ['versions', 'Versions'], ['lineage', 'Lineage'], ['compare', 'Compare'], ['activity', 'Activity']];

function pageDataset() {
  const name = S.route.parts[1];
  const tab = S.route.parts[2] || 'overview';
  const ds = S.ds[name];
  if (!ds || (!ds.info && !ds.error)) {
    loadDataset(name);
    return html`<div class="breadcrumb"><a href="#/datasets">Datasets</a> / ${name}</div>${skeletonCards(1)}${skeletonRows(6)}`;
  }
  if (ds.error && !ds.info) {
    return html`<div class="breadcrumb"><a href="#/datasets">Datasets</a> / ${name}</div>
      ${ds.error.status === 404
        ? emptyState({ icon: '?', title: 'Dataset not found', text: `There is no dataset named “${name}”. It may have been renamed or deleted.`, action: html`<a class="btn" href="#/datasets">Back to datasets</a>` })
        : errorState(ds.error, 'load this dataset', () => loadDataset(name, { force: true }))}`;
  }
  rememberRecent(name);
  const body = { overview: tabOverview, files: tabFiles, versions: tabVersions, lineage: tabLineage, compare: tabCompare, activity: tabActivity }[tab] || tabOverview;
  return html`
    <div class="breadcrumb"><a href="#/datasets">Datasets</a> / ${name}</div>
    ${hero(name, ds)}
    <nav class="tabs" aria-label="Dataset sections">${TABS.map(([id, label]) => html`
      <a class="tab ${tab === id ? 'active' : ''}" href="#/d/${enc(name)}/${id}" ${tab === id ? raw('aria-current="page"') : ''}>${label}${id === 'versions' ? html` <span class="count">${ds.versions.length}</span>` : ''}</a>`)}</nav>
    ${body(name, ds)}`;
}

function hero(name, ds) {
  const d = ds.info.dataset;
  const ref = defaultRef(ds);
  const v = resolveRef(ds, ref);
  const m = v && v.metadata;
  const stats = v ? [
    [fmtCompact(rowsOf(m)), 'rows'], [fmtSize(v.total_size), 'size'], [fmtInt(v.file_count), 'files'],
    [colsOf(m), 'columns'], [formatOf(m) && String(formatOf(m)).replace(/^./, (c) => c.toUpperCase()), 'format'],
  ].filter(([val]) => val !== null && val !== undefined && val !== '') : [];
  return html`<section class="hero">
    <div class="hero-top">
      <div class="grow">
        <h1 class="hero-title">${name}</h1>
        <p class="hero-desc">${d.description || html`<span class="muted">No description</span>`}</p>
        <div class="hero-pills">
          ${ref !== 'latest' ? badge(`⎇ ${ref}`, 'accent plain') : ''}
          ${v ? badge(vLabel(ds, v.id), 'plain mono') : ''}
          ${statusBadge(ds.versions.length)}
        </div>
      </div>
      <div class="hero-actions">
        <button class="btn btn-primary" data-fn="${act(() => openPull(name, ds))}" ${v ? '' : raw('disabled')}>↓ Pull</button>
        ${S.me ? html`<button class="btn" data-fn="${act(() => openNewVersion(name, ds))}">New version</button>` : ''}
        <a class="btn" href="#/d/${enc(name)}/compare">Compare</a>
        <button class="btn btn-ghost btn-icon" aria-label="More actions" aria-haspopup="menu" data-fn="${act((b) => openMenu(b, datasetMenu(name, ds, v)))}">⋯</button>
      </div>
    </div>
    ${stats.length ? html`<div class="hero-stats">${stats.map(([val, label]) => html`<div class="hero-stat"><div class="v">${val}</div><div class="l">${label}</div></div>`)}</div>` : ''}
    <p class="meta section-gap">${v ? html`Updated ${ago(v.created_at)} by ${v.created_by} · owner ${d.owner}` : html`Created ${ago(d.created_at)} by ${d.owner}`}</p>
  </section>`;
}

function datasetMenu(name, ds, v) {
  const items = [];
  if (v) {
    items.push({ label: 'Copy dataset URI', fn: () => copyText(`dataset://${name}/${v.id}`) });
    items.push({ label: 'Copy IDs for MLflow / Aim', fn: () => copyText(mlflowIds(name, v), 'Copied dataset identifiers') });
    if (S.me) {
      items.push({ label: 'Tag this version…', fn: () => openRefDialog(name, ds, v, 'tag') });
      items.push({ label: 'Move a branch here…', fn: () => openRefDialog(name, ds, v, 'branch') });
    }
  }
  items.push({ label: 'Refresh', fn: () => { invalidateDataset(name); loadDataset(name, { force: true }); } });
  return items;
}
const mlflowIds = (name, v) => `dataset.name=${name}\ndataset.version=${v.id}\ndataset.manifest_hash=${v.manifest_hash}\ndataset.uri=dataset://${name}/${v.id}\n`;

// ---------- Overview ----------

function tabOverview(name, ds) {
  if (!ds.versions.length) return noVersions(name, ds);
  const ref = defaultRef(ds);
  const v = resolveRef(ds, ref);
  const m = v.metadata || {};
  const metrics = [
    ['Rows', fmtCompact(rowsOf(m)) || '—'], ['Columns', colsOf(m) ?? '—'], ['Size', fmtSize(v.total_size)],
    ['Files', fmtInt(v.file_count)], ['Format', formatOf(m) || '—'], ['Versions', ds.versions.length],
  ];
  const snippet = `# Download exactly this version\nds pull ${name}@${ref} --to ./${name}\n\n# Resolve to immutable identifiers\nds resolve ${name}@${ref}\n\n# Log data identity with your run (MLflow)\nmlflow.log_params({\n    "dataset.name": "${name}",\n    "dataset.version": "${v.id}",\n    "dataset.manifest_hash": "${v.manifest_hash}",\n})`;
  const refs = ds.info.refs || [];
  return html`
    <div class="metrics">${metrics.map(([l, val]) => html`<div class="metric"><div class="metric-label">${l}</div><div class="metric-value">${val}</div></div>`)}</div>
    <div class="section">
      <div class="section-head"><h2 class="section-title">Current version</h2><a class="btn btn-sm btn-ghost" href="#/d/${enc(name)}/versions">All versions →</a></div>
      <div class="card">
        <div class="row"><span class="card-title grow">${vLabel(ds, v.id)} · ${versionTitle(v)}</span>${refsFor(ds, v.id).map((r) => badge(r.name, r.kind === 'branch' ? 'accent plain' : 'plain'))}</div>
        <p class="meta">${v.created_by} · ${ago(v.created_at)} · ${fmtSize(v.total_size)} · ${fmtInt(v.file_count)} files</p>
        <dl class="kv section-gap">
          <dt>Version ID</dt><dd class="mono">${v.id}</dd>
          <dt>Manifest</dt><dd class="mono">${v.manifest_hash}</dd>
          ${v.schema_hash ? html`<dt>Schema</dt><dd class="mono">${v.schema_hash}</dd>` : ''}
          ${v.producer ? html`<dt>Produced by</dt><dd>${v.producer.type}:${v.producer.id}</dd>` : ''}
        </dl>
      </div>
    </div>
    <div class="section">
      <div class="section-head"><h2 class="section-title">Use this dataset</h2></div>
      ${codeBlock(snippet)}
    </div>
    ${refs.length ? html`<div class="section">
      <div class="section-head"><h2 class="section-title">Branches & tags</h2></div>
      <table class="table"><thead><tr><th>Name</th><th>Kind</th><th>Points to</th><th>Updated</th></tr></thead><tbody>
        ${refs.map((r) => html`<tr tabindex="0" data-row data-href="#/d/${enc(name)}/files?v=${enc(r.name)}">
          <td><strong>${r.name}</strong></td><td>${badge(r.kind === 'branch' ? 'Branch' : 'Tag', r.kind === 'branch' ? 'accent plain' : 'plain')}</td>
          <td>${vLabel(ds, r.version_id)} <span class="mono muted">${shortId(r.version_id)}</span></td><td class="meta">${ago(r.updated_at)}</td></tr>`)}
      </tbody></table></div>` : ''}`;
}

function noVersions(name, ds) {
  return emptyState({
    icon: '⬚', title: 'No versions yet',
    text: 'Add files to create the first immutable version of this dataset.',
    action: S.me ? html`<button class="btn btn-primary" data-fn="${act(() => openNewVersion(name, ds))}">Add files</button>` : '',
  });
}

// ---------- Files ----------

function tabFiles(name, ds) {
  if (!ds.versions.length) return noVersions(name, ds);
  const ref = S.route.q.get('v') || defaultRef(ds);
  const v = resolveRef(ds, ref);
  if (!v) return emptyState({ icon: '?', title: 'Version not found', text: `“${ref}” doesn’t match a version, tag or branch.` });
  const key = `${name}@${v.id}`;
  const entry = S.files[key];
  if (!entry) loadFiles(name, v.id);
  const onSearch = act((el) => { S.fileQuery = el.value; renderFileList(name, v); });
  afterRenderHooks.push(() => renderFileList(name, v));
  return html`
    <div class="catalog-tools">
      ${versionSelect(ds, ref, (val) => setQuery({ v: val }))}
      <div class="search-box"><input class="input" id="file-search" type="search" placeholder="Search files" value="${S.fileQuery || ''}" data-input="${onSearch}" aria-label="Search files"></div>
      <span class="meta">${fmtInt(v.file_count)} files · ${fmtSize(v.total_size)}</span>
    </div>
    <div id="file-list"></div>`;
}

const RENDER_CAP = 2000;

function renderFileList(name, v) {
  const el = $('#file-list');
  if (!el) return;
  clearScope('files');
  const fa = (fn) => act(fn, 'files');
  const entry = S.files[`${name}@${v.id}`];
  if (!entry || entry.loading) { el.innerHTML = esc(skeletonRows(8)); return; }
  if (entry.error) { el.innerHTML = esc(errorState(entry.error, 'load files', () => { delete S.files[`${name}@${v.id}`]; loadFiles(name, v.id); })); return; }
  if (!entry.files.length) { el.innerHTML = esc(emptyState({ icon: '⬚', title: 'This version has no files' })); return; }

  const q = (S.fileQuery || '').trim();
  const rows = [];
  if (q) {
    const hits = entry.files.map((f) => [f, fuzzy(q, f.path)]).filter(([, s]) => s >= 0).sort((a, b) => b[1] - a[1]);
    if (!hits.length) { el.innerHTML = esc(emptyState({ icon: '⌕', title: 'No files match this filter', text: `Nothing matches “${q}”.` })); return; }
    for (const [f] of hits.slice(0, RENDER_CAP)) rows.push({ kind: 'file', f, depth: 0, label: f.path });
  } else {
    // Build directory rows from the sorted path list.
    const seen = new Set();
    for (const f of entry.files) {
      const segs = f.path.split('/');
      let hidden = false;
      for (let i = 1; i < segs.length; i++) {
        const dir = segs.slice(0, i).join('/');
        if (!seen.has(dir) && !hidden) {
          seen.add(dir);
          rows.push({ kind: 'dir', path: dir, depth: i - 1, label: `${segs[i - 1]}/` });
        }
        if (S.collapsed.has(`${v.id}:${dir}`)) { hidden = true; break; }
      }
      if (!hidden) rows.push({ kind: 'file', f, depth: segs.length - 1, label: segs[segs.length - 1] });
      if (rows.length >= RENDER_CAP) break;
    }
  }
  const dirSizes = new Map();
  if (!q) for (const f of entry.files) { const segs = f.path.split('/'); for (let i = 1; i < segs.length; i++) { const d = segs.slice(0, i).join('/'); dirSizes.set(d, (dirSizes.get(d) || 0) + f.size); } }

  el.innerHTML = esc(html`<table class="table">
    <thead><tr><th>Path</th><th class="num">Size</th><th>Blob</th><th class="actions"><span class="sr-only">Actions</span></th></tr></thead>
    <tbody>${rows.map((r) => {
      const indent = `indent-${Math.min(r.depth, 6)}`;
      if (r.kind === 'dir') {
        const collapsed = S.collapsed.has(`${v.id}:${r.path}`);
        const toggle = fa(() => { const k = `${v.id}:${r.path}`; if (S.collapsed.has(k)) S.collapsed.delete(k); else S.collapsed.add(k); renderFileList(name, v); });
        return html`<tr tabindex="0" data-row data-fn="${toggle}" aria-expanded="${!collapsed}">
          <td><div class="file-name ${indent}"><span class="twisty" aria-hidden="true">${collapsed ? '▸' : '▾'}</span><span class="folder" aria-hidden="true">▰</span><span>${r.label}</span></div></td>
          <td class="num mono muted">${fmtSize(dirSizes.get(r.path))}</td><td></td><td></td></tr>`;
      }
      const f = r.f; const ext = extOf(f.path);
      const url = `/api/datasets/${enc(name)}/versions/${enc(v.id)}/files/${f.path.split('/').map(enc).join('/')}`;
      const inspect = fa(() => inspectFile(name, v, f, url));
      return html`<tr tabindex="0" data-row data-fn="${inspect}">
        <td><div class="file-name ${indent}"><span class="twisty" aria-hidden="true"></span>${ext ? html`<span class="format">${ext.slice(0, 7).toUpperCase()}</span>` : html`<span class="fileicon" aria-hidden="true">▢</span>`}<span>${r.label}</span></div></td>
        <td class="num mono">${fmtSize(f.size)}</td>
        <td class="mono muted" title="${f.blob}">${shortHash(f.blob)}…</td>
        <td class="actions"><span class="hover-actions">
          <button class="btn btn-sm btn-ghost" data-fn="${fa(() => copyText(`dataset://${name}/${v.id}/${f.path}`, 'Copied file URI'))}">Copy URI</button>
          <a class="btn btn-sm btn-ghost" href="${url}" download="${f.path.split('/').pop()}" data-stop>Download</a>
          <button class="btn btn-sm btn-ghost" data-fn="${inspect}">Inspect</button>
        </span></td></tr>`;
    })}</tbody></table>
    ${rows.length >= RENDER_CAP ? html`<p class="meta section-gap">Showing the first ${fmtInt(RENDER_CAP)} rows. Search to narrow the list.</p>` : ''}
    ${entry.total > entry.files.length ? html`<p class="meta">Loaded ${fmtInt(entry.files.length)} of ${fmtInt(entry.total)} files.</p>` : ''}`);
}

function inspectFile(name, v, f, url) {
  const curl = `curl -H "Authorization: Bearer $DS_TOKEN" -o ${f.path.split('/').pop()} ${location.origin}${url}`;
  openInspector({
    title: f.path.split('/').pop(), subtitle: f.path,
    rows: [['Size', `${fmtSize(f.size)} (${fmtInt(f.size)} bytes)`], ['Blob', f.blob, true], ['Version', `${vLabel(S.ds[name], v.id)} · ${v.id}`, true], ['URI', `dataset://${name}/${v.id}/${f.path}`, true]],
    actions: [
      { label: 'Download', href: url, download: f.path.split('/').pop(), primary: true },
      { label: 'Copy URI', fn: () => copyText(`dataset://${name}/${v.id}/${f.path}`, 'Copied file URI') },
      { label: 'Copy blob hash', fn: () => copyText(f.blob) },
    ],
    extra: html`<div><div class="field"><label>Download with curl</label></div>${codeBlock(curl)}</div>`,
  });
}

// ---------- Versions ----------

function tabVersions(name, ds) {
  if (!ds.versions.length) return noVersions(name, ds);
  const idx = versionIndex(ds);
  return html`<div class="versions" role="list">${ds.versions.map((v, i) => {
    const m = v.metadata || {};
    const inspect = act(() => inspectVersion(name, ds, v));
    return html`<div class="v-row" role="listitem" tabindex="0" data-row data-fn="${inspect}">
      <div class="v-num">v${idx.get(v.id)}</div>
      <div class="v-main">
        <div class="v-title">${versionTitle(v)}</div>
        <div class="v-sub meta">
          <span class="mono">${shortId(v.id)}</span><span>·</span><span>${v.created_by}</span><span>·</span><span title="${fmtDate(v.created_at)}">${ago(v.created_at)}</span>
          ${i === 0 ? badge('latest', 'success') : ''}
          ${refsFor(ds, v.id).map((r) => badge(r.kind === 'branch' ? `⎇ ${r.name}` : r.name, r.kind === 'branch' ? 'accent plain' : 'plain'))}
        </div>
      </div>
      <div class="v-stats"><span class="mono">${fmtSize(v.total_size)}</span><span>${fmtInt(v.file_count)} files</span>${rowsOf(m) !== null ? html`<span>${fmtCompact(rowsOf(m))} rows</span>` : ''}</div>
      <div class="hover-actions">
        ${v.parent ? html`<a class="btn btn-sm btn-ghost" href="#/d/${enc(name)}/compare?a=${enc(v.parent)}&b=${enc(v.id)}" data-stop>Compare</a>` : ''}
        <button class="btn btn-sm btn-ghost" data-fn="${act(() => copyText(v.id, 'Copied version ID'))}">Copy ID</button>
        <button class="btn btn-sm btn-ghost btn-icon" aria-label="Details" data-fn="${inspect}">⋯</button>
      </div>
    </div>`;
  })}</div>`;
}

function inspectVersion(name, ds, v) {
  const parent = v.parent ? ds.versions.find((x) => x.id === v.parent) : null;
  const actions = [
    { label: 'Browse files', href: `#/d/${enc(name)}/files?v=${enc(v.id)}`, primary: true },
    { label: 'Copy URI', fn: () => copyText(`dataset://${name}/${v.id}`, 'Copied dataset URI') },
    { label: 'Copy IDs for MLflow', fn: () => copyText(mlflowIds(name, v), 'Copied dataset identifiers') },
    { label: 'Lineage', href: `#/d/${enc(name)}/lineage?v=${enc(v.id)}` },
  ];
  if (v.parent) actions.push({ label: 'Compare with parent', href: `#/d/${enc(name)}/compare?a=${enc(v.parent)}&b=${enc(v.id)}` });
  if (S.me) {
    actions.push({ label: 'Tag…', fn: () => openRefDialog(name, ds, v, 'tag') });
    actions.push({ label: 'Move branch here…', fn: () => openRefDialog(name, ds, v, 'branch') });
  }
  openInspector({
    title: `${vLabel(ds, v.id)} · ${versionTitle(v)}`, subtitle: `${v.created_by} · ${fmtDate(v.created_at)}`,
    rows: [
      ['Version ID', v.id, true], ['Manifest', v.manifest_hash, true], ['Size', fmtSize(v.total_size)], ['Files', fmtInt(v.file_count)],
      ['Parent', parent ? `${vLabel(ds, parent.id)} · ${shortId(parent.id)}` : v.parent ? v.parent : '—'],
      ['Produced by', v.producer ? `${v.producer.type}:${v.producer.id}` : '—'],
      ['Schema', v.schema_hash || '—', !!v.schema_hash],
      ['Refs', refsFor(ds, v.id).map((r) => r.name).join(', ') || '—'],
    ],
    actions,
    extra: Object.keys(v.metadata || {}).length ? html`<div><div class="field"><label>Metadata</label></div><div class="codeblock">${JSON.stringify(v.metadata, null, 2)}</div></div>` : '',
  });
}

// ---------- Lineage ----------

function tabLineage(name, ds) {
  if (!ds.versions.length) return noVersions(name, ds);
  const ref = S.route.q.get('v') || defaultRef(ds);
  const v = resolveRef(ds, ref);
  if (!v) return emptyState({ icon: '?', title: 'Version not found' });
  const depth = Number(S.route.q.get('depth') || 3);
  const uri = `dataset://${name}/${v.id}`;
  const key = `${uri}|${depth}`;
  if (!S.lineage[key]) loadLineage(uri, depth, `/datasets/${enc(name)}/versions/${enc(v.id)}/lineage?depth=${depth}`);
  return html`
    <div class="catalog-tools">
      ${versionSelect(ds, ref, (val) => setQuery({ v: val }))}
      <span class="meta">Depth</span>
      ${segmented([['1', '1'], ['3', '3'], ['5', '5'], ['10', '10']], String(depth), (d) => setQuery({ depth: d }), 'Depth')}
    </div>
    ${lineageBody(key, uri, ds, () => { delete S.lineage[key]; renderPage(); })}`;
}

function lineageBody(key, uri, ds, retry) {
  const entry = S.lineage[key];
  if (!entry || entry.loading) return skeletonCards(2);
  if (entry.error) return errorState(entry.error, 'load lineage', retry);
  if (!entry.graph.edges.length) {
    return emptyState({
      icon: '⌥', title: 'No lineage recorded',
      text: 'Record inputs and producers when creating versions (ds version create --input raw@latest --producer pipeline:run-1), or usage with ds used.',
    });
  }
  afterRenderHooks.push(() => mountGraph($('#graph'), entry.graph, uri, ds));
  return html`<div class="graph-wrap" id="graph"></div>
    <div class="legend"><span>▢ Dataset version</span><span>▣ Run / external</span><span><span class="dot busy" aria-hidden="true"></span>Selected path</span><span>Click a node to trace its lineage</span></div>`;
}

function parseUri(uri) {
  const m = /^([a-z][a-z0-9+.-]*):\/\/(.*)$/i.exec(uri);
  if (!m) return { scheme: 'uri', rest: uri };
  const [scheme, rest] = [m[1].toLowerCase(), m[2]];
  if (scheme === 'dataset') {
    const i = rest.lastIndexOf('/');
    return { scheme, dataset: rest.slice(0, i), version: rest.slice(i + 1) };
  }
  return { scheme, rest };
}

/** Layered DAG layout + SVG render. Selection highlights ancestor/descendant paths. */
function mountGraph(container, graph, rootUri, ds) {
  if (!container) return;
  const nodes = graph.nodes.slice();
  const index = new Map(nodes.map((n, i) => [n, i]));
  const edges = graph.edges.map((e) => ({ ...e, a: index.get(e.from), b: index.get(e.to) })).filter((e) => e.a !== undefined && e.b !== undefined && e.a !== e.b);
  const N = nodes.length;
  const layer = new Array(N).fill(0);
  for (let it = 0; it < N; it++) {
    let changed = false;
    for (const e of edges) if (layer[e.b] < layer[e.a] + 1 && layer[e.a] + 1 < N) { layer[e.b] = layer[e.a] + 1; changed = true; }
    if (!changed) break;
  }
  const layers = [];
  nodes.forEach((_, i) => { (layers[layer[i]] = layers[layer[i]] || []).push(i); });
  const pos = new Array(N).fill(0);
  layers.forEach((ids, li) => {
    if (li > 0) {
      const bary = (i) => { const ps = edges.filter((e) => e.b === i).map((e) => pos[e.a]); return ps.length ? ps.reduce((a, b) => a + b, 0) / ps.length : Infinity; };
      ids.sort((x, y) => bary(x) - bary(y));
    }
    ids.forEach((id, k) => { pos[id] = k; });
  });
  const W = 212; const H = 60; const GX = 36; const GY = 76; const PAD = 28;
  const maxCount = Math.max(...layers.map((l) => (l ? l.length : 0)));
  const width = Math.max(maxCount * (W + GX) - GX + PAD * 2, 480);
  const height = layers.length * (H + GY) - GY + PAD * 2;
  const xy = nodes.map((_, i) => {
    const count = layers[layer[i]].length;
    const rowW = count * (W + GX) - GX;
    return [(width - rowW) / 2 + pos[i] * (W + GX), PAD + layer[i] * (H + GY)];
  });
  const idx = ds ? versionIndex(ds) : new Map();
  const label = (uri) => {
    const p = parseUri(uri);
    if (p.scheme === 'dataset') {
      const own = ds && ds.info.dataset.name === p.dataset && idx.get(p.version);
      return { kind: 'DATASET', title: p.dataset, sub: own ? `v${idx.get(p.version)} · ${shortId(p.version)}` : shortId(p.version), run: false };
    }
    return { kind: p.scheme.toUpperCase(), title: truncate(p.rest, 28), sub: '', run: true };
  };
  const edgePath = (e) => {
    const [x1, y1] = [xy[e.a][0] + W / 2, xy[e.a][1] + H];
    const [x2, y2] = [xy[e.b][0] + W / 2, xy[e.b][1] - 4];
    const dy = Math.max(24, (y2 - y1) / 2);
    return `M ${x1} ${y1} C ${x1} ${y1 + dy}, ${x2} ${y2 - dy}, ${x2} ${y2}`;
  };
  clearScope('graph');
  container.innerHTML = esc(html`<svg class="graph" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}" role="img" aria-label="Lineage graph">
    <defs>
      <marker id="arr" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path class="g-arrow" d="M 0 0 L 10 5 L 0 10 z"></path></marker>
      <marker id="arr-active" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path class="g-arrow-active" d="M 0 0 L 10 5 L 0 10 z"></path></marker>
    </defs>
    ${edges.map((e, i) => html`<path class="g-edge" data-edge="${i}" d="${edgePath(e)}" marker-end="url(#arr)"></path>
      <text class="g-edge-label" x="${(xy[e.a][0] + xy[e.b][0]) / 2 + W / 2 + 6}" y="${(xy[e.a][1] + H + xy[e.b][1]) / 2 + 3}">${e.kind}</text>`)}
    ${nodes.map((uri, i) => {
      const l = label(uri);
      return html`<g class="g-node ${l.run ? 'run' : ''} ${uri === rootUri ? 'root' : ''}" data-node="${i}" transform="translate(${xy[i][0]}, ${xy[i][1]})" tabindex="0" role="button" aria-label="${l.kind} ${l.title} ${l.sub}" data-fn="${act(() => selectNode(i), 'graph')}">
        <title>${uri}</title>
        <rect width="${W}" height="${H}" rx="6"></rect>
        <text class="g-kind" x="12" y="17">${l.kind}</text>
        <text class="g-title" x="12" y="35">${truncate(l.title, 28)}</text>
        <text class="g-sub" x="12" y="51">${l.sub}</text>
      </g>`;
    })}
  </svg>`);

  const svg = container.querySelector('svg');
  function selectNode(i) {
    const up = new Set([i]); const down = new Set([i]);
    for (let changed = true; changed;) { changed = false; for (const e of edges) if (up.has(e.b) && !up.has(e.a)) { up.add(e.a); changed = true; } }
    for (let changed = true; changed;) { changed = false; for (const e of edges) if (down.has(e.a) && !down.has(e.b)) { down.add(e.b); changed = true; } }
    const onPath = (e) => (up.has(e.a) && up.has(e.b)) || (down.has(e.a) && down.has(e.b));
    svg.querySelectorAll('.g-edge').forEach((p) => {
      const active = onPath(edges[Number(p.dataset.edge)]);
      p.classList.toggle('active', active); p.classList.toggle('dim', !active);
      p.setAttribute('marker-end', active ? 'url(#arr-active)' : 'url(#arr)');
    });
    svg.querySelectorAll('.g-node').forEach((g) => {
      const n = Number(g.dataset.node);
      g.classList.toggle('selected', n === i);
      g.classList.toggle('dim', !up.has(n) && !down.has(n));
    });
    inspectNode(nodes[i], edges.filter((e) => e.a === i || e.b === i).map((e) => ({ ...e, other: nodes[e.a === i ? e.b : e.a], out: e.a === i })), ds);
  }
  const rootIdx = index.get(rootUri);
  if (rootIdx !== undefined) {
    const g = svg.querySelector(`[data-node="${rootIdx}"]`);
    if (g && g.scrollIntoView) g.scrollIntoView({ block: 'nearest', inline: 'center' });
  }
}

function inspectNode(uri, links, ds) {
  const p = parseUri(uri);
  const actions = [{ label: 'Copy URI', fn: () => copyText(uri, 'Copied URI') }];
  if (p.scheme === 'dataset') {
    actions.unshift({ label: 'Open version', href: `#/d/${enc(p.dataset)}/files?v=${enc(p.version)}`, primary: true });
    actions.push({ label: 'Trace from here', href: `#/d/${enc(p.dataset)}/lineage?v=${enc(p.version)}` });
  }
  openInspector({
    title: p.scheme === 'dataset' ? p.dataset : p.rest, subtitle: uri,
    rows: [
      ['Type', p.scheme === 'dataset' ? 'Dataset version' : p.scheme],
      ...(p.scheme === 'dataset' ? [['Version', ds && ds.info.dataset.name === p.dataset ? `${vLabel(ds, p.version)} · ${p.version}` : p.version, true]] : []),
    ],
    actions,
    extra: html`<div><div class="field"><label>Connections</label></div>
      <div class="timeline">${links.map((l) => html`<div class="tl-item"><span class="tl-icon" aria-hidden="true">${l.out ? '→' : '←'}</span>
        <span><span class="meta">${l.out ? l.kind : `${l.kind} (incoming)`}</span><br><span class="mono">${l.other}</span>${l.producer ? html`<br><span class="meta">via ${l.producer.type}:${l.producer.id}</span>` : ''}</span><span></span></div>`)}</div></div>`,
  });
}

// ---------- Compare ----------

function tabCompare(name, ds) {
  if (ds.versions.length < 1) return noVersions(name, ds);
  if (ds.versions.length < 2) return emptyState({ icon: '⇄', title: 'Only one version so far', text: 'Create another version to compare changes between them.', action: S.me ? html`<button class="btn" data-fn="${act(() => openNewVersion(name, ds))}">New version</button>` : '' });
  const latest = ds.versions[0];
  const a = S.route.q.get('a') || latest.parent || ds.versions[1].id;
  const b = S.route.q.get('b') || latest.id;
  const va = resolveRef(ds, a); const vb = resolveRef(ds, b);
  const bar = html`<div class="compare-bar">
    ${versionSelect(ds, a, (val) => setQuery({ a: val }), 'cmp-a')}
    <span class="compare-arrow" aria-hidden="true">→</span>
    ${versionSelect(ds, b, (val) => setQuery({ b: val }), 'cmp-b')}
    <button class="btn btn-ghost btn-sm" data-fn="${act(() => setQuery({ a: b, b: a }))}" aria-label="Swap">⇄ Swap</button>
  </div>`;
  if (!va || !vb) return html`${bar}${emptyState({ icon: '?', title: 'Version not found' })}`;
  const key = `${name}|${va.id}|${vb.id}`;
  const entry = S.compare[key];
  if (!entry) loadCompare(name, va.id, vb.id);
  if (!entry || entry.loading) return html`${bar}${skeletonRows(6)}`;
  if (entry.error) return html`${bar}${errorState(entry.error, 'compare versions', () => { delete S.compare[key]; renderPage(); })}`;
  return html`${bar}${compareView(ds, va, vb, entry.data)}`;
}

function delta(oldV, newV, fmt = (x) => x) {
  if (oldV === null || newV === null || oldV === undefined || newV === undefined) return '';
  const d = Number(newV) - Number(oldV);
  if (!Number.isFinite(d) || d === 0) return html`<span class="muted">no change</span>`;
  const pct = Number(oldV) ? ` (${d > 0 ? '+' : ''}${((d / Number(oldV)) * 100).toFixed(1)}%)` : '';
  return html`<span class="${d > 0 ? 'delta-up' : 'delta-down'}">${d > 0 ? '+' : '−'}${fmt(Math.abs(d))}${pct}</span>`;
}

function compareView(ds, va, vb, d) {
  const s = d.storage;
  const oldFiles = s.unchanged + s.changed.length + s.removed.length;
  const newFiles = s.unchanged + s.changed.length + s.added.length;
  const ma = va.metadata || {}; const mb = vb.metadata || {};
  const rows = [
    ['Files', fmtInt(oldFiles), fmtInt(newFiles), delta(oldFiles, newFiles, fmtInt)],
    ['Size', fmtSize(s.old_bytes), fmtSize(s.new_bytes), delta(s.old_bytes, s.new_bytes, fmtSize)],
  ];
  if (rowsOf(ma) !== null || rowsOf(mb) !== null) rows.push(['Rows', fmtCompact(rowsOf(ma)) || '—', fmtCompact(rowsOf(mb)) || '—', delta(rowsOf(ma), rowsOf(mb), fmtCompact)]);
  if (colsOf(ma) !== null || colsOf(mb) !== null) rows.push(['Columns', colsOf(ma) ?? '—', colsOf(mb) ?? '—', delta(colsOf(ma), colsOf(mb))]);
  const skip = new Set(['rows', 'row_count', 'num_rows', 'columns', 'num_columns', 'n_columns', 'features', 'message', 'title']);
  const metaChanges = Object.entries(d.structure.metadata_changes || {}).filter(([k]) => !skip.has(k));
  const lines = [
    ...s.added.map((f) => ({ kind: 'add', sign: '+', path: f.path, detail: fmtSize(f.size) })),
    ...s.removed.map((f) => ({ kind: 'remove', sign: '−', path: f.path, detail: fmtSize(f.size) })),
    ...s.changed.map((f) => ({ kind: 'change', sign: '~', path: f.path, detail: `${fmtSize(f.old_size)} → ${fmtSize(f.new_size)}` })),
  ].sort((x, y) => x.path.localeCompare(y.path));
  const LIMIT = 500;
  return html`
    <div class="row">
      <h2 class="section-title grow">${vLabel(ds, va.id)} → ${vLabel(ds, vb.id)}</h2>
      ${d.identical ? badge('Identical content', 'success') : badge(`${lines.length} file change${lines.length === 1 ? '' : 's'}`, 'warning')}
      ${d.structure.schema_changed ? badge('Schema changed', 'warning') : badge('Schema unchanged', 'plain')}
    </div>
    <div class="section-gap">
      <table class="table summary-table"><thead><tr><th>Summary</th><th class="num">${vLabel(ds, va.id)}</th><th class="num">${vLabel(ds, vb.id)}</th><th class="num">Change</th></tr></thead>
      <tbody>${rows.map(([l, o, n, dl]) => html`<tr><td>${l}</td><td class="num mono">${o}</td><td class="num mono">${n}</td><td class="num">${dl}</td></tr>`)}
      <tr><td>Bytes to transfer</td><td class="num"></td><td class="num mono">${fmtSize(s.new_blob_bytes)}</td><td class="num meta">new blobs only</td></tr>
      </tbody></table>
    </div>
    <div class="section">
      <div class="section-head"><h2 class="section-title">Changes</h2><span class="change-summary"><span class="add">+${s.added.length}</span><span class="change">~${s.changed.length}</span><span class="remove">−${s.removed.length}</span></span></div>
      ${lines.length ? html`<div class="diff" role="list">${lines.slice(0, LIMIT).map((l) => html`<div class="diff-line ${l.kind}" role="listitem"><span class="sign" aria-label="${{ add: 'added', remove: 'removed', change: 'changed' }[l.kind]}">${l.sign}</span><span class="path">${l.path}</span><span class="detail">${l.detail}</span></div>`)}</div>
        ${lines.length > LIMIT ? html`<p class="meta section-gap">Showing ${LIMIT} of ${fmtInt(lines.length)} changes.</p>` : ''}`
        : emptyState({ icon: '=', title: 'No file changes', text: 'Both versions contain exactly the same files.' })}
    </div>
    ${metaChanges.length ? html`<div class="section"><div class="section-head"><h2 class="section-title">Metadata</h2></div>
      <div class="diff">${metaChanges.map(([k, v]) => html`<div class="diff-line change"><span class="sign">~</span><span class="path">${k}</span><span class="detail">${JSON.stringify(v.old) ?? '—'} → ${JSON.stringify(v.new) ?? '—'}</span></div>`)}</div></div>` : ''}
    <div class="section">
      <div class="section-head"><h2 class="section-title">Distribution changes</h2></div>
      <p class="meta">Data-level statistics and drift aren’t computed yet. Storage and structure differences are shown above.</p>
    </div>`;
}

// ---------- Activity ----------

function tabActivity(name, ds) {
  const items = [
    ...ds.versions.map((v) => ({ when: v.created_at, icon: '●', text: html`<strong>${v.created_by}</strong> created <a href="#/d/${enc(name)}/files?v=${enc(v.id)}">${vLabel(ds, v.id)}</a> — ${versionTitle(v)}` })),
    ...(ds.info.refs || []).map((r) => ({ when: r.updated_at, icon: r.kind === 'branch' ? '⎇' : '⌗', text: html`${r.kind === 'branch' ? 'Branch' : 'Tag'} <strong>${r.name}</strong> → ${vLabel(ds, r.version_id)}` })),
    { when: ds.info.dataset.created_at, icon: '+', text: html`<strong>${ds.info.dataset.owner}</strong> created the dataset` },
  ].sort((a, b) => String(b.when).localeCompare(String(a.when)));
  return html`<div class="timeline">${items.map((it) => html`<div class="tl-item"><span class="tl-icon" aria-hidden="true">${it.icon}</span><span>${it.text}</span><span class="meta" title="${fmtDate(it.when)}">${ago(it.when)}</span></div>`)}</div>`;
}

// ======================================================================
// Page: Lineage explorer
// ======================================================================

function pageLineage() {
  const target = S.route.q.get('t') || '';
  const depth = Number(S.route.q.get('depth') || 3);
  const submit = act(() => { const t = $('#lineage-target').value.trim(); if (t) setQuery({ t }); });
  let body = emptyState({ icon: '⌥', title: 'Explore lineage', text: 'Enter a dataset (name@ref) or any URI such as mlflow://run/abc to see where data came from and where it went.' });
  if (target) {
    if (target.includes('://')) {
      const key = `${target}|${depth}`;
      if (!S.lineage[key]) loadLineage(target, depth, `/lineage?uri=${enc(target)}&depth=${depth}`);
      body = lineageBody(key, target, null, () => { delete S.lineage[key]; renderPage(); });
    } else {
      const [dsName, ref = 'latest'] = target.split('@');
      const ds = S.ds[dsName];
      if (!ds) { loadDataset(dsName); body = skeletonCards(2); } else if (ds.error) body = errorState(ds.error, `load ${dsName}`, () => loadDataset(dsName, { force: true }));
      else {
        const v = resolveRef(ds, ref);
        if (!v) body = emptyState({ icon: '?', title: 'Version not found', text: `“${ref}” doesn’t match a version, tag or branch of ${dsName}.` });
        else {
          const uri = `dataset://${dsName}/${v.id}`; const key = `${uri}|${depth}`;
          if (!S.lineage[key]) loadLineage(uri, depth, `/datasets/${enc(dsName)}/versions/${enc(v.id)}/lineage?depth=${depth}`);
          body = lineageBody(key, uri, ds, () => { delete S.lineage[key]; renderPage(); });
        }
      }
    }
  }
  const names = (S.datasets || []).map((d) => d.name);
  return html`<div class="page-head"><h1 class="page-title">Lineage</h1></div>
    <form class="catalog-tools" data-submit="${submit}">
      <div class="search-box"><input class="input" id="lineage-target" list="lineage-datasets" placeholder="multicredit@training  or  mlflow://run/abc123" value="${target}" aria-label="Dataset or URI"></div>
      <datalist id="lineage-datasets">${names.map((n) => html`<option value="${n}@latest">`)}</datalist>
      ${segmented([['1', '1'], ['3', '3'], ['5', '5'], ['10', '10']], String(depth), (d) => setQuery({ depth: d }), 'Depth')}
      <button class="btn btn-primary" type="submit">Show lineage</button>
    </form>
    ${body}`;
}

// ======================================================================
// Page: Transfers
// ======================================================================

function pageTransfers() {
  const done = S.transfers.filter((t) => t.state !== 'active').length;
  return html`<div class="page-head"><h1 class="page-title">Transfers</h1>
      ${done ? html`<button class="btn btn-ghost" data-fn="${act(() => { S.transfers = S.transfers.filter((t) => t.state === 'active'); renderPage(); renderStatusbar(); })}">Clear finished</button>` : ''}</div>
    <div id="transfer-list">${transferList()}</div>
    <p class="meta section">Browser uploads stream in 32 MB parts with retries. For very large datasets or resumable transfers across restarts, use <code>ds version create</code> and <code>ds pull</code>.</p>`;
}

function transferList() {
  clearScope('transfers');
  if (!S.transfers.length) return emptyState({ icon: '⇅', title: 'No transfers', text: 'Uploads you start from “New version” appear here with live progress.' });
  return html`${S.transfers.slice().reverse().map((t) => {
    const pct = t.total ? Math.min(100, (t.done / t.total) * 100) : 0;
    const remaining = t.rate > 0 ? (t.total - t.done) / t.rate : NaN;
    return html`<div class="transfer">
      <div class="transfer-head">
        <span class="transfer-title">${t.title}</span>
        ${t.state === 'active' ? badge('Uploading', 'accent') : t.state === 'done' ? badge('Completed', 'success') : badge('Failed', 'danger')}
      </div>
      <div class="meta">${t.state === 'failed' ? t.error : t.phase}</div>
      <progress class="${t.state === 'done' ? 'done' : t.state === 'failed' ? 'failed' : ''}" max="100" value="${t.state === 'done' ? 100 : pct.toFixed(1)}" aria-label="${t.title} progress"></progress>
      <div class="transfer-foot">
        <span>${fmtSize(t.done)} / ${fmtSize(t.total)}</span>
        ${t.state === 'active' && t.rate ? html`<span>${fmtSize(t.rate)}/s</span><span>${fmtDuration(remaining)} remaining</span>` : ''}
        ${t.state === 'done' && t.link ? html`<a href="${t.link}">Open version →</a>` : ''}
        ${t.state === 'failed' && t.retry ? html`<button class="btn btn-sm" data-fn="${act(t.retry, 'transfers')}">Retry</button>` : ''}
      </div>
    </div>`;
  })}`;
}

// Refresh transfer rates and progress UI twice a second.
setInterval(() => {
  const now = performance.now();
  let active = false;
  for (const t of S.transfers) {
    if (t.state !== 'active') continue;
    active = true;
    const dt = (now - (t.lastT || t.started)) / 1000;
    if (dt >= 0.5) {
      const inst = (t.done - (t.lastDone || 0)) / dt;
      t.rate = t.rate ? t.rate * 0.7 + inst * 0.3 : inst;
      t.lastT = now; t.lastDone = t.done;
    }
  }
  if (active || S.transferDirty) {
    S.transferDirty = false;
    if (S.route.name === 'transfers' && $('#transfer-list')) $('#transfer-list').innerHTML = esc(transferList());
    renderStatusbar();
  }
}, 500);

window.addEventListener('beforeunload', (e) => {
  if (S.transfers.some((t) => t.state === 'active')) { e.preventDefault(); e.returnValue = ''; }
});

// ======================================================================
// Uploads (browser)
// ======================================================================

const PART = 32 * 1024 * 1024;
const EMPTY_SHA = 'sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855';

function xhr(method, url, body, onProgress) {
  return new Promise((resolve, reject) => {
    const r = new XMLHttpRequest();
    r.open(method, url);
    r.setRequestHeader('x-ds-csrf', '1');
    r.withCredentials = true;
    if (onProgress) r.upload.onprogress = (e) => onProgress(e.loaded);
    r.onload = () => {
      if (r.status >= 200 && r.status < 300) { try { resolve(r.responseText ? JSON.parse(r.responseText) : null); } catch { resolve(null); } }
      else {
        let msg = r.statusText;
        try { msg = JSON.parse(r.responseText).error || msg; } catch { /* keep status text */ }
        reject(new ApiError(r.status, msg, `HTTP ${r.status}\n${method} ${url}\n\n${r.responseText.slice(0, 1000)}`));
      }
    };
    r.onerror = () => reject(new ApiError(0, 'network', `${method} ${url}`));
    r.send(body);
  });
}

async function withRetry(fn, attempts = 4) {
  for (let i = 0; ; i++) {
    try { return await fn(); } catch (e) {
      if (i + 1 >= attempts || (e.status >= 400 && e.status < 500)) throw e;
      await new Promise((r) => setTimeout(r, 500 * 2 ** (i + 1)));
    }
  }
}

async function sha256Hex(buf) {
  const d = await crypto.subtle.digest('SHA-256', buf);
  return Array.from(new Uint8Array(d), (b) => b.toString(16).padStart(2, '0')).join('');
}

/** Upload one File as a blob; returns { hash, size }. Progress reported via `credit(bytes)` deltas. */
async function uploadBlob(file, credit) {
  if (file.size === 0) {
    await withRetry(() => xhr('PUT', `/api/blobs/${EMPTY_SHA}`, new Blob([])));
    return { hash: EMPTY_SHA, size: 0 };
  }
  if (file.size <= PART && window.crypto && crypto.subtle) {
    const buf = await file.arrayBuffer();
    const hash = `sha256:${await sha256Hex(buf)}`;
    let sent = 0;
    await withRetry(async () => {
      credit(-sent); sent = 0;
      await xhr('PUT', `/api/blobs/${hash}`, buf, (loaded) => { credit(loaded - sent); sent = loaded; });
      credit(file.size - sent); sent = file.size;
    });
    return { hash, size: file.size };
  }
  // Multipart: the server hashes while assembling, so no client-side hashing is needed.
  const up = await api('/uploads', { method: 'POST', body: { size: file.size } });
  const partSize = Math.max(PART, Math.ceil(file.size / 9000));
  const parts = Math.ceil(file.size / partSize);
  let next = 1;
  const worker = async () => {
    while (next <= parts) {
      const n = next++;
      const start = (n - 1) * partSize;
      const blob = file.slice(start, Math.min(file.size, start + partSize));
      let sent = 0;
      await withRetry(async () => {
        credit(-sent); sent = 0;
        await xhr('PUT', `/api/uploads/${up.id}/parts/${n}`, blob, (loaded) => { credit(loaded - sent); sent = loaded; });
        credit(blob.size - sent); sent = blob.size;
      });
    }
  };
  await Promise.all(Array.from({ length: Math.min(3, parts) }, worker));
  const blob = await withRetry(() => api(`/uploads/${up.id}/complete`, { method: 'POST', body: {} }));
  return { hash: blob.hash, size: blob.size };
}

function startVersionJob(name, plan) {
  const t = {
    id: Date.now() + Math.random(), title: `${name} · ${plan.uploads.length} file${plan.uploads.length === 1 ? '' : 's'}`,
    phase: 'Starting', total: plan.uploads.reduce((a, u) => a + u.file.size, 0), done: 0, started: performance.now(), state: 'active',
  };
  S.transfers.push(t);
  S.transferDirty = true;
  const run = async () => {
    t.state = 'active'; t.error = null; t.done = 0; t.rate = 0; t.lastDone = 0; t.lastT = performance.now();
    S.transferDirty = true;
    try {
      const uploaded = new Map();
      let i = 0;
      for (const u of plan.uploads) {
        i++;
        t.phase = `Uploading ${u.path} (${i}/${plan.uploads.length})`;
        const r = await uploadBlob(u.file, (d) => { t.done += d; });
        uploaded.set(u.path, r);
      }
      t.phase = 'Creating version';
      const files = new Map(plan.inherit.map((f) => [f.path, { path: f.path, blob: f.blob, size: f.size }]));
      for (const [path, r] of uploaded) files.set(path, { path, blob: r.hash, size: r.size });
      const v = await api(`/datasets/${enc(name)}/versions`, { method: 'POST', body: {
        files: Array.from(files.values()), parent: plan.parent, branch: plan.branch || null,
        metadata: plan.metadata, producer: plan.producer, inputs: plan.inputs,
      } });
      t.state = 'done'; t.done = t.total; t.phase = `Created version ${shortId(v.id)}`;
      t.link = `#/d/${enc(name)}/files?v=${enc(v.id)}`;
      invalidateDataset(name);
      await loadDataset(name, { force: true });
      const ds = S.ds[name];
      toast(html`✓ ${ds && ds.versions ? vLabel(ds, v.id) : 'Version'} of ${name} created`, 'success', { label: 'Open', href: t.link });
    } catch (e) {
      t.state = 'failed'; t.error = friendly(e, 'upload').message;
      t.retry = () => run();
      toast(html`✕ Upload to ${name} failed`, 'error', { label: 'Retry', fn: () => run() });
    }
    S.transferDirty = true;
  };
  run();
}

// ======================================================================
// Dialogs
// ======================================================================

function openModal(content, { wide = false, onClose } = {}) {
  closeMenu();
  clearScope('modal');
  const ov = $('#overlay');
  ov.innerHTML = esc(html`<div class="${wide ? 'modal modal-wide' : 'modal'}" role="dialog" aria-modal="true">${content}</div>`);
  ov.hidden = false;
  ov.dataset.kind = 'modal';
  S.modalClose = onClose;
  S.lastFocus = document.activeElement;
  setTimeout(() => { const f = ov.querySelector('input:not([type=file]), select, textarea, button'); if (f) f.focus(); }, 0);
  return ov.firstElementChild;
}
function closeModal() {
  const ov = $('#overlay');
  if (ov.hidden) return false;
  ov.hidden = true; ov.innerHTML = '';
  clearScope('modal');
  if (S.modalClose) { const f = S.modalClose; S.modalClose = null; f(); }
  if (S.lastFocus && S.lastFocus.focus) S.lastFocus.focus();
  return true;
}
const ma = (fn) => act(fn, 'modal');

function openCreateDataset() {
  const create = ma(async () => {
    const name = $('#nd-name').value.trim();
    const description = $('#nd-desc').value.trim();
    const err = $('#nd-error');
    if (!/^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/.test(name) || name.includes('..')) {
      err.textContent = 'Use letters, digits, “-”, “_” and “.”; start with a letter or digit.'; err.hidden = false; return;
    }
    try {
      await api('/datasets', { method: 'POST', body: { name, description } });
      closeModal();
      toast(`Created ${name}`, 'success');
      S.datasets = null; loadDatasets();
      go(`#/d/${enc(name)}`);
    } catch (e) { err.textContent = friendly(e, 'create the dataset').message; err.hidden = false; }
  });
  openModal(html`<form data-submit="${create}">
    <div class="modal-head"><h2 class="section-title">Create dataset</h2><button type="button" class="btn btn-ghost btn-icon" aria-label="Close" data-fn="${ma(closeModal)}">✕</button></div>
    <div class="modal-body">
      <div class="field"><label for="nd-name">Name</label><input class="input" id="nd-name" placeholder="multicredit-v2" autocomplete="off" spellcheck="false"></div>
      <div class="field"><label for="nd-desc">Description</label><textarea class="input" id="nd-desc" placeholder="Training dataset for …"></textarea></div>
      <p class="meta">Files are stored as content-addressed blobs on the server’s configured storage. You’ll add files in the next step.</p>
      <p class="meta" id="nd-error" role="alert" hidden></p>
    </div>
    <div class="modal-foot"><button type="button" class="btn" data-fn="${ma(closeModal)}">Cancel</button><button class="btn btn-primary" type="submit">Create</button></div>
  </form>`);
}

function openRefDialog(name, ds, v, kind) {
  closeInspector();
  const submit = ma(async () => {
    const refName = $('#ref-name').value.trim();
    const err = $('#ref-error');
    if (!refName) { err.textContent = 'Enter a name.'; err.hidden = false; return; }
    try {
      await api(`/datasets/${enc(name)}/${kind === 'tag' ? 'tags' : 'branches'}`, { method: 'POST', body: { name: refName, version: v.id } });
      closeModal();
      toast(`${kind === 'tag' ? 'Tagged' : 'Moved branch'} ${refName} → ${vLabel(ds, v.id)}`, 'success');
      invalidateDataset(name); loadDataset(name, { force: true });
    } catch (e) { err.textContent = friendly(e, kind === 'tag' ? 'create the tag' : 'move the branch').message; err.hidden = false; }
  });
  const branches = (ds.info.refs || []).filter((r) => r.kind === 'branch');
  openModal(html`<form data-submit="${submit}">
    <div class="modal-head"><h2 class="section-title">${kind === 'tag' ? 'Tag' : 'Move branch to'} ${vLabel(ds, v.id)}</h2><button type="button" class="btn btn-ghost btn-icon" aria-label="Close" data-fn="${ma(closeModal)}">✕</button></div>
    <div class="modal-body">
      <div class="field"><label for="ref-name">${kind === 'tag' ? 'Tag name' : 'Branch name'}</label>
        <input class="input" id="ref-name" list="ref-branches" placeholder="${kind === 'tag' ? 'v4-training-set' : 'training/latest'}" autocomplete="off" spellcheck="false">
        ${kind === 'branch' ? html`<datalist id="ref-branches">${branches.map((b) => html`<option value="${b.name}">`)}</datalist>` : ''}
        <span class="hint">${kind === 'tag' ? 'Tags are immutable and can’t be moved later.' : 'Branches are mutable pointers; an existing branch will move to this version.'}</span></div>
      <p class="meta" id="ref-error" role="alert" hidden></p>
    </div>
    <div class="modal-foot"><button type="button" class="btn" data-fn="${ma(closeModal)}">Cancel</button><button class="btn btn-primary" type="submit">${kind === 'tag' ? 'Create tag' : 'Move branch'}</button></div>
  </form>`);
}

function openPull(name, ds) {
  const ref = defaultRef(ds);
  const v = resolveRef(ds, ref);
  const cmd = `export DS_URL=${location.origin}\nds pull ${name}@${ref} --to ./${name}`;
  openModal(html`
    <div class="modal-head"><h2 class="section-title">Pull ${name}</h2><button type="button" class="btn btn-ghost btn-icon" aria-label="Close" data-fn="${ma(closeModal)}">✕</button></div>
    <div class="modal-body">
      <p class="secondary">Download ${ref === 'latest' ? 'the latest version' : html`<strong>${ref}</strong>`} (${vLabel(ds, v.id)}, ${fmtSize(v.total_size)}, ${fmtInt(v.file_count)} files). The CLI resumes interrupted downloads, verifies every file’s sha256 and skips files you already have.</p>
      ${codeBlock(cmd)}
      <dl class="kv"><dt>Version</dt><dd class="mono">${v.id}</dd><dt>Manifest</dt><dd class="mono">${v.manifest_hash}</dd></dl>
    </div>
    <div class="modal-foot">
      <button class="btn" data-fn="${ma(() => copyText(mlflowIds(name, v), 'Copied dataset identifiers'))}">Copy IDs</button>
      <a class="btn btn-primary" href="#/d/${enc(name)}/files?v=${enc(ref)}" data-fn="${ma(closeModal)}">Download individual files</a>
    </div>`);
}

/** New version: base version + added/replaced files → upload plan. */
function openNewVersion(name, ds) {
  const branches = (ds.info.refs || []).filter((r) => r.kind === 'branch');
  const nv = { base: ds.versions.length ? defaultRef(ds) : '', picked: new Map(), baseFiles: null, replaceAll: false, prefix: '' };

  const loadBase = async () => {
    nv.baseFiles = null; refresh();
    const v = nv.base ? resolveRef(ds, nv.base) : null;
    if (!v) { nv.baseFiles = []; refresh(); return; }
    try {
      const r = await api(`/datasets/${enc(name)}/versions/${enc(v.id)}/files?limit=100000`);
      nv.baseFiles = r.files;
    } catch (e) { nv.baseFiles = []; toast(friendly(e, 'load base files').message, 'error'); }
    refresh();
  };
  const addFiles = (list) => {
    for (const { file, path } of list) {
      const full = (nv.prefix ? `${nv.prefix.replace(/\/+$/, '')}/` : '') + path;
      nv.picked.set(full, file);
    }
    refresh();
  };
  const plan = () => {
    const base = nv.replaceAll ? [] : (nv.baseFiles || []);
    const baseMap = new Map(base.map((f) => [f.path, f]));
    const added = []; const replaced = [];
    for (const [path, file] of nv.picked) (baseMap.has(path) ? replaced : added).push({ path, file });
    const removed = nv.replaceAll ? (nv.baseFiles || []).filter((f) => !nv.picked.has(f.path)) : [];
    return { added, replaced, removed, inherit: base.filter((f) => !nv.picked.has(f.path)) };
  };
  const refresh = () => {
    const el = $('#nv-preview');
    if (!el) return;
    clearScope('nvp');
    const p = plan();
    const invalid = Array.from(nv.picked.keys()).filter((path) => !validPath(path));
    el.innerHTML = esc(html`
      ${nv.baseFiles === null ? html`<p class="meta"><span class="spinner" aria-hidden="true"></span> Loading base version…</p>` : ''}
      <div class="row"><strong class="grow">Changes</strong><span class="change-summary"><span class="add">+${p.added.length} file${p.added.length === 1 ? '' : 's'}</span><span class="change">~${p.replaced.length} replaced</span><span class="remove">−${p.removed.length}</span></span></div>
      <p class="meta">${fmtInt(p.inherit.length)} unchanged files are reused without uploading.</p>
      ${nv.picked.size ? html`<div class="file-preview diff">${[...p.added.map((x) => ['add', '+', x]), ...p.replaced.map((x) => ['change', '~', x])].slice(0, 300).map(([k, s, x]) => html`
        <div class="diff-line ${k}"><span class="sign">${s}</span><span class="path">${x.path}</span><span class="detail">${fmtSize(x.file.size)}</span>
        <button type="button" class="btn btn-sm btn-ghost" aria-label="Remove ${x.path}" data-fn="${act(() => { nv.picked.delete(x.path); refresh(); }, 'nvp')}">✕</button></div>`)}</div>` : ''}
      ${invalid.length ? html`<p class="meta" role="alert">⚠ ${invalid.length} path(s) are not allowed (e.g. “${invalid[0]}”).</p>` : ''}`);
    $('#nv-create').disabled = nv.baseFiles === null || invalid.length > 0 || (nv.picked.size === 0 && !nv.replaceAll);
  };

  const create = ma(() => {
    const p = plan();
    const baseV = nv.base ? resolveRef(ds, nv.base) : null;
    const message = $('#nv-message').value.trim();
    const producerRaw = $('#nv-producer').value.trim();
    const inputs = $('#nv-inputs').value.split(/[\s,]+/).map((s) => s.trim()).filter(Boolean);
    const branch = $('#nv-branch').value.trim();
    let producer = null;
    if (producerRaw) { const i = producerRaw.indexOf(':'); producer = i > 0 ? { type: producerRaw.slice(0, i), id: producerRaw.slice(i + 1) } : { type: 'run', id: producerRaw }; }
    const metadata = { ...(baseV && !nv.replaceAll ? pickCarryMeta(baseV.metadata) : {}) };
    if (message) metadata.message = message; else delete metadata.message;
    closeModal();
    startVersionJob(name, { uploads: [...p.added, ...p.replaced], inherit: p.inherit, parent: baseV ? baseV.id : null, branch, metadata, producer, inputs });
    toast(html`↑ Uploading ${p.added.length + p.replaced.length} file(s) to ${name}`, 'info', { label: 'View', href: '#/transfers' });
  });

  openModal(html`<form data-submit="${create}">
    <div class="modal-head"><h2 class="section-title">New version of ${name}</h2><button type="button" class="btn btn-ghost btn-icon" aria-label="Close" data-fn="${ma(closeModal)}">✕</button></div>
    <div class="modal-body">
      <div class="row">
        <div class="field grow"><label for="nv-base">Derived from</label>
          ${ds.versions.length ? versionSelect(ds, nv.base, (val) => { nv.base = val; loadBase(); }, 'nv-base', 'modal') : html`<span class="meta">First version — nothing to inherit</span>`}</div>
        <div class="field grow"><label for="nv-prefix">Destination folder</label><input class="input" id="nv-prefix" placeholder="(root)" data-input="${ma((el) => { nv.prefix = el.value.trim(); })}" spellcheck="false"></div>
      </div>
      <div class="dropzone" id="nv-drop">
        Drop files or folders here
        <div class="row">
          <label class="btn btn-sm">Choose files<input type="file" multiple hidden data-change="${ma((el) => { addFiles(Array.from(el.files, (f) => ({ file: f, path: f.name }))); el.value = ''; })}"></label>
          <label class="btn btn-sm">Choose folder<input type="file" webkitdirectory multiple hidden data-change="${ma((el) => { addFiles(Array.from(el.files, (f) => ({ file: f, path: stripFirst(f.webkitRelativePath || f.name) }))); el.value = ''; })}"></label>
        </div>
      </div>
      ${ds.versions.length ? html`<label class="checkbox"><input type="checkbox" id="nv-replace" data-change="${ma((el) => { nv.replaceAll = el.checked; refresh(); })}"> Replace all files (don’t inherit files from the base version)</label>` : ''}
      <div id="nv-preview"></div>
      <div class="field"><label for="nv-message">Message</label><input class="input" id="nv-message" placeholder="Add normalized volatility feature"></div>
      <div class="row">
        <div class="field grow"><label for="nv-producer">Producer</label><input class="input" id="nv-producer" placeholder="pipeline:build-features/run-293" spellcheck="false"></div>
        <div class="field grow"><label for="nv-branch">Update branch</label><input class="input" id="nv-branch" list="nv-branches" value="${branches.some((b) => b.name === nv.base) ? nv.base : ''}" placeholder="(none)" spellcheck="false">
          <datalist id="nv-branches">${branches.map((b) => html`<option value="${b.name}">`)}</datalist></div>
      </div>
      <div class="field"><label for="nv-inputs">Inputs (lineage)</label><input class="input" id="nv-inputs" placeholder="raw-options@latest, features@v8" spellcheck="false"><span class="hint">Datasets as name@ref are pinned to exact versions. URIs like mlflow://run/… are kept as-is.</span></div>
    </div>
    <div class="modal-foot"><button type="button" class="btn" data-fn="${ma(closeModal)}">Cancel</button><button class="btn btn-primary" id="nv-create" type="submit" disabled>Create version</button></div>
  </form>`, { wide: true });

  const drop = $('#nv-drop');
  drop.addEventListener('dragover', (e) => { e.preventDefault(); drop.classList.add('over'); });
  drop.addEventListener('dragleave', () => drop.classList.remove('over'));
  drop.addEventListener('drop', async (e) => {
    e.preventDefault(); drop.classList.remove('over');
    const entries = Array.from(e.dataTransfer.items || []).map((i) => i.webkitGetAsEntry && i.webkitGetAsEntry()).filter(Boolean);
    if (!entries.length) { addFiles(Array.from(e.dataTransfer.files, (f) => ({ file: f, path: f.name }))); return; }
    const out = [];
    const single = entries.length === 1 && entries[0].isDirectory;
    for (const en of entries) await walkEntry(en, single ? '' : en.isDirectory ? `${en.name}/` : '', out, single);
    addFiles(out);
  });
  loadBase();
}

// Only carry stable descriptors; row/column counts could be stale for new files.
const CARRY_META = ['format'];
function pickCarryMeta(m) { const out = {}; for (const k of CARRY_META) if (m && m[k] !== undefined) out[k] = m[k]; return out; }
const stripFirst = (p) => p.split('/').slice(1).join('/') || p;
const validPath = (p) => p.length > 0 && p.length <= 1024 && !p.includes('\\') && p.split('/').every((s) => s && s !== '.' && s !== '..');

async function walkEntry(entry, prefix, out, isRoot) {
  if (entry.isFile) {
    const file = await new Promise((res, rej) => entry.file(res, rej));
    out.push({ file, path: `${prefix}${entry.name}` });
    return;
  }
  const reader = entry.createReader();
  const dirPrefix = isRoot ? prefix : prefix;
  for (;;) {
    const batch = await new Promise((res, rej) => reader.readEntries(res, rej));
    if (!batch.length) break;
    for (const child of batch) await walkEntry(child, child.isDirectory ? `${dirPrefix}${child.name}/` : dirPrefix, out, false);
  }
}

// ======================================================================
// Page: Settings
// ======================================================================

function pageSettings() {
  const theme = document.documentElement.dataset.theme;
  if (S.me && S.tokens === undefined) loadTokens();
  const created = S.createdToken;
  const createToken = act(async () => {
    const nameEl = $('#tok-name');
    const name = nameEl.value.trim();
    if (!name) { nameEl.focus(); return; }
    try {
      const r = await api('/auth/tokens', { method: 'POST', body: { name } });
      S.createdToken = r; S.tokens = undefined; renderPage();
    } catch (e) { toast(friendly(e, 'create the token').message, 'error'); }
  });
  return html`<div class="page-narrow">
    <div class="page-head"><h1 class="page-title">Settings</h1></div>

    <section class="section-gap"><div class="section-head"><h2 class="section-title">Appearance</h2></div>
      <div class="card row"><span class="grow">Theme<br><span class="meta">System follows your operating system setting.</span></span>
        ${segmented([['system', 'System'], ['light', 'Light'], ['dark', 'Dark']], theme, (m) => { applyTheme(m); renderPage(); renderStatusbar(); }, 'Theme')}</div>
    </section>

    <section class="section"><div class="section-head"><h2 class="section-title">Account</h2></div>
      <div class="card row">${S.me
        ? html`<span class="grow">Signed in as <strong>${S.me.username}</strong>${S.me.admin ? html` ${badge('Admin', 'accent plain')}` : ''}</span><button class="btn" data-fn="${act(signOut)}">Sign out</button>`
        : html`<span class="grow">You’re browsing ${S.publicRead ? 'read-only' : 'signed out'}.</span><a class="btn btn-primary" href="#/login?next=/settings">Sign in</a>`}</div>
    </section>

    ${S.me ? html`<section class="section"><div class="section-head"><h2 class="section-title">Access tokens</h2></div>
      <p class="meta">Tokens authenticate the <code>ds</code> CLI, scripts and training jobs. They’re shown once — store them in a secret manager.</p>
      ${created ? html`<div class="card section-gap"><div class="row"><strong class="grow">New token “${created.name}”</strong>${badge('Copy it now', 'warning')}</div>${codeBlock(`export DS_URL=${location.origin}\nexport DS_TOKEN=${created.token}`)}</div>` : ''}
      <form class="catalog-tools section-gap" data-submit="${createToken}"><div class="grow"><input class="input" id="tok-name" placeholder="Token name, e.g. training-cluster" aria-label="Token name"></div><button class="btn btn-primary" type="submit">Create token</button></form>
      ${tokenTable()}
    </section>` : ''}

    <section class="section"><div class="section-head"><h2 class="section-title">Connection</h2></div>
      <div class="card"><dl class="kv">
        <dt>Status</dt><dd>${S.connection === 'ok' ? html`<span class="dot ok" aria-hidden="true"></span>Connected` : html`<span class="dot bad" aria-hidden="true"></span>Unreachable`}</dd>
        <dt>Server</dt><dd class="mono">${location.origin}</dd>
        <dt>API</dt><dd class="mono">${location.origin}/api</dd>
        <dt>Version</dt><dd id="server-version" class="mono">…</dd>
      </dl></div>
      <p class="meta section-gap">The browser uses a secure session cookie. The CLI uses DS_URL and DS_TOKEN.</p>
    </section>
  </div>`;
}

async function loadTokens() {
  S.tokens = null;
  try { S.tokens = await api('/auth/tokens'); } catch (e) { S.tokens = { error: e }; }
  if (S.route.name === 'settings') renderPage();
}

function tokenTable() {
  if (S.tokens === null || S.tokens === undefined) return skeletonRows(2);
  if (S.tokens.error) return errorState(S.tokens.error, 'load tokens', () => { S.tokens = undefined; renderPage(); });
  if (!S.tokens.length) return html`<p class="meta">No tokens yet.</p>`;
  return html`<table class="table"><thead><tr><th>Name</th><th>Created</th><th class="actions"></th></tr></thead><tbody>
    ${S.tokens.map((t) => html`<tr><td>${t.name}</td><td class="meta">${ago(t.created_at)}</td><td class="actions">
      <button class="btn btn-sm btn-ghost btn-danger" data-fn="${act(() => confirmRevoke(t))}">Revoke</button></td></tr>`)}
  </tbody></table>`;
}

function confirmRevoke(t) {
  openModal(html`
    <div class="modal-head"><h2 class="section-title">Revoke “${t.name}”?</h2></div>
    <div class="modal-body"><p class="secondary">Anything using this token will stop working immediately. This can’t be undone.</p></div>
    <div class="modal-foot"><button class="btn" data-fn="${ma(closeModal)}">Cancel</button>
      <button class="btn btn-primary" data-fn="${ma(async () => {
        try { await api(`/auth/tokens/${enc(t.id)}`, { method: 'DELETE' }); closeModal(); toast(`Revoked ${t.name}`, 'success'); S.tokens = undefined; S.createdToken = null; renderPage(); }
        catch (e) { toast(friendly(e, 'revoke the token').message, 'error'); }
      })}">Revoke token</button></div>`);
}

// ======================================================================
// Inspector, menu, toasts, command palette
// ======================================================================

function openInspector({ title, subtitle, rows = [], actions = [], extra = '' }) {
  clearScope('inspector');
  const ia = (fn) => act(fn, 'inspector');
  const el = $('#inspector');
  el.innerHTML = esc(html`
    <div class="inspector-head"><div class="grow"><div class="inspector-title">${title}</div>${subtitle ? html`<div class="meta mono">${subtitle}</div>` : ''}</div>
      <button class="btn btn-ghost btn-icon" aria-label="Close details (Esc)" data-fn="${ia(closeInspector)}">✕</button></div>
    <div class="inspector-body">
      <dl class="kv">${rows.map(([k, v, mono]) => html`<dt>${k}</dt><dd class="${mono ? 'mono' : ''}">${v}</dd>`)}</dl>
      <div class="inspector-actions">${actions.map((a) => (a.href
        ? html`<a class="btn btn-sm ${a.primary ? 'btn-primary' : ''}" href="${a.href}" ${a.download ? html`download="${a.download}"` : ''}>${a.label}</a>`
        : html`<button class="btn btn-sm ${a.primary ? 'btn-primary' : ''}" data-fn="${ia(a.fn)}">${a.label}</button>`))}</div>
      ${extra}
    </div>`);
  el.classList.add('open');
  el.setAttribute('aria-hidden', 'false');
  const close = el.querySelector('.inspector-head button');
  if (close) close.focus({ preventScroll: true });
}
function closeInspector() {
  const el = $('#inspector');
  if (!el.classList.contains('open')) return false;
  el.classList.remove('open'); el.setAttribute('aria-hidden', 'true');
  clearScope('inspector');
  return true;
}

function openMenu(anchor, items) {
  closeMenu();
  clearScope('menu');
  const m = document.createElement('div');
  m.className = 'menu'; m.id = 'menu'; m.setAttribute('role', 'menu');
  m.innerHTML = esc(items.map((it) => html`<button role="menuitem" data-fn="${act(() => { closeMenu(); it.fn(); }, 'menu')}">${it.label}</button>`));
  document.body.appendChild(m);
  const r = anchor.getBoundingClientRect();
  const left = Math.min(window.innerWidth - m.offsetWidth - 8, Math.max(8, r.right - m.offsetWidth));
  m.style.left = `${left}px`;
  m.style.top = `${r.bottom + 4}px`;
  const first = m.querySelector('button'); if (first) first.focus();
}
function closeMenu() { const m = $('#menu'); if (m) { m.remove(); clearScope('menu'); return true; } return false; }

function toast(message, kind = 'info', action = null) {
  const el = document.createElement('div');
  el.className = `toast ${kind}`;
  el.setAttribute('role', kind === 'error' ? 'alert' : 'status');
  const icon = { success: '✓', error: '✕', info: '•' }[kind] || '•';
  const text = message instanceof Raw ? message.s.replace(/^[✓✕↑]\s*/, '') : esc(message);
  let actionHtml = '';
  if (action) {
    actionHtml = action.href
      ? esc(html`<a class="btn btn-sm btn-ghost" href="${action.href}">${action.label}</a>`)
      : esc(html`<button class="btn btn-sm btn-ghost" data-fn="${act(() => { el.remove(); action.fn(); }, 'toast')}">${action.label}</button>`);
  }
  el.innerHTML = `<span class="icon" aria-hidden="true">${icon}</span><span class="grow">${text}</span>${actionHtml}`;
  $('#toasts').appendChild(el);
  setTimeout(() => el.remove(), kind === 'error' ? 8000 : 4500);
}

// ---------- Command palette ----------

function paletteCommands() {
  const cmds = [
    { group: 'Navigate', title: 'Go to Datasets', run: () => go('#/datasets') },
    { group: 'Navigate', title: 'Go to Lineage', run: () => go('#/lineage') },
    { group: 'Navigate', title: 'Go to Transfers', run: () => go('#/transfers') },
    { group: 'Navigate', title: 'Open Settings', run: () => go('#/settings') },
  ];
  if (S.me) cmds.push({ group: 'Actions', title: 'Create dataset', run: openCreateDataset });
  const cur = S.route.name === 'dataset' ? S.route.parts[1] : null;
  const ds = cur && S.ds[cur] && S.ds[cur].info ? S.ds[cur] : null;
  if (ds) {
    const v = resolveRef(ds, defaultRef(ds));
    if (S.me) cmds.push({ group: 'Actions', title: `New version of ${cur}`, run: () => openNewVersion(cur, ds) });
    cmds.push({ group: 'Actions', title: 'Compare versions', run: () => go(`#/d/${enc(cur)}/compare`) });
    if (v) {
      cmds.push({ group: 'Actions', title: 'Pull version', run: () => openPull(cur, ds) });
      cmds.push({ group: 'Actions', title: 'Copy dataset URI', hint: `dataset://${cur}/${shortId(v.id)}`, run: () => copyText(`dataset://${cur}/${v.id}`) });
      cmds.push({ group: 'Actions', title: 'Copy IDs for MLflow / Aim', run: () => copyText(mlflowIds(cur, v), 'Copied dataset identifiers') });
    }
    cmds.push({ group: 'Actions', title: 'Open lineage', run: () => go(`#/d/${enc(cur)}/lineage`) });
    const idx = versionIndex(ds);
    for (const ver of ds.versions.slice(0, 200)) cmds.push({ group: `Versions of ${cur}`, title: `v${idx.get(ver.id)} · ${versionTitle(ver)}`, hint: shortId(ver.id), key: `${ver.id} ${ver.created_by}`, run: () => go(`#/d/${enc(cur)}/files?v=${enc(ver.id)}`) });
    for (const r of ds.info.refs || []) cmds.push({ group: `Versions of ${cur}`, title: `${r.kind === 'branch' ? '⎇' : '⌗'} ${r.name}`, hint: `v${idx.get(r.version_id)}`, run: () => go(`#/d/${enc(cur)}/files?v=${enc(r.name)}`) });
  }
  for (const d of S.datasets || []) cmds.push({ group: 'Datasets', title: d.name, hint: d.latest ? fmtSize(d.latest.total_size) : 'empty', key: `${d.description} ${d.owner}`, run: () => go(`#/d/${enc(d.name)}`) });
  cmds.push({ group: 'Appearance', title: 'Toggle dark / light theme', run: () => { applyTheme(isDark() ? 'light' : 'dark'); renderStatusbar(); if (S.route.name === 'settings') renderPage(); } });
  return cmds;
}

function openPalette() {
  closeMenu(); closeModal();
  clearScope('modal');
  const ov = $('#overlay');
  ov.innerHTML = esc(html`<div class="palette" role="dialog" aria-modal="true" aria-label="Command palette">
    <input id="palette-input" placeholder="Search datasets, versions, tags, actions…" aria-label="Command" autocomplete="off" spellcheck="false">
    <div class="palette-list" id="palette-list" role="listbox"></div></div>`);
  ov.hidden = false; ov.dataset.kind = 'palette';
  S.lastFocus = document.activeElement;
  const all = paletteCommands();
  S.palette = { all, items: [], active: 0 };
  const input = $('#palette-input');
  const renderList = () => {
    const q = input.value.trim();
    const scored = q ? all.map((c) => [c, Math.max(fuzzy(q, c.title), c.key ? fuzzy(q, c.key) - 50 : -1)]).filter(([, s]) => s >= 0).sort((a, b) => b[1] - a[1]).map(([c]) => c) : all;
    S.palette.items = scored.slice(0, 60);
    S.palette.active = Math.min(S.palette.active, Math.max(0, S.palette.items.length - 1));
    clearScope('palette');
    let group = null;
    $('#palette-list').innerHTML = S.palette.items.length ? esc(S.palette.items.map((c, i) => {
      const head = !q && c.group !== group ? html`<div class="palette-group">${(group = c.group)}</div>` : '';
      return html`${head}<div class="palette-item ${i === S.palette.active ? 'active' : ''}" role="option" aria-selected="${i === S.palette.active}" data-fn="${act(() => runPalette(i), 'palette')}"><span>${c.title}</span>${c.hint ? html`<span class="hint mono">${c.hint}</span>` : ''}</div>`;
    })) : esc(html`<div class="palette-empty">No matches for “${q}”</div>`);
    const active = $('.palette-item.active');
    if (active) active.scrollIntoView({ block: 'nearest' });
  };
  input.addEventListener('input', () => { S.palette.active = 0; renderList(); });
  input.addEventListener('keydown', (e) => {
    if (e.key === 'ArrowDown') { e.preventDefault(); S.palette.active = Math.min(S.palette.items.length - 1, S.palette.active + 1); renderList(); }
    if (e.key === 'ArrowUp') { e.preventDefault(); S.palette.active = Math.max(0, S.palette.active - 1); renderList(); }
    if (e.key === 'Enter') { e.preventDefault(); runPalette(S.palette.active); }
  });
  renderList();
  input.focus();
}
function runPalette(i) {
  const c = S.palette && S.palette.items[i];
  closeOverlay();
  if (c) c.run();
}
function closeOverlay() {
  const ov = $('#overlay');
  if (ov.hidden) return false;
  if (ov.dataset.kind === 'modal') return closeModal();
  ov.hidden = true; ov.innerHTML = ''; clearScope('palette');
  if (S.lastFocus && S.lastFocus.focus) S.lastFocus.focus();
  return true;
}

// ======================================================================
// Global events
// ======================================================================

document.addEventListener('click', (e) => {
  const menu = $('#menu');
  if (menu && !menu.contains(e.target)) closeMenu();
  if (e.target === $('#overlay')) { closeOverlay(); return; }
  const stop = e.target.closest('[data-stop]');
  const fnEl = e.target.closest('[data-fn]');
  if (fnEl && !(stop && fnEl.contains(stop) && stop !== fnEl)) {
    const h = handlers.get(fnEl.dataset.fn);
    if (h) {
      if (fnEl.tagName !== 'A' || !fnEl.getAttribute('href')) e.preventDefault();
      h(fnEl, e);
      return;
    }
  }
  const hrefEl = e.target.closest('[data-href]');
  if (hrefEl && !e.target.closest('a[href], button:not([data-href])')) { go(hrefEl.dataset.href); }
});
document.addEventListener('input', (e) => { const h = e.target.dataset && handlers.get(e.target.dataset.input); if (h) h(e.target, e); });
document.addEventListener('change', (e) => { const h = e.target.dataset && handlers.get(e.target.dataset.change); if (h) h(e.target, e); });
document.addEventListener('submit', (e) => {
  const h = e.target.dataset && handlers.get(e.target.dataset.submit);
  if (h) { e.preventDefault(); h(e.target, e); }
});
document.addEventListener('keydown', (e) => {
  const typing = /^(INPUT|TEXTAREA|SELECT)$/.test(e.target.tagName);
  if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') { e.preventDefault(); openPalette(); return; }
  if (e.key === '/' && !typing && $('#overlay').hidden) { e.preventDefault(); openPalette(); return; }
  if (e.key === 'Escape') { if (closeMenu() || closeOverlay() || closeInspector()) e.preventDefault(); return; }
  // Keyboard navigation for rows and graph nodes.
  const row = e.target.closest && e.target.closest('[data-row], .g-node');
  if (row && !typing) {
    if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); row.dispatchEvent(new MouseEvent('click', { bubbles: true })); }
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      const all = $$('[data-row]', $('#page'));
      const i = all.indexOf(row);
      const next = all[i + (e.key === 'ArrowDown' ? 1 : -1)];
      if (next) { e.preventDefault(); next.focus(); }
    }
  }
});
window.addEventListener('hashchange', onRoute);
matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => renderStatusbar());

// ======================================================================
// Boot
// ======================================================================

async function boot() {
  applyTheme(store.get('theme', 'system'));
  $('#kbd-hint').textContent = IS_MAC ? '⌘K' : 'Ctrl K';
  S.route = parseRoute();
  renderNav(); renderStatusbar(); renderPage();
  try {
    S.me = await api('/auth/whoami');
    S.publicRead = !!S.me.public_read;
  } catch (e) {
    S.me = null;
    if (e.status === 401 || e.status === 403) {
      try { S.datasets = await api('/datasets'); S.publicRead = true; } catch { S.publicRead = false; }
    } else if (e.status === 0) {
      S.booted = true; renderStatusbar();
      $('#page').innerHTML = esc(errorState(e, 'connect to the server', () => location.reload()));
      return;
    }
  }
  S.booted = true;
  renderAccount(); renderStatusbar(); renderRecent();
  onRoute();
  if (S.me || S.publicRead) loadDatasets();
  // Keep the server version fresh on the Settings page.
  api('/').then((i) => { S.serverVersion = i.version; const el = $('#server-version'); if (el) el.textContent = i.version; }).catch(() => {});
}

boot();
