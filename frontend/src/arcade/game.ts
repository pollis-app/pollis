// Top-down bullet-hell-lite easter egg. Pure vanilla — no React, no
// framework. Mounts on a <canvas>, owns its own rAF loop, listeners, and
// state. `start(canvas)` returns a handle whose `stop()` removes every
// listener and cancels the loop; unmount the host element to reset.
//
// Conventions:
//  - Logical world space is fixed at WORLD_W x WORLD_H. The canvas is
//    sized in CSS px (via ResizeObserver), the backing store is sized in
//    devicePixelRatio px, and the world is fit-letterboxed inside.
//  - World wraps toroidally — anything that crosses an edge re-enters
//    from the opposite side. Collision pairs only test one wrap copy
//    (closest-image distance) which is exact for radii << world size.
//  - Fixed-timestep simulation at 60 Hz with an accumulator; render runs
//    every rAF. Spiral-of-death guarded by max-frame clamp.

const WORLD_W = 1280;
const WORLD_H = 720;
const TICK_HZ = 60;
const TICK_DT = 1 / TICK_HZ;
const MAX_FRAME_DT = 0.1;

const PLAYER_RADIUS = 16;
const PLAYER_MAX_SPEED = 320;
const PLAYER_ACCEL = 1200;
const PLAYER_DRAG = 1.4;
const PLAYER_FIRE_COOLDOWN = 0.16;
const PLAYER_INVULN_TIME = 1.6;

const BULLET_SPEED = 720;
const BULLET_LIFE = 1.1;
const BULLET_RADIUS = 4;

const ENEMY_BULLET_SPEED = 280;
const ENEMY_BULLET_LIFE = 2.6;
const ENEMY_FIRE_COOLDOWN = 1.4;
const ENEMY_RADIUS = 19;
const ENEMY_MAX_SPEED = 110;

const ASTEROID_LARGE = 54;
const ASTEROID_MED = 33;
const ASTEROID_SMALL = 19;

const STAR_COUNT = 70;

type Vec = { x: number; y: number };

interface Player {
  pos: Vec;
  vel: Vec;
  aim: number;
  fireCd: number;
  invuln: number;
  alive: boolean;
}

interface Bullet {
  pos: Vec;
  vel: Vec;
  life: number;
  alive: boolean;
  hostile: boolean;
}

interface Asteroid {
  pos: Vec;
  vel: Vec;
  radius: number;
  rot: number;
  spin: number;
  shape: number[];
  alive: boolean;
}

interface Enemy {
  pos: Vec;
  vel: Vec;
  rot: number;
  fireCd: number;
  alive: boolean;
}

interface Star {
  x: number;
  y: number;
  brightness: number;
}

interface GameState {
  player: Player;
  bullets: Bullet[];
  asteroids: Asteroid[];
  enemies: Enemy[];
  stars: Star[];
  score: number;
  startedAt: number;
  elapsedMs: number;
  gameOver: boolean;
  finalElapsedMs: number;
  enemySpawnCd: number;
  asteroidSpawnCd: number;
}

export interface GameHandle {
  stop: () => void;
}

export function startGame(canvas: HTMLCanvasElement): GameHandle {
  const ctx = canvas.getContext("2d", { alpha: false });
  if (!ctx) {
    return { stop: () => {} };
  }

  const accentColor = readAccent();

  // ─── DPR + letterbox transform ───────────────────────────────────────────
  let cssW = 0;
  let cssH = 0;
  let dpr = window.devicePixelRatio || 1;
  let viewScale = 1;
  let viewOffsetX = 0;
  let viewOffsetY = 0;

  const resize = () => {
    const rect = canvas.getBoundingClientRect();
    cssW = Math.max(1, Math.floor(rect.width));
    cssH = Math.max(1, Math.floor(rect.height));
    dpr = window.devicePixelRatio || 1;
    canvas.width = Math.floor(cssW * dpr);
    canvas.height = Math.floor(cssH * dpr);
    const sx = cssW / WORLD_W;
    const sy = cssH / WORLD_H;
    viewScale = Math.min(sx, sy);
    viewOffsetX = (cssW - WORLD_W * viewScale) / 2;
    viewOffsetY = (cssH - WORLD_H * viewScale) / 2;
  };
  resize();
  const ro = new ResizeObserver(resize);
  ro.observe(canvas);

  // ─── Input ───────────────────────────────────────────────────────────────
  const keys = new Set<string>();
  const mouse = { x: WORLD_W / 2, y: WORLD_H / 2, down: false };

  const isTypingTarget = () => {
    const a = document.activeElement;
    if (!a) {
      return false;
    }
    const tag = a.tagName;
    return (
      tag === "INPUT" ||
      tag === "TEXTAREA" ||
      (a as HTMLElement).isContentEditable
    );
  };

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      return;
    }
    if (isTypingTarget()) {
      return;
    }
    const k = e.key.toLowerCase();
    keys.add(k);
    if (k === "r" && state.gameOver) {
      reset();
    }
    if (k === " " || k === "spacebar") {
      e.preventDefault();
    }
  };
  const onKeyUp = (e: KeyboardEvent) => {
    keys.delete(e.key.toLowerCase());
  };
  const onMouseMove = (e: MouseEvent) => {
    const rect = canvas.getBoundingClientRect();
    const px = e.clientX - rect.left;
    const py = e.clientY - rect.top;
    mouse.x = (px - viewOffsetX) / viewScale;
    mouse.y = (py - viewOffsetY) / viewScale;
  };
  const onMouseDown = () => {
    mouse.down = true;
  };
  const onMouseUp = () => {
    mouse.down = false;
  };

  window.addEventListener("keydown", onKeyDown);
  window.addEventListener("keyup", onKeyUp);
  canvas.addEventListener("mousemove", onMouseMove);
  canvas.addEventListener("mousedown", onMouseDown);
  window.addEventListener("mouseup", onMouseUp);

  // ─── State + reset ───────────────────────────────────────────────────────
  const state: GameState = {
    player: makePlayer(),
    bullets: [],
    asteroids: [],
    enemies: [],
    stars: makeStars(),
    score: 0,
    startedAt: 0,
    elapsedMs: 0,
    gameOver: false,
    finalElapsedMs: 0,
    enemySpawnCd: 6,
    asteroidSpawnCd: 0,
  };

  const reset = () => {
    state.player = makePlayer();
    state.bullets.length = 0;
    state.asteroids.length = 0;
    state.enemies.length = 0;
    for (let i = 0; i < 4; i++) {
      state.asteroids.push(spawnAsteroidAtEdge(ASTEROID_LARGE));
    }
    state.score = 0;
    state.startedAt = performance.now();
    state.elapsedMs = 0;
    state.gameOver = false;
    state.finalElapsedMs = 0;
    state.enemySpawnCd = 6;
    state.asteroidSpawnCd = 0;
  };
  reset();

  // ─── Loop ────────────────────────────────────────────────────────────────
  let rafId = 0;
  let lastFrame = performance.now();
  let accumulator = 0;

  const frame = (now: number) => {
    const frameDt = Math.min(MAX_FRAME_DT, (now - lastFrame) / 1000);
    lastFrame = now;
    accumulator += frameDt;
    while (accumulator >= TICK_DT) {
      tick(state, TICK_DT);
      accumulator -= TICK_DT;
    }
    if (!state.gameOver) {
      state.elapsedMs = now - state.startedAt;
    }
    render();
    rafId = requestAnimationFrame(frame);
  };
  rafId = requestAnimationFrame(frame);

  // ─── Simulation ──────────────────────────────────────────────────────────
  function tick(s: GameState, dt: number) {
    if (s.gameOver) {
      // Asteroids and enemies keep drifting on the game-over screen so it
      // stays alive-looking, but no spawns and no player input.
      for (const a of s.asteroids) {
        moveWrap(a.pos, a.vel, dt);
        a.rot += a.spin * dt;
      }
      for (const e of s.enemies) {
        moveWrap(e.pos, e.vel, dt);
      }
      return;
    }

    // ─── Player ─────────────────────────────────────────────────────────
    const p = s.player;
    let ax = 0;
    let ay = 0;
    if (keys.has("w") || keys.has("arrowup")) {
      ay -= 1;
    }
    if (keys.has("s") || keys.has("arrowdown")) {
      ay += 1;
    }
    if (keys.has("a") || keys.has("arrowleft")) {
      ax -= 1;
    }
    if (keys.has("d") || keys.has("arrowright")) {
      ax += 1;
    }
    const amag = Math.hypot(ax, ay);
    if (amag > 0) {
      ax = (ax / amag) * PLAYER_ACCEL;
      ay = (ay / amag) * PLAYER_ACCEL;
    }
    p.vel.x += ax * dt;
    p.vel.y += ay * dt;
    // Velocity drag (frame-rate independent exp decay)
    const drag = Math.exp(-PLAYER_DRAG * dt);
    p.vel.x *= drag;
    p.vel.y *= drag;
    // Speed cap
    const vmag = Math.hypot(p.vel.x, p.vel.y);
    if (vmag > PLAYER_MAX_SPEED) {
      p.vel.x = (p.vel.x / vmag) * PLAYER_MAX_SPEED;
      p.vel.y = (p.vel.y / vmag) * PLAYER_MAX_SPEED;
    }
    moveWrap(p.pos, p.vel, dt);
    p.aim = Math.atan2(mouse.y - p.pos.y, mouse.x - p.pos.x);
    p.fireCd = Math.max(0, p.fireCd - dt);
    p.invuln = Math.max(0, p.invuln - dt);

    const wantFire = mouse.down || keys.has(" ") || keys.has("enter");
    if (wantFire && p.fireCd === 0) {
      spawnBullet(s, p.pos, p.aim, false);
      p.fireCd = PLAYER_FIRE_COOLDOWN;
    }

    // ─── Bullets ────────────────────────────────────────────────────────
    for (const b of s.bullets) {
      if (!b.alive) {
        continue;
      }
      moveWrap(b.pos, b.vel, dt);
      b.life -= dt;
      if (b.life <= 0) {
        b.alive = false;
      }
    }

    // ─── Asteroids ──────────────────────────────────────────────────────
    for (const a of s.asteroids) {
      if (!a.alive) {
        continue;
      }
      moveWrap(a.pos, a.vel, dt);
      a.rot += a.spin * dt;
    }

    // ─── Enemies ────────────────────────────────────────────────────────
    for (const e of s.enemies) {
      if (!e.alive) {
        continue;
      }
      const toPx = closestImageDelta(p.pos.x - e.pos.x, WORLD_W);
      const toPy = closestImageDelta(p.pos.y - e.pos.y, WORLD_H);
      const dist = Math.hypot(toPx, toPy) || 1;
      // Light steering toward player, capped speed
      e.vel.x += (toPx / dist) * 60 * dt;
      e.vel.y += (toPy / dist) * 60 * dt;
      const evm = Math.hypot(e.vel.x, e.vel.y);
      if (evm > ENEMY_MAX_SPEED) {
        e.vel.x = (e.vel.x / evm) * ENEMY_MAX_SPEED;
        e.vel.y = (e.vel.y / evm) * ENEMY_MAX_SPEED;
      }
      moveWrap(e.pos, e.vel, dt);
      e.rot = Math.atan2(toPy, toPx);
      e.fireCd -= dt;
      if (e.fireCd <= 0 && dist < 520) {
        spawnBullet(s, e.pos, e.rot, true);
        e.fireCd = ENEMY_FIRE_COOLDOWN + Math.random() * 0.6;
      }
    }

    // ─── Spawns (difficulty ramps with elapsed time) ────────────────────
    const tSec = s.elapsedMs / 1000;
    s.asteroidSpawnCd -= dt;
    if (s.asteroids.length < 5 + Math.floor(tSec / 15) && s.asteroidSpawnCd <= 0) {
      s.asteroids.push(spawnAsteroidAtEdge(ASTEROID_LARGE));
      s.asteroidSpawnCd = 4 - Math.min(2.5, tSec / 30);
    }
    s.enemySpawnCd -= dt;
    if (s.enemySpawnCd <= 0 && s.enemies.length < 1 + Math.floor(tSec / 25)) {
      s.enemies.push(spawnEnemyAtEdge());
      s.enemySpawnCd = 10 - Math.min(6, tSec / 20);
    }

    // ─── Collisions ─────────────────────────────────────────────────────
    // Bullet × asteroid + bullet × enemy + bullet × player (hostile bullets)
    for (const b of s.bullets) {
      if (!b.alive) {
        continue;
      }
      if (b.hostile) {
        if (p.invuln === 0 && circleHit(b.pos, BULLET_RADIUS, p.pos, PLAYER_RADIUS)) {
          b.alive = false;
          killPlayer(s);
          return;
        }
        continue;
      }
      for (const a of s.asteroids) {
        if (!a.alive) {
          continue;
        }
        if (circleHit(b.pos, BULLET_RADIUS, a.pos, a.radius)) {
          b.alive = false;
          shatterAsteroid(s, a);
          break;
        }
      }
      if (!b.alive) {
        continue;
      }
      for (const e of s.enemies) {
        if (!e.alive) {
          continue;
        }
        if (circleHit(b.pos, BULLET_RADIUS, e.pos, ENEMY_RADIUS)) {
          b.alive = false;
          e.alive = false;
          s.score += 150;
          break;
        }
      }
    }
    // Player × asteroid + player × enemy
    if (p.invuln === 0) {
      for (const a of s.asteroids) {
        if (!a.alive) {
          continue;
        }
        if (circleHit(p.pos, PLAYER_RADIUS, a.pos, a.radius)) {
          killPlayer(s);
          return;
        }
      }
      for (const e of s.enemies) {
        if (!e.alive) {
          continue;
        }
        if (circleHit(p.pos, PLAYER_RADIUS, e.pos, ENEMY_RADIUS)) {
          killPlayer(s);
          return;
        }
      }
    }

    // ─── Compact dead entities periodically (every few seconds) ─────────
    // Cheap inline filter — these arrays stay bounded.
    if ((s.elapsedMs | 0) % 500 < 17) {
      compact(s.bullets);
      compact(s.asteroids);
      compact(s.enemies);
    }
  }

  // ─── Render ──────────────────────────────────────────────────────────────
  function render() {
    if (!ctx) {
      return;
    }
    const ctxR = ctx;
    ctxR.save();
    ctxR.setTransform(1, 0, 0, 1, 0, 0);
    ctxR.fillStyle = "#000";
    ctxR.fillRect(0, 0, canvas.width, canvas.height);
    // Map world → canvas (with DPR scaling baked in)
    ctxR.setTransform(
      viewScale * dpr,
      0,
      0,
      viewScale * dpr,
      viewOffsetX * dpr,
      viewOffsetY * dpr,
    );

    // Arena background panel
    ctxR.fillStyle = "#02060a";
    ctxR.fillRect(0, 0, WORLD_W, WORLD_H);

    // Starfield
    ctxR.fillStyle = accentColor;
    for (const s of state.stars) {
      ctxR.globalAlpha = s.brightness;
      ctxR.fillRect(s.x, s.y, 1.2, 1.2);
    }
    ctxR.globalAlpha = 1;

    // Arena border
    ctxR.strokeStyle = accentColor;
    ctxR.lineWidth = 1.5;
    ctxR.globalAlpha = 0.35;
    ctxR.strokeRect(0.5, 0.5, WORLD_W - 1, WORLD_H - 1);
    ctxR.globalAlpha = 1;

    drawWorld(ctxR);

    ctxR.restore();

    // HUD in canvas pixel space
    drawHud(ctxR);
  }

  function drawWorld(c: CanvasRenderingContext2D) {
    c.lineWidth = 2.2;
    c.strokeStyle = accentColor;
    c.fillStyle = accentColor;

    // Asteroids
    for (const a of state.asteroids) {
      if (!a.alive) {
        continue;
      }
      drawAtWrapped(c, a.pos, a.radius, () => drawAsteroid(c, a));
    }
    // Bullets
    for (const b of state.bullets) {
      if (!b.alive) {
        continue;
      }
      c.globalAlpha = b.hostile ? 0.85 : 1;
      c.beginPath();
      c.arc(b.pos.x, b.pos.y, BULLET_RADIUS, 0, Math.PI * 2);
      if (b.hostile) {
        c.stroke();
      } else {
        c.fill();
      }
    }
    c.globalAlpha = 1;

    // Enemies
    for (const e of state.enemies) {
      if (!e.alive) {
        continue;
      }
      drawAtWrapped(c, e.pos, ENEMY_RADIUS, () => drawEnemy(c, e));
    }

    // Player (blink while invulnerable)
    const p = state.player;
    if (p.alive) {
      const blink = p.invuln > 0 && Math.floor(performance.now() / 80) % 2 === 0;
      if (!blink) {
        drawAtWrapped(c, p.pos, PLAYER_RADIUS, () => drawPlayer(c, p));
      }
    }
  }

  function drawAsteroid(c: CanvasRenderingContext2D, a: Asteroid) {
    c.save();
    c.translate(a.pos.x, a.pos.y);
    c.rotate(a.rot);
    c.beginPath();
    const n = a.shape.length;
    for (let i = 0; i < n; i++) {
      const angle = (i / n) * Math.PI * 2;
      const r = a.radius * a.shape[i];
      const x = Math.cos(angle) * r;
      const y = Math.sin(angle) * r;
      if (i === 0) {
        c.moveTo(x, y);
      } else {
        c.lineTo(x, y);
      }
    }
    c.closePath();
    c.stroke();
    c.restore();
  }

  function drawEnemy(c: CanvasRenderingContext2D, e: Enemy) {
    c.save();
    c.translate(e.pos.x, e.pos.y);
    c.rotate(e.rot);
    // Inverted silhouette: solid accent fill so enemies pop against the
    // outline-only asteroids/player. Inner detail is a dark cutout.
    c.beginPath();
    c.moveTo(ENEMY_RADIUS, 0);
    c.lineTo(0, ENEMY_RADIUS * 0.85);
    c.lineTo(-ENEMY_RADIUS * 0.7, 0);
    c.lineTo(0, -ENEMY_RADIUS * 0.85);
    c.closePath();
    c.fill();
    c.strokeStyle = "#02060a";
    c.lineWidth = 2.4;
    c.beginPath();
    c.moveTo(-ENEMY_RADIUS * 0.35, 0);
    c.lineTo(ENEMY_RADIUS * 0.55, 0);
    c.stroke();
    c.beginPath();
    c.arc(ENEMY_RADIUS * 0.15, 0, ENEMY_RADIUS * 0.18, 0, Math.PI * 2);
    c.stroke();
    // Restore outer stroke for subsequent draws
    c.strokeStyle = accentColor;
    c.lineWidth = 2.2;
    c.restore();
  }

  function drawPlayer(c: CanvasRenderingContext2D, p: Player) {
    c.save();
    c.translate(p.pos.x, p.pos.y);
    c.rotate(p.aim);
    c.beginPath();
    c.moveTo(PLAYER_RADIUS, 0);
    c.lineTo(-PLAYER_RADIUS * 0.85, PLAYER_RADIUS * 0.75);
    c.lineTo(-PLAYER_RADIUS * 0.45, 0);
    c.lineTo(-PLAYER_RADIUS * 0.85, -PLAYER_RADIUS * 0.75);
    c.closePath();
    c.stroke();
    // Thrust flicker when accelerating
    if (
      keys.has("w") ||
      keys.has("a") ||
      keys.has("s") ||
      keys.has("d") ||
      keys.has("arrowup") ||
      keys.has("arrowdown") ||
      keys.has("arrowleft") ||
      keys.has("arrowright")
    ) {
      const flick = 0.6 + Math.random() * 0.6;
      c.beginPath();
      c.moveTo(-PLAYER_RADIUS * 0.85, PLAYER_RADIUS * 0.45);
      c.lineTo(-PLAYER_RADIUS * (1 + flick * 0.5), 0);
      c.lineTo(-PLAYER_RADIUS * 0.85, -PLAYER_RADIUS * 0.45);
      c.stroke();
    }
    c.restore();
  }

  // Draw an entity at up to 4 wrapped offsets so wrap-around looks seamless
  // near edges. Skips the offset copies if not near the relevant edge.
  function drawAtWrapped(
    c: CanvasRenderingContext2D,
    pos: Vec,
    r: number,
    draw: () => void,
  ) {
    draw();
    const offsets: Array<[number, number]> = [];
    if (pos.x < r) {
      offsets.push([WORLD_W, 0]);
    } else if (pos.x > WORLD_W - r) {
      offsets.push([-WORLD_W, 0]);
    }
    if (pos.y < r) {
      offsets.push([0, WORLD_H]);
    } else if (pos.y > WORLD_H - r) {
      offsets.push([0, -WORLD_H]);
    }
    if (offsets.length === 2) {
      offsets.push([offsets[0][0], offsets[1][1]]);
    }
    for (const [ox, oy] of offsets) {
      c.save();
      c.translate(ox, oy);
      draw();
      c.restore();
    }
  }

  function drawHud(c: CanvasRenderingContext2D) {
    c.save();
    c.setTransform(dpr, 0, 0, dpr, 0, 0);
    c.fillStyle = accentColor;
    c.font = '14px "JetBrains Mono", ui-monospace, monospace';
    c.textBaseline = "top";
    const t = state.gameOver ? state.finalElapsedMs : state.elapsedMs;
    const mm = Math.floor(t / 60000).toString().padStart(2, "0");
    const ss = Math.floor((t % 60000) / 1000).toString().padStart(2, "0");
    c.fillText(`SCORE  ${state.score.toString().padStart(6, "0")}`, 14, 12);
    c.fillText(`TIME   ${mm}:${ss}`, 14, 30);
    if (state.gameOver) {
      c.textAlign = "center";
      c.font = '28px "JetBrains Mono", ui-monospace, monospace';
      c.fillText("GAME OVER", cssW / 2, cssH / 2 - 28);
      c.font = '14px "JetBrains Mono", ui-monospace, monospace';
      c.fillText(
        `final ${state.score} pts · ${mm}:${ss}`,
        cssW / 2,
        cssH / 2 + 10,
      );
      c.fillText("press R to restart · esc to exit", cssW / 2, cssH / 2 + 32);
    } else {
      c.textAlign = "right";
      c.font = '11px "JetBrains Mono", ui-monospace, monospace';
      c.globalAlpha = 0.6;
      c.fillText("WASD move · mouse aim · click/space fire · esc exit", cssW - 14, 14);
    }
    c.restore();
  }

  // ─── Stop ────────────────────────────────────────────────────────────────
  return {
    stop: () => {
      cancelAnimationFrame(rafId);
      ro.disconnect();
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("keyup", onKeyUp);
      canvas.removeEventListener("mousemove", onMouseMove);
      canvas.removeEventListener("mousedown", onMouseDown);
      window.removeEventListener("mouseup", onMouseUp);
    },
  };
}

// ─── Helpers ───────────────────────────────────────────────────────────────

function makePlayer(): Player {
  return {
    pos: { x: WORLD_W / 2, y: WORLD_H / 2 },
    vel: { x: 0, y: 0 },
    aim: 0,
    fireCd: 0,
    invuln: PLAYER_INVULN_TIME,
    alive: true,
  };
}

function makeStars(): Star[] {
  const out: Star[] = [];
  for (let i = 0; i < STAR_COUNT; i++) {
    out.push({
      x: Math.random() * WORLD_W,
      y: Math.random() * WORLD_H,
      brightness: 0.15 + Math.random() * 0.45,
    });
  }
  return out;
}

function spawnBullet(s: GameState, from: Vec, angle: number, hostile: boolean) {
  // Reuse a dead slot to keep the array bounded.
  let slot: Bullet | undefined;
  for (const b of s.bullets) {
    if (!b.alive) {
      slot = b;
      break;
    }
  }
  const speed = hostile ? ENEMY_BULLET_SPEED : BULLET_SPEED;
  const life = hostile ? ENEMY_BULLET_LIFE : BULLET_LIFE;
  const vx = Math.cos(angle) * speed;
  const vy = Math.sin(angle) * speed;
  // Spawn slightly forward of the firing entity so it doesn't self-hit
  // and visually exits the hull cleanly.
  const px = from.x + Math.cos(angle) * 24;
  const py = from.y + Math.sin(angle) * 24;
  if (slot) {
    slot.pos.x = px;
    slot.pos.y = py;
    slot.vel.x = vx;
    slot.vel.y = vy;
    slot.life = life;
    slot.alive = true;
    slot.hostile = hostile;
  } else {
    s.bullets.push({
      pos: { x: px, y: py },
      vel: { x: vx, y: vy },
      life,
      alive: true,
      hostile,
    });
  }
}

function spawnAsteroidAtEdge(radius: number): Asteroid {
  // Pick a random edge; aim inward with some drift.
  const edge = Math.floor(Math.random() * 4);
  let x = 0;
  let y = 0;
  if (edge === 0) {
    x = Math.random() * WORLD_W;
    y = -radius;
  } else if (edge === 1) {
    x = WORLD_W + radius;
    y = Math.random() * WORLD_H;
  } else if (edge === 2) {
    x = Math.random() * WORLD_W;
    y = WORLD_H + radius;
  } else {
    x = -radius;
    y = Math.random() * WORLD_H;
  }
  const tx = WORLD_W / 2 + (Math.random() - 0.5) * WORLD_W * 0.5;
  const ty = WORLD_H / 2 + (Math.random() - 0.5) * WORLD_H * 0.5;
  const ang = Math.atan2(ty - y, tx - x);
  const speed = 40 + Math.random() * 60;
  return makeAsteroid({ x, y }, ang, speed, radius);
}

function makeAsteroid(pos: Vec, angle: number, speed: number, radius: number): Asteroid {
  const verts = 8 + Math.floor(Math.random() * 4);
  const shape: number[] = [];
  for (let i = 0; i < verts; i++) {
    shape.push(0.78 + Math.random() * 0.34);
  }
  return {
    pos: { x: pos.x, y: pos.y },
    vel: { x: Math.cos(angle) * speed, y: Math.sin(angle) * speed },
    radius,
    rot: Math.random() * Math.PI * 2,
    spin: (Math.random() - 0.5) * 1.5,
    shape,
    alive: true,
  };
}

function spawnEnemyAtEdge(): Enemy {
  const edge = Math.floor(Math.random() * 4);
  let x = 0;
  let y = 0;
  if (edge === 0) {
    x = Math.random() * WORLD_W;
    y = -ENEMY_RADIUS;
  } else if (edge === 1) {
    x = WORLD_W + ENEMY_RADIUS;
    y = Math.random() * WORLD_H;
  } else if (edge === 2) {
    x = Math.random() * WORLD_W;
    y = WORLD_H + ENEMY_RADIUS;
  } else {
    x = -ENEMY_RADIUS;
    y = Math.random() * WORLD_H;
  }
  return {
    pos: { x, y },
    vel: { x: 0, y: 0 },
    rot: 0,
    fireCd: 1.2 + Math.random(),
    alive: true,
  };
}

function shatterAsteroid(s: GameState, a: Asteroid) {
  a.alive = false;
  if (a.radius >= ASTEROID_LARGE - 1) {
    s.score += 20;
    spawnSplit(s, a, ASTEROID_MED, 2);
  } else if (a.radius >= ASTEROID_MED - 1) {
    s.score += 35;
    spawnSplit(s, a, ASTEROID_SMALL, 2);
  } else {
    s.score += 50;
  }
}

function spawnSplit(s: GameState, parent: Asteroid, childRadius: number, count: number) {
  for (let i = 0; i < count; i++) {
    const ang = Math.random() * Math.PI * 2;
    const speed = 60 + Math.random() * 60;
    s.asteroids.push(makeAsteroid(parent.pos, ang, speed, childRadius));
  }
}

function killPlayer(s: GameState) {
  s.player.alive = false;
  s.gameOver = true;
  s.finalElapsedMs = s.elapsedMs;
}

function moveWrap(pos: Vec, vel: Vec, dt: number) {
  pos.x += vel.x * dt;
  pos.y += vel.y * dt;
  if (pos.x < 0) {
    pos.x += WORLD_W;
  } else if (pos.x >= WORLD_W) {
    pos.x -= WORLD_W;
  }
  if (pos.y < 0) {
    pos.y += WORLD_H;
  } else if (pos.y >= WORLD_H) {
    pos.y -= WORLD_H;
  }
}

// Shortest signed delta in a wrapping coordinate so collision near edges
// uses the closer image.
function closestImageDelta(d: number, span: number): number {
  if (d > span / 2) {
    return d - span;
  }
  if (d < -span / 2) {
    return d + span;
  }
  return d;
}

function circleHit(a: Vec, ar: number, b: Vec, br: number): boolean {
  const dx = closestImageDelta(a.x - b.x, WORLD_W);
  const dy = closestImageDelta(a.y - b.y, WORLD_H);
  const r = ar + br;
  return dx * dx + dy * dy <= r * r;
}

function compact<T extends { alive: boolean }>(arr: T[]) {
  let w = 0;
  for (let r = 0; r < arr.length; r++) {
    if (arr[r].alive) {
      if (w !== r) {
        arr[w] = arr[r];
      }
      w++;
    }
  }
  arr.length = w;
}

function readAccent(): string {
  if (typeof window === "undefined") {
    return "#5ce1e6";
  }
  const v = getComputedStyle(document.documentElement)
    .getPropertyValue("--c-accent")
    .trim();
  return v || "#5ce1e6";
}
