import { CircleCheck, CircleHelp, CircleX, ExternalLink, RefreshCw, X } from "lucide-react";
import { useEffect, useMemo, useState, type KeyboardEvent, type ReactNode } from "react";
import { CopyButton } from "@/components/copy-button";
import { HistoryTab, LogsTab } from "@/components/node-tabs";
import { StatePlaque } from "@/components/state-plaque";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { ago, dependentsOf, fmtDayTime, fmtDuration, fmtTime, kindInfo, TONE, toneOf, type Graph } from "@/lib/model";
import { errText, pult, type CheckResult, type ContainerFacts, type NodeState, type NodeView } from "@/lib/pult";
import { cn } from "@/lib/utils";

interface Props {
  node: NodeView;
  state: NodeState | undefined;
  nodes: NodeView[];
  states: Record<string, NodeState>;
  graph: Graph;
  now: number;
  onSelect: (id: string) => void;
  onClose: () => void;
  className?: string;
}

export function NodePanel({ node, state, nodes, states, graph, now, onSelect, onClose, className }: Props) {
  const tone = toneOf(state);
  const { Icon, label: kindLabel } = kindInfo(node.kind);
  const titleOf = useMemo(() => {
    const m = new Map(nodes.map((n) => [n.id, n.title]));
    return (id: string) => m.get(id) ?? id;
  }, [nodes]);

  // «Проверить сейчас»: результат придёт событием, поэтому крутим значок, пока не обновится
  // измерение узла, но не дольше десяти секунд.
  const [rechecking, setRechecking] = useState(false);
  const [recheckError, setRecheckError] = useState<string | null>(null);
  const measuredAt = state?.measuredAt;
  useEffect(() => setRechecking(false), [measuredAt, node.id]);
  useEffect(() => {
    if (!rechecking) return;
    const t = setTimeout(() => setRechecking(false), 10_000);
    return () => clearTimeout(t);
  }, [rechecking]);

  async function recheck() {
    setRechecking(true);
    setRecheckError(null);
    try {
      await pult.recheck(node.id);
    } catch (e) {
      setRecheckError(errText(e));
      setRechecking(false);
    }
  }

  const onKeyDown = (e: KeyboardEvent<HTMLElement>) => {
    if (e.key === "Escape") {
      e.stopPropagation();
      onClose();
    }
  };

  return (
    <aside
      aria-label={`Узел «${node.title}»`}
      onKeyDown={onKeyDown}
      className={cn("flex min-h-0 flex-col border-t bg-background lg:border-t-0 lg:border-l", className)}
    >
      <header className="flex items-start gap-3 border-b px-4 py-3">
        <span className="grid size-9 shrink-0 place-items-center rounded-lg bg-muted text-foreground/80">
          <Icon className="size-5" aria-hidden />
        </span>
        <div className="min-w-0 flex-1">
          <h2 className="truncate text-base leading-tight font-semibold" title={node.title}>
            {node.title}
          </h2>
          <p className="truncate text-xs text-muted-foreground">
            {kindLabel}
            {node.group ? ` · группа «${node.group}»` : ""}
            {node.undeclared ? " · не описан в инвентаре" : ""}
          </p>
        </div>
        <Button size="sm" variant="outline" onClick={recheck} disabled={rechecking}>
          <RefreshCw className={cn(rechecking && "animate-spin")} />
          {rechecking ? "Проверяю…" : "Проверить сейчас"}
        </Button>
        <Button size="icon-sm" variant="ghost" aria-label="Закрыть панель (Esc)" title="Закрыть (Esc)" onClick={onClose}>
          <X />
        </Button>
      </header>

      {recheckError && <p className="border-b bg-root-bg px-4 py-1.5 text-xs text-root-fg">{recheckError}</p>}

      <Tabs key={node.id} defaultValue="overview" className="min-h-0 flex-1 gap-0">
        <TabsList variant="line" className="w-full shrink-0 justify-start border-b px-3">
          <TabsTrigger value="overview">Обзор</TabsTrigger>
          <TabsTrigger value="history">История</TabsTrigger>
          {node.hasLogs && <TabsTrigger value="logs">Логи</TabsTrigger>}
        </TabsList>

        <TabsContent value="overview" className="min-h-0 flex-1 space-y-4 overflow-y-auto px-4 py-3">
          <StateBlock tone={tone} state={state} now={now} />

          {state && state.blockedBy.length > 0 && (
            <Section title="Причина выше">
              <ul className="space-y-1.5">
                {state.blockedBy.map((id) => (
                  <li key={id}>
                    <button
                      type="button"
                      onClick={() => onSelect(id)}
                      className="flex w-full items-start gap-2 rounded-md border border-root/40 bg-root-bg/60 px-2.5 py-1.5 text-left text-sm outline-none hover:border-root focus-visible:ring-3 focus-visible:ring-ring/60"
                    >
                      <CircleX className="mt-0.5 size-4 shrink-0 text-root" aria-hidden />
                      <span className="min-w-0">
                        <span className="block font-medium">{titleOf(id)}</span>
                        <span className="block text-xs text-muted-foreground">{states[id]?.fact}</span>
                      </span>
                    </button>
                  </li>
                ))}
              </ul>
            </Section>
          )}

          {state && state.hints.length > 0 && (
            <Section title="Возможные причины" note="Это предположения: Пульт их не проверял.">
              <ul className="list-disc space-y-1 pl-5 text-sm">
                {state.hints.map((h) => (
                  <li key={h}>{h}</li>
                ))}
              </ul>
            </Section>
          )}

          <Dependents id={node.id} graph={graph} states={states} titleOf={titleOf} onSelect={onSelect} />

          {state && state.checks.length > 0 && <Checks checks={state.checks} titleOf={titleOf} />}
          {state?.container && <ContainerBlock c={state.container} now={now} />}
          {node.access && <Access access={node.access} />}
          {node.links.length > 0 && <Links links={node.links} />}
        </TabsContent>

        <TabsContent value="history" className="min-h-0 flex-1 overflow-y-auto px-4 py-3">
          <HistoryTab id={node.id} state={state} />
        </TabsContent>

        {node.hasLogs && (
          <TabsContent value="logs" className="min-h-0 flex-1 overflow-y-auto px-4 py-3">
            <LogsTab id={node.id} />
          </TabsContent>
        )}
      </Tabs>
    </aside>
  );
}

function Section({ title, note, children }: { title: string; note?: string; children: ReactNode }) {
  return (
    <section>
      <h3 className="mb-1.5 text-[13px] font-semibold">{title}</h3>
      {note && <p className="mb-1.5 text-xs text-muted-foreground">{note}</p>}
      {children}
    </section>
  );
}

function StateBlock({ tone, state, now }: { tone: ReturnType<typeof toneOf>; state: NodeState | undefined; now: number }) {
  if (!state) return <p className="rounded-lg border border-dashed p-3 text-sm text-muted-foreground">Ядро пока не прислало состояние этого узла.</p>;
  return (
    <section className={cn("rounded-lg border p-3", TONE[tone].box)}>
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
        <StatePlaque tone={tone} />
        {!state.confirmed && <span className="text-xs opacity-80">ждёт подтверждения следующей проверкой</span>}
      </div>
      <p className="mt-2 text-sm font-medium break-words">
        {state.own === "stale" ? `Данные от ${fmtTime(state.measuredAt)}. Последнее, что было видно: ${state.fact}` : state.fact}
      </p>
      <p className="mt-1 text-xs opacity-85">
        {state.since ? `С ${fmtDayTime(state.since)} (${fmtDuration(now - new Date(state.since).getTime())}). ` : ""}
        {state.measuredAt ? `Измерено ${ago(state.measuredAt, now)}.` : "Измерений ещё не было."}
      </p>
    </section>
  );
}

function Dependents({
  id,
  graph,
  states,
  titleOf,
  onSelect,
}: {
  id: string;
  graph: Graph;
  states: Record<string, NodeState>;
  titleOf: (id: string) => string;
  onSelect: (id: string) => void;
}) {
  const ids = useMemo(() => [...dependentsOf(graph, id)], [graph, id]);
  if (!ids.length) return null;
  const shown = ids.slice(0, 14);
  return (
    <Section title={`От этого узла зависят: ${ids.length}`}>
      <ul className="flex flex-wrap gap-1.5">
        {shown.map((d) => {
          const t = TONE[toneOf(states[d])];
          return (
            <li key={d}>
              <button
                type="button"
                onClick={() => onSelect(d)}
                className={cn("inline-flex max-w-52 items-center gap-1 rounded-full border px-2 py-0.5 text-xs outline-none focus-visible:ring-3 focus-visible:ring-ring/60", t.chip)}
                title={`${titleOf(d)}: ${t.label}`}
              >
                <t.Icon className="size-3 shrink-0" aria-hidden />
                <span className="truncate">{titleOf(d)}</span>
              </button>
            </li>
          );
        })}
        {ids.length > shown.length && <li className="self-center text-xs text-muted-foreground">и ещё {ids.length - shown.length}</li>}
      </ul>
    </Section>
  );
}

const CHECK_KIND: Record<CheckResult["kind"], string> = { tcp: "TCP", http: "HTTP", container: "Контейнер", vm: "ВМ", collect: "Сбор" };

function Checks({ checks, titleOf }: { checks: CheckResult[]; titleOf: (id: string) => string }) {
  return (
    <Section title="Проверки">
      <div className="overflow-x-auto rounded-md border">
        <Table className="text-xs">
          <TableHeader>
            <TableRow>
              <TableHead>Что</TableHead>
              <TableHead>Откуда</TableHead>
              <TableHead>Результат</TableHead>
              <TableHead className="text-right">Ответ</TableHead>
              <TableHead className="text-right">Когда</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {checks.map((c, i) => (
              <TableRow key={i}>
                <TableCell className="min-w-28 whitespace-normal">
                  <span className="font-medium">{CHECK_KIND[c.kind]}</span>
                  <span className="block break-all text-muted-foreground">{c.target}</span>
                </TableCell>
                <TableCell className="whitespace-normal text-muted-foreground">{c.from ? titleOf(c.from) : "эта машина"}</TableCell>
                <TableCell className="min-w-36 whitespace-normal">
                  <span className="flex items-start gap-1">
                    {c.ok === true ? (
                      <CircleCheck className="mt-0.5 size-3.5 shrink-0 text-ok" aria-label="успешно" />
                    ) : c.ok === false ? (
                      <CircleX className="mt-0.5 size-3.5 shrink-0 text-root" aria-label="не прошла" />
                    ) : (
                      <CircleHelp className="mt-0.5 size-3.5 shrink-0 text-unknown" aria-label="выполнить не удалось" />
                    )}
                    <span>{c.fact}</span>
                  </span>
                </TableCell>
                <TableCell className="text-right whitespace-nowrap tabular-nums">{c.latencyMs === null ? "—" : `${c.latencyMs} мс`}</TableCell>
                <TableCell className="text-right whitespace-nowrap tabular-nums">{fmtTime(c.measuredAt)}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </div>
    </Section>
  );
}

function ContainerBlock({ c, now }: { c: ContainerFacts; now: number }) {
  // Нехватку памяти показываем только когда ядро это установило: null — «не определено», а не «нет».
  const rows: [string, ReactNode][] = [["Состояние", c.state]];
  if (c.exitCode !== null) rows.push(["Код выхода", c.exitCode]);
  rows.push(["Нехватка памяти", c.oomKilled === null ? "не определено" : c.oomKilled ? "да (OOM)" : "нет"]);
  if (c.health) rows.push(["Health", c.health]);
  if (c.restartCount !== null) rows.push(["Перезапусков", c.restartCount]);
  const since = (iso: string) => `${fmtDayTime(iso)} (${fmtDuration(now - new Date(iso).getTime())} назад)`;
  if (c.startedAt) rows.push(["Запущен", since(c.startedAt)]);
  if (c.finishedAt) rows.push(["Остановлен", since(c.finishedAt)]);
  if (c.image) rows.push(["Образ", <span className="font-mono text-xs break-all">{c.image}</span>]);
  return (
    <Section title="Контейнер">
      <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
        {rows.map(([k, v]) => (
          <div key={k} className="contents">
            <dt className="text-muted-foreground">{k}</dt>
            <dd className="min-w-0 break-words">{v}</dd>
          </div>
        ))}
      </dl>
    </Section>
  );
}

function Access({ access }: { access: NonNullable<NodeView["access"]> }) {
  return (
    <Section title="Как зайти">
      {access.how && (
        <div className="flex items-center gap-1 rounded-md border bg-muted/50 py-1 pr-1 pl-2.5">
          <code className="min-w-0 flex-1 font-mono text-xs break-all">{access.how}</code>
          <CopyButton text={access.how} label="Скопировать, как зайти" />
        </div>
      )}
      {access.secret && (
        <p className="mt-2 text-xs">
          <span className="text-muted-foreground">Секрет лежит здесь: </span>
          {access.secret}
          <span className="text-muted-foreground"> — это указатель, самого секрета в Пульте нет.</span>
        </p>
      )}
      {access.who.length > 0 && (
        <p className="mt-2 flex flex-wrap items-center gap-1.5 text-xs">
          <span className="text-muted-foreground">У кого есть доступ:</span>
          {access.who.map((w) => (
            <Badge key={w} variant="secondary">
              {w}
            </Badge>
          ))}
        </p>
      )}
    </Section>
  );
}

// Ссылки приходят из инвентаря, а инвентарь — данные из репозитория: открываем только http(s),
// иначе `javascript:` в поле url выполнился бы внутри окна приложения.
function isWebUrl(url: string): boolean {
  try {
    const p = new URL(url).protocol;
    return p === "https:" || p === "http:";
  } catch {
    return false;
  }
}

function Links({ links }: { links: NodeView["links"] }) {
  return (
    <Section title="Ссылки">
      <ul className="space-y-1">
        {links.map((l) => (
          <li key={l.url} className="flex items-center gap-1.5 text-sm">
            {isWebUrl(l.url) ? (
              <a href={l.url} target="_blank" rel="noreferrer noopener" className="inline-flex shrink-0 items-center gap-1 font-medium underline underline-offset-2">
                {l.title}
                <ExternalLink className="size-3" aria-hidden />
              </a>
            ) : (
              <span className="shrink-0 font-medium">{l.title}</span>
            )}
            <span className="min-w-0 flex-1 truncate text-xs text-muted-foreground" title={l.url}>
              {l.url}
            </span>
            <CopyButton text={l.url} label="Скопировать ссылку" />
          </li>
        ))}
      </ul>
    </Section>
  );
}
