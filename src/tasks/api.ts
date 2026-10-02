// Панель задач: типы из docs/контракт.md (раздел 6), доступ к ядру и загрузка с кэшем.
// Свой выбор между ядром и макетом, как в lib/pult: экран задач не трогает макет карты.
import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useRef, useState } from "react";
import { errText, isTauri } from "@/lib/pult";

export type TaskState = "todo" | "doing" | "review" | "done" | "cancelled" | "unknown";
export type Problem = "noUrl" | "noToken" | "auth" | "unavailable";

export interface Task {
  key: string | null;
  keyNum: number | null;
  epicNum: number | null;
  state: TaskState;
  title: string;
  priority: number | null;
  kind: string | null;
  isEpic: boolean;
  who: string | null;
  project: string;
  tags: string[];
  created: string | null;
  updated: string | null;
  takenAt: string | null;
  doneAt: string | null;
}

export interface TaskRef {
  key: string | null;
  title: string;
  state: TaskState;
  priority: number | null;
  who: string | null;
}

export interface TaskDetail extends Task {
  body: string | null;
  journal: string[];
  parent: TaskRef | null;
  kids: TaskRef[];
}

export interface Epic {
  key: string | null;
  keyNum: number | null;
  title: string;
  state: TaskState;
  project: string;
  total: number;
  done: number;
}

export interface Overview {
  open: number;
  total: number;
  closedWeek: number;
  closedMonth: number;
  byState: { todo: number; doing: number; review: number; done: number; cancelled: number };
}

export interface Fetched<T> {
  data: T | null;
  fetchedAt: string | null;
  stale: boolean;
  problem: Problem | null;
  error: string | null;
}

export interface PanelConfig {
  url: string | null;
  tokenSet: boolean;
  tokenError: string | null;
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (isTauri) return invoke<T>(cmd, args);
  return (await import("./mock")).call(cmd, args ?? {}) as Promise<T>;
}

// Настройки меняются в диалоге, а доска живёт на вкладке: подписка на смену вместо проброса
// колбэка через App и диалог настроек.
const configListeners = new Set<() => void>();
const changed = (c: PanelConfig) => {
  configListeners.forEach((cb) => cb());
  return c;
};

export const panel = {
  config: () => call<PanelConfig>("panel_config"),
  setUrl: (url: string | null) => call<PanelConfig>("panel_set_url", { url }).then(changed),
  // null — удалить токен из связки ключей. Обратно токен не возвращается никогда.
  setToken: (token: string | null) => call<PanelConfig>("panel_set_token", { token }).then(changed),
  tasks: (mine: boolean) => call<Fetched<Task[]>>("panel_tasks", { mine }),
  task: (key: string) => call<Fetched<TaskDetail>>("panel_task", { key }),
  projects: () => call<Fetched<string[]>>("panel_projects"),
  epics: () => call<Fetched<Epic[]>>("panel_epics"),
  overview: () => call<Fetched<Overview>>("panel_overview"),
  onConfigChange(cb: () => void) {
    configListeners.add(cb);
    return () => void configListeners.delete(cb);
  },
};

// Панель не шлёт событий: опрашиваем, пока вкладка открыта. Две минуты — чтобы «обновлено»
// не врало и возвращение панели было видно без кнопки.
const REFRESH_MS = 2 * 60_000;

/** Задачи и всё, что нужно доске. Загружается, только пока вкладка открыта. */
export function useTasks(active: boolean) {
  const [mine, setMine] = useState(false);
  const [tasks, setTasks] = useState<Fetched<Task[]> | null>(null);
  const [projects, setProjects] = useState<Fetched<string[]> | null>(null);
  const [epics, setEpics] = useState<Fetched<Epic[]> | null>(null);
  const [overview, setOverview] = useState<Fetched<Overview> | null>(null);
  const [loading, setLoading] = useState(false);
  // Ядро не ответило вовсе — не путать с недоступной панелью, у той свой `problem`.
  const [error, setError] = useState<string | null>(null);
  const seq = useRef(0);
  const loadedAt = useRef(0);

  const load = useCallback(async () => {
    const n = ++seq.current;
    setLoading(true);
    try {
      // Список — главное; остальное дополняет его и падать вместе с ним не должно.
      const [t, p, e, o] = await Promise.allSettled([panel.tasks(mine), panel.projects(), panel.epics(), panel.overview()]);
      if (n !== seq.current) return; // «мои» переключили, пока шёл запрос
      if (t.status === "fulfilled") {
        setTasks(t.value);
        setError(null);
      } else setError(errText(t.reason));
      if (p.status === "fulfilled") setProjects(p.value);
      if (e.status === "fulfilled") setEpics(e.value);
      if (o.status === "fulfilled") setOverview(o.value);
      loadedAt.current = Date.now();
    } finally {
      if (n === seq.current) setLoading(false);
    }
  }, [mine]);

  // Переключили «мои» — грузим сразу; вернулись на вкладку — только если данные несвежие.
  const loadedMine = useRef(mine);
  useEffect(() => {
    if (!active) return;
    if (loadedMine.current !== mine || Date.now() - loadedAt.current > REFRESH_MS / 2) void load();
    loadedMine.current = mine;
    const id = setInterval(() => void load(), REFRESH_MS);
    return () => clearInterval(id);
  }, [active, mine, load]);

  useEffect(() => panel.onConfigChange(() => void load()), [load]);

  return { tasks, projects, epics, overview, loading, error, mine, setMine, reload: load };
}

// ───────────── Подписи ─────────────

/** Колонки доски слева направо по ходу работы; отменённые — отдельно и по желанию. */
export const COLUMNS: { state: TaskState; label: string }[] = [
  { state: "todo", label: "Не начата" },
  { state: "doing", label: "В работе" },
  { state: "review", label: "На ревью" },
  { state: "done", label: "Готово" },
];
export const CANCELLED = { state: "cancelled" as const, label: "Отменены" };
export const OTHER = { state: "unknown" as const, label: "Другое" };

export const STATE_LABEL: Record<TaskState, string> = {
  todo: "не начата",
  doing: "в работе",
  review: "на ревью",
  done: "готово",
  cancelled: "отменена",
  unknown: "состояние неизвестно",
};

/** Ключ родителя: номер ищем в том же проекте — нумерация у каждого проекта своя. */
export const refKey = (project: string, num: number) => `${project}#${num}`;
