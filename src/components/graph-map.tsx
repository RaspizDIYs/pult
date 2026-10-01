import {
  Background,
  BackgroundVariant,
  Controls,
  ReactFlow,
  ReactFlowProvider,
  useReactFlow,
  type Edge as FlowEdge,
  type NodeChange,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { Maximize, Undo2 } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { NodeCard, type CardData, type CardNode } from "@/components/node-card";
import { Button } from "@/components/ui/button";
import { autoLayout, CARD_H, CARD_W, loadMoved, saveMoved, type Positions } from "@/lib/layout";
import { buildEdges, buildGraph, cardText, dependentsOf, TONE, toneOf, type Tone } from "@/lib/model";
import type { NodeState, NodeView } from "@/lib/pult";

const nodeTypes = { card: NodeCard };

// Подписи xyflow по умолчанию английские — переводим всё, что слышит скринридер.
const ARIA_RU = {
  "node.a11yDescription.default": "Enter или пробел — выбрать узел и открыть его панель.",
  "node.a11yDescription.keyboardDisabled": "Enter или пробел — выбрать узел и открыть его панель.",
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

interface Props {
  nodes: NodeView[];
  states: Record<string, NodeState>;
  selectedId: string | null;
  /** Растёт, когда узел выбрали не на карте (например, в сводке): тогда карта подъезжает к нему. */
  revealToken: number;
  onSelect: (id: string | null) => void;
}

export function GraphMap(props: Props) {
  return (
    <ReactFlowProvider>
      <MapView {...props} />
    </ReactFlowProvider>
  );
}

function MapView({ nodes, states, selectedId, revealToken, onSelect }: Props) {
  const { fitView } = useReactFlow();
  const graph = useMemo(() => buildGraph(nodes), [nodes]);
  const auto = useMemo(() => autoLayout(nodes), [nodes]);
  const [moved, setMoved] = useState<Positions>(loadMoved);

  // Запоминаем перестановки с задержкой: во время перетаскивания позиция меняется на каждый кадр.
  useEffect(() => {
    const t = setTimeout(() => saveMoved(moved), 400);
    return () => clearTimeout(t);
  }, [moved]);

  const titles = useMemo(() => new Map(nodes.map((n) => [n.id, n.title])), [nodes]);

  // Выбранный узел, всё, что от него зависит, и корни, из-за которых он не работает.
  const focus = useMemo(() => {
    if (!selectedId || !titles.has(selectedId)) return null;
    return new Set([selectedId, ...dependentsOf(graph, selectedId), ...(states[selectedId]?.blockedBy ?? [])]);
  }, [selectedId, graph, states, titles]);

  const flowNodes = useMemo<CardNode[]>(() => {
    const titleOf = (id: string) => titles.get(id) ?? id;
    return nodes.map((n) => {
      const st = states[n.id];
      const tone = toneOf(st);
      const text = cardText(st, titleOf);
      const data: CardData = {
        view: n,
        state: st,
        tone,
        text,
        focus: focus ? (n.id === selectedId ? "self" : focus.has(n.id) ? "related" : "dim") : null,
      };
      return {
        id: n.id,
        type: "card",
        position: moved[n.id] ?? auto[n.id] ?? { x: 0, y: 0 },
        // Размер задан явно: раскладка знает его заранее, измерять карточки не нужно.
        width: CARD_W,
        height: CARD_H,
        selected: n.id === selectedId,
        ariaLabel: `${n.title}. ${TONE[tone].label}. ${text.fact}${text.cause ? `. ${text.cause}` : ""}`,
        data,
      };
    });
  }, [nodes, states, titles, moved, auto, focus, selectedId]);

  const flowEdges = useMemo<FlowEdge[]>(
    () =>
      buildEdges(nodes).map((e) => {
        const tone = toneOf(states[e.target]);
        const hot = !!focus && focus.has(e.source) && focus.has(e.target);
        return {
          id: e.id,
          source: e.source,
          target: e.target,
          focusable: false,
          selectable: false,
          zIndex: hot ? 10 : 0,
          // Сплошная — «размещён на», пунктир — «зависит от»; цвет — по состоянию потомка.
          style: {
            stroke: TONE[tone].stroke,
            strokeWidth: hot ? 3 : e.via === "on" ? 2 : 1.5,
            strokeDasharray: e.via === "dep" ? "6 5" : undefined,
            opacity: focus && !hot ? 0.12 : tone === "ok" ? 0.5 : 0.95,
          },
        };
      }),
    [nodes, states, focus],
  );

  const onNodesChange = useCallback(
    (changes: NodeChange<CardNode>[]) => {
      const dragged: Positions = {};
      let picked: string | null = null;
      let dropped = false;
      for (const c of changes) {
        if (c.type === "position" && c.position) dragged[c.id] = c.position;
        else if (c.type === "select") {
          if (c.selected) picked = c.id;
          else if (c.id === selectedId) dropped = true;
        }
      }
      if (Object.keys(dragged).length) setMoved((m) => ({ ...m, ...dragged }));
      // Выбор идёт и от клавиатуры (Enter на карточке), а там нет onNodeClick — слушаем изменения.
      if (picked) onSelect(picked);
      else if (dropped) onSelect(null);
    },
    [onSelect, selectedId],
  );

  // Подъезд к узлу, выбранному в сводке. Состав выбранного читаем из ref, чтобы не ездить
  // по карте каждый раз, когда приходит новое состояние.
  const focusRef = useRef<string[]>([]);
  focusRef.current = focus ? [...focus] : [];
  useEffect(() => {
    if (revealToken === 0 || !focusRef.current.length) return;
    // Панель узла открывается в тот же кадр и сужает карту; ждём, пока xyflow узнает новую ширину.
    const t = setTimeout(
      () => void fitView({ nodes: focusRef.current.map((id) => ({ id })), padding: 0.3, maxZoom: 1, minZoom: 0.4, duration: 400 }),
      80,
    );
    return () => clearTimeout(t);
  }, [revealToken, fitView]);

  const fitAll = () => void fitView({ padding: 0.06, maxZoom: 1, duration: 300 });

  return (
    <div className="flex h-full flex-col">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b px-3 py-1.5 text-[11px] text-muted-foreground">
        <ul className="hidden flex-wrap items-center gap-x-3 gap-y-1 lg:flex" aria-label="Обозначения">
          {LEGEND.map((t) => (
            <li key={t} className="inline-flex items-center gap-1">
              <span className={`size-2 rounded-full ${TONE[t].dot}`} aria-hidden />
              {TONE[t].label}
            </li>
          ))}
          <li className="inline-flex items-center gap-1.5 border-l pl-3">
            <svg width="22" height="6" aria-hidden><line x1="0" y1="3" x2="22" y2="3" stroke="currentColor" strokeWidth="2" /></svg>
            размещён на
          </li>
          <li className="inline-flex items-center gap-1.5">
            <svg width="22" height="6" aria-hidden><line x1="0" y1="3" x2="22" y2="3" stroke="currentColor" strokeWidth="1.5" strokeDasharray="5 4" /></svg>
            зависит от
          </li>
        </ul>
        <span className="flex-1" />
        {Object.keys(moved).length > 0 && (
          <Button size="xs" variant="outline" onClick={() => setMoved({})} title="Вернуть автоматическую раскладку">
            <Undo2 /> Сбросить раскладку
          </Button>
        )}
        <Button size="xs" variant="outline" onClick={fitAll}>
          <Maximize /> Уместить всё
        </Button>
      </div>

      <div className="relative min-h-0 flex-1">
        <ReactFlow<CardNode>
          nodes={flowNodes}
          edges={flowEdges}
          nodeTypes={nodeTypes}
          onNodesChange={onNodesChange}
          onPaneClick={() => onSelect(null)}
          fitView
          fitViewOptions={{ padding: 0.06, maxZoom: 1 }}
          minZoom={0.15}
          maxZoom={1.6}
          nodesConnectable={false}
          edgesFocusable={false}
          selectNodesOnDrag={false}
          deleteKeyCode={null}
          zoomOnDoubleClick={false}
          colorMode="system"
          ariaLabelConfig={ARIA_RU}
          aria-label="Карта узлов"
        >
          <Background variant={BackgroundVariant.Dots} gap={22} size={1.2} className="opacity-60" />
          <Controls showInteractive={false} position="bottom-left" />
        </ReactFlow>
      </div>
    </div>
  );
}
