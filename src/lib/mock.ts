// Встроенный макет ядра для запуска в браузере (npm run dev без Tauri).
// Вся инфраструктура здесь выдуманная: репозиторий публичный, реальных имён и адресов быть не должно.
//
// Макет — маленький движок по правилам контракта (раздел 2): задаём только первопричины
// («контейнер упал», «хост молчит»), а корни, вторичные отказы, «неизвестно» и
// blockedBy выводятся из зависимостей. Так на карте честно видны все строки таблицы состояний.
//
// Параметр адреса ?mock= фиксирует сценарий, чтобы его можно было рассматривать и снимать:
//   ok | containers | host | tunnel | mixed   — шаг сценария (по умолчанию шаги сменяются сами);
//   no-inventory | empty | inventory-error | loading — экраны без карты и с ошибкой инвентаря.
//
// Первая площадка — «Эта машина» с папкой MCP: серверы во всех состояниях (запущен, не запущен,
// адрес доступен и нет, отказ настоящей проверки); кнопка «Проверить по-настоящему» отвечает
// через полторы секунды. У «Локальной модели» на хосте Г — проверка ollama и «Спросить модель».
//
// Дальше три площадки (два хоста и гипервизор с ВМ) и «Сеть и внешнее», сервисы разложены
// по проектам; заглушка на хосте А скрыта (`скрыть: true`) — её проверяют, но не показывают;
// в шаге containers корень отказа лежит глубоко: гипервизор → хост Б → «Задачи» → панель.
import type {
  Backend,
  CheckResult,
  ContainerFacts,
  EnvCheck,
  HistoryEntry,
  McpInfo,
  McpProbe,
  NodeState,
  NodeView,
  OllamaAnswer,
  Settings,
  Snapshot,
} from "./pult";

// ───────────── Описание выдуманной инфраструктуры ─────────────

interface Probe {
  kind: "tcp" | "http" | "ollama";
  target: string;
  from?: string;
  ms: number;
  ok: string;
  down: string;
  models?: string[];
}

const tcp = (host: string, port: number, ms = 20, from?: string): Probe => ({
  kind: "tcp",
  target: `${host}:${port}`,
  from,
  ms,
  ok: `порт ${port}: открыт`,
  down: `порт ${port}: таймаут 3 с`,
});

const http = (url: string, ms = 60, from?: string): Probe => {
  const path = new URL(url).pathname;
  return { kind: "http", target: url, from, ms, ok: `GET ${path} → 200`, down: `GET ${path}: нет ответа, таймаут 5 с` };
};

const OLLAMA_MODELS = ["qwen3:8b", "llama3.2:3b", "nomic-embed-text:latest"];
const ollama = (url: string, ms: number, from?: string): Probe => ({
  kind: "ollama",
  target: url,
  from,
  ms,
  ok: `отвечает за ${ms} мс · моделей: ${OLLAMA_MODELS.length} · в памяти: qwen3:8b (GPU)`,
  down: "не отвечает: соединение отклонено",
  models: OLLAMA_MODELS,
});

interface Def {
  id: string;
  title: string;
  kind: string;
  group?: string;
  project?: string;
  hidden?: boolean; // скрыть: true — ядро проверяет, но в снимок не кладёт
  on?: string;
  dependsOn?: string[];
  probes?: Probe[];
  collect?: string; // что собирается по ssh с этого узла
  via?: string; // через какой узел идёт сбор
  container?: string;
  expected?: "остановлен";
  access?: NodeView["access"];
  links?: NodeView["links"];
  undeclared?: boolean;
}

// Порядок важен: предки раньше потомков (движок идёт сверху вниз).
const DEFS: Def[] = [
  { id: "интернет", title: "Интернет", kind: "внешнее", probes: [tcp("1.1.1.1", 443, 18), tcp("example.com", 443, 24)] },
  { id: "репозитории", title: "Хостинг репозиториев", kind: "внешнее", dependsOn: ["интернет"], probes: [tcp("git.example.com", 22, 41)] },
  {
    id: "хост-а", title: "Хост А", kind: "хост", group: "прод", dependsOn: ["интернет"],
    probes: [tcp("a.example.com", 22, 24)], collect: "docker, wireguard",
    access: { how: "ssh host-a", secret: "хранилище → ключ ssh хоста А", who: ["alice", "bob"] },
    links: [{ title: "Кабинет хостинга", url: "https://hosting.example.com/servers/a" }],
  },
  {
    id: "хост-в", title: "Хост В (резерв)", kind: "хост", group: "прод", dependsOn: ["интернет"],
    probes: [tcp("c.example.com", 22, 31)], collect: "docker",
    access: { how: "ssh host-c", secret: "хранилище → ключ ssh хоста В", who: ["alice"] },
  },
  { id: "прокси", title: "Обратный прокси", kind: "контейнер", group: "прод", on: "хост-а", container: "edge-proxy", probes: [http("https://example.com/ping", 35)] },
  { id: "база", title: "База данных", kind: "контейнер", group: "прод", project: "Магазин", on: "хост-а", container: "pg-main", access: { how: "docker exec -it pg-main psql", secret: "хранилище → пароль БД", who: ["alice"] } },
  { id: "кэш", title: "Кэш", kind: "контейнер", group: "прод", project: "Магазин", on: "хост-а", container: "redis-cache" },
  { id: "очередь", title: "Очередь задач", kind: "контейнер", group: "прод", project: "Магазин", on: "хост-а", container: "queue" },
  { id: "заглушка", title: "Заглушка «сайт обновляется»", kind: "контейнер", group: "прод", on: "хост-а", container: "maintenance", hidden: true },
  { id: "хост-а/grafana-old", title: "grafana-old", kind: "контейнер", group: "прод", on: "хост-а", container: "grafana-old", undeclared: true },
  { id: "почта", title: "Почтовый шлюз", kind: "контейнер", group: "прод", project: "Почта", on: "хост-в", container: "mail-gw", probes: [tcp("mail.example.com", 25, 55)] },
  { id: "вики", title: "Вики", kind: "контейнер", group: "прод", project: "Вики", on: "хост-в", container: "wiki", probes: [http("https://wiki.example.com/", 80)], links: [{ title: "Открыть вики", url: "https://wiki.example.com" }] },
  { id: "копии", title: "Резервные копии", kind: "сервис", group: "прод", on: "хост-в" },
  { id: "сайт", title: "Сайт", kind: "контейнер", group: "прод", project: "Магазин", on: "хост-а", dependsOn: ["прокси"], container: "web", probes: [http("https://www.example.com/", 90)], links: [{ title: "Открыть сайт", url: "https://www.example.com" }] },
  { id: "api", title: "API", kind: "контейнер", group: "прод", project: "Магазин", on: "хост-а", dependsOn: ["прокси", "база"], container: "api", probes: [http("https://api.example.com/health", 48)], links: [{ title: "Метрики", url: "https://metrics.example.com/d/api" }] },
  { id: "воркер", title: "Воркер", kind: "контейнер", group: "прод", project: "Магазин", on: "хост-а", dependsOn: ["очередь", "база"], container: "worker" },
  {
    id: "туннель-дом", title: "WireGuard до дома", kind: "туннель", group: "дом", dependsOn: ["хост-а"],
    probes: [tcp("10.0.0.2", 22, 38, "хост-а")],
  },
  { id: "гипервизор", title: "Гипервизор", kind: "хост", group: "дом", dependsOn: ["туннель-дом"], probes: [tcp("10.0.0.1", 8006, 44, "хост-а")], collect: "proxmox", via: "хост-а" },
  { id: "принтер", title: "Сетевой принтер", kind: "сервис", group: "дом", dependsOn: ["туннель-дом"], probes: [tcp("10.0.0.30", 9100, 12, "хост-а")] },
  {
    id: "хост-б", title: "Хост Б (за туннелем)", kind: "вм", group: "дом", on: "гипервизор", dependsOn: ["туннель-дом"],
    collect: "docker", via: "хост-а",
    access: { how: "ssh -J host-a user@10.0.0.2", secret: "хранилище → ключ ssh хоста Б", who: ["alice", "bob"] },
  },
  { id: "хост-г", title: "Хост Г (медиа)", kind: "вм", group: "дом", on: "гипервизор", dependsOn: ["туннель-дом"], collect: "docker", via: "хост-а" },
  {
    id: "панель", title: "Панель задач", kind: "контейнер", group: "дом", project: "Задачи", on: "хост-б", container: "tasks-panel",
    probes: [http("https://tasks.example.com/health", 52)],
    access: { how: "https://tasks.example.com", secret: "хранилище → токен панели", who: ["alice", "bob"] },
    links: [{ title: "Открыть панель", url: "https://tasks.example.com" }, { title: "Логи", url: "https://logs.example.com/tasks-panel" }],
  },
  { id: "мигратор", title: "Мигратор БД", kind: "контейнер", group: "дом", project: "Задачи", on: "хост-б", container: "db-migrator", expected: "остановлен" },
  { id: "мониторинг", title: "Мониторинг", kind: "контейнер", group: "дом", project: "Мониторинг", on: "хост-б", container: "prom", probes: [http("http://10.0.0.2:9090/-/healthy", 20, "хост-а")] },
  { id: "бот", title: "Бот уведомлений", kind: "контейнер", group: "дом", project: "Задачи", on: "хост-б", dependsOn: ["панель"], container: "notify-bot", probes: [http("https://bot.example.com/health", 70)] },
  { id: "статистика", title: "Статистика", kind: "сервис", group: "дом", project: "Мониторинг", on: "хост-б", dependsOn: ["панель"], probes: [tcp("10.0.0.2", 9100, 15, "хост-а")] },
  { id: "медиа", title: "Медиасервер", kind: "контейнер", group: "дом", project: "Медиа", on: "хост-г", container: "media" },
  { id: "загрузчик", title: "Загрузчик", kind: "контейнер", group: "дом", project: "Медиа", on: "хост-г", container: "fetcher" },
  { id: "ollama", title: "Локальная модель", kind: "сервис", group: "дом", project: "ИИ", on: "хост-г", probes: [ollama("http://10.0.0.4:11434", 40, "хост-а")] },
];

const byId = new Map(DEFS.map((d) => [d.id, d]));
const titleOf = (id: string) => byId.get(id)?.title ?? id;

const requiredMemo = new Map<string, string[]>();
function requiredAncestors(id: string): string[] {
  const hit = requiredMemo.get(id);
  if (hit) return hit;
  const def = byId.get(id)!;
  const direct = [...(def.on ? [def.on] : []), ...(def.dependsOn ?? [])];
  const all = new Set<string>(direct);
  for (const p of direct) for (const a of requiredAncestors(p)) all.add(a);
  const res = [...all];
  requiredMemo.set(id, res);
  return res;
}

// ───────────── Сценарий ─────────────

type Fault =
  | { t: "check"; fact: string; hints?: string[] } // собственная проверка не прошла
  | { t: "container"; fact: string; hints?: string[]; facts: Partial<ContainerFacts> }
  | { t: "blind"; fact: string } // узнать нечем, причины выше нет
  | { t: "stale"; minutes: number };

interface Step {
  name: string;
  faults: Record<string, Fault>;
}

const MIN = 60_000;

const PANEL_DOWN: Fault = {
  t: "container",
  fact: "контейнер tasks-panel остановлен: код выхода 137 (остановлен принудительно, SIGKILL)",
  hints: [
    "Возможно, на хосте не хватило памяти: посмотри dmesg и docker stats",
    "Возможно, контейнер остановили вручную или прервалась выкатка",
  ],
  facts: { state: "exited", exitCode: 137, oomKilled: null, health: null, restartCount: 0, finishedAt: new Date(Date.now() - 14 * MIN).toISOString() },
};
const WORKER_LOOP: Fault = {
  t: "container",
  fact: "контейнер worker перезапускается: 14 раз, последний код выхода 1",
  hints: ["Возможно, воркер не может подключиться к очереди после смены пароля"],
  facts: { state: "restarting", exitCode: 1, oomKilled: false, health: "unhealthy", restartCount: 14 },
};
const HOST_A_DOWN: Fault = {
  t: "check",
  fact: "порт 22: таймаут 3 с",
  hints: ["Возможно, хост перезагружается или недоступна его сеть", "Это не твой интернет: внешние проверки отвечают"],
};
const TUNNEL_DOWN: Fault = {
  t: "check",
  fact: "порт 22 на 10.0.0.2 с узла хост-а: таймаут 3 с",
  hints: [
    "Возможно, дома нет света или интернета: handshake WireGuard старше 3 минут",
    "Возможно, у домашнего роутера сменился адрес",
  ],
};
const BLIND_HYPERVISOR: Record<string, Fault> = {
  гипервизор: { t: "blind", fact: "нет пути наблюдения: ключ для сбора с гипервизора не найден" },
  "хост-г": { t: "blind", fact: "нет данных: гипервизор не наблюдается" },
  медиа: { t: "blind", fact: "нет данных: хост-г не наблюдается" },
  загрузчик: { t: "blind", fact: "нет данных: хост-г не наблюдается" },
};

const STEPS: Step[] = [
  { name: "ok", faults: {} },
  { name: "containers", faults: { панель: PANEL_DOWN, воркер: WORKER_LOOP } },
  { name: "host", faults: { "хост-а": HOST_A_DOWN } },
  { name: "tunnel", faults: { "туннель-дом": TUNNEL_DOWN } },
  {
    // Всё сразу: корни, вторичный отказ, неизвестно с причиной и без, устарело.
    name: "mixed",
    faults: {
      панель: PANEL_DOWN,
      воркер: WORKER_LOOP,
      "хост-б": { t: "stale", minutes: 14 },
      мигратор: { t: "stale", minutes: 14 },
      мониторинг: { t: "stale", minutes: 14 },
      ...BLIND_HYPERVISOR,
    },
  },
];

// ───────────── Движок макета ─────────────

const jitter = (ms: number) => Math.max(1, Math.round(ms * (0.8 + Math.random() * 0.5)));

function upText(hours: number): string {
  return hours < 48 ? `${hours} ч` : `${Math.round(hours / 24)} дн.`;
}

function containerFacts(def: Def, idx: number, at: number, patch: Partial<ContainerFacts> = {}): ContainerFacts {
  const stopped = def.expected === "остановлен";
  return {
    state: stopped ? "exited" : "running",
    exitCode: stopped ? 0 : null,
    oomKilled: false,
    health: stopped || !def.probes?.length ? null : "healthy",
    restartCount: 0,
    startedAt: new Date(at - (40 + idx * 9) * 3_600_000).toISOString(),
    finishedAt: stopped ? new Date(at - (30 + idx) * 3_600_000).toISOString() : null,
    image: `registry.example.com/studio/${def.container}:2.${idx % 7}.1`,
    ...patch,
  };
}

function checksFor(def: Def, how: "ok" | "down" | "blocked", at: number, override?: string): CheckResult[] {
  const iso = new Date(at).toISOString();
  const res: CheckResult[] = [];
  const host = def.on ? titleOf(def.on) : "родителя";
  (def.probes ?? []).forEach((p, i) => {
    const base = { kind: p.kind, target: p.target, from: p.from ?? null, measuredAt: iso } as const;
    if (how === "ok") res.push({ ...base, ok: true, fact: p.ok, latencyMs: jitter(p.ms), ...(p.models && { models: p.models }) });
    else if (how === "blocked" && p.from)
      res.push({ ...base, ok: null, fact: `выполнить не удалось: узел «${titleOf(p.from)}» недоступен`, latencyMs: null });
    else res.push({ ...base, ok: false, fact: i === 0 && override ? override : p.down, latencyMs: null });
  });
  if (def.container) {
    const t = `контейнер ${def.container} на «${host}»`;
    if (how === "ok") res.push({ kind: "container", target: t, from: def.on ?? null, ok: true, fact: def.expected ? "exited, код 0 (ожидается остановленным)" : "running", latencyMs: null, measuredAt: iso });
    else if (how === "blocked") res.push({ kind: "container", target: t, from: def.on ?? null, ok: null, fact: `сбор с узла «${host}» не удался`, latencyMs: null, measuredAt: iso });
    else res.push({ kind: "container", target: t, from: def.on ?? null, ok: false, fact: override ?? "не запущен", latencyMs: null, measuredAt: iso });
  }
  if (def.kind === "вм") {
    const t = `ВМ на «${host}»`;
    res.push(how === "ok"
      ? { kind: "vm", target: t, from: def.on ?? null, ok: true, fact: "запущена", latencyMs: null, measuredAt: iso }
      : { kind: "vm", target: t, from: def.on ?? null, ok: null, fact: `сбор с узла «${host}» не удался`, latencyMs: null, measuredAt: iso });
  }
  if (def.collect) {
    const t = `сбор: ${def.collect}${def.via ? ` (через «${titleOf(def.via)}»)` : " (ssh)"}`;
    res.push(how === "ok"
      ? { kind: "collect", target: t, from: def.via ?? null, ok: true, fact: "собрано без ошибок", latencyMs: jitter(1200), measuredAt: iso }
      : { kind: "collect", target: t, from: def.via ?? null, ok: null, fact: "ssh: нет соединения, таймаут 8 с", latencyMs: null, measuredAt: iso });
  }
  return res;
}

function okFact(def: Def, idx: number): string {
  if (def.container) return def.expected ? "остановлен, код 0 — так и ожидается" : `работает ${upText(40 + idx * 9)}`;
  if (def.kind === "вм") return "ВМ запущена, сбор docker идёт";
  const p = def.probes?.[0];
  if (p?.kind === "ollama") return p.ok;
  return p ? `${p.ok}, ${p.ms} мс` : "—";
}

function evaluate(faults: Record<string, Fault>, at: number): Record<string, NodeState> {
  const out: Record<string, NodeState> = {};
  DEFS.forEach((def, idx) => {
    const f = faults[def.id];
    const roots = requiredAncestors(def.id).filter((a) => out[a].own === "fail" && out[a].isRoot);
    const s: NodeState = {
      id: def.id, own: "ok", confirmed: true, fact: "", hints: [], isRoot: false, blockedBy: [],
      checks: [], container: null, measuredAt: new Date(at).toISOString(), since: null,
    };
    const hasContainer = !!def.container;

    if (f?.t === "check" || f?.t === "container") {
      s.own = "fail";
      s.fact = f.fact;
      s.hints = f.hints ?? [];
      s.isRoot = roots.length === 0;
      s.blockedBy = roots;
      s.checks = checksFor(def, "down", at, f.fact);
      if (f.t === "container") s.container = containerFacts(def, idx, at, f.facts);
    } else if (f?.t === "blind") {
      s.own = "unknown";
      s.fact = f.fact;
      s.blockedBy = roots;
      s.measuredAt = null;
      s.checks = checksFor(def, "blocked", at);
    } else if (f?.t === "stale") {
      const old = at - f.minutes * MIN;
      s.own = "stale";
      s.fact = okFact(def, idx);
      s.measuredAt = new Date(old).toISOString();
      s.checks = checksFor(def, "ok", old);
      if (hasContainer) s.container = containerFacts(def, idx, old);
    } else if (roots.length) {
      s.blockedBy = roots;
      const direct = (def.probes ?? []).filter((p) => !p.from);
      if (direct.length) {
        // Свои сетевые проверки идут с этой машины и честно не проходят — это вторичный отказ.
        s.own = "fail";
        s.fact = direct[0].down;
        s.checks = checksFor(def, "blocked", at);
      } else {
        const via = def.probes?.find((p) => p.from)?.from ?? def.on;
        s.own = "unknown";
        s.fact = via ? `проверка идёт через узел «${titleOf(via)}», он недоступен` : "нет данных: узел выше недоступен";
        s.measuredAt = null;
        s.checks = checksFor(def, "blocked", at);
      }
    } else if (!def.probes?.length && !def.collect && !hasContainer && def.kind !== "вм") {
      s.own = "unchecked";
      s.fact = "не проверяется";
    } else {
      s.fact = okFact(def, idx);
      s.checks = checksFor(def, "ok", at);
      if (hasContainer) s.container = containerFacts(def, idx, at);
    }
    out[def.id] = s;
  });
  return out;
}

// ───────────── Эта машина: MCP-серверы ─────────────
// Как в ядре, эти узлы идут мимо движка: состояние задано прямо. Имена и пути выдуманы.

const NOT_RUNNING = "сейчас не запущен: стартует вместе с сессией Claude";
const STDIO: McpInfo = { transport: "stdio", sources: ["~/.mcp.json"], command: "node", script: null, host: null, envNames: [], headerNames: [] };
const REMOTE: McpInfo = { transport: "http", sources: ["~/work/shop/.mcp.json"], command: null, script: null, host: null, envNames: [], headerNames: [] };

interface McpDef {
  name: string;
  info: McpInfo;
  /** Что видно пассивно: запущен ли процесс или открыт ли порт. */
  seen: { ok: boolean | null; fact: string };
  /** Итог настоящей проверки: что уже есть при открытии и что ответит кнопка. */
  real?: McpProbe;
  onProbe: McpProbe;
}

const BROKEN: McpProbe = { ok: false, fact: "процесс завершился с кодом 1, не ответив на initialize; stderr: Error: Cannot find module '/home/alice/tools/archive-mcp/dist/index.js'" };
const MCP_DEFS: McpDef[] = [
  {
    name: "вики",
    info: { ...STDIO, sources: ["Claude Code: все проекты", "Claude Desktop"], command: "python", script: "~/tools/wiki-mcp/server.py", envNames: ["WIKI_URL", "WIKI_TOKEN"] },
    seen: { ok: true, fact: "запущен, процессов: 2" },
    onProbe: { ok: true, fact: "отвечает · инструментов: 6" },
  },
  {
    name: "поиск",
    info: { ...STDIO, command: "uvx", envNames: ["SEARCH_API_KEY"] },
    seen: { ok: null, fact: NOT_RUNNING },
    onProbe: { ok: true, fact: "отвечает · инструментов: 3" },
  },
  {
    name: "трекер",
    info: { ...REMOTE, host: "https://mcp.example.com", headerNames: ["Authorization"] },
    seen: { ok: true, fact: "порт 443: открыт" },
    onProbe: { ok: true, fact: "отвечает · сервер tracker 2.1.0" },
  },
  {
    name: "архив",
    info: { ...STDIO, sources: ["Claude Code: проект ~/work/shop"], script: "~/tools/archive-mcp/dist/index.js" },
    seen: { ok: null, fact: NOT_RUNNING },
    real: BROKEN,
    onProbe: BROKEN,
  },
  {
    name: "метрики",
    info: { ...REMOTE, transport: "sse", host: "http://10.0.0.7:8811" },
    seen: { ok: false, fact: "порт 8811: таймаут 3 с" },
    onProbe: { ok: false, fact: "нет ответа, таймаут 20 с" },
  },
  {
    name: "склад",
    info: { ...STDIO, sources: ["~/work/shop/.mcp.json, выключен в настройках Claude"], script: "~/work/shop/tools/stock-mcp.js" },
    seen: { ok: null, fact: NOT_RUNNING },
    onProbe: { ok: true, fact: "отвечает · инструментов: 11" },
  },
];

const mcpId = (d: McpDef) => `#mcp/${d.name}`;
const mcpViews: NodeView[] = MCP_DEFS.map((d) => ({
  id: mcpId(d), title: d.name, kind: "mcp", group: null, project: "MCP", on: null, dependsOn: [],
  access: null, links: [], undeclared: false, hasLogs: false, mcp: d.info,
}));
const mcpReal = new Map(MCP_DEFS.flatMap((d) => (d.real ? [[mcpId(d), { ...d.real, at: Date.now() - 9 * 60_000 }] as const] : [])));
const mcpSince = new Date(Date.now() - 3 * 3_600_000).toISOString();

function mcpState(d: McpDef, at: number): NodeState {
  const iso = new Date(at).toISOString();
  const real = mcpReal.get(mcpId(d));
  const failed = real && !real.ok;
  const stdio = d.info.transport === "stdio";
  const checks: CheckResult[] = [
    { kind: stdio ? "process" : "tcp", target: stdio ? "процесс сервера" : (d.info.host ?? "").replace(/^\w+:\/\//, ""), from: null, ok: d.seen.ok, fact: d.seen.fact, latencyMs: d.seen.ok && !stdio ? jitter(30) : null, measuredAt: iso },
  ];
  if (real) checks.push({ kind: "mcp", target: "initialize и tools/list", from: null, ok: real.ok, fact: real.fact, latencyMs: null, measuredAt: new Date(real.at).toISOString() });
  return {
    id: mcpId(d),
    own: failed ? "fail" : d.seen.ok === null ? "unchecked" : d.seen.ok ? "ok" : "fail",
    confirmed: true,
    fact: failed ? real.fact : d.seen.fact,
    hints: [],
    // Корень — только отказ настоящей проверки; закрытый порт и «не запущен» тревогой не считаются.
    isRoot: !!failed,
    blockedBy: [],
    checks,
    container: null,
    measuredAt: iso,
    since: mcpSince,
  };
}
const mcpStates = (at = Date.now()) => Object.fromEntries(MCP_DEFS.map((d) => [mcpId(d), mcpState(d, at)]));

async function mcpProbe(id: string): Promise<McpProbe> {
  const d = MCP_DEFS.find((x) => mcpId(x) === id);
  if (!d) throw `сервера «${id}» нет в настройках Claude`;
  await new Promise((r) => setTimeout(r, 1500));
  mcpReal.set(id, { ...d.onProbe, at: Date.now() });
  emit("pult://states", { cycle, states: [mcpState(d, Date.now())] });
  return d.onProbe;
}

async function ollamaAsk(model: string): Promise<OllamaAnswer> {
  await new Promise((r) => setTimeout(r, 1800));
  // Модель эмбеддингов отвечать текстом не умеет — так в макете видна и ошибка сервера «как есть».
  if (model.includes("embed")) return { ok: false, fact: `"${model}" does not support generate`, seconds: null };
  const cold = model !== "qwen3:8b";
  return { ok: true, fact: cold ? "ответила за 6.4 с, из них загрузка в память — 5.8 с" : "ответила за 0.8 с", seconds: cold ? 6.4 : 0.8 };
}

// ───────────── Состояние макета ─────────────

type Mode = "run" | "no-inventory" | "empty" | "loading" | "inventory-error";
const param = typeof location !== "undefined" ? new URLSearchParams(location.search).get("mock") : null;
const FROZEN: Record<string, number> = { ok: 0, containers: 1, host: 2, tunnel: 3, mixed: 4 };
let mode: Mode = (["no-inventory", "empty", "loading", "inventory-error"] as const).find((m) => m === param) ?? "run";
const frozen: number | null = param && param in FROZEN ? FROZEN[param] : mode === "inventory-error" ? 1 : null;

const TICK_MS = 3500;
const INVENTORY_PATH = "~/pult-inventory-example";
let settings: Settings = { inventoryPath: mode === "no-inventory" ? null : INVENTORY_PATH, notifications: true, autostart: false };

let cycle = 1;
let curStep = 0;
let stepAt = Date.now();
let states: Record<string, NodeState> = {};
const history: Record<string, HistoryEntry[]> = {};
let started = false;
let streamSeq = 0;

const listeners = new Map<string, Set<(p: unknown) => void>>();
const emit = (event: string, payload: unknown) => listeners.get(event)?.forEach((cb) => cb(payload));

const stepIndex = (c: number) => frozen ?? Math.floor((c - 1) / 2) % STEPS.length;
const record = (s: NodeState) => (history[s.id] ??= []).push({ at: s.since ?? new Date().toISOString(), own: s.own, fact: s.fact });

// Скрытые узлы макет оценивает, как ядро, но в снимок и события не отдаёт (контракт, раздел 3).
const hiddenIds = new Set(DEFS.filter((d) => d.hidden).map((d) => d.id));
const shown = (all: Record<string, NodeState>) => Object.fromEntries(Object.entries(all).filter(([id]) => !hiddenIds.has(id)));

const views: NodeView[] = DEFS.filter((d) => !d.hidden).map((d) => ({
  id: d.id, title: d.title, kind: d.kind, group: d.group ?? null, project: d.project ?? null, on: d.on ?? null,
  dependsOn: d.dependsOn ?? [], access: d.access ?? null, links: d.links ?? [],
  undeclared: !!d.undeclared, hasLogs: !!d.container,
}));

function start() {
  if (started) return;
  started = true;
  if (mode !== "run" && mode !== "inventory-error") return;
  begin();
}

function begin() {
  const now = Date.now();
  curStep = stepIndex(cycle);
  stepAt = now;
  states = evaluate(STEPS[curStep].faults, now);
  Object.values(states).forEach((s, i) => {
    s.since = new Date(now - (45 + i * 13) * MIN).toISOString();
    // Прошлые переходы нужны вкладке «История»: у каждого четвёртого узла была короткая просадка.
    const h = (history[s.id] ??= []);
    h.push({ at: new Date(now - 30 * 3_600_000).toISOString(), own: "ok", fact: s.own === "ok" ? s.fact : okFact(byId.get(s.id)!, i) });
    if (i % 4 === 1) {
      h.push({ at: new Date(now - 12 * 3_600_000).toISOString(), own: "fail", fact: "порт 22: таймаут 3 с" });
      h.push({ at: new Date(now - 12 * 3_600_000 + 40 * MIN).toISOString(), own: "ok", fact: okFact(byId.get(s.id)!, i) });
    }
    if (s.own !== "ok" || h.at(-1)?.fact !== s.fact) record(s);
  });
  setInterval(tick, TICK_MS);
}

function tick() {
  cycle += 1;
  const idx = stepIndex(cycle);
  if (idx !== curStep) {
    curStep = idx;
    stepAt = Date.now();
  }
  const next = evaluate(STEPS[idx].faults, stepAt);
  const changed: NodeState[] = [];
  for (const [id, cur] of Object.entries(next)) {
    const prev = states[id];
    const same = prev.own === cur.own && prev.fact === cur.fact && prev.isRoot === cur.isRoot && prev.blockedBy.join() === cur.blockedBy.join();
    if (!same) {
      // Новое состояние сначала «неподтверждённое», подтверждается вторым циклом подряд (контракт, п. 7).
      cur.confirmed = false;
      cur.since = new Date().toISOString();
      states[id] = cur;
      record(cur);
      changed.push(cur);
    } else if (!prev.confirmed) {
      states[id] = { ...prev, confirmed: true };
      changed.push(states[id]);
    }
  }
  if (mode === "run" || mode === "inventory-error") emit("pult://states", { cycle, states: changed.filter((s) => !hiddenIds.has(s.id)) });
}

function snapshot(): Snapshot {
  const visible = mode === "run" || mode === "inventory-error";
  const hasPath = settings.inventoryPath !== null;
  return {
    cycle,
    takenAt: new Date().toISOString(),
    inventory: {
      path: settings.inventoryPath,
      commit: hasPath ? "3f9c2ab" : null,
      loadedAt: hasPath ? new Date(Date.now() - 20 * MIN).toISOString() : null,
      error: mode === "inventory-error"
        ? "строка 41: узел «api» зависит от «база-данных», узла с таким id нет. Действует последний принятый инвентарь."
        : null,
      warnings: visible ? ["узел «кэш»: незнакомое поле «приоритет», оно проигнорировано"] : [],
    },
    // MCP-серверы ядро находит и без инвентаря — но одних их для карты мало (см. App).
    nodes: visible ? [...views, ...mcpViews] : mcpViews,
    states: visible ? { ...shown(states), ...mcpStates() } : mcpStates(),
  };
}

const LOG_LINES = [
  "GET /health 200 3ms",
  "GET /api/items?page=2 200 41ms",
  "POST /api/session 201 87ms",
  "cache miss key=item:48121",
  "job 8812 done in 42ms",
  "pool: 4/20 connections busy",
  "slow query 812ms: select * from items where owner = $1",
  "GET /metrics 200 2ms",
];
const clock = (t: number) => new Date(t).toLocaleTimeString("ru-RU");
function logLine(t: number): string {
  const level = Math.random() < 0.08 ? "WARN " : "INFO ";
  return `${clock(t)} ${level} ${LOG_LINES[Math.floor(Math.random() * LOG_LINES.length)]}`;
}

function openLogs(id: string, tail: number): { streamId: string } {
  const streamId = `mock-${++streamSeq}`;
  const now = Date.now();
  const burst = Array.from({ length: tail }, (_, i) => logLine(now - (tail - i) * 1500));
  setTimeout(() => emit("pult://log", { streamId, lines: burst }), 40);
  const st = states[id];
  if (st?.own === "fail" && st.container) {
    // Контейнер лежит: логи заканчиваются и поток закрывается сам.
    setTimeout(() => {
      emit("pult://log", { streamId, lines: [`${clock(Date.now())} INFO  job 9120 started`] });
      emit("pult://log-end", { streamId, error: null });
    }, 400);
    return { streamId };
  }
  timers.set(streamId, setInterval(() => {
    const n = 1 + Math.floor(Math.random() * 3);
    emit("pult://log", { streamId, lines: Array.from({ length: n }, () => logLine(Date.now())) });
  }, 900));
  return { streamId };
}
const timers = new Map<string, ReturnType<typeof setInterval>>();

function recheck(id?: string) {
  setTimeout(() => {
    const at = new Date().toISOString();
    const ids = id ? [id] : Object.keys(shown(states));
    const local = Object.values(mcpStates()).filter((s) => !id || s.id === id);
    const upd = ids.filter((i) => states[i]).map((i) => {
      const cur = states[i];
      return (states[i] = {
        ...cur,
        measuredAt: cur.own === "unknown" ? null : at,
        checks: cur.checks.map((c) => ({ ...c, measuredAt: at, latencyMs: c.latencyMs === null ? null : jitter(c.latencyMs) })),
      });
    });
    emit("pult://states", { cycle, states: [...upd, ...local] });
  }, 700);
}

async function call(cmd: string, args: Record<string, unknown> = {}): Promise<unknown> {
  start();
  await new Promise((r) => setTimeout(r, 120)); // как настоящий вызов: не мгновенно
  switch (cmd) {
    case "get_snapshot":
      if (mode === "loading") return new Promise(() => {});
      return snapshot();
    case "recheck":
      recheck(args.id as string | undefined);
      return null;
    case "get_history":
      return [...(history[args.id as string] ?? [])].reverse().slice(0, (args.limit as number) ?? 50);
    case "open_logs":
      return openLogs(args.id as string, (args.tail as number) ?? 200);
    case "close_logs": {
      const t = timers.get(args.streamId as string);
      if (t) clearInterval(t);
      timers.delete(args.streamId as string);
      return null;
    }
    case "get_settings":
      return settings;
    case "set_settings": {
      settings = args.settings as Settings;
      if ((mode === "no-inventory" || mode === "empty") && settings.inventoryPath) {
        mode = "run";
        begin();
        setTimeout(() => emit("pult://snapshot", snapshot()), 300);
      }
      return settings;
    }
    case "open_url":
      window.open(args.url as string, "_blank", "noopener,noreferrer");
      return null;
    case "get_update_blocker":
      return null;
    case "ollama_ask":
      return ollamaAsk(args.model as string);
    case "mcp_probe":
      return mcpProbe(args.id as string);
    case "check_environment": {
      const res: EnvCheck[] = [
        { name: "ssh", ok: true, detail: "/usr/bin/ssh (OpenSSH_9.8)" },
        { name: "git", ok: true, detail: "/usr/bin/git 2.45" },
        { name: "ключ ssh", ok: true, detail: "~/.ssh/id_ed25519 найден, агент запущен" },
        settings.inventoryPath
          ? { name: "доступ к инвентарю", ok: true, detail: `${settings.inventoryPath}: каталог читается, инвентарь.yaml найден` }
          : { name: "доступ к инвентарю", ok: false, detail: "каталог не задан" },
      ];
      return res;
    }
    default:
      throw `Command ${cmd} not found`;
  }
}

export const mockBackend: Backend = {
  call: (cmd, args) => call(cmd, args) as never,
  async listen(event, cb) {
    start();
    const set = listeners.get(event) ?? new Set();
    listeners.set(event, set);
    const fn = cb as (p: unknown) => void;
    set.add(fn);
    return () => void set.delete(fn);
  },
};
