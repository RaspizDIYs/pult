import { Handle, Position, useStore, type Node, type NodeProps } from "@xyflow/react";
import { memo } from "react";
import { StatePlaque } from "@/components/state-plaque";
import { kindInfo, TONE, type CardText, type Tone } from "@/lib/model";
import type { NodeState, NodeView } from "@/lib/pult";
import { cn } from "@/lib/utils";

export type CardData = {
  view: NodeView;
  state: NodeState | undefined;
  tone: Tone;
  text: CardText;
  /** Что выбрано: сам узел, зависимое от него или всё остальное (притушено). */
  focus: "self" | "related" | "dim" | null;
};
export type CardNode = Node<CardData, "card">;

// Размер фиксирован (CARD_W × CARD_H в layout.ts): раскладке нужны точные габариты,
// а длинный факт обрезается — полный текст виден в панели узла.
// При уменьшении карта «уместить всё» показывает мелкий текст, который не прочитать, — тогда
// карточка переходит в крупный вид: только значок, название и состояние.
const FAR_ZOOM = 0.62;

export const NodeCard = memo(function NodeCard({ data, selected }: NodeProps<CardNode>) {
  const { view, state, tone, text, focus } = data;
  const t = TONE[tone];
  const { Icon } = kindInfo(view.kind);
  const far = useStore((s) => s.transform[2] < FAR_ZOOM);

  return (
    <div
      className={cn(
        "flex h-full w-full flex-col gap-1.5 overflow-hidden rounded-xl border px-2.5 py-2 text-left transition-opacity",
        far && "justify-center",
        t.card,
        selected && "ring-2 ring-foreground/70 ring-offset-2 ring-offset-background",
        focus === "dim" && "opacity-40",
      )}
    >
      <Handle type="target" position={Position.Left} isConnectable={false} className="!size-1 !min-h-0 !min-w-0 !border-0 !bg-transparent" />
      <Handle type="source" position={Position.Right} isConnectable={false} className="!size-1 !min-h-0 !min-w-0 !border-0 !bg-transparent" />

      <div className="flex items-center gap-2">
        {!far && (
          <span className="grid size-6 shrink-0 place-items-center rounded-md bg-foreground/8 text-foreground/70">
            <Icon className="size-3.5" aria-hidden />
          </span>
        )}
        <span
          className={cn("min-w-0 flex-1 leading-tight font-semibold", far ? "line-clamp-2 text-[22px] leading-[1.1]" : "truncate text-[13px]")}
          title={view.title}
        >
          {view.title}
        </span>
      </div>

      <div className="flex items-center gap-1.5">
        <StatePlaque tone={tone} className={far ? "h-7 gap-1.5 px-3 text-[17px] [&_svg]:size-[18px]" : undefined} />
        {view.undeclared && (
          <span className={cn("truncate rounded-full border border-dashed border-foreground/40 px-1.5 text-foreground/70", far ? "shrink-0 text-[13px] leading-6" : "text-[10px] leading-4")} title="Найден сбором, в инвентаре не описан">
            не описан
          </span>
        )}
        {state && !state.confirmed && (
          <span className="size-1.5 shrink-0 rounded-full bg-foreground/50" title="Состояние только что изменилось, ждём подтверждения следующей проверкой" />
        )}
      </div>

      <div className={cn("min-h-0 flex-1 text-[11.5px] leading-snug text-foreground/75", far && "hidden")}>
        {text.cause ? (
          <>
            <p className="truncate" title={text.fact}>{text.fact}</p>
            <p className="truncate font-medium text-foreground/90" title={text.cause}>{text.cause}</p>
          </>
        ) : (
          <p className="line-clamp-2" title={text.fact}>{text.fact}</p>
        )}
      </div>
    </div>
  );
});

