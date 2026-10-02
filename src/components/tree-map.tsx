import {
  Background,
  BackgroundVariant,
  Controls,
  Handle,
  Position,
  ReactFlow,
  ReactFlowProvider,
  useReactFlow,
  useStoreApi,
  type Edge,
  type Node,
  type NodeProps,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { ChevronRight, ChevronsDownUp, Folder, Globe, Maximize } from "lucide-react";
import { createContext, memo, useCallback, useContext, useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { Button } from "@/components/ui/button";
import { cardText, dependentsOf, kindInfo, TONE, toneOf, type Graph, type Tone } from "@/lib/model";
import type { NodeState, NodeView } from "@/lib/pult";
import {
  ancestorsOf,
  buildTree,
  layoutTree,
  NET_KEY,
  PAD,
  ROW_H,
  summarizeBranch,
  type Rect,
  type TreeItem,
} from "@/lib/tree";
import { cn } from "@/lib/utils";

// Подписи xyflow по умолчанию английские — переводим всё, что слышит скринридер.
const ARIA_RU = {
  "node.a11yDescription.default": "Стрелки — по строкам и ветвям, Enter — открыть панель узла.",
  "node.a11yDescription.keyboardDisabled": "Стрелки — по строкам и ветвям, Enter — открыть панель узла.",
  "node.a11yDescription.ariaLiveMessage": ({ direction, x, y }: { direction: string; x: number; y: number }) =>
    `Узел сдвинут (${direction}). Новая позиция: ${x}, ${y}`,
  "edge.a11yDescription.default": "Связь между узлами",
  "controls.ariaLabel": "Управление картой",
  "controls.zoomIn.ariaLabel": "Приблизить",
  "controls.zoomOut.ariaLabel": "Отдалить",
  "controls.fitView.ariaLabel": "Уместить всё",
  "controls.interactive.ariaLabel": "Переключить взаимодействие",
  "minimap.ariaLabel": "Миникарта",
  "handle.ariaLabel": "Точка связи",
};

const LEGEND: Tone[] = ["ok", "root", "cascade", "unknown", "stale", "unchecked"];
// Классы целиком: Tailwind собирает только то, что видит дословно.
const ICON: Record<Tone, string> = {
  ok: "text-ok-fg",
  root: "text-root",
  cascade: "text-cascade-fg",
  unknown: "text-unknown-fg",
  stale: "text-stale-fg",
  unchecked: "text-unchecked-fg",
};
const ROW_BG: Partial<Record<Tone, string>> = { root: "bg-root-bg", cascade: "bg-cascade-bg" };
const HANDLE = "!size-1 !min-h-0 !min-w-0 !border-0 !bg-transparent";
const START = { x: 16, y: 16, zoom: 1 };

type ItemData = {
  item: TreeItem;
  card: boolean;
  tone: Tone;
  line: string;
  hint: string;
  branch: boolean;
  open: boolean;
  focus: "self" | "related" | "dim" | null;
  tab: boolean;
  confirmed: boolean;
};
type ItemNode = Node<ItemData, "item">;
type PanelNode = Node<Record<string, never>, "panel">;

interface Actions {
  activate: (key: string) => void;
  toggle: (key: string) => void;
  keyDown: (e: KeyboardEvent<HTMLButtonElement>, key: string) => void;
  focused: (key: string) => void;
}
// Действия — через контекст, а не в data: так данные узла не меняются от каждой новой функции.
const ActionsCtx = createContext<Actions | null>(null);

const nodeTypes = { item: memo(ItemView), panel: memo(PanelView) };

interface Props {
  nodes: NodeView[];
  states: Record<string, NodeState>;
  graph: Graph;
  selectedId: string | null;
  /** Растёт, когда узел выбрали не на карте (сводка, панель): тогда ветка раскрывается до него. */
  revealToken: number;
  onSelect: (id: string | null) => void;
}

export function TreeMap(props: Props) {
  return (
    <ReactFlowProvider>
      <MapView {...props} />
    </ReactFlowProvider>
  );
}

function MapView({ nodes, states, graph, selectedId, revealToken, onSelect }: Props) {
  const { setViewport, setCenter, getZoom, fitView } = useReactFlow();
  const store = useStoreApi();
  const wrapper = useRef<HTMLDivElement>(null);
  const tree = useMemo(() => buildTree(nodes), [nodes]);
  const [overrides, setOverrides] = useState<Record<string, boolean>>(loadOpen);
  const [active, setActive] = useState<string | null>(null);
  useEffect(() => saveOpen(overrides), [overrides]);

  // Ветки, внутри которых есть корень отказа, раскрыты сами: свёрнутая карта не должна прятать проблему.
  const rootsKey = nodes.filter((n) => toneOf(states[n.id]) === "root").map((n) => n.id).join("\n");
  const autoOpen = useMemo(
    () => new Set(rootsKey ? rootsKey.split("\n").flatMap((id) => ancestorsOf(tree, id)) : []),
    [tree, rootsKey],
  );
  // Ручное «свернуть» держится, пока не появится новый корень: о новой поломке нужно узнать сразу.
  const seenRoots = useRef(new Set<string>());
  useEffect(() => {
    const roots = rootsKey ? rootsKey.split("\n") : [];
    const fresh = roots.filter((r) => !seenRoots.current.has(r));
    seenRoots.current = new Set(roots);
    const reopen = fresh.flatMap((r) => ancestorsOf(tree, r));
    if (!reopen.length) return;
    setOverrides((o) => {
      const closed = reopen.filter((k) => o[k] === false);
      if (!closed.length) return o;
      const next = { ...o };
      for (const k of closed) delete next[k];
      return next;
    });
  }, [rootsKey, tree]);

  const isOpen = useCallback((k: string) => overrides[k] ?? autoOpen.has(k), [overrides, autoOpen]);
  const layout = useMemo(() => layoutTree(tree, isOpen), [tree, isOpen]);
  const rects = useMemo(() => new Map<string, Rect>(layout.items.map((p) => [p.key, p])), [layout]);
  const rectsRef = useRef(rects);
  rectsRef.current = rects;

  // Сдвинуть вид ровно настолько, чтобы прямоугольник оказался на экране; масштаб не трогаем.
  const ensureVisible = useCallback(
    (r: Rect) => {
      const { width, height, transform } = store.getState();
      const [tx, ty, zoom] = transform;
      const m = 24;
      const left = r.x * zoom + tx;
      const top = r.y * zoom + ty;
      const right = left + r.w * zoom;
      const bottom = top + Math.min(r.h, 6 * ROW_H) * zoom;
      let dx = right > width - m ? width - m - right : 0;
      if (left + dx < m) dx = m - left;
      let dy = bottom > height - m ? height - m - bottom : 0;
      if (top + dy < m) dy = m - top;
      if (dx || dy) void setViewport({ x: tx + dx, y: ty + dy, zoom }, { duration: 200 });
    },
    [store, setViewport],
  );
  // Новое раскрытие или открытая панель меняют раскладку и ширину карты: смотрим после отрисовки.
  const showLater = useCallback(
    (get: () => Rect | undefined) => setTimeout(() => {
      const r = get();
      if (r) ensureVisible(r);
    }, 80),
    [ensureVisible],
  );

  const setOpen = useCallback(
    (k: string, open: boolean) => {
      setOverrides((o) => ({ ...o, [k]: open }));
      if (open) showLater(() => {
        const p = rectsRef.current.get(tree.items.get(k)?.children[0] ?? "");
        return p && { x: p.x, y: p.y - PAD, w: p.w, h: 6 * ROW_H };
      });
    },
    [tree, showLater],
  );

  const focusKey = useCallback(
    (k: string) => {
      wrapper.current?.querySelector<HTMLElement>(`[data-tree-key="${CSS.escape(k)}"]`)?.focus({ preventScroll: true });
      const r = rectsRef.current.get(k);
      if (r) ensureVisible(r);
    },
    [ensureVisible],
  );

  const actions = useMemo<Actions>(
    () => ({
      toggle: (k) => setOpen(k, !isOpen(k)),
      // Узел — открыть панель (и ветку, если она свёрнута); папка или «Сеть» — раскрыть или свернуть.
      activate: (k) => {
        const item = tree.items.get(k);
        if (!item) return;
        if (!item.node) return setOpen(k, !isOpen(k));
        onSelect(item.node.id);
        if (item.children.length && !isOpen(k)) setOpen(k, true);
        // Панель узла сужает карту — строка не должна уехать из вида.
        else showLater(() => rectsRef.current.get(k));
      },
      focused: setActive,
      keyDown: (e, k) => {
        const item = tree.items.get(k);
        if (!item) return;
        const siblings = item.parent ? tree.items.get(item.parent)!.children : tree.top;
        const i = siblings.indexOf(k);
        const branch = item.children.length > 0;
        let next: string | null | undefined = null;
        switch (e.key) {
          case "ArrowDown":
            next = siblings[i + 1];
            break;
          case "ArrowUp":
            next = siblings[i - 1];
            break;
          case "Home":
            next = siblings[0];
            break;
          case "End":
            next = siblings.at(-1);
            break;
          case "ArrowRight":
            if (!branch) break;
            if (isOpen(k)) next = item.children[0];
            else setOpen(k, true);
            break;
          case "ArrowLeft":
            if (branch && isOpen(k)) setOpen(k, false);
            else next = item.parent;
            break;
          default:
            return;
        }
        e.preventDefault();
        if (next) focusKey(next);
      },
    }),
    [tree, isOpen, setOpen, onSelect, focusKey, showLater],
  );

  // Узел выбран в сводке или панели — раскрыть ветки до него и подвезти к нему вид.
  const selectedRef = useRef(selectedId);
  selectedRef.current = selectedId;
  useEffect(() => {
    const id = selectedRef.current;
    if (!revealToken || !id) return;
    const up = ancestorsOf(tree, id);
    setOverrides((o) => (up.every((k) => o[k] ?? autoOpen.has(k)) ? o : { ...o, ...Object.fromEntries(up.map((k) => [k, true])) }));
    // Ждём, пока раскроются ветки и панель узла сузит карту, — и ставим узел в середину.
    const t = setTimeout(() => {
      const r = rectsRef.current.get(id);
      if (r) void setCenter(r.x + r.w / 2, r.y + r.h / 2, { zoom: getZoom(), duration: 300 });
    }, 80);
    return () => clearTimeout(t);
    // Только по новому выбору извне: смена состояний не должна снова раскрывать свёрнутое.
  }, [revealToken]);

  // Выбранный узел, его корни отказа и всё, что от него зависит. Что спрятано в свёрнутой ветке,
  // подсвечивается самой этой веткой.
  const related = useMemo(() => {
    if (!selectedId || !tree.items.has(selectedId)) return null;
    const repOf = (id: string) => {
      let k: string | null = id;
      while (k && !rects.has(k)) k = tree.items.get(k)?.parent ?? null;
      return k;
    };
    const self = repOf(selectedId);
    if (!self) return null;
    const causes = (states[selectedId]?.blockedBy ?? []).map(repOf);
    const deps = [...dependentsOf(graph, selectedId)].map(repOf);
    const keys = new Set([self, ...causes, ...deps].filter((k): k is string => !!k));
    // Пунктир — только между разными ветками: внутри одной связь и так видна по дереву.
    const apart = (a: string, b: string) =>
      a !== b &&
      tree.items.get(a)?.parent !== tree.items.get(b)?.parent &&
      !ancestorsOf(tree, a).includes(b) &&
      !ancestorsOf(tree, b).includes(a);
    // Стрелок нет, поэтому линия всегда идёт слева направо: иначе она огибает строку петлёй.
    const links = new Map<string, [string, string]>();
    for (const other of [...causes, ...deps]) {
      if (!other || !apart(self, other)) continue;
      const pair: [string, string] = rects.get(other)!.x < rects.get(self)!.x ? [other, self] : [self, other];
      links.set(pair.join(">"), pair);
    }
    return { self, keys, path: new Set(ancestorsOf(tree, self)), links: [...links.values()] };
  }, [selectedId, tree, rects, states, graph]);

  const tabKey = active && rects.has(active) ? active : (layout.items[0]?.key ?? null);
  const titles = useMemo(() => new Map(nodes.map((n) => [n.id, n.title])), [nodes]);

  const flowNodes = useMemo<(ItemNode | PanelNode)[]>(() => {
    const titleOf = (id: string) => titles.get(id) ?? id;
    const panels: PanelNode[] = layout.panels.map((p) => ({
      id: `p:${p.key}`,
      type: "panel",
      position: { x: p.x, y: p.y },
      width: p.w,
      height: p.h,
      data: {},
    }));
    const items: ItemNode[] = layout.items.map((p) => {
      const item = tree.items.get(p.key)!;
      const branch = item.children.length > 0;
      const st = item.node ? states[item.node.id] : undefined;
      let tone: Tone;
      let line: string;
      let hint: string;
      if (branch) {
        ({ tone, text: line } = summarizeBranch(item.members, states));
        hint = item.node && st && toneOf(st) !== "ok" ? `${TONE[toneOf(st)].label}: ${cardText(st, titleOf).fact}. ${line}` : line;
      } else {
        tone = toneOf(st);
        const t = cardText(st, titleOf);
        line = t.cause ?? t.fact;
        hint = t.cause ? `${t.fact}. ${t.cause}` : t.fact;
      }
      if (item.node?.undeclared) hint += ". Найден сбором, в инвентаре не описан";
      return {
        id: `i:${p.key}`,
        type: "item",
        position: { x: p.x, y: p.y },
        width: p.w,
        height: p.h,
        zIndex: 1,
        data: {
          item,
          card: p.kind === "card",
          tone,
          line,
          hint,
          branch,
          open: branch && isOpen(p.key),
          // Путь к выбранному не притушен: по нему видно, где узел лежит.
          focus: !related || related.path.has(p.key) ? null : p.key === related.self ? "self" : related.keys.has(p.key) ? "related" : "dim",
          tab: p.key === tabKey,
          confirmed: st?.confirmed ?? true,
        },
      };
    });
    return [...panels, ...items];
  }, [layout, tree, states, titles, isOpen, related, tabKey]);

  const flowEdges = useMemo<Edge[]>(() => {
    const treeEdges: Edge[] = layout.panels.map((p) => ({
      id: `t:${p.key}`,
      source: `i:${p.key}`,
      target: `p:${p.key}`,
      type: "smoothstep",
      focusable: false,
      selectable: false,
      style: { stroke: "var(--muted-foreground)", strokeWidth: 1.5, opacity: related ? 0.15 : 0.45 },
    }));
    // Цвет пунктира — по состоянию другого конца: красный ведёт к корню, приглушённый — к пострадавшим.
    const tones = new Map(flowNodes.flatMap((n) => (n.type === "item" ? [[n.data.item.key, n.data.tone] as const] : [])));
    const depEdges: Edge[] = (related?.links ?? []).map(([a, b]) => ({
      id: `d:${a}>${b}`,
      source: `i:${a}`,
      target: `i:${b}`,
      focusable: false,
      selectable: false,
      zIndex: 10,
      style: { stroke: TONE[tones.get(a === related!.self ? b : a) ?? "unknown"].stroke, strokeWidth: 2, strokeDasharray: "6 5" },
    }));
    return [...treeEdges, ...depEdges];
  }, [layout, related, flowNodes]);

  const collapseAll = () => {
    setOverrides({});
    void setViewport(START, { duration: 300 });
  };

  return (
    <ActionsCtx.Provider value={actions}>
      <div ref={wrapper} className="flex h-full flex-col">
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b px-3 py-1.5 text-[11px] text-muted-foreground">
          <ul className="hidden flex-wrap items-center gap-x-2.5 gap-y-1 lg:flex" aria-label="Обозначения">
            {LEGEND.map((t) => {
              const { Icon, label } = TONE[t];
              return (
                <li key={t} className="inline-flex items-center gap-1">
                  <Icon className={cn("size-3.5", ICON[t])} aria-hidden />
                  {label}
                </li>
              );
            })}
            <li className="inline-flex items-center gap-1.5 border-l pl-2.5">
              <svg width="22" height="6" aria-hidden><line x1="0" y1="3" x2="22" y2="3" stroke="currentColor" strokeWidth="1.5" strokeDasharray="5 4" /></svg>
              связь с другой веткой
            </li>
          </ul>
          <span className="ml-auto flex gap-2">
            <Button size="xs" variant="outline" onClick={collapseAll} title="Свернуть до площадок; ветки с отказом останутся раскрытыми">
              <ChevronsDownUp /> Свернуть всё
            </Button>
            <Button size="xs" variant="outline" onClick={() => void fitView({ padding: 0.06, maxZoom: 1, duration: 300 })}>
              <Maximize /> Уместить всё
            </Button>
          </span>
        </div>

        <div className="relative min-h-0 flex-1">
          <ReactFlow<ItemNode | PanelNode>
            nodes={flowNodes}
            edges={flowEdges}
            nodeTypes={nodeTypes}
            onPaneClick={() => onSelect(null)}
            defaultViewport={START}
            minZoom={0.2}
            maxZoom={1.6}
            // Дерево читается как документ: колесо и тачпад листают, щипок и Ctrl+колесо — масштаб.
            panOnScroll
            nodesDraggable={false}
            nodesConnectable={false}
            nodesFocusable={false}
            edgesFocusable={false}
            elementsSelectable={false}
            deleteKeyCode={null}
            zoomOnDoubleClick={false}
            colorMode="system"
            ariaLabelConfig={ARIA_RU}
            aria-label="Карта: площадки, их машины, проекты и сервисы"
          >
            <Background variant={BackgroundVariant.Dots} gap={22} size={1.2} className="opacity-60" />
            <Controls showInteractive={false} position="bottom-left" />
          </ReactFlow>
        </div>
      </div>
    </ActionsCtx.Provider>
  );
}

function PanelView() {
  return (
    <div className="h-full w-full rounded-xl border bg-card shadow-xs">
      <Handle type="target" position={Position.Left} isConnectable={false} className={HANDLE} style={{ top: PAD + ROW_H / 2 }} />
    </div>
  );
}

function ItemView({ data }: NodeProps<ItemNode>) {
  const a = useContext(ActionsCtx)!;
  const { item, card, tone, line, hint, branch, open, focus, tab, confirmed } = data;
  const t = TONE[tone];
  const KindIcon = item.node ? kindInfo(item.node.kind).Icon : item.key === NET_KEY ? Globe : Folder;
  const kind = item.node ? kindInfo(item.node.kind).label : item.key === NET_KEY ? "Площадка" : "Папка проекта";
  const selected = focus === "self";

  return (
    // Узлы карты невыбираемые, и xyflow снимает с их обёртки события мыши — возвращаем их строке.
    <div className={cn("pointer-events-auto relative h-full w-full transition-opacity", focus === "dim" && "opacity-35")}>
      <Handle type="target" position={Position.Left} isConnectable={false} className={HANDLE} />
      <Handle type="source" position={Position.Right} isConnectable={false} className={HANDLE} />
      <button
        type="button"
        data-tree-key={item.key}
        tabIndex={tab ? 0 : -1}
        aria-expanded={branch ? open : undefined}
        aria-current={selected || undefined}
        aria-label={`${item.title}. ${branch ? kind : t.label}: ${hint}`}
        title={`${item.title}\n${hint}`}
        onClick={() => a.activate(item.key)}
        onKeyDown={(e) => a.keyDown(e, item.key)}
        onFocus={() => a.focused(item.key)}
        className={cn(
          "flex h-full w-full items-center text-left outline-none focus-visible:ring-3 focus-visible:ring-ring/60",
          card ? cn("gap-2.5 rounded-xl border px-3", t.card) : cn("gap-2 rounded-md px-2 hover:bg-muted/70", ROW_BG[tone]),
          branch && "pr-8",
          open && !card && "bg-muted ring-1 ring-border",
          open && card && "ring-1 ring-foreground/25",
          selected && "ring-2 ring-foreground/70",
          focus === "related" && !selected && "ring-2 ring-foreground/30",
        )}
      >
        {card ? (
          <span className="grid size-8 shrink-0 place-items-center rounded-lg bg-foreground/8 text-foreground/75">
            <KindIcon className="size-4" aria-hidden />
          </span>
        ) : (
          <t.Icon className={cn("size-4 shrink-0", ICON[tone])} aria-hidden />
        )}
        <span className="min-w-0 flex-1">
          <span className={cn("flex items-center gap-1.5 font-semibold", card ? "text-sm" : "text-[13px]")}>
            {!card && branch && <KindIcon className="size-3.5 shrink-0 text-muted-foreground" aria-hidden />}
            <span className="truncate">{item.title}</span>
            {item.node?.undeclared && (
              <span className="shrink-0 rounded-full border border-dashed border-foreground/40 px-1.5 text-[10px] leading-4 font-normal text-foreground/70">не описан</span>
            )}
            {!confirmed && <span className="size-1.5 shrink-0 rounded-full bg-foreground/50" title="Состояние только что изменилось, ждём подтверждения следующей проверкой" />}
          </span>
          <span className={cn("flex items-center gap-1 truncate text-[11.5px] leading-snug", card ? "text-foreground/80" : "text-muted-foreground")}>
            {card && <t.Icon className={cn("size-3.5 shrink-0", ICON[tone])} aria-hidden />}
            <span className="truncate">{line}</span>
          </span>
        </span>
      </button>
      {branch && (
        <button
          type="button"
          tabIndex={-1}
          aria-hidden
          title={open ? "Свернуть" : "Развернуть"}
          onMouseDown={(e) => e.preventDefault()}
          onClick={() => a.toggle(item.key)}
          className={cn(
            "absolute top-1/2 right-1.5 grid size-6 -translate-y-1/2 place-items-center rounded-md text-muted-foreground hover:bg-foreground/10 hover:text-foreground",
            open && "text-foreground",
          )}
        >
          <ChevronRight className={cn("size-4 transition-transform", open && "rotate-180")} />
        </button>
      )}
    </div>
  );
}

// ───────────── Что раскрыто вручную ─────────────

const KEY = "pult.map.open.v1";

// localStorage может быть недоступен (приватный режим, запрет в webview) — карта работает и без него.
function loadOpen(): Record<string, boolean> {
  try {
    // Позиции карточек прежней свободной карты больше не нужны.
    localStorage.removeItem("pult.map.positions.v1");
    const parsed: unknown = JSON.parse(localStorage.getItem(KEY) ?? "{}");
    return parsed && typeof parsed === "object" ? (parsed as Record<string, boolean>) : {};
  } catch {
    return {};
  }
}

function saveOpen(open: Record<string, boolean>): void {
  try {
    if (Object.keys(open).length) localStorage.setItem(KEY, JSON.stringify(open));
    else localStorage.removeItem(KEY);
  } catch {
    /* запомнить не вышло — при следующем запуске всё свёрнуто, кроме веток с отказом */
  }
}
