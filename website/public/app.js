// steinchain.com: live devnet status and the page's motion.
//
// Data: polls the devnet's public RPC (/status, /blocks) while the tab is
// visible, the follower first and the validator if the follower does not
// answer. No state, no cookies, no tracking. On any failure the panels keep
// what they show and the pill says the devnet is unreachable.
//
// Motion (anime.js 4.5, vendored in /vendor): a post-quantum lattice in the
// hero that qubits keep probing and that holds; a ripple and a new block card
// for every real block; the headline rising word by word; a roadmap timeline
// that fills as it scrolls past. Everything respects prefers-reduced-motion.
import {
  animate, createTimeline, createTimer, stagger, svg, utils, onScroll,
  scrambleText, engine,
} from './vendor/anime.esm.min.js';

const NODES = ['https://stein-follower.nick-598.workers.dev', 'https://rpc.steinchain.com'];
const REFRESH_MS = 4000;
const SVGNS = 'http://www.w3.org/2000/svg';

const motionQuery = window.matchMedia('(prefers-reduced-motion: reduce)');
const reduced = () => motionQuery.matches;
engine.reducedMotion = 'auto';

const $ = (sel, root = document) => root.querySelector(sel);
const fmt = new Intl.NumberFormat('en');

/* ------------------------------------------------------------------ */
/* Headline: split into words without losing the gradient span.        */
/* ------------------------------------------------------------------ */

function splitWords(el) {
  const words = [];
  const wrap = (text, extraClass) => {
    const frag = document.createDocumentFragment();
    text.split(/(\s+)/).forEach((part) => {
      if (!part) return;
      if (/^\s+$/.test(part)) { frag.appendChild(document.createTextNode(' ')); return; }
      const w = document.createElement('span');
      w.className = extraClass ? 'word ' + extraClass : 'word';
      w.textContent = part;
      words.push(w);
      frag.appendChild(w);
    });
    return frag;
  };
  [...el.childNodes].forEach((node) => {
    if (node.nodeType === Node.TEXT_NODE) {
      node.replaceWith(wrap(node.textContent, ''));
    } else if (node.nodeType === Node.ELEMENT_NODE) {
      node.replaceWith(wrap(node.textContent, node.className));
    }
  });
  return words;
}

/* ------------------------------------------------------------------ */
/* The lattice: a triangular lattice of points, a nod to the lattice    */
/* problems ML-DSA rests on.                                            */
/* ------------------------------------------------------------------ */

const COLS = 13;
const ROWS = 11;
const DX = 40;
const DY = 34.6;
const X0 = (560 - (COLS - 1) * DX - DX / 2) / 2;
const Y0 = (460 - (ROWS - 1) * DY) / 2;
const PT_R = 2.4;

const lattice = { points: [], pos: [], nbrs: [], edges: [] };

function el(name, attrs, parent) {
  const node = document.createElementNS(SVGNS, name);
  for (const k in attrs) node.setAttribute(k, attrs[k]);
  if (parent) parent.appendChild(node);
  return node;
}

function buildLattice() {
  const gEdges = $('#lattice-edges');
  const gPoints = $('#lattice-points');
  if (!gEdges || !gPoints) return false;

  for (let r = 0; r < ROWS; r++) {
    for (let c = 0; c < COLS; c++) {
      const x = X0 + c * DX + (r % 2 ? DX / 2 : 0);
      const y = Y0 + r * DY;
      lattice.pos.push([x, y]);
      lattice.nbrs.push([]);
    }
  }
  const idx = (r, c) => (r >= 0 && r < ROWS && c >= 0 && c < COLS ? r * COLS + c : -1);
  const link = (a, b) => {
    if (a < 0 || b < 0) return;
    lattice.nbrs[a].push(b);
    lattice.nbrs[b].push(a);
    const [x1, y1] = lattice.pos[a];
    const [x2, y2] = lattice.pos[b];
    lattice.edges.push(el('line', { class: 'edge', x1, y1, x2, y2 }, gEdges));
  };
  for (let r = 0; r < ROWS; r++) {
    for (let c = 0; c < COLS; c++) {
      const i = idx(r, c);
      link(i, idx(r, c + 1));
      if (r % 2 === 0) { link(i, idx(r + 1, c - 1)); link(i, idx(r + 1, c)); }
      else { link(i, idx(r + 1, c)); link(i, idx(r + 1, c + 1)); }
    }
  }
  lattice.pos.forEach(([x, y]) => {
    lattice.points.push(el('circle', { class: 'pt', cx: x, cy: y, r: PT_R }, gPoints));
  });
  return true;
}

const randomPoint = () => utils.random(0, lattice.points.length - 1);

/** A ripple through the lattice from point `from`: a block, or an absorbed probe. */
function ripple(from, color, strength = 1) {
  // The radius attribute, not a CSS scale: an SVG circle scales about the
  // view box's corner, not its own centre.
  animate(lattice.points, {
    r: [{ to: PT_R * (1 + 1.6 * strength), duration: 260 }, { to: PT_R, duration: 620 }],
    fill: [{ to: color, duration: 220 }, { to: '#3b82f6', duration: 900 }],
    delay: stagger(16, { grid: [COLS, ROWS], from }),
    ease: 'out(3)',
    composition: 'blend',
  });
}

/** A transaction's path across the lattice, drawn as a short random walk. */
function beam(from, color) {
  const gBeams = $('#lattice-beams');
  let at = from;
  const pts = [lattice.pos[at]];
  for (let s = 0; s < 6; s++) {
    const next = utils.randomPick(lattice.nbrs[at]);
    if (next === undefined) break;
    at = next;
    pts.push(lattice.pos[at]);
  }
  const path = el('polyline', {
    class: 'beam',
    points: pts.map((p) => p.join(',')).join(' '),
    stroke: color,
  }, gBeams);
  animate(svg.createDrawable(path), {
    draw: ['0 0', '0 1', '1 1'],
    duration: 1400,
    ease: 'inOut(2)',
    onComplete: () => path.remove(),
  });
}

/* ------------------------------------------------------------------ */
/* Quantum side: qubits as Bloch spheres, probes, interference rings.   */
/* ------------------------------------------------------------------ */

const QUBITS = [
  { x: 74, y: 70, r: 22 },
  { x: 132, y: 34, r: 15 },
  { x: 40, y: 140, r: 13 },
];

function buildQuantum() {
  const root = $('#lattice');
  const gWaves = el('g', { class: 'waves' });
  root.insertBefore(gWaves, root.querySelector('g[mask]'));
  const gQ = el('g', { class: 'qubits' }, root);

  // Interference: two sources, rings expanding and fading out of phase.
  const sources = [[QUBITS[0].x, QUBITS[0].y], [QUBITS[2].x, QUBITS[2].y]];
  const rings = [];
  sources.forEach(([cx, cy]) => {
    for (let k = 0; k < 4; k++) rings.push(el('circle', { class: 'wave', cx, cy, r: 0 }, gWaves));
  });

  // Rotations go through SVG transform attributes, which always turn about
  // the given centre, rather than CSS transforms on SVG children.
  const spheres = QUBITS.map((q) => {
    const g = el('g', { class: 'qubit' }, gQ);
    el('circle', { class: 'q-core', cx: q.x, cy: q.y, r: q.r * 0.16 }, g);
    el('circle', { class: 'q-shell', cx: q.x, cy: q.y, r: q.r }, g);
    const orbits = el('g', {}, g);
    el('ellipse', { class: 'q-orbit', cx: q.x, cy: q.y, rx: q.r, ry: q.r * 0.32 }, orbits);
    el('ellipse', { class: 'q-orbit', cx: q.x, cy: q.y, rx: q.r * 0.32, ry: q.r }, orbits);
    const vec = el('line', { class: 'q-vec', x1: q.x, y1: q.y, x2: q.x, y2: q.y - q.r }, g);
    const tip = el('circle', { class: 'q-tip', cx: q.x, cy: q.y - q.r, r: 2.6 }, g);
    return { g, orbits, vec, tip, q };
  });

  return { rings, spheres };
}

function animateQuantum({ rings, spheres }) {
  // Orbits tumble and the state vector precesses: a qubit is never at rest.
  spheres.forEach(({ orbits, vec, tip, q }, i) => {
    const spin = { a: 0 };
    animate(spin, {
      a: 360,
      duration: 6000 + i * 1700,
      loop: true,
      ease: 'linear',
      onUpdate: () => orbits.setAttribute('transform', `rotate(${spin.a.toFixed(2)} ${q.x} ${q.y})`),
    });
    // The state vector precesses around a tilted axis, so its tip traces an
    // ellipse inside the sphere and its length changes as it turns.
    const state = { t: utils.random(0, 6) };
    const tilt = 0.45 + i * 0.12;
    animate(state, {
      t: state.t + Math.PI * 2,
      duration: 3200 + i * 900,
      loop: true,
      ease: 'linear',
      onUpdate: () => {
        const x = q.x + q.r * 0.82 * Math.sin(state.t);
        const y = q.y - q.r * (0.62 + 0.3 * Math.cos(state.t) * tilt);
        vec.setAttribute('x2', x.toFixed(2));
        vec.setAttribute('y2', y.toFixed(2));
        tip.setAttribute('cx', x.toFixed(2));
        tip.setAttribute('cy', y.toFixed(2));
      },
    });
  });

  animate(rings, {
    r: [0, 260],
    opacity: [0.5, 0],
    duration: 4800,
    delay: stagger(1200, { start: 0 }),
    loop: true,
    ease: 'out(2)',
  });

  // Superposition: every lattice point flickers between two states.
  animate(lattice.points, {
    opacity: [{ to: 0.95 }, { to: 0.35 }],
    duration: () => utils.random(1400, 3200),
    delay: () => utils.random(0, 2400),
    loop: true,
    alternate: true,
    ease: 'inOutSine',
  });
}

/** A qubit probes the lattice with a wave; the lattice holds, and flashes green. */
function probe(spheres) {
  const gBeams = $('#lattice-beams');
  const { q } = utils.randomPick(spheres);
  const target = randomPoint();
  const [tx, ty] = lattice.pos[target];
  // A wave between the two points: a sine along the line.
  const dx = tx - q.x;
  const dy = ty - q.y;
  const len = Math.hypot(dx, dy) || 1;
  const nx = -dy / len;
  const ny = dx / len;
  const steps = 48;
  const amp = 7;
  const pts = [];
  for (let s = 0; s <= steps; s++) {
    const t = s / steps;
    const w = Math.sin(t * Math.PI * 9) * amp * Math.sin(t * Math.PI);
    pts.push(`${(q.x + dx * t + nx * w).toFixed(1)},${(q.y + dy * t + ny * w).toFixed(1)}`);
  }
  const wave = el('polyline', { class: 'probe', points: pts.join(' ') }, gBeams);
  const shield = el('circle', { class: 'shield', cx: tx, cy: ty, r: 4 }, gBeams);

  createTimeline({ onComplete: () => { wave.remove(); shield.remove(); } })
    .add(svg.createDrawable(wave), { draw: ['0 0', '0 1'], duration: 900, ease: 'in(2)' })
    .add(svg.createDrawable(wave), { draw: '1 1', opacity: 0, duration: 500, ease: 'out(2)' })
    .add(shield, { r: [4, 46], opacity: [0.9, 0], strokeWidth: [3, 0.5], duration: 900, ease: 'out(3)' }, '<<')
    .call(() => ripple(target, '#34d399', 0.6), '<<');
}

/* ------------------------------------------------------------------ */
/* The latest blocks, under the lattice.                                */
/* ------------------------------------------------------------------ */

const MAX_BLOCKS = 5;
let lastHeight = null;

function blockCard(b, fresh) {
  const card = document.createElement('div');
  card.className = 'blk' + (fresh ? ' new' : '');
  const h = document.createElement('span');
  h.className = 'h';
  h.textContent = '#' + fmt.format(b.height);
  const x = document.createElement('span');
  x.className = 'x';
  x.textContent = b.hash.slice(0, 6) + ' · ' + b.tx_count + ' tx';
  card.append(h, x);
  return card;
}

function pushBlock(b, fresh) {
  const chain = $('#chain');
  if (!chain) return;
  const card = blockCard(b, fresh);
  chain.appendChild(card);
  if (fresh && !reduced()) {
    animate(card, { opacity: [0, 1], x: [36, 0], scale: [0.85, 1], duration: 700, ease: 'out(4)' });
    animate(card.querySelector('.x'), {
      innerHTML: scrambleText({ chars: '0-9a-f', duration: 900 }),
    });
    setTimeout(() => card.classList.remove('new'), 1800);
  } else {
    card.classList.remove('new');
  }
  while (chain.children.length > MAX_BLOCKS) {
    const old = chain.firstElementChild;
    if (reduced()) { old.remove(); continue; }
    const width = old.getBoundingClientRect().width + 10;
    old.dataset.leaving = '1';
    animate(old, {
      marginLeft: [0, -width],
      opacity: [1, 0],
      duration: 600,
      ease: 'inOut(3)',
      onComplete: () => old.remove(),
    });
    // Count only cards that are staying.
    if ([...chain.children].filter((c) => !c.dataset.leaving).length <= MAX_BLOCKS) break;
  }
}

/* ------------------------------------------------------------------ */
/* Live status.                                                         */
/* ------------------------------------------------------------------ */

const shown = { 'st-height': null, 'st-txs': null, 'st-accounts': null };

function setText(id, value) {
  const node = document.getElementById(id);
  if (node) node.textContent = value;
}

/** Counts a number up from what is shown to its new value. */
function countTo(id, value) {
  const from = shown[id];
  shown[id] = value;
  if (from === null || from === value || reduced()) { setText(id, fmt.format(value)); return; }
  const o = { v: from };
  animate(o, {
    v: value,
    duration: 900,
    ease: 'out(3)',
    modifier: utils.round(0),
    onUpdate: () => setText(id, fmt.format(o.v)),
  });
}

function schemeLabel(tag) {
  switch (tag) {
    case 'fn-dsa-512-draft': return 'FN-DSA-512 (draft FIPS 206)';
    case 'ml-dsa-44': return 'ML-DSA-44';
    case 'ml-dsa-65': return 'ML-DSA-65';
    default: return String(tag);
  }
}

function validStatus(s) {
  return s && Number.isFinite(s.height) && Number.isFinite(s.tx_total) &&
    Number.isFinite(s.accounts) && Number.isFinite(s.block_time_ms) &&
    Number.isFinite(s.chain_id);
}

function validBlock(b) {
  return b && Number.isFinite(b.height) && typeof b.hash === 'string' &&
    /^[0-9a-f]{64}$/.test(b.hash) && Number.isFinite(b.tx_count);
}

async function getJSON(node, path) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 4000);
  try {
    const res = await fetch(node + path, { signal: controller.signal, cache: 'no-store', credentials: 'omit' });
    if (!res.ok) throw new Error('HTTP ' + res.status);
    return await res.json();
  } finally {
    clearTimeout(timer);
  }
}

/** The status of the first node that answers and is in sync, and that node. */
async function status() {
  let last = new Error('no node');
  for (const node of NODES) {
    try {
      const s = await getJSON(node, '/status');
      // A follower that has stopped following shows a stale chain.
      if (s && s.sync_error) throw new Error('follower out of sync');
      if (!validStatus(s)) throw new Error('unexpected status');
      return [s, node];
    } catch (e) {
      last = e;
    }
  }
  throw last;
}

function renderUp(s) {
  const pill = $('#net-pill');
  if (pill) { pill.classList.add('live'); pill.classList.remove('down'); }
  setText('net-pill-text', 'Devnet live · chain ' + s.chain_id);
  countTo('st-height', s.height);
  countTo('st-txs', s.tx_total);
  countTo('st-accounts', s.accounts);
  setText('st-blocktime', (s.block_time_ms / 1000) + ' s');
  if (Array.isArray(s.tx_schemes)) setText('st-schemes', s.tx_schemes.map(schemeLabel).join(' · '));
  setText('chain-state', 'height ' + fmt.format(s.height));
}

function renderDown() {
  const pill = $('#net-pill');
  if (pill) { pill.classList.add('down'); pill.classList.remove('live'); }
  setText('net-pill-text', 'Devnet unreachable right now');
  setText('chain-state', 'devnet unreachable');
}

async function refresh() {
  try {
    const [s, node] = await status();
    renderUp(s);
    if (lastHeight === null || s.height > lastHeight) {
      const blocks = await getJSON(node, '/blocks');
      const list = (Array.isArray(blocks) ? blocks : []).filter(validBlock)
        .sort((a, b) => a.height - b.height);
      const fresh = lastHeight === null
        ? list.slice(-MAX_BLOCKS)
        : list.filter((b) => b.height > lastHeight);
      const first = lastHeight === null;
      fresh.forEach((b) => pushBlock(b, !first));
      if (!first && fresh.length && lattice.points.length && !reduced()) {
        const from = randomPoint();
        ripple(from, '#e0f2fe', 1);
        beam(from, '#93c5fd');
      }
      lastHeight = s.height;
    }
  } catch (_) {
    renderDown();
  }
}

/* ------------------------------------------------------------------ */
/* Roadmap.                                                             */
/* ------------------------------------------------------------------ */

function setupRoadmap() {
  const wrap = $('.road-wrap');
  if (!wrap) return;
  const items = [...wrap.querySelectorAll('.road li')];
  const fill = wrap.querySelector('.road-fill');
  // A phase lights up once the drawn line reaches its marker, read from the
  // line's rendered end so the two can never disagree.
  const lightUp = () => {
    const end = fill.getBoundingClientRect().bottom;
    items.forEach((li) => {
      const marker = li.querySelector('.marker').getBoundingClientRect();
      li.classList.toggle('lit', end >= marker.top + marker.height / 2);
    });
  };

  if (reduced()) {
    utils.set('.road-fill', { scaleY: 1 });
    items.forEach((li) => { li.style.opacity = '1'; li.classList.add('lit'); });
    return;
  }

  // Items slide in once, as the list comes into view.
  createTimeline({
    autoplay: onScroll({ target: wrap, enter: '85% top', repeat: false }),
  })
    .add(items, {
      opacity: [0, 1],
      x: [-28, 0],
      duration: 700,
      ease: 'out(3)',
    }, stagger(90))
    .add('.road li.done .tag', {
      scale: [0.6, 1],
      duration: 500,
      ease: 'outBack(2)',
    }, stagger(90, { start: 200 }));

  // The track fills with the scroll position.
  animate(fill, {
    scaleY: [0, 1],
    ease: 'linear',
    onUpdate: lightUp,
    // Fills from when the list's top reaches 70% of the viewport to when its
    // bottom does, tied directly to the scroll position.
    autoplay: onScroll({
      target: wrap,
      enter: '70% top',
      leave: '70% bottom',
      sync: true,
    }),
  });

  // The phase in progress pulses.
  animate('.road li.next .ring', {
    scale: [1, 2.6],
    opacity: [0.7, 0],
    duration: 1800,
    delay: stagger(900),
    loop: true,
    ease: 'out(2)',
  });
}

/* ------------------------------------------------------------------ */
/* Intro and start-up.                                                  */
/* ------------------------------------------------------------------ */

function intro(quantum) {
  const title = $('#hero-title');
  const words = title ? splitWords(title) : [];
  const intros = document.querySelectorAll('[data-intro]');
  const visual = $('.hero-visual');

  if (reduced()) {
    [title, visual, ...intros].forEach((n) => { if (n) n.style.opacity = '1'; });
    return;
  }

  utils.set([title, visual], { opacity: 1 });
  const tl = createTimeline({ defaults: { ease: 'out(4)' } });
  tl.add(words, {
    opacity: [0, 1],
    y: ['0.55em', 0],
    filter: ['blur(10px)', 'blur(0px)'],
    duration: 900,
  }, stagger(70))
    .add(intros, { opacity: [0, 1], y: [18, 0], duration: 800 }, stagger(110, { start: 250 }))
    .add(lattice.edges, {
      opacity: [0, 1],
      duration: 700,
      delay: stagger(1.4, { from: 'center' }),
    }, 0)
    .add(lattice.points, {
      r: [0, PT_R],
      duration: 600,
      delay: stagger(9, { grid: [COLS, ROWS], from: 'center' }),
      ease: 'outBack(2)',
    }, 100)
    .add(quantum.spheres.map((s) => s.g), {
      opacity: [0, 1],
      duration: 900,
    }, stagger(140, { start: 500 }));

  // The gradient across "post-quantum" drifts back and forth.
  animate('.grad', {
    backgroundPosition: ['0% 50%', '100% 50%'],
    duration: 5000,
    loop: true,
    alternate: true,
    ease: 'inOutSine',
  });
}

function start() {
  const hasLattice = buildLattice();
  const quantum = hasLattice ? buildQuantum() : null;
  intro(quantum || { spheres: [] });
  if (quantum && !reduced()) {
    animateQuantum(quantum);
    // Qubits probe the lattice every few seconds; the lattice absorbs it.
    createTimer({
      duration: 3600,
      loop: true,
      delay: 2200,
      onLoop: () => { if (!document.hidden) probe(quantum.spheres); },
    });
  }
  setupRoadmap();

  let handle = null;
  const run = () => { if (handle === null) { refresh(); handle = setInterval(refresh, REFRESH_MS); } };
  const stop = () => { if (handle !== null) { clearInterval(handle); handle = null; } };
  document.addEventListener('visibilitychange', () => (document.hidden ? stop() : run()));
  run();
}

start();
