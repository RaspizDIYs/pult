// Макет роя для браузера без Tauri. Всё выдумано: репозиторий публичный.
//
// Параметр адреса ?fleet= выбирает сценарий, чтобы его можно было рассматривать и снимать:
//   problems (по умолчанию) — замок на ушедшей сессии, молчащий держатель, просроченный
//                             локальный замок, пересечение по файлу, задачи во всех состояниях;
//   calm      — проблем нет;
//   hub-down  — хаб недоступен, видно только эту машину;
//   off       — рой на машине не настроен;
//   empty     — хаб на связи, в рою никого;
//   loading   — ядро не отвечает на первый запрос.
//
// Параметр ?release= — чем кончается «Снять замок»:
//   ok (по умолчанию) — замок исчезает из картины;
//   refused           — хаб отказал, замок на месте;
//   no-dispatcher     — на машине нет диспетчера: кнопка локального замка неактивна.
import type { FleetLock, FleetSession, FleetTask, FleetView, ReleaseResult } from "./fleet";

const MIN = 60_000;
const t0 = Date.now();
const ago = (min: number) => new Date(t0 - min * MIN).toISOString();

const id = (n: number, head: string) => `${head}-0000-4000-8000-00000000000${n}`;
const S = {
  map: id(1, "a1f3c9e2"),
  cart: id(2, "4b9c1d2e"),
  relay: "мак-а-relay-service",
  danya: id(4, "7d20aa41"),
  deploy: id(5, "c33e0b77"),
  review: id(6, "e81f5a90"),
  runner: id(7, "5f6a7b8c"),
  ghost: id(8, "9a0b2c4d"),
};

function session(sid: string, p: Partial<FleetSession>): FleetSession {
  return { id: sid, name: null, machine: "мак-а", local: false, presence: "online", silent: false, lastSeenAt: ago(1), cwd: null, note: null, ...p };
}

const SESSIONS: FleetSession[] = [
  session(S.map, { name: "карта", local: true, lastSeenAt: ago(0.5), cwd: "~/projects/pult", note: "правлю экран роя" }),
  session(S.cart, { local: true, lastSeenAt: ago(6), cwd: "~/projects/shop", note: "чиню оплату в корзине" }),
  session(S.relay, { name: "подписка", local: true, lastSeenAt: ago(0.3) }),
  session(S.ghost, { local: true, presence: "offline", lastSeenAt: ago(290), cwd: "~/projects/shop" }),
  session(S.danya, { name: "Даня", machine: "дом-пк", lastSeenAt: ago(2), cwd: "D:/work/pult", note: "миграция схемы заказов" }),
  session(S.review, { name: "ревью", machine: "дом-пк", silent: true, lastSeenAt: ago(34), cwd: "D:/work/shop" }),
  session(S.deploy, { name: "деплой", machine: "ноут-б", presence: "offline", lastSeenAt: ago(170), cwd: "~/code/shop", note: "выкатываю 2.14" }),
  session(S.runner, { name: "исполнитель", machine: "сервер-сборки", lastSeenAt: ago(1), cwd: "/srv/runner/shop" }),
];

function lock(p: Partial<FleetLock> & Pick<FleetLock, "resource" | "session">): FleetLock {
  return { key: p.resource, scope: "fleet", repo: null, since: ago(3), ttlMin: null, units: 1, command: null, cwd: null, queue: [], problem: null, ...p };
}

const LOCKS: FleetLock[] = [
  lock({
    resource: "deploy", key: "shop-1a2b3c4d::deploy", repo: "shop-1a2b3c4d", session: S.deploy, since: ago(180), ttlMin: 40,
    command: "./deploy.sh prod", problem: "offline",
    queue: [
      { session: S.cart, since: ago(12), reason: "выкатить фикс корзины" },
      { session: S.danya, since: ago(4), reason: null },
    ],
  }),
  lock({ resource: "push", key: "shop-1a2b3c4d::push", repo: "shop-1a2b3c4d", session: S.review, since: ago(3), ttlMin: 5, command: "git push origin test", problem: "silent" }),
  lock({
    resource: "deploy", key: "shop-77aa__deploy", scope: "local", repo: "shop-77aa", session: S.ghost, since: ago(260), ttlMin: 40,
    command: "npm run deploy:stage", cwd: "~/projects/shop", problem: "offline",
  }),
  lock({ resource: "db-migrate", key: "pult-9f8e7d6c::db-migrate", repo: "pult-9f8e7d6c", session: S.danya, since: ago(4), ttlMin: 20, command: "npm run migrate" }),
  lock({ resource: "heavy-misc", scope: "local", session: S.cart, since: ago(48), ttlMin: 30, command: "docker build -t shop .", cwd: "~/projects/shop", problem: "overdue" }),
  lock({ resource: "backend-build", scope: "local", session: S.map, since: ago(2), ttlMin: 30, units: 2, command: "cargo test", cwd: "~/projects/pult" }),
  lock({ resource: "frontend-check", scope: "local", session: S.cart, since: ago(1), ttlMin: 30, command: "npx vitest run src/cart.test.ts", cwd: "~/projects/shop", queue: [{ session: S.map, since: ago(0.5), reason: "npm run build" }] }),
];

function task(n: number, title: string, status: FleetTask["status"], p: Partial<FleetTask> = {}): FleetTask {
  return {
    id: `TASK-${n}`, title, status, priority: "NORMAL", executor: "claude", machine: null, session: null, node: null, attempts: 0,
    createdAt: ago(400 - n * 10), startedAt: null, finishedAt: null, note: null, ...p,
  };
}

const TASKS: FleetTask[] = [
  task(14, "прогнать e2e на тест-стенде", "IN_PROGRESS", { machine: "сервер-сборки", session: S.runner, node: "сборщик", startedAt: ago(15) }),
  task(16, "починить CSP на витрине", "TODO", { priority: "HIGH" }),
  task(15, "переименовать поле статуса", "TODO", { executor: "codex" }),
  task(13, "починить гонку в драфте", "FAILED", { attempts: 3, finishedAt: ago(90), note: "сессия-исполнитель пропала (попыток 3, дальше — к человеку)" }),
  task(12, "перевести отчёт на новую схему", "DONE", { finishedAt: ago(60), machine: "сервер-сборки" }),
  task(11, "обновить зависимости витрины", "DONE", { finishedAt: ago(300), machine: "сервер-сборки", note: "готово, ветка feature/task-11" }),
];

const FLEET_RESOURCES = [
  { name: "deploy", label: "деплой общего окружения", capacity: 1, used: 1 },
  { name: "push", label: "push в общую ветку", capacity: 1, used: 1 },
  { name: "db-migrate", label: "миграции общей БД", capacity: 1, used: 1 },
  { name: "test-stand", label: "тест-стенд целиком (e2e, ресет VM)", capacity: 1, used: 0 },
];

const LOCAL = {
  dir: "~/.claude/orchestrator",
  found: true,
  resources: [
    { name: "frontend-check", label: "фронт: tsc, eslint, vitest, build", capacity: 2, used: 1 },
    { name: "backend-build", label: "бэк: dotnet, cargo, gradle, mvn, pytest, go", capacity: 2, used: 2 },
    { name: "e2e", label: "браузерные тесты: Playwright, Cypress", capacity: 1, used: 0 },
    { name: "heavy-misc", label: "прочее тяжёлое: docker build, make, кодогенерация", capacity: 2, used: 0 },
  ],
  runs: [
    { resource: "backend-build", session: S.map, command: "cargo test", cwd: "~/projects/pult", startedAt: ago(2), background: false },
    { resource: "frontend-check", session: S.cart, command: "npx vitest run src/cart.test.ts", cwd: "~/projects/shop", startedAt: ago(1), background: false },
    { resource: "e2e", session: S.ghost, command: "npx playwright test", cwd: "~/projects/shop", startedAt: ago(300), background: true },
  ],
  errors: ["runs/shop-77aa__e2e.json: битый JSON (EOF while parsing an object at line 1 column 212)"],
  releaseError: null as string | null,
};

function problems(): FleetView {
  return {
    takenAt: new Date().toISOString(),
    hub: { state: "ok", address: "127.0.0.1:8787", error: null, lastOkAt: new Date().toISOString() },
    machine: "мак-а",
    sessions: SESSIONS,
    locks: LOCKS,
    fleetResources: FLEET_RESOURCES,
    claims: [
      { file: "src/App.tsx", repo: "pult-9f8e7d6c", sessions: [{ session: S.map, since: ago(5) }, { session: S.danya, since: ago(8) }], overlap: true },
      { file: "src/lib/cart.ts", repo: "shop-1a2b3c4d", sessions: [{ session: S.cart, since: ago(9) }], overlap: false },
      { file: "docs/контракт.md", repo: "pult-9f8e7d6c", sessions: [{ session: S.map, since: ago(6) }, { session: S.deploy, since: ago(70) }], overlap: false },
    ],
    board: [
      { session: S.map, text: "правлю экран роя", task: null, cwd: "~/projects/pult", at: ago(2) },
      { session: S.danya, text: "миграция схемы заказов", task: "KAN-81", cwd: "D:/work/pult", at: ago(5) },
      { session: S.cart, text: "чиню оплату в корзине", task: "KAN-77", cwd: "~/projects/shop", at: ago(10) },
      { session: S.deploy, text: "выкатываю 2.14", task: null, cwd: "~/code/shop", at: ago(175) },
    ],
    tasks: TASKS,
    local: LOCAL,
    problems: 5,
  };
}

function calm(): FleetView {
  const v = problems();
  const fine = new Set([S.map, S.cart, S.relay, S.danya, S.runner]);
  return {
    ...v,
    sessions: v.sessions.filter((s) => fine.has(s.id)),
    locks: v.locks.filter((l) => !l.problem),
    fleetResources: FLEET_RESOURCES.map((r) => ({ ...r, used: r.name === "db-migrate" ? 1 : 0 })),
    claims: v.claims.filter((c) => !c.overlap).map((c) => ({ ...c, sessions: c.sessions.filter((s) => fine.has(s.session)) })),
    board: v.board.filter((b) => fine.has(b.session)),
    local: { ...LOCAL, runs: LOCAL.runs.slice(0, 2), errors: [] },
    problems: 0,
  };
}

/** Хаб недоступен: остаётся только то, что лежит в файлах этой машины. */
function hubDown(): FleetView {
  const v = problems();
  const mine = new Set(SESSIONS.filter((s) => s.local).map((s) => s.id));
  return {
    ...v,
    hub: { state: "unavailable", address: "127.0.0.1:8787", error: "соединение отклонено — туннель к хабу не поднят?", lastOkAt: ago(25) },
    sessions: SESSIONS.filter((s) => mine.has(s.id)).map((s) => (s.id === S.relay ? { ...s, presence: "unknown", lastSeenAt: null } : s)),
    locks: v.locks.filter((l) => l.scope === "local"),
    fleetResources: [],
    claims: [],
    board: v.board.filter((b) => mine.has(b.session)),
    tasks: [],
    problems: 2,
  };
}

function empty(): FleetView {
  return {
    ...problems(),
    sessions: [],
    locks: [],
    fleetResources: FLEET_RESOURCES.map((r) => ({ ...r, used: 0 })),
    claims: [],
    board: [],
    tasks: [],
    local: { ...LOCAL, resources: LOCAL.resources.map((r) => ({ ...r, used: 0 })), runs: [], errors: [] },
    problems: 0,
  };
}

function off(): FleetView {
  return { ...hubDown(), hub: { state: "off", address: null, error: null, lastOkAt: null } };
}

const SCENARIOS: Record<string, () => FleetView> = { problems, calm, "hub-down": hubDown, empty, off };
const query = typeof location !== "undefined" ? new URLSearchParams(location.search) : null;
const param = query?.get("fleet") ?? null;
const release = query?.get("release") ?? "ok";
const pause = (ms: number) => new Promise((r) => setTimeout(r, ms)); // как настоящий вызов: не мгновенно

// Картина одна на вкладку: снятый замок должен исчезнуть и из списка, и из счётчика проблем.
let current: FleetView | null = null;
const listeners = new Set<(v: FleetView) => void>();

function scene(): FleetView {
  const v = (SCENARIOS[param ?? ""] ?? problems)();
  return release === "no-dispatcher" ? { ...v, local: { ...v.local, releaseError: "не найден node — запустить диспетчер нечем" } } : v;
}

export async function getFleet(): Promise<FleetView> {
  if (param === "loading") return new Promise(() => {});
  await pause(150);
  return (current ??= scene());
}

export async function onFleet(cb: (v: FleetView) => void): Promise<() => void> {
  listeners.add(cb);
  return () => listeners.delete(cb);
}

const PID = 48213;

export async function releasePreview(l: FleetLock): Promise<string> {
  await pause(400);
  return `Замки:   ${l.resource}(260 мин)\nТалоны:  нет\nПроцессы: 1\n  ${PID}  ${l.command ?? ""}`;
}

export async function releaseLock(l: FleetLock): Promise<ReleaseResult> {
  await pause(700);
  if (release === "refused") return { released: false, message: "хаб отказал в доступе: токен в fleet.json не подходит", output: null };
  const v = (current ??= scene());
  const same = (x: FleetLock) => x.scope === l.scope && x.key === l.key && x.session === l.session;
  current = {
    ...v,
    locks: v.locks.filter((x) => !same(x)),
    fleetResources: v.fleetResources.map((r) => (l.scope === "fleet" && r.name === l.resource ? { ...r, used: 0 } : r)),
    problems: v.problems - 1,
  };
  listeners.forEach((cb) => cb(current!));
  return { released: true, message: "замок снят", output: l.scope === "local" ? `  убит ${PID}\nЗамки отпущены, талоны сняты, доска очищена.` : null };
}
