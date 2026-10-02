// Карта — дерево, как в проводнике: площадка → гостевые системы и папки проектов → сервисы.
// Дерево строится только по `на` (где размещён) и `проект`; зависимости в нём не участвуют —
// их видно, когда узел выбран. Раскладка своя: колонки слева направо, раскрытая ветка — список
// строк справа от себя, следующая площадка — ниже всего, что раскрыто у предыдущей.
import { toneOf, type Tone } from "./model";
import type { NodeState, NodeView } from "./pult";

export interface TreeItem {
  key: string;
  /** null — ветка без узла: «Сеть и внешнее» или папка проекта. */
  node: NodeView | null;
  title: string;
  parent: string | null;
  children: string[];
  /** id узлов поддерева вместе с самим узлом — для сводки ветки. */
  members: string[];
}

export interface Tree {
  items: Map<string, TreeItem>;
  /** Площадки сверху вниз. */
  top: string[];
}

// Ключи веток без узла собраны через «#»: id узлов в инвентаре его не содержат.
export const NET_KEY = "#сеть";
const NO_PROJECT = "Общее";
const MACHINE = new Set(["хост", "host", "вм", "vm"]);

export function buildTree(nodes: NodeView[]): Tree {
  const ids = new Set(nodes.map((n) => n.id));
  // Родитель, которого нет в снимке (скрыт или пропал), не должен прятать узел вместе с собой.
  const parentOf = (n: NodeView) => (n.on && n.on !== n.id && ids.has(n.on) ? n.on : null);
  const hosted = new Map<string, NodeView[]>();
  for (const n of nodes) {
    const p = parentOf(n);
    if (p) hosted.set(p, [...(hosted.get(p) ?? []), n]);
  }
  const items = new Map<string, TreeItem>();
  const put = (key: string, node: NodeView | null, title: string, parent: string | null) => {
    const item: TreeItem = { key, node, title, parent, children: [], members: node ? [node.id] : [] };
    items.set(key, item);
    return item;
  };

  const addNode = (n: NodeView, parent: string | null): string => {
    const item = put(n.id, n, n.title, parent);
    item.children = contents(n.id, hosted.get(n.id) ?? []);
    return n.id;
  };

  // Сначала гостевые системы (они раскрываются дальше), затем сервисы — по папкам проектов.
  // Папка из одного проекта ничего не говорит, поэтому тогда сервисы лежат прямо в ветке.
  const contents = (key: string, list: NodeView[]): string[] => {
    const guest = (n: NodeView) => MACHINE.has(n.kind) || hosted.has(n.id);
    const out = list.filter(guest).map((g) => addNode(g, key));
    const byProject = new Map<string, NodeView[]>();
    for (const s of list.filter((n) => !guest(n))) {
      const p = s.project?.trim() || NO_PROJECT;
      byProject.set(p, [...(byProject.get(p) ?? []), s]);
    }
    if (byProject.size <= 1) return [...out, ...[...byProject.values()].flat().map((s) => addNode(s, key))];
    const order = [...byProject.keys()].sort((a, b) => Number(a === NO_PROJECT) - Number(b === NO_PROJECT));
    for (const p of order) {
      const folder = put(`${key}#${p}`, null, p, key);
      folder.children = byProject.get(p)!.map((s) => addNode(s, folder.key));
      out.push(folder.key);
    }
    return out;
  };

  // Площадки — машины без `на`; всё прочее верхнего уровня (интернет, внешние сервисы,
  // туннели) — одной веткой, на месте первого из них.
  const top: string[] = [];
  const net: NodeView[] = [];
  for (const n of nodes.filter((n) => !parentOf(n))) {
    if (MACHINE.has(n.kind)) top.push(addNode(n, null));
    else {
      if (!net.length) top.push(NET_KEY);
      net.push(n);
    }
  }
  if (net.length) put(NET_KEY, null, "Сеть и внешнее", null).children = contents(NET_KEY, net);

  const collect = (key: string): string[] => {
    const item = items.get(key)!;
    item.members = [...item.members, ...item.children.flatMap(collect)];
    return item.members;
  };
  top.forEach(collect);
  return { items, top };
}

/** Ветки от площадки до узла, без него самого. */
export function ancestorsOf(tree: Tree, key: string): string[] {
  const out: string[] = [];
  for (let p = tree.items.get(key)?.parent ?? null; p; p = tree.items.get(p)?.parent ?? null) out.push(p);
  return out;
}

export interface BranchSummary {
  tone: Tone;
  text: string;
}

/** «12 работает» / «2 из 12 сломано» / «3 из 12 неизвестно»: «не проверяется» в счёт не идёт. */
export function summarizeBranch(members: string[], states: Record<string, NodeState>): BranchSummary {
  const c: Record<Tone, number> = { ok: 0, root: 0, cascade: 0, unknown: 0, stale: 0, unchecked: 0 };
  for (const id of members) c[toneOf(states[id])]++;
  const total = members.length - c.unchecked;
  const fail = c.root + c.cascade;
  const blind = c.unknown + c.stale;
  if (fail) return { tone: c.root ? "root" : "cascade", text: `${fail} из ${total} сломано` };
  if (blind) return { tone: c.unknown ? "unknown" : "stale", text: `${blind} из ${total} неизвестно` };
  if (!total) return { tone: "unchecked", text: "не проверяется" };
  return { tone: "ok", text: `${c.ok} работает` };
}

// ───────────── Раскладка ─────────────

export const CARD_W = 248;
export const CARD_H = 64;
export const LIST_W = 296;
export const ROW_H = 40;
export const PAD = 4;
const GAP_X = 56; // между колонками: здесь идут линии
const GAP_Y = 12;
const BAND = 20; // зазор под раскрытой веткой

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}
export interface Placed extends Rect {
  key: string;
  kind: "card" | "row";
}
export interface Layout {
  items: Placed[];
  /** Подложки списков: ключ — ветка, чьих детей список показывает. */
  panels: (Rect & { key: string })[];
}

export function layoutTree(tree: Tree, isOpen: (key: string) => boolean): Layout {
  const items: Placed[] = [];
  const panels: Layout["panels"] = [];
  const kids = (key: string) => tree.items.get(key)?.children ?? [];

  // Список детей с верхом в y; возвращает нижнюю границу всего раскрытого под ним.
  const list = (key: string, x: number, y: number): number => {
    const children = kids(key);
    const panel = { key, x, y, w: LIST_W, h: children.length * ROW_H + 2 * PAD };
    panels.push(panel);
    let bottom = y + panel.h;
    let free = -Infinity; // где кончается уже раскрытое в следующей колонке
    children.forEach((k, i) => {
      const ry = y + PAD + i * ROW_H;
      items.push({ key: k, kind: "row", x: x + PAD, y: ry, w: LIST_W - 2 * PAD, h: ROW_H });
      if (isOpen(k) && kids(k).length) {
        // Первая строка подсписка — вровень со своей веткой, если её не теснит соседний подсписок.
        const b = list(k, x + LIST_W + GAP_X, Math.max(ry - PAD, free));
        free = b + BAND;
        bottom = Math.max(bottom, b);
      }
    });
    return bottom;
  };

  let y = 0;
  for (const key of tree.top) {
    items.push({ key, kind: "card", x: 0, y, w: CARD_W, h: CARD_H });
    const open = isOpen(key) && kids(key).length > 0;
    const bottom = open ? Math.max(y + CARD_H, list(key, CARD_W + GAP_X, y + (CARD_H - ROW_H) / 2 - PAD)) : y + CARD_H;
    y = bottom + (open ? BAND : GAP_Y);
  }
  return { items, panels };
}
