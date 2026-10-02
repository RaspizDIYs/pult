// Как состояние узла превращается в то, что видит человек (таблица «Как интерфейс показывает
// состояние» из контракта) и общие помощники по графу зависимостей и времени.
import {
  Box,
  Cable,
  CircleCheck,
  CircleDashed,
  CircleHelp,
  CircleX,
  Clock,
  Container,
  Cpu,
  Globe,
  Layers,
  Link2Off,
  Server,
  type LucideIcon,
} from "lucide-react";
import type { NodeState, NodeView, OwnStatus } from "./pult";

// ───────────── Тон состояния ─────────────

export type Tone = "ok" | "root" | "cascade" | "unknown" | "stale" | "unchecked";

interface ToneStyle {
  label: string;
  Icon: LucideIcon;
  /** Плашка: фон, текст и рамка из токенов состояния (index.css). */
  chip: string;
  /** Карточка площадки на карте. */
  card: string;
  /** Цвет пунктира к узлу, когда выбран связанный с ним. */
  stroke: string;
  /** Заливка точки в легенде и в полосе сводки. */
  dot: string;
  /** Подложка блока в панели узла. */
  box: string;
}

// Классы записаны целиком: Tailwind собирает только то, что видит в исходниках дословно.
export const TONE: Record<Tone, ToneStyle> = {
  ok: {
    label: "Работает",
    Icon: CircleCheck,
    chip: "border-ok/40 bg-ok-bg text-ok-fg",
    card: "border-ok/45 bg-card",
    stroke: "var(--ok)",
    dot: "bg-ok",
    box: "border-ok/40 bg-ok-bg text-ok-fg",
  },
  root: {
    label: "Сломано",
    Icon: CircleX,
    chip: "border-root/60 bg-root text-white",
    card: "border-2 border-root bg-root-bg shadow-[0_0_0_3px_color-mix(in_oklch,var(--root)_22%,transparent)]",
    stroke: "var(--root)",
    dot: "bg-root",
    box: "border-root/50 bg-root-bg text-root-fg",
  },
  cascade: {
    label: "Недоступен",
    Icon: Link2Off,
    chip: "border-cascade/60 bg-cascade-bg text-cascade-fg",
    card: "border-cascade/80 bg-cascade-bg",
    stroke: "var(--cascade)",
    dot: "bg-cascade",
    box: "border-cascade/50 bg-cascade-bg text-cascade-fg",
  },
  unknown: {
    label: "Неизвестно",
    Icon: CircleHelp,
    chip: "border-unknown/50 bg-unknown-bg text-unknown-fg",
    card: "border-unknown/55 bg-unknown-bg",
    stroke: "var(--unknown)",
    dot: "bg-unknown",
    box: "border-unknown/40 bg-unknown-bg text-unknown-fg",
  },
  stale: {
    label: "Устарело",
    Icon: Clock,
    chip: "border-stale/60 bg-stale-bg text-stale-fg",
    card: "border-stale/70 bg-stale-bg",
    stroke: "var(--stale)",
    dot: "bg-stale",
    box: "border-stale/50 bg-stale-bg text-stale-fg",
  },
  unchecked: {
    label: "Не проверяется",
    Icon: CircleDashed,
    chip: "border-dashed border-unchecked bg-transparent text-unchecked-fg",
    card: "border-dashed border-unchecked bg-transparent",
    stroke: "var(--unchecked)",
    dot: "bg-unchecked",
    box: "border-dashed border-unchecked text-unchecked-fg",
  },
};

export function toneOf(s: NodeState | undefined): Tone {
  if (!s) return "unknown"; // ядро ещё не прислало состояние этого узла
  switch (s.own) {
    case "ok":
      return "ok";
    case "fail":
      return s.isRoot ? "root" : "cascade";
    case "stale":
      return "stale";
    case "unchecked":
      return "unchecked";
    default:
      return "unknown";
  }
}

/** Тон для отдельной записи истории: там известно только `own`. */
export const toneOfOwn = (own: OwnStatus): Tone => (own === "fail" ? "root" : own);

export interface CardText {
  fact: string;
  /** «причина выше: …» — отдельной строкой, чтобы не потерялась за длинным фактом. */
  cause: string | null;
}

export function cardText(s: NodeState | undefined, titleOf: (id: string) => string): CardText {
  if (!s) return { fact: "ждём первых данных от ядра", cause: null };
  const cause = s.blockedBy.length
    ? `причина выше: ${s.blockedBy.slice(0, 2).map(titleOf).join(", ")}${s.blockedBy.length > 2 ? ` и ещё ${s.blockedBy.length - 2}` : ""}`
    : null;
  switch (s.own) {
    case "fail":
      return { fact: s.fact, cause };
    case "unknown":
    case "stale":
      if (cause) return { fact: "неизвестно", cause };
      return s.own === "stale" ? { fact: `данные от ${fmtTime(s.measuredAt)}`, cause: null } : { fact: s.fact, cause: null };
    case "unchecked":
      return { fact: "не проверяется", cause: null };
    default:
      return { fact: s.fact, cause: null };
  }
}

// ───────────── Виды узлов ─────────────

// Ядро присылает `вид` как в инвентаре; английские имена оставлены на случай, если оно их нормализует.
const KINDS: Record<string, { label: string; Icon: LucideIcon }> = {
  внешнее: { label: "Внешнее", Icon: Globe },
  external: { label: "Внешнее", Icon: Globe },
  хост: { label: "Хост", Icon: Server },
  host: { label: "Хост", Icon: Server },
  вм: { label: "Виртуальная машина", Icon: Cpu },
  vm: { label: "Виртуальная машина", Icon: Cpu },
  контейнер: { label: "Контейнер", Icon: Container },
  container: { label: "Контейнер", Icon: Container },
  сервис: { label: "Сервис", Icon: Layers },
  service: { label: "Сервис", Icon: Layers },
  туннель: { label: "Туннель", Icon: Cable },
  tunnel: { label: "Туннель", Icon: Cable },
};
export const kindInfo = (kind: string) => KINDS[kind] ?? { label: kind, Icon: Box };

// ───────────── Граф зависимостей ─────────────

export interface Graph {
  /** Обязательные прямые предки: `на` плюс `зависит_от`. */
  parents: Map<string, string[]>;
  children: Map<string, string[]>;
}

export function buildGraph(nodes: NodeView[]): Graph {
  const ids = new Set(nodes.map((n) => n.id));
  const parents = new Map<string, string[]>();
  const children = new Map<string, string[]>();
  for (const n of nodes) {
    const ps = [...new Set([...(n.on ? [n.on] : []), ...n.dependsOn])].filter((p) => ids.has(p) && p !== n.id);
    parents.set(n.id, ps);
    for (const p of ps) children.set(p, [...(children.get(p) ?? []), n.id]);
  }
  return { parents, children };
}

/** Всё, что транзитивно зависит от узла (без него самого). */
export function dependentsOf(g: Graph, id: string): Set<string> {
  const seen = new Set<string>();
  const stack = [...(g.children.get(id) ?? [])];
  while (stack.length) {
    const cur = stack.pop()!;
    if (seen.has(cur)) continue;
    seen.add(cur);
    stack.push(...(g.children.get(cur) ?? []));
  }
  return seen;
}

// ───────────── Сводка ─────────────

export interface Summary {
  /** Узлы, про которые вообще можно сказать «работает»: без «не проверяется». */
  observable: number;
  working: number;
  roots: NodeView[];
  counts: Record<Tone, number>;
  undeclared: number;
}

export function summarize(nodes: NodeView[], states: Record<string, NodeState>): Summary {
  const counts: Record<Tone, number> = { ok: 0, root: 0, cascade: 0, unknown: 0, stale: 0, unchecked: 0 };
  const roots: NodeView[] = [];
  for (const n of nodes) {
    const t = toneOf(states[n.id]);
    counts[t]++;
    if (t === "root") roots.push(n);
  }
  return {
    observable: nodes.length - counts.unchecked,
    working: counts.ok,
    roots,
    counts,
    undeclared: nodes.filter((n) => n.undeclared).length,
  };
}

// ───────────── Время ─────────────

const hhmm = new Intl.DateTimeFormat("ru-RU", { hour: "2-digit", minute: "2-digit" });
const hhmmss = new Intl.DateTimeFormat("ru-RU", { hour: "2-digit", minute: "2-digit", second: "2-digit" });
const dayTime = new Intl.DateTimeFormat("ru-RU", { day: "2-digit", month: "2-digit", hour: "2-digit", minute: "2-digit" });

export const fmtTime = (iso: string | null | undefined) => (iso ? hhmm.format(new Date(iso)) : "—");
export const fmtTimeSec = (iso: string | null | undefined) => (iso ? hhmmss.format(new Date(iso)) : "—");
export const fmtDayTime = (iso: string | null | undefined) => (iso ? dayTime.format(new Date(iso)) : "—");

export function fmtDuration(ms: number): string {
  const min = Math.max(0, Math.round(ms / 60_000));
  if (min < 1) return "меньше минуты";
  if (min < 60) return `${min} мин`;
  const h = Math.floor(min / 60);
  if (h < 24) return min % 60 ? `${h} ч ${min % 60} мин` : `${h} ч`;
  return `${Math.floor(h / 24)} дн.`;
}

export function ago(iso: string | null | undefined, now: number): string {
  if (!iso) return "—";
  const ms = now - new Date(iso).getTime();
  if (ms < 45_000) return "только что";
  return `${fmtDuration(ms)} назад`;
}

/** Склонение по числу: plural(2, "узел", "узла", "узлов"). */
export function plural(n: number, one: string, few: string, many: string): string {
  const m10 = n % 10;
  const m100 = n % 100;
  if (m10 === 1 && m100 !== 11) return one;
  if (m10 >= 2 && m10 <= 4 && (m100 < 12 || m100 > 14)) return few;
  return many;
}
