// Раскладка карты: слои по глубине зависимостей слева направо, внутри слоя — по группам,
// затем ближе к предкам. Своя и простая: пока 30 узлов, библиотека раскладки не нужна.
// ponytail: порядок внутри слоя — жадный, пересечения связей не минимизируются; при сотнях узлов взять dagre/elk.
import { buildGraph } from "./model";
import type { NodeView } from "./pult";

export const CARD_W = 216;
export const CARD_H = 104;
const GAP_X = 56; // между слоями: здесь идут связи
const GAP_Y = 12;
const GROUP_GAP = 26; // заметнее зазор при смене группы

export type Positions = Record<string, { x: number; y: number }>;

export function autoLayout(nodes: NodeView[]): Positions {
  const { parents } = buildGraph(nodes);

  const depth = new Map<string, number>();
  const visiting = new Set<string>();
  const depthOf = (id: string): number => {
    const known = depth.get(id);
    if (known !== undefined) return known;
    // Ядро циклы в инвентаре не принимает, но интерфейс из-за этого зависать не должен.
    if (visiting.has(id)) return 0;
    visiting.add(id);
    const d = Math.max(-1, ...(parents.get(id) ?? []).map(depthOf)) + 1;
    visiting.delete(id);
    depth.set(id, d);
    return d;
  };

  const layers: { n: NodeView; order: number }[][] = [];
  nodes.forEach((n, order) => {
    (layers[depthOf(n.id)] ??= []).push({ n, order });
  });

  // Порядок групп — как они впервые встречаются в инвентаре.
  const groupRank = new Map<string, number>();
  for (const n of nodes) if (!groupRank.has(n.group ?? "")) groupRank.set(n.group ?? "", groupRank.size);

  const centerY = new Map<string, number>();
  const pos: Positions = {};

  layers.forEach((layer, d) => {
    const items = layer.map(({ n, order }) => {
      const ys = (parents.get(n.id) ?? []).map((p) => centerY.get(p)).filter((y): y is number => y !== undefined);
      return { n, order, want: ys.length ? ys.reduce((a, b) => a + b, 0) / ys.length : null };
    });
    items.sort(
      (a, b) =>
        groupRank.get(a.n.group ?? "")! - groupRank.get(b.n.group ?? "")! ||
        (a.want ?? Infinity) - (b.want ?? Infinity) ||
        a.order - b.order,
    );

    // Каждый узел тянется к середине своих предков, но не наезжает на предыдущего.
    let prev: { y: number; group: string } | null = null;
    const placed = items.map(({ n, want }) => {
      const group = n.group ?? "";
      const floor: number = prev ? prev.y + CARD_H + (group === prev.group ? GAP_Y : GROUP_GAP) : -Infinity;
      const y = Math.max(want === null ? (prev ? floor : 0) : want - CARD_H / 2, floor);
      prev = { y, group };
      return { n, want, y };
    });
    // Слой целиком сдвигаем так, чтобы в среднем он оказался напротив своих предков.
    const pulled = placed.filter((p) => p.want !== null);
    const shift = pulled.length ? pulled.reduce((a, p) => a + (p.want! - CARD_H / 2 - p.y), 0) / pulled.length : 0;
    for (const p of placed) {
      p.y += shift;
      pos[p.n.id] = { x: d * (CARD_W + GAP_X), y: p.y };
      centerY.set(p.n.id, p.y + CARD_H / 2);
    }
  });

  return pos;
}

// ───────────── Ручные перестановки ─────────────

const KEY = "pult.map.positions.v1";

// localStorage может быть недоступен (приватный режим, запрет в webview) — карта работает и без него.
export function loadMoved(): Positions {
  try {
    const raw = localStorage.getItem(KEY);
    const parsed: unknown = raw ? JSON.parse(raw) : {};
    return parsed && typeof parsed === "object" ? (parsed as Positions) : {};
  } catch {
    return {};
  }
}

export function saveMoved(moved: Positions): void {
  try {
    if (Object.keys(moved).length) localStorage.setItem(KEY, JSON.stringify(moved));
    else localStorage.removeItem(KEY);
  } catch {
    /* запомнить не вышло — не страшно, при следующем запуске будет автораскладка */
  }
}
