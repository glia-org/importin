/* Importin's website: the one script (I3 + chase, S3). The page is complete without it: the pore's CSS keyframes run
   and the pause box pauses them. Under prefers-reduced-motion this script does nothing at all.
   With it, the drawing's letters become one system. Free letters drift in the cytoplasm and move away from a moving
   pointer; a click or tap there places one; any letter can be dragged. An importin takes a letter in through the
   pore: it comes in from the cytoplasm for a placed letter, and takes any letter that gets within reach of the pore.
   Inside, letters pile faintly and fade; one dragged toward the membrane stops at the pore. Nothing comes out.
   The header's mark carries a tiny letter in at the pace of scrolling once the drawing is out of view.
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

  const letters = [], imps = [], rings = [];
  const ptr = { in: false, mouse: false, x: 0, y: 0, vx: 0, vy: 0, t: -1e9 };
  let T = 0, nextSpawn = 1.5, raf = 0, last = 0, inView = true, paused = box.checked, flashT = 0;
  let W = 380, H = 140, S = 1, OX = 0, OY = 0; // visible half-extents, px per unit, the centre in page px
  let drag = null, down = null, cursor = '';

  function letter(x, y, op, fade) {
    const l = { x, y, vx: 0, vy: 0, rot: 0, s: sc(Math.hypot(x, y)), op, fade, st: 'free', t: 0, imp: null,
      heat: 0, w: rnd(0, 2 * Math.PI), calm: 0, due: 0, bend: Math.random() < 0.5 ? -1 : 1,
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
    rings.push({ x: 0, y: 0, rot: 0, s: 0.3, op: 0, t, el: make('circle', { class: 'p-tag', r: 5, opacity: 0 }, gRel) });
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
    if (r < REACH) grab(l);
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
    l.x = l.px * e; l.y = l.py * e; l.rot = l.rot0 * e; l.s = 0.34; l.op = 0.85 - 0.53 * k;
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
      g.t += dt;
      const k = g.t < 0.5 ? g.t / 0.5 : (g.t - 0.5) / 1.1;
      if (g.t < 0.5) { g.s = 0.3 + 0.7 * k; g.op = 0.9 * k; } else { g.s = 1 + 1.6 * k; g.op = 0.9 * (1 - k); }
      if (g.t >= 1.6) g.gone = true;
    }
    flashT = Math.max(0, flashT - dt);
    for (const a of [letters, imps, rings]) {
      for (let k = a.length - 1; k >= 0; k--) if (a[k].gone) { a[k].el.remove(); a.splice(k, 1); }
    }
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
    if (paused) { drag = down = null; setCursor('default'); } else { setCursor(''); kick(); }
  }
  box.addEventListener('change', pause);
  if (still.addEventListener) still.addEventListener('change', pause);

  // S3: the header's mark takes a tiny letter in, driven by how far the page scrolls, while the drawing is away
  const mg = document.querySelector('.top .brand svg g');
  const mk = mg && make('use', { href: '#p-env', opacity: 0 }, mg);
  let mp = 0, mOn = false, mraf = 0, lastY = scrollY;
  function markDraw() {
    mraf = 0;
    if (!mk) return;
    const k = mp % 1, a = (22.5 + 135 * Math.floor(mp)) / DEG, r = 16 * (1 - k);
    mk.setAttribute('transform', `translate(${(Math.sin(a) * r).toFixed(2)} ${(-Math.cos(a) * r).toFixed(2)}) scale(${(0.24 * (1 - 0.75 * k)).toFixed(3)})`);
    mk.setAttribute('opacity', mOn ? clamp(Math.min(k / 0.1, (1 - k) / 0.3), 0, 1).toFixed(2) : 0);
  }
  function markOn(on) { if (on !== mOn) { mOn = on; if (!on) mp = Math.ceil(mp); markDraw(); } }
  addEventListener('scroll', () => {
    const y = scrollY;
    if (mOn && !paused) { mp += Math.abs(y - lastY) / 600; if (!mraf) mraf = requestAnimationFrame(markDraw); }
    lastY = y;
  }, { passive: true });

  new IntersectionObserver(([e]) => {
    inView = e.isIntersecting;
    markOn(!inView && e.boundingClientRect.top < 0);
    kick();
  }).observe(svg);
})();
