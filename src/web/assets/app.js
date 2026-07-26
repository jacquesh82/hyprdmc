'use strict';

// All the logic lives in this single file: no dependency, no build step.
// State comes from the daemon (SSE), the draft lives here until the user
// clicks "Apply".

const SNAP = 60;            // snapping distance, in logical pixels
const ROTATIONS = [0, 90, 180, 270];

const el = (id) => document.getElementById(id);
const canvas = el('canvas');
const panel = el('panel');

let live = null;            // last state received from the daemon
let draft = [];             // layout currently being edited
let selected = null;        // name of the selected connector
let dirty = false;          // the draft diverges from the live state
let guardTimer = null;

// -------------------------------------------------------------------- i18n --

// English fallback used if `/api/i18n` cannot be reached, so the UI never
// depends on the network for its own language. Kept in sync by hand with the
// `web.*` section of `locales/app.yml`.
const I18N_DEFAULTS = {
  'web.title': 'hyprdmc — displays',
  'web.profile_badge': 'profile: %{name}',
  'web.connection': 'connection to the daemon',
  'web.canvas_label': 'Display arrangement',
  'web.hint': "Drag an output to move it — it snaps to neighbouring edges. Arrow keys for fine adjustment.",
  'web.select_prompt': 'Select an output to configure it.',
  'web.guard.applied': 'Configuration applied. Automatic revert in %{seconds}s.',
  'web.guard.keep': 'Keep',
  'web.guard.revert': 'Revert',
  'web.action.apply': 'Apply',
  'web.action.reset': 'Discard changes',
  'web.action.auto': 'Arrange automatically',
  'web.action.save': 'Save as profile…',
  'web.action.persist': 'Make permanent',
  'web.field.enabled': 'Output enabled',
  'web.field.mode': 'Mode',
  'web.field.scale': 'Scale',
  'web.field.rotation': 'Rotation',
  'web.field.flip': 'Flip the image',
  'web.field.mirror': 'Mirror',
  'web.field.vrr': 'Variable refresh rate (VRR)',
  'web.mirror.none': 'none',
  'web.screen.disabled': 'disabled',
  'web.screen.flipped': 'flipped',
  'web.prompt.profile_name': 'Profile name?',
  'web.toast.applied': 'Configuration applied.',
  'web.toast.rolled_back': 'Hyprland did not apply the configuration: previous state restored.',
  'web.toast.kept': 'Configuration kept.',
  'web.toast.reverted': 'Previous configuration restored.',
  'web.toast.profile_saved': 'Profile "%{name}" saved.',
  'web.toast.persisted': 'Layout written to %{path}.',
  'web.issue.overlap': '"%{a}" and "%{b}" overlap',
  'web.issue.all_disabled': 'every output would be disabled',
  'web.issue.mirror_unavailable': '"%{name}" mirrors an unavailable output',
  'web.not_found': 'not found',
  'web.no_outputs': 'No display detected.',
  'web.disconnected': 'Daemon unreachable — is hyprdmc still running?',
  // Theme toggle.
  'web.theme.toggle_label': 'Theme: %{mode}',
  'web.theme.auto': 'Auto',
  'web.theme.light': 'Light',
  'web.theme.dark': 'Dark',
  // History panel.
  'web.history.title': 'History',
  'web.history.empty': 'No layout applied yet.',
  'web.history.remembered': '%{count} layout(s) remembered',
  'web.history.origin_manual': 'manual',
  'web.history.restore': 'Restore',
  'web.history.restore_aria': 'Restore the configuration from %{when}',
  'web.history.restored': 'Configuration #%{index} restored (%{when}).',
};

let i18nStrings = {};

/**
 * Translates `key`, substituting `%{name}` placeholders from `vars`.
 *
 * Resolution order: the strings fetched from `/api/i18n`, then the built-in
 * English default, then the key's last segment — so a missing key or a
 * failed fetch degrades gracefully instead of leaving the UI blank.
 */
function t(key, vars) {
  const template = i18nStrings[key] ?? I18N_DEFAULTS[key] ?? key.split('.').pop();
  if (!vars) return template;
  return template.replace(/%\{(\w+)\}/g, (match, name) => (name in vars ? String(vars[name]) : match));
}

/** Fetches the active locale's strings. Never throws: falls back to English. */
async function loadI18n() {
  try {
    const data = await api('/api/i18n');
    i18nStrings = data.strings ?? {};
    if (data.locale) document.documentElement.lang = data.locale;
  } catch (err) {
    console.error('translations unavailable, falling back to English defaults', err);
    i18nStrings = {};
  }
}

/**
 * Applies translations to the static markup.
 *
 * Convention: `data-i18n="web.some.key"` sets the element's text content;
 * `data-i18n-attr="attr1:web.key1,attr2:web.key2"` sets one or more
 * attributes instead (comma-separated `attribute:key` pairs).
 */
function applyStaticI18n() {
  for (const node of document.querySelectorAll('[data-i18n]')) {
    node.textContent = t(node.dataset.i18n);
  }
  for (const node of document.querySelectorAll('[data-i18n-attr]')) {
    for (const pair of node.dataset.i18nAttr.split(',')) {
      const [attr, key] = pair.split(':');
      node.setAttribute(attr, t(key));
    }
  }
}

// ------------------------------------------------------------------ theme --

const THEME_KEY = 'hyprdmc.theme';
const THEME_ORDER = ['auto', 'light', 'dark'];

/** Reads the persisted choice, defaulting to "auto" if unset or unreadable. */
function storedTheme() {
  try {
    const value = localStorage.getItem(THEME_KEY);
    return THEME_ORDER.includes(value) ? value : 'auto';
  } catch (err) {
    return 'auto';
  }
}

/**
 * Applies `mode` to the document: an explicit `data-theme` for "light"/"dark",
 * or no attribute at all for "auto" so `prefers-color-scheme` takes over.
 * Mirrors the inline bootstrap script in index.html, which does the same
 * thing before this file has even loaded, to avoid a flash of the wrong theme.
 */
function applyTheme(mode) {
  if (mode === 'light' || mode === 'dark') {
    document.documentElement.setAttribute('data-theme', mode);
  } else {
    document.documentElement.removeAttribute('data-theme');
  }
}

function updateThemeButton(mode) {
  const button = el('btn-theme');
  const label = t(`web.theme.${mode}`);
  button.textContent = label;
  button.setAttribute('aria-label', t('web.theme.toggle_label', { mode: label }));
}

function setTheme(mode) {
  try {
    localStorage.setItem(THEME_KEY, mode);
  } catch (err) { /* localStorage unavailable: the choice just won't persist */ }
  applyTheme(mode);
  updateThemeButton(mode);
}

el('btn-theme').addEventListener('click', () => {
  const next = THEME_ORDER[(THEME_ORDER.indexOf(storedTheme()) + 1) % THEME_ORDER.length];
  setTheme(next);
});

// ------------------------------------------------------------------ model --

const clone = (v) => JSON.parse(JSON.stringify(v));
const byName = (name) => draft.find((o) => o.name === name);

/** Footprint in the workspace: rotation swaps the axes. */
function logicalSize(o) {
  if (!o.mode) return [0, 0];
  const swap = o.transform.rotation === 'R90' || o.transform.rotation === 'R270';
  const w = swap ? o.mode.height : o.mode.width;
  const h = swap ? o.mode.width : o.mode.height;
  const scale = o.scale > 0 ? o.scale : 1;
  return [Math.round(w / scale), Math.round(h / scale)];
}

const occupies = (o) => o.enabled && !o.mirror_of;

function overlaps(a, b) {
  const [aw, ah] = logicalSize(a);
  const [bw, bh] = logicalSize(b);
  return a.x < b.x + bw && b.x < a.x + aw && a.y < b.y + bh && b.y < a.y + ah;
}

function conflicting(name) {
  const o = byName(name);
  if (!o || !occupies(o)) return false;
  return draft.some((other) => other.name !== name && occupies(other) && overlaps(o, other));
}

// --------------------------------------------------------------- 2D render --

function bounds() {
  const boxes = draft.filter(occupies).map((o) => {
    const [w, h] = logicalSize(o);
    return [o.x, o.y, o.x + w, o.y + h];
  });
  if (!boxes.length) return { x: 0, y: 0, w: 1920, h: 1080 };
  return {
    x: Math.min(...boxes.map((b) => b[0])),
    y: Math.min(...boxes.map((b) => b[1])),
    w: Math.max(...boxes.map((b) => b[2])) - Math.min(...boxes.map((b) => b[0])),
    h: Math.max(...boxes.map((b) => b[3])) - Math.min(...boxes.map((b) => b[1])),
  };
}

/** Scale factor and offset to fit the layout inside the frame. */
/**
 * Scale factor and offset to fit the layout inside the frame.
 *
 * Returns `null` while the canvas has no usable size. That happens for real:
 * a tab opened in the background by `--open` can run this before its first
 * layout, and `clientWidth` then reads 0. Without the guard the scale goes
 * *negative*, every output is positioned outside the frame, and `overflow:
 * hidden` makes the canvas look empty with nothing to hint why. The
 * ResizeObserver below redraws as soon as a real size exists.
 */
function viewport() {
  const b = bounds();
  const pad = 24;
  const usableW = canvas.clientWidth - pad * 2;
  const usableH = canvas.clientHeight - pad * 2;
  if (!(usableW > 0) || !(usableH > 0)) return null;

  const scale = Math.min(usableW / Math.max(b.w, 1), usableH / Math.max(b.h, 1), 0.5);
  if (!(scale > 0) || !Number.isFinite(scale)) return null;

  return {
    scale,
    ox: (canvas.clientWidth - b.w * scale) / 2 - b.x * scale,
    oy: (canvas.clientHeight - b.h * scale) / 2 - b.y * scale,
  };
}

function render() {
  const view = viewport();
  // No usable size yet: leave whatever is on screen alone rather than
  // replacing it with misplaced boxes. observeCanvas() calls us back.
  if (!view) return;
  canvas.innerHTML = '';

  // An empty canvas is indistinguishable from a broken one: say which it is.
  if (!draft.length) {
    const note = document.createElement('p');
    note.className = 'canvas-note';
    note.textContent = live ? t('web.no_outputs') : t('web.disconnected');
    canvas.append(note);
    renderPanel();
    renderIssues();
    return;
  }

  // Disabled outputs are drawn last, underneath the others.
  for (const o of [...draft].sort((a, b) => Number(a.enabled) - Number(b.enabled))) {
    const [lw, lh] = logicalSize(o);
    const node = document.createElement('div');
    node.className = 'screen';
    node.dataset.name = o.name;
    if (o.name === selected) node.classList.add('selected');
    if (!o.enabled) node.classList.add('disabled');
    if (o.mirror_of) node.classList.add('mirrored');
    if (conflicting(o.name)) node.classList.add('conflict');

    const w = Math.max(lw * view.scale, 54);
    const h = Math.max(lh * view.scale, 34);
    node.style.left = `${o.x * view.scale + view.ox}px`;
    node.style.top = `${o.y * view.scale + view.oy}px`;
    node.style.width = `${w}px`;
    node.style.height = `${h}px`;

    const name = document.createElement('div');
    name.className = 'name';
    name.textContent = o.name;
    node.append(name);

    const detail = document.createElement('div');
    detail.className = 'detail';
    detail.textContent = o.enabled
      ? `${lw}×${lh}${o.transform.rotation !== 'R0' ? ` · ${degrees(o)}°` : ''}` +
        `${o.transform.flipped ? ` · ${t('web.screen.flipped')}` : ''}${o.mirror_of ? ` · ⧉ ${o.mirror_of}` : ''}`
      : t('web.screen.disabled');
    node.append(detail);

    node.addEventListener('pointerdown', onPointerDown);
    canvas.append(node);
  }

  renderPanel();
  renderIssues();
  el('btn-apply').disabled = !dirty;
  el('btn-reset').disabled = !dirty;
}

const degrees = (o) => Number(o.transform.rotation.slice(1));

// ------------------------------------------------------------- drag-and-drop --

function onPointerDown(event) {
  const node = event.currentTarget;
  const o = byName(node.dataset.name);
  if (!o) return;

  selected = o.name;
  render();

  const view = viewport();
  if (!view) return;
  const startX = event.clientX;
  const startY = event.clientY;
  const originX = o.x;
  const originY = o.y;
  const live = canvas.querySelector(`.screen[data-name="${CSS.escape(o.name)}"]`);
  live?.classList.add('dragging');
  live?.setPointerCapture(event.pointerId);

  const move = (ev) => {
    o.x = Math.round(originX + (ev.clientX - startX) / view.scale);
    o.y = Math.round(originY + (ev.clientY - startY) / view.scale);
    snap(o);
    dirty = true;
    render();
  };

  const up = () => {
    document.removeEventListener('pointermove', move);
    document.removeEventListener('pointerup', up);
    normalize();
    render();
  };

  document.addEventListener('pointermove', move);
  document.addEventListener('pointerup', up);
  event.preventDefault();
}

/** Snaps the output against its neighbours when it gets close to them. */
function snap(o) {
  if (!occupies(o)) return;
  const [w, h] = logicalSize(o);

  for (const other of draft) {
    if (other.name === o.name || !occupies(other)) continue;
    const [ow, oh] = logicalSize(other);

    // Vertical edges: right against left, left against right, alignment.
    for (const [candidate, target] of [
      [o.x, other.x + ow], [o.x, other.x],
      [o.x + w, other.x], [o.x + w, other.x + ow],
    ]) {
      if (Math.abs(candidate - target) < SNAP) o.x += target - candidate;
    }
    for (const [candidate, target] of [
      [o.y, other.y + oh], [o.y, other.y],
      [o.y + h, other.y], [o.y + h, other.y + oh],
    ]) {
      if (Math.abs(candidate - target) < SNAP) o.y += target - candidate;
    }
  }
}

/** Brings the top-left corner of the whole set back to the origin. */
function normalize() {
  const active = draft.filter(occupies);
  if (!active.length) return;
  const dx = Math.min(...active.map((o) => o.x));
  const dy = Math.min(...active.map((o) => o.y));
  if (!dx && !dy) return;
  for (const o of active) { o.x -= dx; o.y -= dy; }
}

canvas.addEventListener('keydown', (event) => {
  const o = byName(selected);
  if (!o) return;
  const step = event.shiftKey ? 100 : 10;
  const moves = { ArrowLeft: [-step, 0], ArrowRight: [step, 0], ArrowUp: [0, -step], ArrowDown: [0, step] };
  const delta = moves[event.key];
  if (!delta) return;
  o.x += delta[0];
  o.y += delta[1];
  dirty = true;
  event.preventDefault();
  render();
});

// ----------------------------------------------------------------- panel --

function renderPanel() {
  const o = byName(selected);
  if (!o) {
    panel.innerHTML = '';
    panel.append(node('p', t('web.select_prompt'), 'empty'));
    return;
  }
  const monitor = live?.monitors?.find((m) => m.name === o.name);
  const modes = monitor?.availableModes ?? [];

  panel.innerHTML = '';
  panel.append(
    node('h2', o.name),
    node('p', monitor ? `${monitor.make} ${monitor.model} ${monitor.serial}`.trim() : '', 'sub'),
  );

  panel.append(field('', checkbox(t('web.field.enabled'), o.enabled, (v) => { o.enabled = v; touch(); })));

  const modeSelect = document.createElement('select');
  for (const m of modes) {
    const option = document.createElement('option');
    option.value = m;
    option.textContent = m;
    modeSelect.append(option);
  }
  if (o.mode) {
    const current = `${o.mode.width}x${o.mode.height}`;
    const match = modes.find((m) => m.startsWith(current));
    if (match) modeSelect.value = match;
  }
  modeSelect.addEventListener('change', () => { o.mode = parseMode(modeSelect.value); touch(); });
  panel.append(field(t('web.field.mode'), modeSelect));

  const scale = document.createElement('input');
  scale.type = 'number';
  scale.step = '0.05';
  scale.min = '0.1';
  scale.value = o.scale;
  scale.addEventListener('change', () => {
    o.scale = Math.max(0.1, Number(scale.value) || 1);
    touch();
  });
  panel.append(field(t('web.field.scale'), scale));

  const rotation = document.createElement('div');
  rotation.className = 'segmented';
  for (const deg of ROTATIONS) {
    const button = document.createElement('button');
    button.textContent = `${deg}°`;
    button.setAttribute('aria-pressed', String(degrees(o) === deg));
    button.addEventListener('click', () => { o.transform.rotation = `R${deg}`; touch(); });
    rotation.append(button);
  }
  panel.append(field(t('web.field.rotation'), rotation));

  panel.append(field('', checkbox(t('web.field.flip'), o.transform.flipped, (v) => {
    o.transform.flipped = v;
    touch();
  })));

  const mirror = document.createElement('select');
  const none = document.createElement('option');
  none.value = '';
  none.textContent = t('web.mirror.none');
  mirror.append(none);
  for (const other of draft.filter((x) => x.name !== o.name && x.enabled)) {
    const option = document.createElement('option');
    option.value = other.name;
    option.textContent = other.name;
    mirror.append(option);
  }
  mirror.value = o.mirror_of ?? '';
  mirror.addEventListener('change', () => { o.mirror_of = mirror.value || null; touch(); });
  panel.append(field(t('web.field.mirror'), mirror));

  panel.append(field('', checkbox(t('web.field.vrr'), o.vrr, (v) => { o.vrr = v; touch(); })));
}

function node(tag, text, className) {
  const n = document.createElement(tag);
  n.textContent = text;
  if (className) n.className = className;
  return n;
}

function field(label, control) {
  const wrap = document.createElement('div');
  wrap.className = 'field';
  if (label) wrap.append(node('label', label));
  wrap.append(control);
  return wrap;
}

function checkbox(label, checked, onChange) {
  const wrap = document.createElement('label');
  wrap.className = 'check';
  const input = document.createElement('input');
  input.type = 'checkbox';
  input.checked = checked;
  input.addEventListener('change', () => onChange(input.checked));
  wrap.append(input, document.createTextNode(label));
  return wrap;
}

function parseMode(text) {
  const m = /^(\d+)x(\d+)(?:@([\d.]+))?/.exec(text);
  if (!m) return null;
  return { width: Number(m[1]), height: Number(m[2]), refresh: Number(m[3] ?? 0) };
}

function touch() {
  dirty = true;
  render();
}

// ------------------------------------------------------------- validation --

/** Mirrors the blocking rules client-side, to avoid sending the pointless. */
function localIssues() {
  const issues = [];
  const active = draft.filter(occupies);

  for (let i = 0; i < active.length; i += 1) {
    for (let j = i + 1; j < active.length; j += 1) {
      if (overlaps(active[i], active[j])) {
        issues.push({ severity: 'error', message: t('web.issue.overlap', { a: active[i].name, b: active[j].name }) });
      }
    }
  }
  if (draft.length && !active.length) {
    issues.push({ severity: 'error', message: t('web.issue.all_disabled') });
  }
  for (const o of draft.filter((x) => x.enabled && x.mirror_of)) {
    const target = byName(o.mirror_of);
    if (!target || !target.enabled) {
      issues.push({ severity: 'error', message: t('web.issue.mirror_unavailable', { name: o.name }) });
    }
  }
  return issues;
}

function renderIssues() {
  const box = el('issues');
  const issues = dirty ? localIssues() : (live?.issues ?? []);
  if (!issues.length) {
    box.hidden = true;
    return;
  }
  box.hidden = false;
  box.innerHTML = '';
  const list = document.createElement('ul');
  for (const issue of issues) {
    const item = document.createElement('li');
    item.className = issue.severity;
    item.textContent = issue.message;
    list.append(item);
  }
  box.append(list);
  el('btn-apply').disabled = issues.some((i) => i.severity === 'error');
}

// --------------------------------------------------------------- network --

async function api(path, options = {}) {
  const response = await fetch(path, {
    headers: { 'content-type': 'application/json' },
    ...options,
  });
  const body = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(body.error ?? `HTTP ${response.status}`);
  return body;
}

function toast(message, isError = false) {
  const box = el('toast');
  box.textContent = message;
  box.className = `toast${isError ? ' error' : ''}`;
  box.hidden = false;
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => { box.hidden = true; }, 6000);
}

function adopt(state) {
  live = state;
  if (!dirty) {
    draft = clone(state.layout.outputs);
    if (!byName(selected)) selected = draft[0]?.name ?? null;
  }
  el('profile-badge').hidden = !state.activeProfile;
  el('profile-badge').textContent = state.activeProfile ? t('web.profile_badge', { name: state.activeProfile }) : '';
  showGuard(state.revertPending, state.confirmTimeoutSecs);
  render();
  // `state.history` (pushed over SSE) only carries the raw snapshot fields;
  // the formatted labels this panel needs come from GET /api/history, so we
  // re-fetch it here rather than duplicating the server's age/summary logic.
  refreshHistory();
}

function showGuard(pending, seconds) {
  const guard = el('guard');
  clearInterval(guardTimer);
  if (!pending) {
    guard.hidden = true;
    return;
  }
  guard.hidden = false;
  let left = seconds;
  const tick = () => {
    el('guard-text').textContent = t('web.guard.applied', { seconds: Math.max(left, 0) });
    if (left <= 0) clearInterval(guardTimer);
    left -= 1;
  };
  tick();
  guardTimer = setInterval(tick, 1000);
}

function connect() {
  const source = new EventSource('/api/events');
  source.addEventListener('open', () => el('connection').classList.add('live'));
  source.addEventListener('error', () => el('connection').classList.remove('live'));
  source.addEventListener('message', (event) => {
    try {
      adopt(JSON.parse(event.data));
    } catch (err) {
      console.error('unreadable state', err);
    }
  });
}

// ------------------------------------------------------------------ actions --

el('btn-apply').addEventListener('click', async () => {
  try {
    const report = await api('/api/apply', {
      method: 'POST',
      body: JSON.stringify({ outputs: draft, guard: true }),
    });
    dirty = false;
    if (report.rolled_back) {
      toast(t('web.toast.rolled_back'), true);
    } else {
      const warnings = (report.drifts ?? []).map((d) => d.message);
      toast(warnings.length ? warnings.join(' · ') : t('web.toast.applied'));
    }
    await refresh();
  } catch (err) {
    toast(err.message, true);
  }
});

el('btn-reset').addEventListener('click', () => {
  dirty = false;
  if (live) adopt(live);
});

el('btn-auto').addEventListener('click', () => {
  let cursor = 0;
  for (const o of draft.filter(occupies)) {
    o.x = cursor;
    o.y = 0;
    cursor += logicalSize(o)[0];
  }
  touch();
});

el('btn-confirm').addEventListener('click', async () => {
  await api('/api/confirm', { method: 'POST' });
  toast(t('web.toast.kept'));
  await refresh();
});

el('btn-revert').addEventListener('click', async () => {
  await api('/api/revert', { method: 'POST' });
  dirty = false;
  toast(t('web.toast.reverted'));
  await refresh();
});

el('btn-save').addEventListener('click', async () => {
  const name = prompt(t('web.prompt.profile_name'), live?.activeProfile ?? '');
  if (!name) return;
  try {
    await api(`/api/profiles/${encodeURIComponent(name)}`, {
      method: 'PUT',
      body: JSON.stringify({ outputs: draft }),
    });
    toast(t('web.toast.profile_saved', { name }));
    await refresh();
  } catch (err) {
    toast(err.message, true);
  }
});

el('btn-persist').addEventListener('click', async () => {
  try {
    const res = await api('/api/persist', { method: 'POST' });
    toast(t('web.toast.persisted', { path: res.path }));
  } catch (err) {
    toast(err.message, true);
  }
});

async function refresh() {
  try {
    adopt(await api('/api/state'));
  } catch (err) {
    toast(err.message, true);
  }
}

// ---------------------------------------------------------------- history --

/**
 * Pulls the formatted history listing (age label and summary are computed
 * server-side, in the user's language — see GET /api/history). Called
 * whenever a fresh state arrives over SSE, rather than on a timer: the
 * daemon already tells us when something changed, so a separate poll loop
 * would be redundant.
 */
async function refreshHistory() {
  try {
    renderHistory(await api('/api/history'));
  } catch (err) {
    console.error('history unavailable', err);
  }
}

function renderHistory(data) {
  const list = el('history-list');
  const empty = el('history-empty');
  const entries = (data.entries ?? []).slice(0, 5);

  el('history-remembered').textContent = t('web.history.remembered', { count: data.remembered ?? 0 });

  list.innerHTML = '';
  empty.hidden = entries.length > 0;
  list.hidden = entries.length === 0;

  for (const entry of entries) {
    const item = document.createElement('li');
    item.className = 'history-entry';

    const meta = document.createElement('div');
    meta.className = 'history-meta';
    meta.append(
      node('span', entry.profile ? t('web.profile_badge', { name: entry.profile }) : t('web.history.origin_manual'), 'history-origin'),
      node('span', entry.when, 'history-when'),
    );

    const restore = document.createElement('button');
    restore.textContent = t('web.history.restore');
    restore.setAttribute('aria-label', t('web.history.restore_aria', { when: entry.when }));
    restore.addEventListener('click', () => restoreHistoryEntry(entry.index));

    item.append(meta, node('div', entry.summary, 'history-summary'), restore);
    list.append(item);
  }
}

/**
 * Restoring is exactly like applying a layout: the daemon re-arms the
 * revert guard, so we just refresh the state and let the existing guard
 * banner (see showGuard) do its usual thing — nothing to reimplement here.
 */
async function restoreHistoryEntry(index) {
  try {
    const res = await api(`/api/history/${index}/restore`, { method: 'POST' });
    dirty = false;
    toast(t('web.history.restored', { index: res.restored, when: res.when }));
    await refresh();
  } catch (err) {
    toast(err.message, true);
  }
}

/**
 * Redraws whenever the canvas changes size — including the very first time it
 * gets one. A `resize` listener alone is not enough: a window that opens at
 * its final size never fires one, so a first render that happened before
 * layout would stay wrong forever.
 */
function observeCanvas() {
  if (typeof ResizeObserver === 'function') {
    new ResizeObserver(() => render()).observe(canvas);
    return;
  }
  window.addEventListener('resize', () => render());
}

async function start() {
  await loadI18n();
  applyStaticI18n();
  updateThemeButton(storedTheme());
  observeCanvas();
  await refresh();
  connect();
}

start();
