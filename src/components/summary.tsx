import { CircleCheck, CircleHelp } from "lucide-react";
import type { KeyboardEvent } from "react";
import { TONE, dependentsOf, plural, type Graph, type Summary as SummaryData, type Tone } from "@/lib/model";
import type { NodeState } from "@/lib/pult";
import { cn } from "@/lib/utils";

const BAR: Tone[] = ["ok", "root", "cascade", "unknown", "stale", "unchecked"];
const CHIPS: Partial<Record<Tone, string>> = {
  cascade: "недоступны из-за корня",
  unknown: "неизвестно",
  stale: "устарело",
  unchecked: "не проверяется",
};

// Ответ на «что сломано и почему» должен читаться за секунды: число, затем корни отказов с фактом.
export function Summary({
  summary,
  graph,
  states,
  selectedId,
  compact,
  onSelect,
}: {
  summary: SummaryData;
  graph: Graph;
  states: Record<string, NodeState>;
  selectedId: string | null;
  /** Узкое окно с открытой панелью узла: большие карточки корней сворачиваются в строку. */
  compact: boolean;
  onSelect: (id: string) => void;
}) {
  const { working, observable, roots, counts, undeclared } = summary;
  const noData = counts.unknown + counts.stale;

  // Стрелки двигают фокус по списку: Tab остаётся для перехода дальше, стрелки — для выбора.
  function onKeyDown(e: KeyboardEvent<HTMLUListElement>) {
    if (!["ArrowRight", "ArrowDown", "ArrowLeft", "ArrowUp", "Home", "End"].includes(e.key)) return;
    const items = [...e.currentTarget.querySelectorAll<HTMLButtonElement>("button")];
    const i = items.indexOf(document.activeElement as HTMLButtonElement);
    if (i < 0) return;
    e.preventDefault();
    const step = e.key === "ArrowRight" || e.key === "ArrowDown" ? 1 : -1;
    const next = e.key === "Home" ? 0 : e.key === "End" ? items.length - 1 : (i + step + items.length) % items.length;
    items[next].focus();
  }

  return (
    <section aria-label="Сводка" className="border-b bg-card/40 px-4 py-2.5">
      <div className="flex flex-wrap items-center gap-x-6 gap-y-2">
        <div className="min-w-0">
          <h2 className="text-lg leading-tight font-semibold" aria-live="polite" title="Узлы, которые не проверяются, в счёт не идут">
            Работает {working} из {observable}
          </h2>
          <div
            className="mt-1.5 flex h-1.5 w-56 max-w-full overflow-hidden rounded-full bg-muted"
            role="img"
            aria-label={BAR.filter((t) => counts[t]).map((t) => `${TONE[t].label}: ${counts[t]}`).join(", ")}
          >
            {BAR.map((t) => (counts[t] ? <span key={t} className={TONE[t].dot} style={{ flexGrow: counts[t] }} /> : null))}
          </div>
        </div>

        <ul className="flex flex-wrap items-center gap-1.5 text-xs">
          {(Object.keys(CHIPS) as Tone[]).filter((t) => counts[t] > 0).map((t) => (
            <li key={t} className={cn("inline-flex items-center gap-1 rounded-full border px-2 py-0.5", TONE[t].chip)}>
              <span className="font-semibold">{counts[t]}</span> {CHIPS[t]}
            </li>
          ))}
          {undeclared > 0 && (
            <li className="inline-flex items-center gap-1 rounded-full border border-dashed border-foreground/40 px-2 py-0.5 text-foreground/80">
              <span className="font-semibold">{undeclared}</span> не описано в инвентаре
            </li>
          )}
        </ul>
      </div>

      {roots.length > 0 ? (
        <div className="mt-2">
          <h3 className="mb-1 flex items-center gap-1.5 text-sm font-semibold text-root-fg">
            <TONE.root.Icon className="size-4" aria-hidden /> Сломано: {roots.length}
            <span className="font-normal text-muted-foreground">— корни проблем, остальное от них зависит</span>
          </h3>
          <ul
            onKeyDown={onKeyDown}
            className={cn("grid max-h-44 grid-cols-[repeat(auto-fill,minmax(260px,1fr))] gap-2 overflow-y-auto p-0.5", compact && "max-lg:hidden")}
            aria-label="Сломано"
          >
            {roots.map((n) => {
              const affected = dependentsOf(graph, n.id).size;
              return (
                <li key={n.id} className="min-w-0">
                  <button
                    type="button"
                    onClick={() => onSelect(n.id)}
                    aria-pressed={selectedId === n.id}
                    className={cn(
                      "flex w-full items-start gap-2 rounded-lg border px-2.5 py-1 text-left outline-none transition-colors",
                      "border-root/50 bg-root-bg text-root-fg hover:border-root focus-visible:ring-3 focus-visible:ring-ring/60",
                      selectedId === n.id && "ring-2 ring-foreground/60",
                    )}
                  >
                    <TONE.root.Icon className="mt-0.5 size-4 shrink-0" aria-hidden />
                    <span className="min-w-0 flex-1">
                      <span className="flex items-baseline gap-2">
                        <span className="min-w-0 flex-1 truncate text-sm font-semibold">{n.title}</span>
                        {affected > 0 && <span className="shrink-0 text-[11px] opacity-80">зависят от него: {affected}</span>}
                      </span>
                      <span className="line-clamp-2 text-xs">{states[n.id]?.fact}</span>
                    </span>
                  </button>
                </li>
              );
            })}
          </ul>
          {compact && (
            <ul onKeyDown={onKeyDown} className="flex flex-wrap gap-1.5 lg:hidden" aria-label="Сломано (кратко)">
              {roots.map((n) => (
                <li key={n.id}>
                  <button
                    type="button"
                    onClick={() => onSelect(n.id)}
                    aria-pressed={selectedId === n.id}
                    className={cn(
                      "inline-flex max-w-52 items-center gap-1 rounded-full border border-root/50 bg-root-bg px-2 py-0.5 text-xs font-medium text-root-fg outline-none focus-visible:ring-3 focus-visible:ring-ring/60",
                      selectedId === n.id && "ring-2 ring-foreground/60",
                    )}
                  >
                    <TONE.root.Icon className="size-3 shrink-0" aria-hidden />
                    <span className="truncate">{n.title}</span>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      ) : (
        <p className={cn("mt-2.5 flex items-center gap-1.5 text-sm", noData ? "text-unknown-fg" : "text-ok-fg")}>
          {noData ? (
            <>
              <CircleHelp className="size-4" aria-hidden /> Поломок не видно, но по {noData} {plural(noData, "узлу", "узлам", "узлам")} данных нет или они устарели.
            </>
          ) : (
            <>
              <CircleCheck className="size-4" aria-hidden /> Всё в порядке: сломанного нет.
            </>
          )}
        </p>
      )}
    </section>
  );
}
