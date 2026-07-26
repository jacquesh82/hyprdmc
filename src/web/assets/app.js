'use strict';

// Toute la logique tient dans ce fichier : pas de dépendance, pas d'étape de
// compilation. L'état vient du démon (SSE), le brouillon vit ici jusqu'à ce que
// l'utilisateur clique sur « Appliquer ».

const SNAP = 60;            // aimantation, en pixels logiques
const ROTATIONS = [0, 90, 180, 270];

const el = (id) => document.getElementById(id);
const canvas = el('canvas');
const panel = el('panel');

let live = null;            // dernier état reçu du démon
let draft = [];             // agencement en cours d'édition
let selected = null;        // nom du connecteur sélectionné
let dirty = false;          // le brouillon diverge de l'état live
let guardTimer = null;

// ----------------------------------------------------------------- modèle ---

const clone = (v) => JSON.parse(JSON.stringify(v));
const byName = (name) => draft.find((o) => o.name === name);

/** Taille occupée dans l'espace de travail : la rotation échange les axes. */
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

// -------------------------------------------------------------- rendu 2D ---

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

/** Facteur d'échelle et décalage pour faire tenir l'agencement dans le cadre. */
function viewport() {
  const b = bounds();
  const pad = 24;
  const k = Math.min(
    (canvas.clientWidth - pad * 2) / Math.max(b.w, 1),
    (canvas.clientHeight - pad * 2) / Math.max(b.h, 1),
  );
  const scale = Math.min(k, 0.5);
  return {
    scale,
    ox: (canvas.clientWidth - b.w * scale) / 2 - b.x * scale,
    oy: (canvas.clientHeight - b.h * scale) / 2 - b.y * scale,
  };
}

function render() {
  const view = viewport();
  canvas.innerHTML = '';

  // Les écrans désactivés sont dessinés en dernier, sous les autres.
  for (const o of [...draft].sort((a, b) => Number(a.enabled) - Number(b.enabled))) {
    const [lw, lh] = logicalSize(o);
    const node = document.createElement('div');
    node.className = 'screen';
    node.dataset.name = o.name;
    if (o.name === selected) node.classList.add('selected');
    if (!o.enabled) node.classList.add('disabled');
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
        `${o.transform.flipped ? ' · inversé' : ''}${o.mirror_of ? ` · ⧉ ${o.mirror_of}` : ''}`
      : 'désactivé';
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

// ------------------------------------------------------- glisser-déposer ---

function onPointerDown(event) {
  const node = event.currentTarget;
  const o = byName(node.dataset.name);
  if (!o) return;

  selected = o.name;
  render();

  const view = viewport();
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

/** Colle l'écran contre ses voisins quand il en approche. */
function snap(o) {
  if (!occupies(o)) return;
  const [w, h] = logicalSize(o);

  for (const other of draft) {
    if (other.name === o.name || !occupies(other)) continue;
    const [ow, oh] = logicalSize(other);

    // Bords verticaux : droite contre gauche, gauche contre droite, alignement.
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

/** Ramène le coin supérieur gauche de l'ensemble à l'origine. */
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

// ------------------------------------------------------------- panneau ---

function renderPanel() {
  const o = byName(selected);
  if (!o) {
    panel.innerHTML = '<p class="empty">Sélectionnez un écran pour le configurer.</p>';
    return;
  }
  const monitor = live?.monitors?.find((m) => m.name === o.name);
  const modes = monitor?.availableModes ?? [];

  panel.innerHTML = '';
  panel.append(
    node('h2', o.name),
    node('p', monitor ? `${monitor.make} ${monitor.model} ${monitor.serial}`.trim() : '', 'sub'),
  );

  panel.append(field('', checkbox('Écran activé', o.enabled, (v) => { o.enabled = v; touch(); })));

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
  panel.append(field('Mode', modeSelect));

  const scale = document.createElement('input');
  scale.type = 'number';
  scale.step = '0.05';
  scale.min = '0.1';
  scale.value = o.scale;
  scale.addEventListener('change', () => {
    o.scale = Math.max(0.1, Number(scale.value) || 1);
    touch();
  });
  panel.append(field('Échelle', scale));

  const rotation = document.createElement('div');
  rotation.className = 'segmented';
  for (const deg of ROTATIONS) {
    const button = document.createElement('button');
    button.textContent = `${deg}°`;
    button.setAttribute('aria-pressed', String(degrees(o) === deg));
    button.addEventListener('click', () => { o.transform.rotation = `R${deg}`; touch(); });
    rotation.append(button);
  }
  panel.append(field('Rotation', rotation));

  panel.append(field('', checkbox('Inverser l\'image', o.transform.flipped, (v) => {
    o.transform.flipped = v;
    touch();
  })));

  const mirror = document.createElement('select');
  const none = document.createElement('option');
  none.value = '';
  none.textContent = 'aucune';
  mirror.append(none);
  for (const other of draft.filter((x) => x.name !== o.name && x.enabled)) {
    const option = document.createElement('option');
    option.value = other.name;
    option.textContent = other.name;
    mirror.append(option);
  }
  mirror.value = o.mirror_of ?? '';
  mirror.addEventListener('change', () => { o.mirror_of = mirror.value || null; touch(); });
  panel.append(field('Dupliquer', mirror));

  panel.append(field('', checkbox('Rafraîchissement variable (VRR)', o.vrr, (v) => { o.vrr = v; touch(); })));
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

// ----------------------------------------------------------- validation ---

/** Reprend côté client les règles bloquantes, pour ne pas envoyer l'inutile. */
function localIssues() {
  const issues = [];
  const active = draft.filter(occupies);

  for (let i = 0; i < active.length; i += 1) {
    for (let j = i + 1; j < active.length; j += 1) {
      if (overlaps(active[i], active[j])) {
        issues.push({ severity: 'error', message: `« ${active[i].name} » et « ${active[j].name} » se chevauchent` });
      }
    }
  }
  if (draft.length && !active.length) {
    issues.push({ severity: 'error', message: 'tous les écrans seraient désactivés' });
  }
  for (const o of draft.filter((x) => x.enabled && x.mirror_of)) {
    const target = byName(o.mirror_of);
    if (!target || !target.enabled) {
      issues.push({ severity: 'error', message: `« ${o.name} » duplique un écran indisponible` });
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

// ---------------------------------------------------------------- réseau ---

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
  el('profile-badge').textContent = state.activeProfile ? `profil : ${state.activeProfile}` : '';
  showGuard(state.revertPending, state.confirmTimeoutSecs);
  render();
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
    el('guard-text').textContent =
      `Configuration appliquée. Retour arrière automatique dans ${Math.max(left, 0)} s.`;
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
      console.error('état illisible', err);
    }
  });
}

// --------------------------------------------------------------- actions ---

el('btn-apply').addEventListener('click', async () => {
  try {
    const report = await api('/api/apply', {
      method: 'POST',
      body: JSON.stringify({ outputs: draft, guard: true }),
    });
    dirty = false;
    if (report.rolled_back) {
      toast('Hyprland n\'a pas appliqué la configuration : état précédent restauré.', true);
    } else {
      const warnings = (report.drifts ?? []).map((d) => d.message);
      toast(warnings.length ? warnings.join(' · ') : 'Configuration appliquée.');
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
  toast('Configuration conservée.');
  await refresh();
});

el('btn-revert').addEventListener('click', async () => {
  await api('/api/revert', { method: 'POST' });
  dirty = false;
  toast('Configuration précédente restaurée.');
  await refresh();
});

el('btn-save').addEventListener('click', async () => {
  const name = prompt('Nom du profil ?', live?.activeProfile ?? '');
  if (!name) return;
  try {
    await api(`/api/profiles/${encodeURIComponent(name)}`, {
      method: 'PUT',
      body: JSON.stringify({ outputs: draft }),
    });
    toast(`Profil « ${name} » enregistré.`);
    await refresh();
  } catch (err) {
    toast(err.message, true);
  }
});

el('btn-persist').addEventListener('click', async () => {
  try {
    const res = await api('/api/persist', { method: 'POST' });
    toast(`Agencement écrit dans ${res.path}.`);
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

window.addEventListener('resize', () => render());

refresh().then(connect);
