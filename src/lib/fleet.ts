// Рой агентов: типы из docs/контракт.md (раздел 5), доступ к ядру и подписи для людей.
// Свой выбор между ядром и макетом, как в ./pult: экран роя не трогает макет карты, и наоборот.
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { info } from "@tauri-apps/plugin-log";
import { useEffect, useState } from "react";
import { fmtDuration } from "./model";
import { errText, isTauri } from "./pult";

export type HubState = "off" | "connecting" | "ok" | "unavailable";
export type Presence = "online" | "offline" | "unknown";
export type LockProblem = "offline" | "overdue" | "silent";

export interface FleetSession {
  id: string;
  name: string | null;
  machine: string | null;
  local: boolean;
  presence: Presence;
  silent: boolean;
  lastSeenAt: string | null;
  cwd: string | null;
  note: string | null;
}

export interface FleetLock {
  key: string;
  resource: string;
  scope: "fleet" | "local";
  repo: string | null;
  session: string | null;
  since: string | null;
  ttlMin: number | null;
  units: number;
  command: string | null;
  cwd: string | null;
  queue: { session: string; since: string; reason: string | null }[];
  problem: LockProblem | null;
}

export interface FleetResource {
  name: string;
  label: string | null;
  capacity: number;
  used: number;
}

export interface FleetClaim {
  file: string;
  repo: string | null;
  sessions: { session: string; since: string }[];
  overlap: boolean;
}

export interface FleetNote {
  session: string;
  text: string;
  task: string | null;
  cwd: string | null;
  at: string;
}

export type TaskStatus = "TODO" | "IN_PROGRESS" | "DONE" | "FAILED";

export interface FleetTask {
  id: string;
  title: string;
  status: TaskStatus;
  priority: string;
  executor: string | null;
  machine: string | null;
  session: string | null;
  node: string | null;
  attempts: number;
  createdAt: string | null;
  startedAt: string | null;
  finishedAt: string | null;
  note: string | null;
}

export interface FleetRun {
  resource: string;
  session: string | null;
  command: string | null;
  cwd: string | null;
  startedAt: string | null;
  background: boolean;
}

export interface FleetView {
  takenAt: string;
  hub: { state: HubState; address: string | null; error: string | null; lastOkAt: string | null };
  machine: string | null;
  sessions: FleetSession[];
  locks: FleetLock[];
  fleetResources: FleetResource[];
  claims: FleetClaim[];
  board: FleetNote[];
  tasks: FleetTask[];
  local: { dir: string; found: boolean; resources: FleetResource[]; runs: FleetRun[]; errors: string[] };
  problems: number;
}

const mock = () => import("./fleet-mock");

export async function getFleet(): Promise<FleetView> {
  return isTauri ? invoke<FleetView>("get_fleet") : (await mock()).getFleet();
}

async function onFleet(cb: (v: FleetView) => void): Promise<() => void> {
  return isTauri ? listen<FleetView>("pult://fleet", (e) => cb(e.payload)) : (await mock()).onFleet(cb);
}

/** Снимок роя плюс поток изменений. Подписываемся раньше, чем просим снимок, чтобы не пропустить событие между ними. */
export function useFleet() {
  const [view, setView] = useState<FleetView | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let dead = false;
    let off: (() => void) | null = null;
    let first = true;
    onFleet((v) => {
      // Первое событие — в лог приложения: так видно, что поток от ядра до окна работает.
      if (first && isTauri) void info(`окно: первое pult://fleet — хаб ${v.hub.state}, сессий ${v.sessions.length}, проблем ${v.problems}`).catch(() => {});
      first = false;
      setView(v);
      setError(null);
    }).then(
      (o) => (dead ? o() : (off = o)),
      (e) => setError(`не удалось подписаться на рой: ${errText(e)}`),
    );
    getFleet().then(
      (v) => !dead && setView((prev) => prev ?? v),
      (e) => {
        // console.warn в окне Tauri уходит в лог приложения (main.tsx).
        console.warn(`get_fleet: ${errText(e)}`);
        if (!dead) setError(errText(e));
      },
    );
    return () => {
      dead = true;
      off?.();
    };
  }, []);
  return { view, error };
}

// ───────────── Подписи ─────────────

export const shortId = (id: string) => id.slice(0, 8);

/** Служба подписки машины — не собеседник, а почтовый ящик; в счёт сессий её не берём (как `fleet who`). */
export const isService = (id: string) => /-relay-service$/.test(id);

/** Последняя папка пути: по ней сессию без имени узнают быстрее, чем по идентификатору. */
export const folder = (path: string | null) => path?.split(/[\\/]/).filter(Boolean).pop() ?? null;

/**
 * Как назвать сессию человеку. Имя — если она представилась; иначе машина и папка:
 * «4b9c» никто не помнит, а «ноут-б, shop» узнают сразу. Идентификатор всё равно
 * показываем рядом — агенты называют сессии именно им.
 */
export function sessionTitle(s: FleetSession | undefined, id: string): string {
  if (!s) return shortId(id);
  if (s.name) return s.name;
  return [s.machine, folder(s.cwd)].filter(Boolean).join(" · ") || shortId(id);
}

export function silenceMs(s: FleetSession | undefined, now: number): number | null {
  return s?.lastSeenAt ? now - new Date(s.lastSeenAt).getTime() : null;
}

/** «в сети · молчит 5 мин», «не в сети · 3 ч», «неизвестно». */
export function presenceText(s: FleetSession | undefined, now: number): string {
  const ms = silenceMs(s, now);
  const quiet = ms === null ? "" : ms < 60_000 ? "активна" : `молчит ${fmtDuration(ms)}`;
  if (!s || s.presence === "unknown") return "в сети ли — неизвестно";
  if (s.presence === "offline") return ms === null ? "не в сети" : `не в сети · ${fmtDuration(ms)}`;
  return `в сети · ${quiet || "активна"}`;
}

export function problemText(l: FleetLock, holder: FleetSession | undefined, now: number): string {
  const held = l.since ? fmtDuration(now - new Date(l.since).getTime()) : "";
  const ms = silenceMs(holder, now);
  switch (l.problem) {
    case "offline":
      return `держатель не в сети${ms !== null ? ` уже ${fmtDuration(ms)}` : ""}, а замок висит ${held}`;
    case "overdue":
      return `держит ${held} при сроке ${l.ttlMin} мин`;
    case "silent":
      return `держатель молчит ${ms !== null ? fmtDuration(ms) : "давно"}`;
    default:
      return "";
  }
}

export const TASK_LABEL: Record<TaskStatus, string> = {
  IN_PROGRESS: "в работе",
  TODO: "в очереди",
  FAILED: "провалены",
  DONE: "готовы",
};

/** Та же подпись для одной задачи в строке списка. */
export const TASK_STATE: Record<TaskStatus, string> = { ...TASK_LABEL, FAILED: "провалена", DONE: "готова" };
