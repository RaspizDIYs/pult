// Слой данных интерфейса. Типы — из docs/контракт.md (разделы 2 и 3), от них не отступаем.
// Под Tauri зовём настоящие команды и события ядра; в браузере (npm run dev без окна Tauri)
// работает встроенный макет из ./mock — иначе интерфейс нельзя ни смотреть, ни править без ядра.
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export type OwnStatus = "ok" | "fail" | "unknown" | "stale" | "unchecked";

export interface CheckResult {
  kind: "tcp" | "http" | "container" | "vm" | "collect";
  target: string;
  from: string | null;
  ok: boolean | null;
  fact: string;
  latencyMs: number | null;
  measuredAt: string;
}

export interface ContainerFacts {
  state: string;
  exitCode: number | null;
  oomKilled: boolean | null;
  health: string | null;
  restartCount: number | null;
  startedAt: string | null;
  finishedAt: string | null;
  image: string | null;
}

export interface NodeState {
  id: string;
  own: OwnStatus;
  confirmed: boolean;
  fact: string;
  hints: string[];
  isRoot: boolean;
  blockedBy: string[];
  checks: CheckResult[];
  container: ContainerFacts | null;
  measuredAt: string | null;
  since: string | null;
}

export interface NodeView {
  id: string;
  title: string;
  kind: string;
  group: string | null;
  on: string | null;
  dependsOn: string[];
  access: { how: string | null; secret: string | null; who: string[] } | null;
  links: { title: string; url: string }[];
  undeclared: boolean;
  hasLogs: boolean;
}

export interface InventoryInfo {
  path: string | null;
  commit: string | null;
  loadedAt: string | null;
  error: string | null;
  warnings: string[];
}

export interface Snapshot {
  cycle: number;
  takenAt: string;
  inventory: InventoryInfo;
  nodes: NodeView[];
  states: Record<string, NodeState>;
}

export interface Settings {
  inventoryPath: string | null;
  notifications: boolean;
  autostart: boolean;
}

export interface EnvCheck {
  name: string;
  ok: boolean;
  detail: string;
}

export interface HistoryEntry {
  at: string;
  own: OwnStatus;
  fact: string;
}

export interface StatesEvent {
  cycle: number;
  states: NodeState[];
}
export interface LogEvent {
  streamId: string;
  lines: string[];
}
export interface LogEndEvent {
  streamId: string;
  error: string | null;
}

export interface Backend {
  call<T>(cmd: string, args?: Record<string, unknown>): Promise<T>;
  listen<T>(event: string, cb: (payload: T) => void): Promise<() => void>;
}

export const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

const tauri: Backend = {
  call: (cmd, args) => invoke(cmd, args),
  listen: <T>(event: string, cb: (payload: T) => void) => listen<T>(event, (e) => cb(e.payload)),
};

// Макет грузится отдельным куском и только вне Tauri: в приложении его нет в памяти.
let mockBackend: Promise<Backend> | null = null;
function backend(): Promise<Backend> {
  if (isTauri) return Promise.resolve(tauri);
  mockBackend ??= import("./mock").then((m) => m.mockBackend);
  return mockBackend;
}

export function errText(e: unknown): string {
  if (e instanceof Error) return e.message;
  if (typeof e === "string") return e;
  try {
    return JSON.stringify(e);
  } catch {
    return String(e);
  }
}

// Ядро пишется параллельно: команды, которой ещё нет, достаточно показать понятной строкой
// там, где она нужна, — остальной интерфейс при этом работает.
async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await (await backend()).call<T>(cmd, args);
  } catch (e) {
    const text = errText(e);
    throw new Error(
      /not found|not allowed|unknown command/i.test(text)
        ? `ядро пока не поддерживает команду «${cmd}» (${text})`
        : text,
    );
  }
}

async function on<T>(event: string, cb: (payload: T) => void): Promise<() => void> {
  return (await backend()).listen<T>(event, cb);
}

export const pult = {
  getSnapshot: () => call<Snapshot>("get_snapshot"),
  recheck: (id?: string) => call<void>("recheck", { id }),
  getHistory: (id: string, limit = 50) => call<HistoryEntry[]>("get_history", { id, limit }),
  openLogs: (id: string, tail = 200) => call<{ streamId: string }>("open_logs", { id, tail }),
  closeLogs: (streamId: string) => call<void>("close_logs", { streamId }),
  getSettings: () => call<Settings>("get_settings"),
  setSettings: (settings: Settings) => call<Settings>("set_settings", { settings }),
  checkEnvironment: () => call<EnvCheck[]>("check_environment"),
  getUpdateBlocker: () => call<string | null>("get_update_blocker"),
  // Ссылки из инвентаря открывает ядро в системном браузере: внутри окна Tauri `<a target>` не работает.
  openUrl: (url: string) => call<void>("open_url", { url }),

  onSnapshot: (cb: (s: Snapshot) => void) => on("pult://snapshot", cb),
  onStates: (cb: (e: StatesEvent) => void) => on("pult://states", cb),
  onLog: (cb: (e: LogEvent) => void) => on("pult://log", cb),
  onLogEnd: (cb: (e: LogEndEvent) => void) => on("pult://log-end", cb),
};
