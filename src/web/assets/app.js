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
let draftPrimary = null;    // connector the draft calls the main screen
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
  'web.compositor_badge': '%{name}',
  'web.compositor.no_live': 'The %{name} plugin only writes configuration files. Arrange your screens here, then use “Make permanent” to write %{file} and reload your compositor.',
  'web.connection': 'connection to the daemon',
  'web.canvas_label': 'Display arrangement',
  'web.hint': "Drag an output to move it — it snaps to neighbouring edges. Arrow keys for fine adjustment.",
  'web.select_prompt': 'Select an output to configure it.',
  'web.guard.applied': 'Configuration applied — keep it?',
  'web.guard.countdown': 'Reverting automatically in %{seconds} s.',
  'web.guard.aria': 'Configuration applied. %{seconds} seconds before it is reverted automatically. Keep it?',
  'web.guard.keep': 'Keep',
  'web.guard.revert': 'Revert now',
  'web.guard.keys': 'Enter to keep · Esc to revert',
  'web.action.apply': 'Apply',
  'web.action.apply_title': 'Apply the pending changes (Ctrl+Enter)',
  'web.action.pending': '%{count} change(s)',
  'web.action.reset': 'Discard changes',
  'web.action.auto': 'Arrange automatically',
  'web.action.rescan': 'Detect new displays',
  'web.action.save': 'Save as profile…',
  'web.action.export': 'Export',
  'web.action.export_title': 'Download the whole configuration as a JSON file',
  'web.action.import': 'Import',
  'web.action.import_title': 'Replace the configuration with an exported file',
  'web.action.persist': 'Make permanent',
  'web.field.enabled': 'Output enabled',
  'web.field.mode': 'Mode',
  'web.field.scale': 'Scale',
  'web.field.rotation': 'Rotation',
  'web.field.flip': 'Flip the image',
  'web.field.mirror': 'Mirror',
  'web.field.vrr': 'Variable refresh rate (VRR)',
  'web.field.primary': 'Main screen',
  'web.field.primary_help': 'The main screen sits at 0×0, opens the row when the displays are arranged automatically, and takes the focus after an apply. Only one at a time.',
  'web.mirror.none': 'none',
  'web.screen.disabled': 'disabled',
  'web.screen.flipped': 'flipped',
  'web.screen.primary': 'main screen',
  'web.prompt.profile_name': 'Profile name?',
  'web.toast.applied': 'Configuration applied.',
  'web.toast.rolled_back': 'Hyprland did not apply the configuration: previous state restored.',
  'web.toast.kept': 'Configuration kept.',
  'web.toast.reverted': 'Previous configuration restored.',
  'web.toast.profile_saved': 'Profile "%{name}" saved.',
  'web.toast.persisted': 'Layout written to %{path}.',
  'web.toast.rescan_found': 'New display detected: %{names}.',
  'web.toast.rescan_none': 'No new display: the list is up to date.',
  'web.toast.exported': 'Configuration exported to %{name}.',
  'web.toast.imported': '%{count} profile(s) imported into %{path}.',
  'web.toast.import_unreadable': 'Unreadable file: this is not valid JSON.',
  'web.import.confirm': 'Replace the current configuration with this file (%{count} profile(s), keyboard and pointer, behaviour)?\n\nThe web port and the paths of the generated files stay as they are on this machine.',
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
  'web.history.close': 'Close',
  // Tabs.
  'web.tabs_label': 'Sections',
  'web.tab.screens': 'Displays',
  'web.tab.input': 'Keyboard & pointer',
  // Keyboard and pointer.
  'web.input.keyboard': 'Keyboard',
  'web.input.layout': 'Layout',
  'web.input.variant': 'Variant',
  'web.input.variant_none': 'none (plain layout)',
  'web.input.variant_help': 'Variants of the selected layout only.',
  'web.input.options': 'Options',
  'web.input.options_add': 'Add an option…',
  'web.input.options_none': 'No option set.',
  'web.input.option_remove': 'Remove the option %{name}',
  'web.input.pointer': 'Pointer',
  'web.input.touchpad': 'Touchpad',
  'web.input.mouse': 'Mouse',
  'web.input.scroll': 'Scroll direction',
  'web.input.scroll_normal': 'Normal',
  'web.input.scroll_inverted': 'Inverted',
  'web.input.scroll_help': 'Inverted is “natural” scrolling: the content follows your fingers.',
  'web.input.note': 'Applied immediately. “Make permanent” writes inputs.lua so the settings survive a restart.',
  'web.toast.input_applied': 'Keyboard and pointer settings applied.',
  'web.toast.input_persisted': 'Keyboard and pointer written to %{path}.',
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

// --------------------------------------------------------- import/export --

/**
 * Downloads the whole configuration as a JSON file.
 *
 * Written out with indentation: an export is something a person opens, diffs
 * and edits, not just something a machine reads back.
 */
el('btn-export').addEventListener('click', async () => {
  try {
    const bundle = await api('/api/config');
    const blob = new Blob([JSON.stringify(bundle, null, 2)], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const link = document.createElement('a');
    link.href = url;
    link.download = `hyprdmc-config-${new Date().toISOString().slice(0, 10)}.json`;
    link.click();
    // Revoked on the next turn of the event loop: Chrome cancels the download
    // if the object URL disappears before it has started reading it.
    setTimeout(() => URL.revokeObjectURL(url), 0);
    toast(t('web.toast.exported', { name: link.download }));
  } catch (err) {
    toast(err.message, true);
  }
});

el('btn-import').addEventListener('click', () => el('import-file').click());

el('import-file').addEventListener('change', async (event) => {
  const file = event.target.files?.[0];
  // Reset straight away, so picking the same file twice in a row still fires.
  event.target.value = '';
  if (!file) return;

  let bundle;
  try {
    bundle = JSON.parse(await file.text());
  } catch (err) {
    toast(t('web.toast.import_unreadable'), true);
    return;
  }

  // Replacing every profile in one click deserves the one question.
  const count = bundle?.config?.profile?.length ?? 0;
  if (!confirm(t('web.import.confirm', { count }))) return;

  try {
    const res = await api('/api/config', { method: 'POST', body: JSON.stringify(bundle) });
    toast(t('web.toast.imported', { count: res.profiles, path: res.path }));
    await refresh();
    await refreshInput();
  } catch (err) {
    toast(err.message, true);
  }
});

// ------------------------------------------------------------------ tabs --

// The ARIA tabs pattern: one tab in the tab order at a time, arrows to move
// between them. Switching hides a panel rather than rebuilding it, so an
// unapplied arrangement survives a trip to the keyboard settings and back.
const TABS = ['tab-screens', 'tab-input'];

function selectTab(id, { moveFocus = true } = {}) {
  for (const tabId of TABS) {
    const tab = el(tabId);
    const active = tabId === id;
    tab.setAttribute('aria-selected', String(active));
    tab.tabIndex = active ? 0 : -1;
    el(tab.dataset.panel).hidden = !active;
  }
  el('actions-screens').hidden = id !== 'tab-screens';
  el('actions-input').hidden = id !== 'tab-input';
  if (moveFocus) el(id).focus();
  // The canvas may have been hidden when it last tried to draw itself.
  if (id === 'tab-screens') render();
}

for (const tabId of TABS) {
  el(tabId).addEventListener('click', () => selectTab(tabId, { moveFocus: false }));
  el(tabId).addEventListener('keydown', (event) => {
    const step = { ArrowRight: 1, ArrowLeft: -1, Home: -TABS.length, End: TABS.length }[event.key];
    if (step === undefined) return;
    event.preventDefault();
    const index = Math.min(Math.max(TABS.indexOf(tabId) + step, 0), TABS.length - 1);
    selectTab(TABS[index]);
  });
}

// ----------------------------------------------------------- history drawer --

// The history is reference material — read now and then, never edited — so it
// lives in a drawer instead of permanently costing the arrangement canvas a
// third of its width. Closed by default; the choice is remembered.
const HISTORY_KEY = 'hyprdmc.history';
const drawer = () => el('history-panel');

const historyOpen = () => document.body.classList.contains('history-open');

/**
 * Opens or closes the drawer.
 *
 * `inert` is what actually takes the closed drawer out of the tab order and
 * out of the accessibility tree — the CSS `visibility` switch only handles the
 * pointer. `moveFocus` is off during the initial restore, where stealing focus
 * on page load would be rude.
 */
function setHistoryOpen(open, { moveFocus = true } = {}) {
  document.body.classList.toggle('history-open', open);
  if (open) drawer().removeAttribute('inert');
  else drawer().setAttribute('inert', '');
  el('btn-history').setAttribute('aria-expanded', String(open));
  try {
    localStorage.setItem(HISTORY_KEY, open ? 'open' : 'closed');
  } catch (err) { /* localStorage unavailable: the choice just won't persist */ }
  // Focus follows the panel in, and comes back to the button on the way out,
  // so the keyboard never lands on nothing.
  if (moveFocus) (open ? el('btn-history-close') : el('btn-history')).focus();
}

el('btn-history').addEventListener('click', () => setHistoryOpen(!historyOpen()));
el('btn-history-close').addEventListener('click', () => setHistoryOpen(false));
el('history-scrim').addEventListener('click', () => setHistoryOpen(false));

// ------------------------------------------------------------------ model --

const clone = (v) => JSON.parse(JSON.stringify(v));
const byName = (name) => draft.find((o) => o.name === name);

/** The main screen the daemon currently has on record, or null. */
const livePrimary = () => live?.layout?.primary ?? null;

/**
 * The main screen of the draft, but only if it can actually anchor anything.
 *
 * Mirrors `Layout::primary_output` server-side: a screen that is off, or
 * mirroring another one, occupies no space of its own — anchoring on it would
 * mean anchoring on nothing.
 */
function anchorOutput() {
  const o = byName(draftPrimary);
  return o && occupies(o) ? o : null;
}

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
    if (o.name === draftPrimary) node.classList.add('primary');

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

    // A star on the box, so the main screen is visible in the arrangement and
    // not only in the panel of whichever output happens to be selected. The
    // glyph is decorative and the words go with it: a bare ★ says nothing to a
    // screen reader, and nothing here is important enough to say twice.
    if (o.name === draftPrimary) {
      const star = document.createElement('span');
      star.className = 'primary-mark';
      star.textContent = '★';
      star.setAttribute('aria-hidden', 'true');
      const label = document.createElement('span');
      label.className = 'visually-hidden';
      label.textContent = t('web.screen.primary');
      node.append(star, label);
    }

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
  renderPending();
}

/** How many outputs the draft would actually change. */
function changedCount() {
  const before = live?.layout?.outputs ?? [];
  const changed = new Set(draft
    .filter((o) => {
      const was = before.find((p) => p.name === o.name);
      return !was || JSON.stringify(was) !== JSON.stringify(o);
    })
    .map((o) => o.name));

  // The main screen is a choice about the layout, not a field of an output, so
  // it has to be counted separately — otherwise picking a new one and nothing
  // else would badge Apply with "0 changes" while the button sits enabled.
  if (draftPrimary !== livePrimary()) changed.add(draftPrimary ?? '');
  return changed.size;
}

/**
 * Badges Apply with the number of outputs affected. An enabled button says
 * "you may"; the count says "you have something to apply, and how much".
 */
function renderPending() {
  const badge = el('pending');
  const count = dirty ? changedCount() : 0;
  badge.hidden = count === 0;
  if (count) badge.textContent = t('web.action.pending', { count });
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

/**
 * Brings the set back to the origin — the main screen if there is one, the
 * top-left corner otherwise. Mirrors `Layout::normalize` server-side.
 */
function normalize() {
  const active = draft.filter(occupies);
  if (!active.length) return;
  const anchor = anchorOutput();
  const dx = anchor ? anchor.x : Math.min(...active.map((o) => o.x));
  const dy = anchor ? anchor.y : Math.min(...active.map((o) => o.y));
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

  // Exclusive by construction: checking it here is what unchecks it everywhere
  // else, since there is only one `draftPrimary`. Unavailable for a screen that
  // is off or mirroring another one — neither can anchor a layout.
  const canAnchor = occupies(o);
  const main = checkbox(t('web.field.primary'), o.name === draftPrimary, (v) => {
    draftPrimary = v ? o.name : null;
    // The whole arrangement moves with the anchor, so it is re-normalized now
    // rather than at apply time: what the canvas shows is what gets sent.
    normalize();
    touch();
  });
  main.querySelector('input').disabled = !canAnchor;
  const mainField = field('', main);
  mainField.append(node('p', t('web.field.primary_help'), 'helper'));
  panel.append(mainField);
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
  // A compositor hyprdmc cannot drive is standing news, not an issue with this
  // particular layout, so it is stated whether or not anything else is wrong.
  if (!liveApply()) {
    issues.unshift({
      severity: 'warning',
      message: t('web.compositor.no_live', {
        name: live.compositor.label,
        file: live.compositor.monitorsFile,
      }),
    });
  }
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
  el('btn-apply').disabled = !liveApply() || issues.some((i) => i.severity === 'error');
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

/**
 * Reconciles the draft with the outputs the daemon reports.
 *
 * A screen plugged in while you have unapplied changes has to show up anyway —
 * otherwise the only way to see it is to throw your work away first. Edits are
 * kept per output: an output already in the draft keeps the version being
 * edited, one that just appeared is adopted from the live state, and one that
 * vanished is dropped.
 */
function syncDraft(outputs) {
  draft = outputs.map((o) => byName(o.name) ?? clone(o));
  if (!byName(selected)) selected = draft[0]?.name ?? null;
  // A main screen that has just been unplugged is no longer a choice we can
  // send; the daemon would answer the same thing anyway.
  if (draftPrimary && !byName(draftPrimary)) draftPrimary = null;
}

function adopt(state) {
  live = state;
  if (dirty) {
    syncDraft(state.layout.outputs);
  } else {
    // Nothing being edited: the live state wins outright, positions included.
    draft = clone(state.layout.outputs);
    draftPrimary = livePrimary();
    if (!byName(selected)) selected = draft[0]?.name ?? null;
  }
  el('profile-badge').hidden = !state.activeProfile;
  el('profile-badge').textContent = state.activeProfile ? t('web.profile_badge', { name: state.activeProfile }) : '';
  renderCompositor(state.compositor);
  showGuard(state.revertPending, state.confirmTimeoutSecs);
  render();
  // `state.history` (pushed over SSE) only carries the raw snapshot fields;
  // the formatted labels this panel needs come from GET /api/history, so we
  // re-fetch it here rather than duplicating the server's age/summary logic.
  refreshHistory();
}

/**
 * Names the compositor plugin in force, and says so when it cannot apply.
 *
 * A plugin that only writes files must not leave an enabled Apply button that
 * fails on every click: the button goes away and the note explains what to do
 * instead. `liveApply` is read from the state rather than assumed, so the day a
 * transport lands for another compositor the UI needs no change.
 */
function renderCompositor(compositor) {
  const badge = el('compositor-badge');
  badge.hidden = !compositor;
  if (!compositor) return;
  badge.textContent = t('web.compositor_badge', { name: compositor.label });
  badge.classList.toggle('warn', !compositor.supportsLive);
}

/** Can the current compositor be reconfigured without a restart? */
const liveApply = () => live?.compositor?.supportsLive !== false;

/** Seconds left below which the guard switches to its urgent styling. */
const GUARD_URGENT_AT = 5;
/**
 * Seconds at which the countdown is spoken. Announcing every tick would talk
 * over the two buttons that resolve it; announcing only on open would leave a
 * screen-reader user with no sense of the deadline.
 */
const GUARD_ANNOUNCE_AT = new Set([5, 3, 2, 1]);
/** Matches `r` on the ring in index.html. */
const GUARD_RING_RADIUS = 34;
const GUARD_RING_LENGTH = 2 * Math.PI * GUARD_RING_RADIUS;

/**
 * Shows (or hides) the revert countdown, as a centred modal dialog.
 *
 * Modal on purpose: this is the one moment where the user may be facing a
 * screen they cannot read, and nothing else on the page matters until they
 * answer. `showModal()` brings the focus trap, the inert background and the
 * top layer; focus then moves to "Keep" so the whole thing can be resolved
 * with one key — Enter keeps, Escape reverts (see the `cancel` handler).
 *
 * The remaining time is stated three ways — a large number, a sentence, and a
 * ring that drains — because a ring alone is not readable and a number alone
 * is not noticeable.
 */
function showGuard(pending, seconds) {
  const guard = el('guard');
  clearInterval(guardTimer);

  if (!pending) {
    if (guard.open) guard.close();
    guard.classList.remove('urgent');
    return;
  }

  const wasClosed = !guard.open;
  // showModal() on an already-open dialog throws: every state push would land
  // here, not just the one that opens it.
  if (wasClosed) guard.showModal();

  const total = Math.max(seconds, 1);
  let left = seconds;

  const ring = el('guard-ring-bar');
  ring.style.strokeDasharray = GUARD_RING_LENGTH;

  const tick = () => {
    const shown = Math.max(left, 0);
    el('guard-seconds').textContent = shown;
    el('guard-countdown').textContent = t('web.guard.countdown', { seconds: shown });
    ring.style.strokeDashoffset = GUARD_RING_LENGTH * (1 - shown / total);
    guard.classList.toggle('urgent', shown <= GUARD_URGENT_AT);
    // Read by assistive tech, which never sees the ring or the big digits.
    if (shown === seconds || GUARD_ANNOUNCE_AT.has(shown)) {
      el('guard-announce').textContent = t('web.guard.aria', { seconds: shown });
    }
    if (left <= 0) clearInterval(guardTimer);
    left -= 1;
  };

  tick();
  guardTimer = setInterval(tick, 1000);

  // Only on the transition into the guard: re-focusing on every state push
  // would steal the caret while the user is already answering.
  if (wasClosed) el('btn-confirm').focus();
}

/**
 * Escape inside a <dialog> fires `cancel` and would just close it, leaving the
 * layout applied and the countdown invisible. Here Escape means "revert" — the
 * keyboard is the way out of a screen you cannot read.
 */
el('guard').addEventListener('cancel', (event) => {
  event.preventDefault();
  el('btn-revert').click();
});

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
      body: JSON.stringify({ outputs: draft, primary: draftPrimary, guard: true }),
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

/**
 * Keyboard equivalents for the decisions that matter.
 *
 * Escape closes the history drawer; while the guard is up it reverts instead,
 * but that is the dialog's own `cancel` event, which never reaches here.
 * Ctrl/Cmd+Enter applies from anywhere, including mid-drag.
 */
document.addEventListener('keydown', (event) => {
  if (event.key === 'Escape' && historyOpen()) {
    event.preventDefault();
    setHistoryOpen(false);
    return;
  }
  if (event.key === 'Enter' && (event.ctrlKey || event.metaKey)) {
    const apply = el('btn-apply');
    if (apply.disabled) return;
    event.preventDefault();
    apply.click();
  }
});

/**
 * Re-reads the outputs and says what changed.
 *
 * `GET /api/state` always queries Hyprland afresh, so this needs nothing new
 * server-side — and it deliberately does *not* trigger a reconcile: reapplying
 * the matching profile would move the screens already in place, which is not
 * what "detect" means. The draft keeps its edits (see syncDraft), so a screen
 * plugged in mid-arrangement joins the work in progress instead of erasing it.
 */
el('btn-rescan').addEventListener('click', async () => {
  const button = el('btn-rescan');
  const before = new Set((live?.monitors ?? []).map((m) => m.name));
  button.disabled = true;
  try {
    adopt(await api('/api/state'));
    const found = (live.monitors ?? []).map((m) => m.name).filter((name) => !before.has(name));
    toast(found.length
      ? t('web.toast.rescan_found', { names: found.join(', ') })
      : t('web.toast.rescan_none'));
  } catch (err) {
    toast(err.message, true);
  } finally {
    button.disabled = false;
  }
});

el('btn-auto').addEventListener('click', () => {
  // The main screen opens the row, like `Layout::auto_arrange` server-side:
  // "left to right" has to start somewhere, and it should not start beside the
  // screen the user calls their main one.
  const anchor = anchorOutput();
  const active = draft.filter(occupies);
  const order = anchor ? [anchor, ...active.filter((o) => o !== anchor)] : active;
  let cursor = 0;
  for (const o of order) {
    o.x = cursor;
    o.y = 0;
    cursor += logicalSize(o)[0];
  }
  touch();
});

/**
 * Answers the guard. Both buttons go dead while the request is in flight —
 * the dialog stays up until the daemon has actually decided, and a double
 * click must not send the opposite answer twice.
 */
async function resolveGuard(path, message) {
  const buttons = [el('btn-confirm'), el('btn-revert')];
  for (const button of buttons) button.disabled = true;
  try {
    await api(path, { method: 'POST' });
    dirty = false;
    toast(message());
    await refresh();
  } catch (err) {
    toast(err.message, true);
  } finally {
    for (const button of buttons) button.disabled = false;
  }
}

el('btn-confirm').addEventListener('click', () => resolveGuard('/api/confirm', () => t('web.toast.kept')));
el('btn-revert').addEventListener('click', () => resolveGuard('/api/revert', () => t('web.toast.reverted')));

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
  // The drawer scrolls, so it can afford more than the sidebar could.
  const entries = (data.entries ?? []).slice(0, 10);

  el('history-remembered').textContent = t('web.history.remembered', { count: data.remembered ?? 0 });

  // Closed, the drawer still has to say it holds something worth opening.
  const count = el('history-count');
  count.hidden = entries.length === 0;
  count.textContent = entries.length;

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

// --------------------------------------------------- keyboard and pointer --

// Kept entirely apart from `draft`/`live`: the keyboard is not part of a
// screen profile, and docking a laptop must never change what you type in.
let inputCatalog = { layouts: [], variants: [], options: [] };
let inputLive = null;     // last state read from the compositor
let inputDraft = null;    // what the form currently says

// Scroll direction: one segmented control per device, each bound to its own
// field of the draft by `data-target`. Two Hyprland settings, two controls.
const SCROLL_CONTROLS = ['scroll-touchpad', 'scroll-mouse'];

const inputDirty = () => JSON.stringify(inputLive) !== JSON.stringify(inputDraft);

/** Options are stored comma-separated; the UI works on the list. */
const optionList = () => (inputDraft.kb_options ? inputDraft.kb_options.split(',').filter(Boolean) : []);

function setOptionList(list) {
  inputDraft.kb_options = list.join(',');
  renderInput();
}

function fillSelect(select, entries, value, placeholder) {
  select.innerHTML = '';
  if (placeholder !== undefined) {
    const empty = document.createElement('option');
    empty.value = '';
    empty.textContent = placeholder;
    select.append(empty);
  }
  for (const entry of entries) {
    const option = document.createElement('option');
    option.value = entry.code;
    // Code first: it is what ends up in the config file, and what the user
    // will recognise if they have ever edited hyprland.lua by hand.
    option.textContent = `${entry.code} — ${entry.label}`;
    select.append(option);
  }
  select.value = value ?? '';
  // A layout set by hand that the catalogue does not know about must still
  // show, rather than silently snapping to the first entry in the list.
  if (value && select.value !== value) {
    const unknown = document.createElement('option');
    unknown.value = value;
    unknown.textContent = value;
    select.append(unknown);
    select.value = value;
  }
}

function renderInput() {
  if (!inputDraft) return;

  fillSelect(el('kb-layout'), inputCatalog.layouts, inputDraft.kb_layout);

  // Only the variants of the chosen layout: the full list is thousands of
  // entries, and 99% of them are meaningless next to the current layout.
  const variants = inputCatalog.variants.filter((v) => v.layout === inputDraft.kb_layout);
  fillSelect(el('kb-variant'), variants, inputDraft.kb_variant, t('web.input.variant_none'));
  el('kb-variant').disabled = variants.length === 0 && !inputDraft.kb_variant;

  const chosen = optionList();
  fillSelect(
    el('kb-option-add'),
    inputCatalog.options.filter((o) => !chosen.includes(o.code)),
    '',
    t('web.input.options_add'),
  );

  const list = el('kb-option-list');
  list.innerHTML = '';
  el('kb-options-empty').hidden = chosen.length > 0;
  for (const code of chosen) {
    const entry = inputCatalog.options.find((o) => o.code === code);
    const item = document.createElement('li');
    item.className = 'chip';
    item.append(document.createTextNode(entry ? `${code} — ${entry.label}` : code));

    const remove = document.createElement('button');
    remove.type = 'button';
    remove.setAttribute('aria-label', t('web.input.option_remove', { name: code }));
    remove.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" '
      + 'stroke-linecap="round" aria-hidden="true" focusable="false"><path d="M18 6 6 18M6 6l12 12"/></svg>';
    remove.addEventListener('click', () => setOptionList(chosen.filter((c) => c !== code)));
    item.append(remove);
    list.append(item);
  }

  for (const id of SCROLL_CONTROLS) {
    const group = el(id);
    const natural = inputDraft[group.dataset.target];
    for (const button of group.querySelectorAll('button')) {
      button.setAttribute('aria-pressed', String((button.dataset.natural === 'true') === natural));
    }
  }

  el('btn-input-apply').disabled = !inputDirty() || !liveApply();
  el('btn-input-reset').disabled = !inputDirty();
}

el('kb-layout').addEventListener('change', (event) => {
  inputDraft.kb_layout = event.target.value;
  // A variant belongs to one layout: keeping "oss" after switching to "us"
  // would send Hyprland a pair it will reject.
  inputDraft.kb_variant = '';
  renderInput();
});

el('kb-variant').addEventListener('change', (event) => {
  inputDraft.kb_variant = event.target.value;
  renderInput();
});

el('kb-option-add').addEventListener('change', (event) => {
  if (!event.target.value) return;
  setOptionList([...optionList(), event.target.value]);
});

for (const id of SCROLL_CONTROLS) {
  el(id).addEventListener('click', (event) => {
    const button = event.target.closest('button');
    if (!button) return;
    inputDraft[event.currentTarget.dataset.target] = button.dataset.natural === 'true';
    renderInput();
  });
}

/** Loads the live settings and the xkb catalogue. */
async function refreshInput() {
  try {
    const data = await api('/api/input');
    inputCatalog = data.catalog ?? inputCatalog;
    inputLive = data.current;
    // Never clobber an edit in progress with a background refresh.
    if (!inputDraft || !inputDirty()) inputDraft = clone(inputLive);
    renderInput();
  } catch (err) {
    console.error('input settings unavailable', err);
  }
}

el('btn-input-apply').addEventListener('click', async () => {
  const button = el('btn-input-apply');
  button.disabled = true;
  try {
    await api('/api/input', { method: 'PUT', body: JSON.stringify(inputDraft) });
    inputLive = clone(inputDraft);
    toast(t('web.toast.input_applied'));
  } catch (err) {
    toast(err.message, true);
  } finally {
    renderInput();
  }
});

el('btn-input-reset').addEventListener('click', () => {
  inputDraft = clone(inputLive);
  renderInput();
});

el('btn-input-persist').addEventListener('click', async () => {
  try {
    const res = await api('/api/input/persist', { method: 'POST' });
    toast(t('web.toast.input_persisted', { path: res.path }));
  } catch (err) {
    toast(err.message, true);
  }
});

/** Restores the drawer's last state. Closed unless it was explicitly opened. */
function restoreHistoryDrawer() {
  let stored = 'closed';
  try {
    stored = localStorage.getItem(HISTORY_KEY) ?? 'closed';
  } catch (err) { /* localStorage unavailable: start closed */ }
  setHistoryOpen(stored === 'open', { moveFocus: false });
}

async function start() {
  await loadI18n();
  applyStaticI18n();
  updateThemeButton(storedTheme());
  restoreHistoryDrawer();
  observeCanvas();
  await refresh();
  // Not awaited: the arrangement is what the user came for, and the keyboard
  // tab can populate a moment later without holding up the first paint.
  refreshInput();
  connect();
}

start();
