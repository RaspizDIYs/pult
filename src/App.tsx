import { getVersion } from "@tauri-apps/api/app";
import { RefreshCw, Settings as SettingsIcon, TriangleAlert } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { FleetScreen } from "@/components/fleet-screen";
import { NodePanel } from "@/components/node-panel";
import {
  EmptyInventoryScreen,
  InventoryBanner,
  InventoryErrorScreen,
  LoadErrorScreen,
  LoadingScreen,
  NoInventoryScreen,
} from "@/components/screens";
import { SettingsDialog } from "@/components/settings-dialog";
import { Summary } from "@/components/summary";
import { TreeMap } from "@/components/tree-map";
import { Button } from "@/components/ui/button";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { UpdateBanner } from "@/components/update-banner";
import { useFleet } from "@/lib/fleet";
import { buildGraph, fmtTimeSec, summarize } from "@/lib/model";
import { errText, isTauri, pult, type NodeState, type NodeView } from "@/lib/pult";
import { useUpdater } from "@/lib/updater";
import { useNow, usePult } from "@/lib/use-pult";
import { TasksScreen } from "@/tasks/tasks-screen";

const NO_NODES: NodeView[] = [];
const NO_STATES: Record<string, NodeState> = {};
// Три интервала проверки — срок годности измерения (контракт, п. 5): дольше тишины от ядра быть не должно.
const SILENCE_MS = 3 * 60_000;

export default function App() {
  const updater = useUpdater();
  const { snapshot, error, cycleAt, reload } = usePult();
  const now = useNow(15_000);
  const [version, setVersion] = useState("…");
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [reveal, setReveal] = useState(0);
  const [actionError, setActionError] = useState<string | null>(null);
  const [view, setView] = useState<"map" | "fleet" | "tasks">("map");
  // Рой слушаем и с карты: число проблем на ярлыке вкладки видно, не переключаясь.
  const fleet = useFleet();
  const fleetProblems = fleet.view?.problems ?? 0;
  const returnFocus = useRef<HTMLElement | null>(null);

  useEffect(() => {
    getVersion().then(setVersion, () => setVersion(isTauri ? "—" : "макет"));
  }, []);

  const nodes = snapshot?.nodes ?? NO_NODES;
  const states = snapshot?.states ?? NO_STATES;
  const graph = useMemo(() => buildGraph(nodes), [nodes]);
  const summary = useMemo(() => summarize(nodes, states), [nodes, states]);
  const selected = selectedId ? (nodes.find((n) => n.id === selectedId) ?? null) : null;

  // Инвентарь поменялся, и выбранного узла больше нет — панель закрываем.
  useEffect(() => {
    if (snapshot && selectedId && !selected) setSelectedId(null);
  }, [snapshot, selectedId, selected]);

  // Откуда пришёл выбор, туда возвращаем фокус при закрытии панели — иначе с клавиатуры
  // приходится заново искать место в списке.
  const select = useCallback(
    (id: string, fromMap = false) => {
      if (!selectedId) returnFocus.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      setSelectedId(id);
      if (!fromMap) setReveal((n) => n + 1);
    },
    [selectedId],
  );
  const close = useCallback(() => {
    setSelectedId(null);
    const el = returnFocus.current;
    returnFocus.current = null;
    if (el?.isConnected) requestAnimationFrame(() => el.focus());
  }, []);
  const onMapSelect = useCallback((id: string | null) => (id ? select(id, true) : close()), [select, close]);

  // Esc закрывает панель, даже если фокус никуда не попал (после клика по пустому месту карты).
  // Диалог настроек сам обрабатывает Esc — тогда мы не вмешиваемся.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && selectedId && !settingsOpen && !e.defaultPrevented) close();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [selectedId, settingsOpen, close]);

  async function recheckAll() {
    setActionError(null);
    try {
      await pult.recheck();
    } catch (e) {
      setActionError(errText(e));
    }
  }

  const inv = snapshot?.inventory ?? null;
  const silent = cycleAt !== null && now - cycleAt > SILENCE_MS;

  let body;
  if (!snapshot) {
    body = error ? <LoadErrorScreen error={error} onRetry={reload} /> : <LoadingScreen />;
  } else if (!nodes.length) {
    const openSettings = () => setSettingsOpen(true);
    body =
      inv?.path === null ? (
        <NoInventoryScreen />
      ) : inv?.error ? (
        <InventoryErrorScreen error={inv.error} onSettings={openSettings} />
      ) : (
        <EmptyInventoryScreen path={inv?.path ?? null} onSettings={openSettings} />
      );
  } else {
    body = (
      <>
        {inv?.error && <InventoryBanner error={inv.error} onSettings={() => setSettingsOpen(true)} />}
        <Summary summary={summary} graph={graph} states={states} selectedId={selected?.id ?? null} compact={!!selected} onSelect={select} />
        <div className="flex min-h-0 flex-1 flex-col lg:flex-row">
          {/* Панель стоит в разметке раньше карты (порядок Tab: сводка → панель → узлы), а на экране — после неё. */}
          {selected && (
            <NodePanel
              node={selected}
              state={states[selected.id]}
              nodes={nodes}
              states={states}
              graph={graph}
              now={now}
              onSelect={select}
              onClose={close}
              className="order-2 shrink-0 grow-0 basis-[56%] lg:basis-auto lg:w-[26rem] xl:w-[30rem]"
            />
          )}
          <main className="relative order-1 min-h-0 min-w-0 flex-1">
            <div className="absolute inset-0">
              <TreeMap nodes={nodes} states={states} graph={graph} selectedId={selected?.id ?? null} revealToken={reveal} onSelect={onMapSelect} />
            </div>
          </main>
        </div>
      </>
    );
  }

  const warnings = inv?.warnings.length ?? 0;
  return (
    <Tabs value={view} onValueChange={(v) => setView(v as "map" | "fleet" | "tasks")} className="h-screen gap-0">
      <header className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b px-4 py-2">
        <h1 className="text-base font-semibold">Пульт</h1>
        <span className="text-xs text-muted-foreground">{version}</span>
        <TabsList variant="line" className="h-7">
          <TabsTrigger value="map">Карта</TabsTrigger>
          <TabsTrigger value="fleet">
            Рой
            {fleetProblems > 0 && (
              <span className="rounded-full bg-root px-1.5 text-[10px] leading-4 font-semibold text-white" aria-label={`проблем: ${fleetProblems}`}>
                {fleetProblems}
              </span>
            )}
          </TabsTrigger>
          <TabsTrigger value="tasks">Задачи</TabsTrigger>
        </TabsList>
        {!isTauri && (
          <span className="rounded-full border border-warn/50 bg-warn-bg px-2 text-[11px] leading-5 text-warn-fg" title="Пульт открыт в браузере без ядра: все данные выдуманные">
            демо-данные
          </span>
        )}
        <span className="flex-1" />
        {actionError && <span className="max-w-72 truncate text-xs text-root-fg" title={actionError}>{actionError}</span>}
        {view === "map" && snapshot && nodes.length > 0 && (
          <span className={silent ? "text-xs font-medium text-warn-fg" : "text-xs text-muted-foreground"} aria-live="off">
            {silent ? `Новых проверок нет с ${fmtTimeSec(new Date(cycleAt!).toISOString())}` : `Проверка № ${snapshot.cycle} · ${fmtTimeSec(new Date(cycleAt ?? Date.now()).toISOString())}`}
          </span>
        )}
        {view === "map" && warnings > 0 && (
          <Button size="xs" variant="outline" onClick={() => setSettingsOpen(true)} className="border-warn/50 bg-warn-bg text-warn-fg">
            <TriangleAlert /> Предупреждений: {warnings}
          </Button>
        )}
        {view === "map" && snapshot && nodes.length > 0 && (
          <Button size="sm" variant="outline" onClick={recheckAll}>
            <RefreshCw /> Проверить всё
          </Button>
        )}
        <Button size="icon-sm" variant="ghost" aria-label="Настройки" title="Настройки" onClick={() => setSettingsOpen(true)}>
          <SettingsIcon />
        </Button>
      </header>

      <UpdateBanner updater={updater} />
      {/* Обе вкладки остаются смонтированными: карта не теряет вид и раскладку, доска — фильтры. */}
      <TabsContent value="map" forceMount className="flex min-h-0 flex-col text-base data-[state=inactive]:hidden">
        {snapshot && error && (
          <p role="alert" className="border-b bg-warn-bg px-4 py-1.5 text-xs text-warn-fg">
            Карта может не обновляться: {error}
          </p>
        )}
        {body}
      </TabsContent>
      <TabsContent value="fleet" className="flex min-h-0 flex-col text-base">
        <FleetScreen view={fleet.view} error={fleet.error} now={now} />
      </TabsContent>
      <TabsContent value="tasks" forceMount className="flex min-h-0 flex-col text-base data-[state=inactive]:hidden">
        <TasksScreen active={view === "tasks"} now={now} onSettings={() => setSettingsOpen(true)} />
      </TabsContent>

      <SettingsDialog open={settingsOpen} onOpenChange={setSettingsOpen} inventory={inv} updater={updater} version={version} />
    </Tabs>
  );
}
