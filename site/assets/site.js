/* Importin's website: the one script (I3 + chase, S3). The page is complete without it: the pore's CSS keyframes run
   and the pause box pauses them. Under prefers-reduced-motion this script does nothing at all.
   With it, the drawing's letters become one system. Free letters drift in the cytoplasm and move away from a moving
   pointer; a click or tap there places one; any letter can be dragged. An importin takes a letter in through the
   pore: it comes in from the cytoplasm for a placed letter, and takes any letter that gets within reach of the pore.
   Inside, letters pile faintly and fade; one dragged toward the membrane stops at the pore. Nothing comes out.
   Letters the user brings in (fed, dragged to the pore, or herded) arrive with a ripple and a brighter swirl and each
   lights one subunit; at eight the ring plays a short display and goes dark again. Nothing of it is shown in words.
   While the drawing is out of view, faint letters come in from the sides of the window now and then, and an importin
   carries each up to the mark in the header, where it goes into the centre.
   Units are the drawing's: the viewBox is 760 x 280 around the pore's centre. */
(() => {
  'use strict';
  const still = matchMedia('(prefers-reduced-motion: reduce)');
  if (still.matches) return;
  const plate = document.getElementById('pore'), svg = plate && plate.querySelector('svg');
  const env = svg && svg.querySelector('#p-env'), box = plate && plate.querySelector('.pause input');
  if (!env || !box || !window.IntersectionObserver || !window.DOMMatrix) return;
  const NS = svg.namespaceURI, DEG = 180 / Math.PI;

  // the pore: its rim, the channel; a free letter this near the centre is taken in; pile and wall radii inside
  const RIM = 94, CHAN = 52, REACH = 118, PILE = 30, WALL = 42;
  // the chase: a moving pointer pushes free letters within FLEE with PUSH * (1 - d/FLEE)^2 (units/s^2); FRICTION
  // takes e^-FRICTION of the speed per second; speed is capped at CAP, raised up to (1 + BOOST) x CAP by heat,
  // which fills in HEAT_UP s while the pointer keeps closing in on a letter and decays with HEAT_DOWN s otherwise
  const FLEE = 130, PUSH = 900, FRICTION = 2.4, CAP = 75, BOOST = 1.4, HEAT_UP = 2.5, HEAT_DOWN = 1.5;
  // drift when nothing chases; soft edges; letters keep CROWD apart
  const WANDER = 14, BOUNCE = 0.5, EDGE = 18, CROWD = 30;
  // carrying: speed and bend of the way in, walking speed of an importin; letters in the cytoplasm, at most and ambient
  const CARRY = 40, BEND = 0.7, WALK = 85, MAX = 14, AMBIENT = 4;
  // the display at eight earned arrivals: its length in s; a push counts as herding for HERD s
  const EGG = 2.8, HERD = 1.5;

  const rnd = (a, b) => a + Math.random() * (b - a);
  const clamp = (v, a, b) => (v < a ? a : v > b ? b : v);
  const make = (tag, attrs, parent) => {
    const e = document.createElementNS(NS, tag);
    for (const k in attrs) e.setAttribute(k, attrs[k]);
    if (parent) parent.append(e);
    return e;
  };
  const sc = (r) => 0.62 + 0.38 * Math.min(1, r / 120); // the drawing's depth: smaller nearer the channel

  // the drawing's own symbols, split: the letter without its importin, and the importin's arc alone
  const lt = env.cloneNode(true);
  lt.id = 'p-let';
  lt.querySelector('.p-imp').remove();
  const im = make('g', { id: 'p-imv', transform: env.getAttribute('transform') });
  im.append(env.querySelector('.p-imp').cloneNode());
  env.parentNode.append(lt, im);
  const at0 = svg.querySelector('.p-rel');
  const layer = () => svg.insertBefore(make('g', {}), at0);
  const gIn = layer(), gRel = layer(), gImp = layer(), gLet = layer();
  const flash = svg.insertBefore(make('circle', { class: 'p-flash', r: CHAN, opacity: 0 }), at0);
  // for earned arrivals: an accent copy of the swirl over it, a fill over each subunit, and a light for each far
  // pore, clockwise from the top
  const swirl = svg.querySelector('.p-swirl'), subs = svg.querySelector('.p-subs');
  const glow = make('g', { class: 'p-glow', opacity: 0 }), spin = make('g', {}, glow);
  for (const u of svg.querySelector('#p-fgs').children) spin.append(u.cloneNode());
  swirl.after(glow);
  const gLit = make('g', { class: 'p-lit' });
  subs.after(gLit);
  const lit = [0, 1, 2, 3, 4, 5, 6, 7].map((k) => make('use', { href: '#p-sb', transform: `rotate(${45 * k})`, opacity: 0 }, gLit));
  const gFar = make('g', {});
  svg.querySelector('.p-far').after(gFar);
  const far = [...svg.querySelectorAll('.p-far use')].map((u) => {
    const [x, y, k] = (u.getAttribute('transform').match(/[-\d.]+/g) || []).map(Number);
    return { a: (Math.atan2(x, -y) + 2 * Math.PI) % (2 * Math.PI),
      ring: make('circle', { class: 'p-flash', cx: x, cy: y, r: 94 * k, opacity: 0 }, gFar),
      disc: make('circle', { class: 'p-tag', cx: x, cy: y, r: 52 * k, opacity: 0 }, gFar) };
  }).sort((p, q) => p.a - q.a);

  const letters = [], imps = [], rings = [];
  const ptr = { in: false, mouse: false, x: 0, y: 0, vx: 0, vy: 0, t: -1e9 };
  let T = 0, nextSpawn = 1.5, raf = 0, last = 0, inView = true, paused = box.checked, flashT = 0;
  let earned = 0, egg = null, glowT = 9, spinA = 0, cascade = 0; // egg: s since the display began, null when none
  const litT = new Array(8).fill(-9);
  let W = 380, H = 140, S = 1, OX = 0, OY = 0; // visible half-extents, px per unit, the centre in page px
  let drag = null, down = null, cursor = '';

  function letter(x, y, op, fade) {
    const l = { x, y, vx: 0, vy: 0, rot: 0, s: sc(Math.hypot(x, y)), op, fade, st: 'free', t: 0, imp: null,
      heat: 0, w: rnd(0, 2 * Math.PI), calm: 0, due: 0, bend: Math.random() < 0.5 ? -1 : 1, pushed: -9, earned: false,
      el: make('use', { href: '#p-let' }, gLet) };
    letters.push(l);
    return l;
  }
  function importin(x, y, op) {
    const i = { x, y, vx: 0, rot: 0, s: sc(Math.hypot(x, y)), op, st: 'walk', L: null,
      el: make('use', { href: '#p-imv' }, gImp) };
    imps.push(i);
    return i;
  }
  const inCyto = (l) => l.st === 'free' || l.st === 'carry';
  function carry(l, i) { l.st = 'carry'; l.imp = i; i.st = 'carry'; i.L = l; }
  function letGo(l) { // its importin, holding or on its way, leaves the focal plane
    if (l.imp) { l.imp.st = 'leave'; l.imp.L = null; l.imp = null; }
    if (l.st === 'carry') l.st = 'free';
  }
  function grab(l) { letGo(l); carry(l, importin(l.x, l.y, 0)); } // one comes into focus under it
  function send(l) { // one walks over from deeper in the cytoplasm, never out of the pore
    const r = Math.hypot(l.x, l.y) || 1, i = importin(l.x + (l.x / r) * 150, l.y + (l.y / r) * 150, 0);
    i.L = l;
    l.imp = i;
  }
  function enter(l, t) {
    const a = rnd(0, 2 * Math.PI), r = PILE * Math.sqrt(Math.random());
    Object.assign(l, { st: 'in', t, x: 0, y: 0, px: r * Math.cos(a), py: r * Math.sin(a), rot0: rnd(-25, 25) });
    release(t, l.earned);
    if (!l.earned || t) return;
    glowT = 0; // an earned arrival: the swirl brightens and, unless the display runs, one more subunit lights
    if (egg === null) { litT[earned] = T; if (++earned === 8) egg = -0.6; }
  }
  function release(t, big) { // the keyframes' release ring; for an earned letter larger, with a hairline ripple
    rings.push({ x: 0, y: 0, rot: 0, s: 0.3, op: 0, t, big, el: make('circle', { class: 'p-tag', r: 5, opacity: 0 }, gRel),
      rip: big ? make('circle', { class: 'p-flash', r: 6, opacity: 0 }, gRel) : null });
  }
  function spawn() {
    for (let n = 0; n < 12; n++) {
      const x = rnd(-W + EDGE, W - EDGE), y = rnd(-H + EDGE, H - EDGE);
      if (Math.hypot(x, y) > (n < 8 ? 170 : REACH + 12)) { letter(x, y, 0, 1 / 0.6).due = T + rnd(2.5, 6); return; }
    }
  }
  function place(p) { // feed
    if (Math.hypot(p.x, p.y) <= RIM || letters.filter(inCyto).length >= MAX) return;
    const l = letter(clamp(p.x, -W + EDGE, W - EDGE), clamp(p.y, -H + EDGE, H - EDGE), 0, 1 / 0.2);
    l.calm = T + 0.8;
    l.earned = true;
    if (Math.hypot(l.x, l.y) < REACH) grab(l); else send(l);
  }
  const tilt = (o, vx, dt) => { o.rot += (clamp(vx * 0.12, -10, 10) - o.rot) * Math.min(1, dt * 4); };

  function free(l, dt, act) {
    l.op = Math.min(1, l.op + dt * l.fade);
    l.w += rnd(-6, 6) * dt;
    let ax = WANDER * Math.cos(l.w), ay = WANDER * Math.sin(l.w), closing = false;
    if (act && T > l.calm) {
      const dx = l.x - ptr.x, dy = l.y - ptr.y, d = Math.hypot(dx, dy) || 1e-3;
      if (d < FLEE) {
        const f = PUSH * act * (1 - d / FLEE) ** 2;
        ax += (f * dx) / d; ay += (f * dy) / d;
        closing = ptr.vx * dx + ptr.vy * dy > 0;
        l.pushed = T;
      }
    }
    l.heat = closing ? Math.min(1, l.heat + dt / HEAT_UP) : l.heat * Math.exp(-dt / HEAT_DOWN);
    for (const o of letters) {
      if (o === l || o.st !== 'free') continue;
      const dx = l.x - o.x, dy = l.y - o.y, d = Math.hypot(dx, dy);
      if (d > 0 && d < CROWD) { const f = 80 * (1 - d / CROWD); ax += (f * dx) / d; ay += (f * dy) / d; }
    }
    const k = Math.exp(-FRICTION * dt);
    l.vx = (l.vx + ax * dt) * k; l.vy = (l.vy + ay * dt) * k;
    const v = Math.hypot(l.vx, l.vy), cap = CAP * (1 + BOOST * l.heat);
    if (v > cap) { l.vx *= cap / v; l.vy *= cap / v; }
    l.x += l.vx * dt; l.y += l.vy * dt;
    const bx = W - EDGE, by = H - EDGE;
    if (Math.abs(l.x) > bx) { l.x = Math.sign(l.x) * bx; l.vx = -l.vx * BOUNCE; l.w = Math.PI - l.w; }
    if (Math.abs(l.y) > by) { l.y = Math.sign(l.y) * by; l.vy = -l.vy * BOUNCE; l.w = -l.w; }
    const r = Math.hypot(l.x, l.y);
    l.s = sc(r) * (0.75 + 0.25 * l.op);
    tilt(l, l.vx, dt);
    if (r < REACH) { if (T - l.pushed < HERD) l.earned = true; grab(l); } // herded in
    else if (!l.imp && l.due && T > l.due && !ptr.in) send(l);
  }
  function carried(l, dt) { // a spiral in: toward the centre, bent less the nearer it gets
    l.op = Math.min(1, l.op + dt * l.fade);
    const r = Math.hypot(l.x, l.y);
    if (r < 2) return enter(l, 0);
    const b = l.bend * BEND * Math.min(1, r / 220), ux = -l.x / r, uy = -l.y / r;
    const dx = ux * Math.cos(b) - uy * Math.sin(b), dy = ux * Math.sin(b) + uy * Math.cos(b), d = Math.min(r, CARRY * dt);
    l.vx = dx * CARRY;
    l.x += dx * d; l.y += dy * d;
    l.s = sc(r);
    tilt(l, l.vx, dt);
  }
  function entering(l, dt) { // as the keyframes: shrink into the centre with a ring; then released into the pile
    const t = (l.t += dt);
    if (t < 0.6) { const k = t / 0.6; l.s = 0.62 - 0.28 * k; l.op = 1 - 0.15 * k; return; }
    if (l.el.parentNode !== gIn) gIn.append(l.el);
    const k = Math.min(1, (t - 0.6) / 1.4), e = k * (2 - k);
    l.x = l.px * e; l.y = l.py * e; l.rot = l.rot0 * e; l.op = 0.85 - 0.53 * k;
    l.s = 0.34 + (l.earned ? 0.16 * Math.sin(Math.PI * k) : 0); // an earned one unfolds and shrinks as it settles
    if (k === 1) { l.st = 'inside'; l.t = 0; }
  }
  function step(dt, now) {
    T += dt;
    if (T > nextSpawn) { nextSpawn = T + rnd(1.8, 3.4); if (letters.filter(inCyto).length < AMBIENT) spawn(); }
    const act = ptr.in && ptr.mouse && !drag ? clamp(1 - (now - ptr.t) / 200, 0, 1) : 0;
    for (const l of letters) {
      if (drag && drag.l === l) continue; // the pointer places it
      if (l.st === 'free') free(l, dt, act);
      else if (l.st === 'carry') carried(l, dt);
      else if (l.st === 'in') entering(l, dt);
      else if (l.egg) tidy(l);
      else if ((l.t += dt) < 20) l.op = 0.32 * (1 - l.t / 20); // inside, fading
      else l.gone = true;
    }
    for (const i of imps) {
      const l = i.L;
      if (i.st === 'walk') {
        i.op = Math.min(1, i.op + dt / 0.4);
        const dx = l.x - i.x, dy = l.y - i.y, d = Math.hypot(dx, dy), s = WALK * dt;
        if (d <= s) { i.x = l.x; i.y = l.y; carry(l, i); }
        else { i.vx = (dx / d) * WALK; i.x += (dx / d) * s; i.y += (dy / d) * s; }
        i.s = sc(Math.hypot(i.x, i.y));
        tilt(i, i.vx, dt);
      } else if (i.st === 'carry' && !(l.st === 'in' && l.t >= 0.6)) {
        Object.assign(i, { x: l.x, y: l.y, rot: l.rot, s: l.s, op: Math.min(l.st === 'in' ? l.op : 1, i.op + dt / 0.3) });
      } else if (i.st === 'carry') { // it goes in with the letter and is not seen again
        const k = Math.min(1, (l.t - 0.6) / 1);
        i.s = 0.34 - 0.29 * k; i.op = 0.85 * (1 - k);
        if (k === 1) i.gone = true;
      } else if ((i.op -= dt / 0.6) <= 0) i.gone = true; // leaving
    }
    for (const g of rings) { // the keyframes' release ring
      const t = (g.t += dt), top = g.big ? 1 : 0.9, out = g.big ? 1.3 : 1.1, k = t < 0.5 ? t / 0.5 : (t - 0.5) / out;
      if (t < 0.5) { g.s = 0.3 + 0.7 * k; g.op = top * k; } else { g.s = 1 + (g.big ? 2.2 : 1.6) * k; g.op = top * (1 - k); }
      if (g.rip) {
        const q = Math.min(1, t / 1.4);
        g.rip.setAttribute('r', (6 + 40 * q * (2 - q)).toFixed(1));
        setOp(g.rip, 0.7 * (1 - q));
      }
      if (t >= 0.5 + out) { g.gone = true; if (g.rip) g.rip.remove(); }
    }
    ringFx(dt);
    flashT = Math.max(0, flashT - dt);
    for (const a of [letters, imps, rings]) {
      for (let k = a.length - 1; k >= 0; k--) if (a[k].gone) { a[k].el.remove(); a.splice(k, 1); }
    }
  }
  const setOp = (e, v) => { const s = Math.max(0, v).toFixed(3); if (e._op !== s) e.setAttribute('opacity', (e._op = s)); };
  function ringFx(dt) { // the swirl's glow, the lit subunits, and the display at eight
    glowT += dt;
    let g = 0.5 * Math.min(1, glowT / 0.15) * Math.max(0, 1 - glowT / 0.9), t = -1;
    if (egg !== null) {
      t = egg += dt;
      if (t >= 0 && t - dt < 0) settle();
      if (t >= 0 && t < EGG) {
        spinA = (spinA + 900 * Math.sin((Math.PI * t) / EGG) * dt) % 360; // the swirl spins up and down
        g = Math.max(g, 0.85 * Math.min(1, t / 0.4, (EGG - t) / 0.6));
        while (cascade < 6 && t >= cascade * 0.3) { release(0, true); cascade++; }
      }
      if (t >= 0) far.forEach((f, j) => { // the far pores light one after another, clockwise
        const x = (t - 0.2 - j * 0.2) / 0.9, b = x > 0 && x < 1 ? Math.sin(Math.PI * x) : 0;
        setOp(f.ring, 0.7 * b); setOp(f.disc, 0.22 * b);
      });
      if (t >= EGG + 1.26) { egg = null; earned = 0; spinA = 0; }
    }
    const sa = spinA.toFixed(1);
    if (spin._a !== sa) spin.setAttribute('transform', `rotate(${(spin._a = sa)})`);
    setOp(glow, g);
    setOp(swirl, t >= 0 && t < EGG ? 1 - g : 1);
    for (let k = 0; k < 8; k++) {
      const a = T - litT[k];
      let o = k < earned ? Math.min(1, a / 0.25) * (0.2 + 0.3 * Math.exp(-a / 0.5)) : 0;
      if (t >= 0 && t < EGG) { // pulses run round the ring
        const p = (((t * 1.25 - k / 8) % 1) + 1) % 1;
        o = 0.2 + 0.55 * Math.max(0, 1 - p / 0.3) * Math.min(1, (EGG - t) / 0.5);
      } else if (t >= EGG) o = 0.2 * clamp(1 - (t - EGG - k * 0.12) / 0.3, 0, 1); // and go out one by one
      setOp(lit[k], o);
    }
  }
  function settle() { // the pile inside lines up in a ring round the centre, and fades with the display
    const pile = letters.filter((l) => l.st === 'inside' && !(drag && drag.l === l));
    pile.forEach((l, k) => {
      const a = (2 * Math.PI * k) / pile.length - Math.PI / 2, tr = a * DEG + 90;
      l.egg = { x: l.x, y: l.y, rot: l.rot, op: l.op, tx: 30 * Math.cos(a), ty: 30 * Math.sin(a), dr: ((tr - l.rot + 540) % 360) - 180 };
    });
    cascade = 0;
  }
  function tidy(l) {
    const e = l.egg, k = clamp((egg - 0.2) / 1, 0, 1), q = k * (2 - k);
    l.x = e.x + (e.tx - e.x) * q; l.y = e.y + (e.ty - e.y) * q; l.rot = e.rot + e.dr * q;
    l.op = (e.op + (0.5 - e.op) * q) * clamp((EGG - egg) / 0.8, 0, 1);
    if (egg >= EGG) l.gone = true;
  }
  const put = (o) => {
    o.el.setAttribute('transform', `translate(${o.x.toFixed(1)} ${o.y.toFixed(1)}) rotate(${o.rot.toFixed(1)}) scale(${o.s.toFixed(3)})`);
    o.el.setAttribute('opacity', Math.max(0, o.op).toFixed(3));
  };
  function draw() {
    letters.forEach(put); imps.forEach(put); rings.forEach(put);
    flash.setAttribute('opacity', (0.8 * Math.sin((Math.PI * flashT) / 0.7)).toFixed(3));
  }
  function frame(now) {
    raf = 0;
    if (!inView || paused) return;
    const dt = clamp((now - last) / 1000, 0, 0.05);
    last = now;
    step(dt, now);
    draw();
    if (letters.length || imps.length || rings.length || ptr.in || drag) raf = requestAnimationFrame(frame);
  }
  function kick() { if (!raf && inView && !paused) { last = performance.now(); raf = requestAnimationFrame(frame); } }

  // take over from the keyframes where they are: each letter and its importin, mid-way or going in
  for (const c of svg.querySelectorAll('.cargo')) {
    const cs = getComputedStyle(c), m = new DOMMatrix(cs.transform === 'none' ? '' : cs.transform);
    const a = c.getAnimations ? c.getAnimations()[0] : null, p = (a && a.effect.getComputedTiming().progress) || 0;
    const l = letter(m.e, m.f, +cs.opacity, 1 / 0.6), i = importin(m.e, m.f, l.op);
    l.rot = i.rot = Math.atan2(m.b, m.a) * DEG;
    l.s = i.s = Math.hypot(m.a, m.b);
    carry(l, i);
    if (p >= 0.84) enter(l, (p - 0.84) * 10);
  }
  plate.classList.add('play');
  draw();
  svg.setAttribute('aria-label', svg.getAttribute('aria-label') + ' Clicking or tapping around the pore puts a ' +
    'letter there for an importin to carry in; a letter can be dragged to the pore, and one already inside stops ' +
    'at the pore when dragged outward and stays in.');

  // the pointer; sizes are read once and on resize, positions come as page coordinates
  function measure() {
    const b = svg.getBoundingClientRect();
    S = Math.max(b.width / 760, b.height / 280); W = b.width / S / 2; H = b.height / S / 2;
    OX = b.left + scrollX + b.width / 2; OY = b.top + scrollY + b.height / 2;
  }
  measure();
  addEventListener('resize', measure);
  addEventListener('load', measure);
  if (document.fonts) document.fonts.ready.then(measure);
  const at = (e) => ({ x: (e.pageX - OX) / S, y: (e.pageY - OY) / S });
  function hit(p, wide) {
    let best = null, bd = 1e9;
    for (const l of letters) {
      if (!(inCyto(l) || l.st === 'inside') || l.op < 0.08) continue;
      const d = Math.hypot(l.x - p.x, l.y - p.y);
      if (d < (wide ? 30 : 20) * l.s + 4 && d < bd) { best = l; bd = d; }
    }
    return best;
  }
  const setCursor = (c) => { if (c !== cursor) svg.style.cursor = cursor = c; };
  const hover = (p) => setCursor(hit(p) ? 'grab' : Math.hypot(p.x, p.y) > RIM ? 'crosshair' : 'default');
  function track(e, p) {
    const dt = (e.timeStamp - ptr.t) / 1000;
    if (dt > 0 && dt < 0.2) {
      ptr.vx += ((p.x - ptr.x) / Math.max(dt, 0.008) - ptr.vx) * 0.5;
      ptr.vy += ((p.y - ptr.y) / Math.max(dt, 0.008) - ptr.vy) * 0.5;
    } else if (dt >= 0.2) ptr.vx = ptr.vy = 0;
    Object.assign(ptr, { x: p.x, y: p.y, t: e.timeStamp, in: true, mouse: e.pointerType !== 'touch' });
  }
  function hold(p) {
    const l = drag.l;
    let x = p.x + drag.ox, y = p.y + drag.oy;
    if (l.st === 'inside') { // the pull test: from inside, the pore is a wall
      const r = Math.hypot(x, y);
      if (r > WALL) { x *= WALL / r; y *= WALL / r; if (!drag.wall) flashT = 0.7; drag.wall = true; } else drag.wall = false;
    } else { x = clamp(x, -W + EDGE, W - EDGE); y = clamp(y, -H + EDGE, H - EDGE); l.s = sc(Math.hypot(x, y)); }
    Object.assign(l, { x, y, vx: ptr.vx, vy: ptr.vy });
  }
  svg.addEventListener('pointerdown', (e) => {
    if (paused || e.button !== 0) return;
    const p = at(e), l = hit(p, e.pointerType !== 'mouse');
    track(e, p);
    if (l) {
      drag = { l, id: e.pointerId, ox: l.x - p.x, oy: l.y - p.y, wall: false };
      if (l.st !== 'inside') letGo(l);
      svg.setPointerCapture(e.pointerId);
      setCursor('grabbing');
      e.preventDefault();
    } else down = { x: e.pageX, y: e.pageY, t: e.timeStamp, p };
    kick();
  });
  svg.addEventListener('pointermove', (e) => {
    if (paused) return;
    const p = at(e);
    track(e, p);
    if (drag && e.pointerId === drag.id) hold(p); else if (e.pointerType !== 'touch') hover(p);
    kick();
  });
  function up(e, cancel) {
    if (drag && e.pointerId === drag.id) {
      const l = drag.l;
      drag = null;
      // released in the cytoplasm it is free again; within reach of the pore an importin takes it at once
      if (l.st !== 'inside') Object.assign(l, { calm: T + 0.6, heat: 0, due: T + rnd(2.5, 6) });
      if (l.st !== 'inside' && Math.hypot(l.x, l.y) < REACH) l.earned = true; // dragged to the pore
      if (e.pointerType !== 'touch') hover(at(e));
    } else if (down && !cancel && Math.hypot(e.pageX - down.x, e.pageY - down.y) < 10 && e.timeStamp - down.t < 600) {
      place(down.p);
    }
    down = null;
    kick();
  }
  svg.addEventListener('pointerup', (e) => up(e, false));
  svg.addEventListener('pointercancel', (e) => up(e, true));
  svg.addEventListener('pointerleave', () => { if (!drag) ptr.in = false; });
  // a touch that starts on a letter drags it instead of scrolling the page
  svg.addEventListener('touchstart', (e) => {
    if (!paused && e.touches.length === 1 && hit(at(e.touches[0]), true)) e.preventDefault();
  }, { passive: false });
  function pause() { // the pause box, or reduced motion switched on during the visit
    paused = box.checked || still.matches;
    if (paused) { drag = down = null; setCursor('default'); } else { setCursor(''); kick(); skyKick(); }
  }
  box.addEventListener('change', pause);
  if (still.addEventListener) still.addEventListener('change', pause);

  // S3: while the drawing is out of view, now and then a faint letter comes in from a side of the window, drifts in
  // and up, and an importin carries it up to the mark in the header, into its centre; at most two at a time.
  // Window pixels; the layer sits in the header under the mark. Idle arrivals: they light nothing.
  const top = document.querySelector('.top'), mark = top && top.querySelector('.brand svg');
  // big and slow: each letter is SKY_SCALE times the drawing's symbol and takes about 10 s from the edge to the mark
  const SKY_SCALE = 1.6, SKY_SLOW = 2;
  const sky = make('svg', { class: 'p-sky', 'aria-hidden': 'true' }), fly = [];
  let skyOn = false, skyTimer = 0, skyRaf = 0, skyLast = 0, VW = 1200, VH = 900, MX = 0, MY = 0;
  if (mark) top.append(sky);
  function skyMeasure() {
    VW = document.documentElement.clientWidth; VH = document.documentElement.clientHeight;
    const m = mark ? mark.getBoundingClientRect() : { left: 0, top: 0, width: 0, height: 0 };
    MX = m.left + m.width / 2; MY = m.top + m.height / 2; // the header is at the top whenever the drawing is away
  }
  skyMeasure();
  addEventListener('resize', skyMeasure);
  function skySet(on) {
    if (on === skyOn) return;
    skyOn = on;
    clearTimeout(skyTimer);
    if (on) skyTimer = setTimeout(skySpawn, rnd(2000, 5000));
    else for (const f of fly) f.out = true; // the drawing is back: those on their way fade
    skyKick();
  }
  function skySpawn() {
    skyTimer = setTimeout(skySpawn, rnd(2000, 5000));
    if (!skyOn || !mark || paused || document.hidden || fly.length >= 5) return;
    const k = clamp(VW / 1200, 0.7, 1), dir = Math.random() < 0.5 ? 1 : -1;
    fly.push({ t: 0, k, dir, x0: dir > 0 ? -70 * k : VW + 70 * k, y0: rnd(VH / 3, VH - 40 * k), o: rnd(0.3, 0.4), fo: 1,
      out: false, cup: make('use', { href: '#p-imv', opacity: 0 }, sky), el: make('use', { href: '#p-let', opacity: 0 }, sky) });
    skyKick();
  }
  function skyKick() { if (!skyRaf && fly.length && !paused) { skyLast = performance.now(); skyRaf = requestAnimationFrame(skyFrame); } }
  function skyFrame(now) {
    skyRaf = 0;
    if (paused) return;
    const dt = clamp((now - skyLast) / 1000, 0, 0.05);
    skyLast = now;
    for (const f of fly) {
      const t = (f.t += dt / SKY_SLOW), d = Math.min(1, t / 1.1), de = d * (2 - d); // t runs at 1/SKY_SLOW of real time
      const px = f.x0 + f.dir * 110 * f.k * de, py = f.y0 - 24 * f.k * de; // in from the edge, a little up
      let x = px, y = py, s = SKY_SCALE * f.k, op = f.o * Math.min(1, t / 0.4), cup = f.o * clamp((t - 1.1) / 0.4, 0, 1);
      if (t > 1.5) { // taken: a gentle curve up to the mark, smaller as it goes, then into the centre
        const e = Math.min(1, (t - 1.5) / 3), q = e * e * (3 - 2 * e), cx = px + (MX - px) * 0.55, cy = py + (MY - py) * 0.12;
        x = (1 - q) ** 2 * px + 2 * (1 - q) * q * cx + q * q * MX;
        y = (1 - q) ** 2 * py + 2 * (1 - q) * q * cy + q * q * MY;
        const z = clamp((t - 4.1) / 0.4, 0, 1);
        s *= (1 - 0.55 * q) * (1 - z); op *= 1 - z; cup *= 1 - z;
      }
      if (t > 4.3 && !f.rip) f.rip = make('circle', { class: 'p-flash', cx: MX, cy: MY, r: 4, opacity: 0 }, sky);
      if (f.rip) { const q = Math.min(1, (t - 4.3) / 0.8); f.rip.setAttribute('r', (4 + 14 * q).toFixed(1)); setOp(f.rip, 0.45 * (1 - q) * f.fo); }
      if (f.out) f.fo -= dt / 0.4;
      const tr = `translate(${x.toFixed(1)} ${y.toFixed(1)}) scale(${s.toFixed(3)})`;
      f.el.setAttribute('transform', tr); f.cup.setAttribute('transform', tr);
      setOp(f.el, op * f.fo); setOp(f.cup, cup * f.fo);
      if (t >= 5.1 || f.fo <= 0) { f.gone = true; f.el.remove(); f.cup.remove(); if (f.rip) f.rip.remove(); }
    }
    for (let k = fly.length - 1; k >= 0; k--) if (fly[k].gone) fly.splice(k, 1);
    if (fly.length) skyRaf = requestAnimationFrame(skyFrame);
  }

  new IntersectionObserver(([e]) => {
    inView = e.isIntersecting;
    skySet(!inView && e.boundingClientRect.top < 0);
    kick();
  }).observe(svg);
})();
