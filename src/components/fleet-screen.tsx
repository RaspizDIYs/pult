import { CircleCheck, CircleHelp, CircleX, FileWarning, LoaderCircle, PlugZap, Unplug } from "lucide-react";
import { useMemo, useState, type ReactNode } from "react";
import { ago, fmtDuration, fmtTime, plural } from "@/lib/model";
import {
  folder,
  isService,
  presenceText,
  problemText,
  sessionTitle,
  shortId,
  TASK_LABEL,
  TASK_STATE,
  type FleetClaim,
  type FleetLock,
  type FleetSession,
  type FleetTask,
  type FleetView,
  type TaskStatus,
} from "@/lib/fleet";
import { cn } from "@/lib/utils";

// Экран роя отвечает на три вопроса, и именно в таком порядке: есть ли беда (замок на
// ушедшей сессии, двое в одном файле), кто в сети и что держит, что происходит дальше.

type Lookup = (id: string) => FleetSession | undefined;

export function FleetScreen({ view, error, now }: { view: FleetView | null; error: string | null; now: number }) {
  const byId = useMemo(() => {
    const m = new Map(view?.sessions.map((s) => [s.id, s]) ?? []);
    return (id: string) => m.get(id);
  }, [view]);

  if (!view) {
    return error ? (
      <div className="flex flex-1 items-center justify-center p-6">
        <div className="max-w-lg space-y-3 text-center">
          <PlugZap className="mx-auto size-6 text-muted-foreground" aria-hidden />
          <h2 className="text-lg font-semibold">Ядро не отдало картину роя</h2>
          <p className="rounded-md border border-root/50 bg-root-bg px-3 py-2 font-mono text-xs break-words text-root-fg">{error}</p>
        </div>
      </div>
    ) : (
      <div className="flex flex-1 items-center justify-center gap-2 text-sm text-muted-foreground" role="status">
        <LoaderCircle className="size-4 animate-spin" aria-hidden /> Загружаю рой…
      </div>
    );
  }

  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <FleetSummary view={view} byId={byId} now={now} />
      {/* Две колонки с постоянным составом: «кто» слева, «что держат и что идёт» справа. */}
      <div className="grid items-start gap-x-8 gap-y-6 px-4 py-4 lg:grid-cols-2">
        <div className="min-w-0 space-y-6">
          <Sessions view={view} now={now} />
          <Board view={view} byId={byId} now={now} />
          <Claims claims={view.claims} byId={byId} now={now} />
        </div>
        <div className="min-w-0 space-y-6">
          <Locks view={view} byId={byId} now={now} />
          <Machine view={view} byId={byId} now={now} />
          <Tasks tasks={view.tasks} hubOk={view.hub.state === "ok"} byId={byId} now={now} />
        </div>
      </div>
    </div>
  );
}

// ───────────── Итог и проблемы ─────────────

function FleetSummary({ view, byId, now }: { view: FleetView; byId: Lookup; now: number }) {
  const chats = view.sessions.filter((s) => s.presence === "online" && !isService(s.id));
  const machines = new Set(chats.map((s) => s.machine ?? "?")).size;
  const lockProblems = view.locks.filter((l) => l.problem);
  const overlaps = view.claims.filter((c) => c.overlap);
  const hubOk = view.hub.state === "ok";

  return (
    <section aria-label="Итог по рою" className="border-b bg-card/40 px-4 py-2.5">
      <div className="flex flex-wrap items-baseline gap-x-4 gap-y-1">
        <h2 className="text-lg leading-tight font-semibold" aria-live="polite">
          {chats.length || !hubOk
            ? `В сети ${chats.length} ${plural(chats.length, "сессия", "сессии", "сессий")} на ${machines} ${plural(machines, "машине", "машинах", "машинах")}`
            : "В рою никого"}
          {" · "}
          <span className={view.problems ? "text-root-fg" : "text-ok-fg"}>проблем: {view.problems}</span>
        </h2>
        <HubLine view={view} now={now} />
      </div>

      <HubBanner view={view} now={now} />

      {view.problems > 0 ? (
        <div className="mt-2">
          <h3 className="mb-1 flex items-center gap-1.5 text-sm font-semibold text-root-fg">
            <CircleX className="size-4" aria-hidden /> Проблемы
            <span className="font-normal text-muted-foreground">— то, что само не рассосётся</span>
          </h3>
          <ul className="grid max-h-56 grid-cols-[repeat(auto-fill,minmax(300px,1fr))] gap-2 overflow-y-auto p-0.5" aria-label="Проблемы">
            {lockProblems.map((l) => (
              <li key={`${l.scope}:${l.key}:${l.session}`} className="rounded-lg border border-root/50 bg-root-bg px-2.5 py-1.5 text-root-fg">
                <p className="flex items-baseline gap-2 text-sm font-semibold">
                  <span className="truncate">
                    {l.resource}
                    {l.repo && <span className="font-normal opacity-80"> · {l.repo}</span>}
                  </span>
                  <ScopeTag scope={l.scope} />
                </p>
                <p className="text-xs">
                  держит <SessionRef id={l.session!} byId={byId} now={now} plain /> — {problemText(l, byId(l.session!), now)}
                </p>
                {l.queue.length > 0 && (
                  <p className="text-xs opacity-90">
                    из-за этого ждут: <RefList ids={l.queue.map((q) => q.session)} byId={byId} now={now} />
                  </p>
                )}
              </li>
            ))}
            {overlaps.map((c) => (
              <li key={`claim:${c.repo}:${c.file}`} className="rounded-lg border border-root/50 bg-root-bg px-2.5 py-1.5 text-root-fg">
                <p className="truncate font-mono text-xs font-semibold" title={c.file}>
                  {c.file}
                </p>
                <p className="text-xs">
                  правят одновременно: <RefList ids={c.sessions.map((s) => s.session)} byId={byId} now={now} />
                </p>
              </li>
            ))}
          </ul>
        </div>
      ) : (
        <p className={cn("mt-2 flex items-center gap-1.5 text-sm", hubOk ? "text-ok-fg" : "text-unknown-fg")}>
          {hubOk ? (
            <>
              <CircleCheck className="size-4" aria-hidden /> Проблем нет: замки держат сессии в сети, файлы никто не делит.
            </>
          ) : (
            <>
              <CircleHelp className="size-4" aria-hidden /> На этой машине проблем не видно. Про остальной рой без хаба сказать нечего.
            </>
          )}
        </p>
      )}
    </section>
  );
}

function HubLine({ view, now }: { view: FleetView; now: number }) {
  const { hub } = view;
  if (hub.state === "connecting")
    return (
      <span className="inline-flex items-center gap-1 text-xs text-muted-foreground" role="status">
        <LoaderCircle className="size-3 animate-spin" aria-hidden /> подключаюсь к хабу…
      </span>
    );
  if (hub.state !== "ok") return null;
  return (
    <span className="text-xs text-muted-foreground" title={hub.lastOkAt ? `Последний снимок ${ago(hub.lastOkAt, now)}` : undefined}>
      хаб {hub.address ?? ""} на связи
    </span>
  );
}

/** Хаб недоступен — говорим вслух: тишина в этом месте читается как «никого нет». */
function HubBanner({ view, now }: { view: FleetView; now: number }) {
  const { hub, local } = view;
  if (hub.state === "unavailable")
    return (
      <div role="alert" className="mt-2 flex items-start gap-2 rounded-md border border-warn/50 bg-warn-bg px-3 py-2 text-sm text-warn-fg">
        <Unplug className="mt-0.5 size-4 shrink-0" aria-hidden />
        <p className="min-w-0">
          <strong>
            Хаб недоступен{hub.address ? ` (${hub.address})` : ""}: {hub.error}
            {/[.!?…]$/.test(hub.error ?? "") ? "" : "."}
          </strong>{" "}
          Ниже — только эта машина: общие замки, другие
          машины и задачи брокера сейчас не видны, общие ресурсы координируются на доверии.
          {hub.lastOkAt && ` Последний ответ хаба — в ${fmtTime(hub.lastOkAt)}, ${ago(hub.lastOkAt, now)}.`}
        </p>
      </div>
    );
  if (hub.state === "off")
    return (
      <p className="mt-2 rounded-md border border-dashed px-3 py-2 text-sm text-muted-foreground">
        Рой на этой машине не настроен: в <code className="font-mono text-xs">{local.dir}</code> нет <code className="font-mono text-xs">fleet.json</code>. Видно
        только локальный диспетчер. Подключить: <code className="font-mono text-xs">agent-orch fleet setup --url … --token-file …</code>
      </p>
    );
  return null;
}

// ───────────── Сессии ─────────────

function Sessions({ view, now }: { view: FleetView; now: number }) {
  const groups = useMemo(() => {
    const m = new Map<string, FleetSession[]>();
    for (const s of view.sessions) {
      const key = s.machine ?? (s.local ? (view.machine ?? "эта машина") : "машина неизвестна");
      m.set(key, [...(m.get(key) ?? []), s]);
    }
    return [...m.entries()];
  }, [view]);

  return (
    <Section title="Сессии" note={view.sessions.length ? "Без имени сессию узнают по папке и заметке; рядом — начало идентификатора, которым её называют агенты." : undefined}>
      {groups.length === 0 ? (
        <Empty>{view.hub.state === "ok" ? "В рою никого: ни одна сессия не отмечалась последний час." : "Сессий этой машины не видно."}</Empty>
      ) : (
        <div className="space-y-3">
          {groups.map(([machine, list]) => {
            const online = list.filter((s) => s.presence === "online").length;
            return (
              <div key={machine}>
                <h4 className="mb-1 flex items-baseline gap-2 border-b pb-0.5 text-[13px] font-semibold">
                  {machine}
                  {list.some((s) => s.local) && <span className="text-xs font-normal text-muted-foreground">эта машина</span>}
                  <span className="ml-auto text-xs font-normal text-muted-foreground">
                    в сети {online}
                    {list.length > online ? ` из ${list.length}` : ""}
                  </span>
                </h4>
                <ul className="divide-y divide-border/60">
                  {list.map((s) => (
                    <SessionRow key={s.id} s={s} now={now} />
                  ))}
                </ul>
              </div>
            );
          })}
        </div>
      )}
    </Section>
  );
}

function SessionRow({ s, now }: { s: FleetSession; now: number }) {
  const title = s.name ?? folder(s.cwd);
  return (
    <li className={cn("flex items-start gap-2 py-1.5", s.presence === "offline" && "opacity-70")}>
      <Dot s={s} className="mt-1.5" />
      <div className="min-w-0 flex-1">
        <p className="flex items-baseline gap-2">
          <span className={cn("truncate text-sm font-medium", !s.name && "italic")}>{title ?? "без имени"}</span>
          {isService(s.id) ? (
            <span className="shrink-0 text-[11px] text-muted-foreground">служба подписки, не чат</span>
          ) : (
            <code className="shrink-0 font-mono text-[11px] text-muted-foreground" title={s.id}>
              {shortId(s.id)}
            </code>
          )}
        </p>
        <p className={cn("truncate text-xs", s.silent ? "text-stale-fg" : "text-muted-foreground")}>
          {presenceText(s, now)}
          {s.cwd && <span title={s.cwd}> · {s.cwd}</span>}
        </p>
        {s.note && <p className="truncate text-xs">«{s.note}»</p>}
      </div>
    </li>
  );
}

function Dot({ s, className }: { s: FleetSession | undefined; className?: string }) {
  const tone =
    !s || s.presence === "unknown"
      ? "border border-dashed border-unknown bg-transparent"
      : s.presence === "offline"
        ? "bg-unchecked"
        : s.silent
          ? "bg-stale"
          : "bg-ok";
  return <span className={cn("inline-block size-2 shrink-0 rounded-full", tone, className)} aria-hidden />;
}

/** Ссылка на сессию в строке: имя (или машина и папка) с точкой присутствия; подробности — во всплывающей подсказке. */
function SessionRef({ id, byId, now, plain }: { id: string; byId: Lookup; now: number; plain?: boolean }) {
  const s = byId(id);
  return (
    <span className="inline-flex max-w-full items-baseline gap-1" title={`${shortId(id)} · ${presenceText(s, now)}${s?.cwd ? ` · ${s.cwd}` : ""}`}>
      {!plain && <Dot s={s} className="self-center" />}
      <span className={cn("truncate font-medium", !s?.name && "italic")}>{sessionTitle(s, id)}</span>
      {!s?.name && <code className="font-mono text-[10px] opacity-75">{shortId(id)}</code>}
    </span>
  );
}

function RefList({ ids, byId, now }: { ids: string[]; byId: Lookup; now: number }) {
  return (
    <>
      {ids.map((id, i) => (
        <span key={id}>
          {i > 0 && ", "}
          <SessionRef id={id} byId={byId} now={now} plain />
        </span>
      ))}
    </>
  );
}

// ───────────── Замки ─────────────

function ScopeTag({ scope }: { scope: FleetLock["scope"] }) {
  return (
    <span className="shrink-0 rounded-full border border-current/30 px-1.5 text-[10px] leading-4 font-normal opacity-80">
      {scope === "fleet" ? "общий" : "этой машины"}
    </span>
  );
}

function Locks({ view, byId, now }: { view: FleetView; byId: Lookup; now: number }) {
  return (
    <Section title="Замки" note="Общие (хаб) — одни на все машины; остальные — ёмкость и ресурсы этой машины.">
      {view.fleetResources.length > 0 && (
        <ul className="mb-2 flex flex-wrap gap-1.5 text-xs" aria-label="Общие ресурсы">
          {view.fleetResources.map((r) => (
            <li
              key={r.name}
              title={r.label ?? undefined}
              className={cn("rounded-full border px-2 py-0.5", r.used ? "border-foreground/30 font-medium" : "border-ok/40 bg-ok-bg text-ok-fg")}
            >
              {r.name} · {r.used ? "занят" : "свободен"}
            </li>
          ))}
        </ul>
      )}
      {view.locks.length === 0 ? (
        <Empty>Никто ничего не держит.</Empty>
      ) : (
        <ul className="space-y-1.5">
          {view.locks.map((l) => (
            <LockRow key={`${l.scope}:${l.key}:${l.session}`} l={l} byId={byId} now={now} />
          ))}
        </ul>
      )}
    </Section>
  );
}

function LockRow({ l, byId, now }: { l: FleetLock; byId: Lookup; now: number }) {
  const held = l.since ? now - new Date(l.since).getTime() : null;
  return (
    <li className={cn("rounded-md border px-2.5 py-1.5", l.problem && "border-root/50 bg-root-bg/60")}>
      <p className="flex items-baseline gap-2 text-sm">
        <span className="font-semibold">{l.resource}</span>
        {l.repo && <span className="truncate text-xs text-muted-foreground">{l.repo}</span>}
        <ScopeTag scope={l.scope} />
        <span className="ml-auto shrink-0 text-xs text-muted-foreground">
          {held === null ? "свободен" : `держит ${fmtDuration(held)}`}
          {held !== null && l.ttlMin ? ` · срок ${l.ttlMin} мин` : ""}
        </span>
      </p>
      {l.session ? (
        <p className="text-xs">
          <SessionRef id={l.session} byId={byId} now={now} />
          {l.units > 1 && <span className="text-muted-foreground"> · единиц ёмкости: {l.units}</span>}
        </p>
      ) : (
        <p className="text-xs text-muted-foreground">Никто не держит, но очередь стоит.</p>
      )}
      {l.command && (
        <p className="truncate font-mono text-[11px] text-muted-foreground" title={l.command}>
          {l.command}
        </p>
      )}
      {l.problem && <p className="text-xs font-medium text-root-fg">{problemText(l, l.session ? byId(l.session) : undefined, now)}</p>}
      {l.queue.length > 0 && (
        <p className="text-xs">
          <span className="text-muted-foreground">в очереди: </span>
          {l.queue.map((q, i) => (
            <span key={q.session}>
              {i > 0 && ", "}
              <SessionRef id={q.session} byId={byId} now={now} plain />
              <span className="text-muted-foreground"> ({fmtDuration(now - new Date(q.since).getTime())})</span>
            </span>
          ))}
        </p>
      )}
    </li>
  );
}

// ───────────── Эта машина ─────────────

function Machine({ view, byId, now }: { view: FleetView; byId: Lookup; now: number }) {
  const { local } = view;
  return (
    <Section title={`Эта машина${view.machine ? ` — ${view.machine}` : ""}`}>
      {!local.found ? (
        <Empty>
          Диспетчер прогонов здесь не установлен: нет каталога <code className="font-mono text-xs">{local.dir}</code>.
        </Empty>
      ) : (
        <div className="space-y-3">
          <ul className="grid grid-cols-[repeat(auto-fill,minmax(150px,1fr))] gap-2" aria-label="Ёмкость машины">
            {local.resources.map((r) => (
              <li key={r.name} title={r.label ?? undefined} className="rounded-md border px-2 py-1">
                <p className="flex items-baseline justify-between gap-2 text-xs">
                  <span className="truncate font-medium">{r.name}</span>
                  <span className={cn("shrink-0", r.used >= r.capacity ? "font-semibold" : "text-muted-foreground")}>
                    {r.used} из {r.capacity}
                  </span>
                </p>
                <div className="mt-1 flex h-1.5 gap-0.5" role="img" aria-label={`занято ${r.used} из ${r.capacity}`}>
                  {Array.from({ length: r.capacity }, (_, i) => (
                    <span key={i} className={cn("flex-1 rounded-full", i < r.used ? "bg-foreground/70" : "bg-muted")} />
                  ))}
                </div>
              </li>
            ))}
          </ul>

          <div>
            <h4 className="mb-1 text-xs font-semibold">Идущие прогоны</h4>
            {local.runs.length === 0 ? (
              <Empty>Ничего не идёт.</Empty>
            ) : (
              <ul className="space-y-1">
                {local.runs.map((r, i) => {
                  const gone = r.session ? byId(r.session)?.presence === "offline" : false;
                  return (
                    <li key={i} className="text-xs">
                      <p className="flex items-baseline gap-2">
                        <span className="font-medium">{r.resource}</span>
                        <span className="truncate font-mono text-[11px] text-muted-foreground" title={r.command ?? undefined}>
                          {r.command}
                        </span>
                        <span className="ml-auto shrink-0 text-muted-foreground">
                          {r.background && "в фоне · "}
                          {r.startedAt ? `идёт ${fmtDuration(now - new Date(r.startedAt).getTime())}` : ""}
                        </span>
                      </p>
                      <p className="text-muted-foreground">
                        {r.session ? <SessionRef id={r.session} byId={byId} now={now} /> : "сессия неизвестна"}
                        {gone && <span className="text-warn-fg"> — сессия не в сети, прогон, похоже, потерян</span>}
                      </p>
                    </li>
                  );
                })}
              </ul>
            )}
          </div>

          {local.errors.length > 0 && (
            <div className="rounded-md border border-warn/50 bg-warn-bg px-2.5 py-1.5 text-xs text-warn-fg">
              <p className="flex items-center gap-1 font-medium">
                <FileWarning className="size-3.5" aria-hidden /> Не прочитались файлы диспетчера — остальное показано:
              </p>
              <ul className="mt-0.5 list-disc pl-4 font-mono text-[11px] break-all">
                {local.errors.map((e) => (
                  <li key={e}>{e}</li>
                ))}
              </ul>
            </div>
          )}
        </div>
      )}
    </Section>
  );
}

// ───────────── Заявки, доска, задачи ─────────────

function Claims({ claims, byId, now }: { claims: FleetClaim[]; byId: Lookup; now: number }) {
  return (
    <Section title="Заявки на файлы" note="Пересечением считаются только сессии в сети: заявка ушедшей — призрак, а не предупреждение.">
      {claims.length === 0 ? (
        <Empty>Заявок нет.</Empty>
      ) : (
        <ul className="space-y-1">
          {claims.map((c) => (
            <li key={`${c.repo}:${c.file}`} className={cn("rounded-md px-2 py-1 text-xs", c.overlap ? "border border-root/50 bg-root-bg/60" : "bg-muted/40")}>
              <p className="flex items-baseline gap-2">
                <span className="truncate font-mono text-[11px] font-medium" title={c.file}>
                  {c.file}
                </span>
                {c.repo && <span className="truncate text-muted-foreground">{c.repo}</span>}
                {c.overlap && <span className="ml-auto shrink-0 font-semibold text-root-fg">правят {c.sessions.length}</span>}
              </p>
              <p>
                {c.sessions.map((h, i) => (
                  <span key={h.session} className={cn(byId(h.session)?.presence === "offline" && "opacity-60")}>
                    {i > 0 && ", "}
                    <SessionRef id={h.session} byId={byId} now={now} />
                    <span className="text-muted-foreground"> {ago(h.since, now)}</span>
                  </span>
                ))}
              </p>
            </li>
          ))}
        </ul>
      )}
    </Section>
  );
}

function Board({ view, byId, now }: { view: FleetView; byId: Lookup; now: number }) {
  return (
    <Section title="Доска">
      {view.board.length === 0 ? (
        <Empty>Никто не отмечался последние полтора часа.</Empty>
      ) : (
        <ul className="space-y-1.5">
          {view.board.map((b) => (
            <li key={b.session} className="text-xs">
              <p className="flex items-baseline gap-2">
                <SessionRef id={b.session} byId={byId} now={now} />
                {b.task && <span className="shrink-0 rounded border px-1 text-[10px]">{b.task}</span>}
                <span className="ml-auto shrink-0 text-muted-foreground">{ago(b.at, now)}</span>
              </p>
              <p className="text-sm break-words">{b.text || "—"}</p>
            </li>
          ))}
        </ul>
      )}
    </Section>
  );
}

const STATUSES: TaskStatus[] = ["IN_PROGRESS", "TODO", "FAILED", "DONE"];

function Tasks({ tasks, hubOk, byId, now }: { tasks: FleetTask[]; hubOk: boolean; byId: Lookup; now: number }) {
  // По умолчанию — всё, кроме готового: готовые копятся, а смотрят на них редко.
  const [only, setOnly] = useState<TaskStatus | null>(null);
  const count = (st: TaskStatus) => tasks.filter((t) => t.status === st).length;
  const shown = tasks.filter((t) => (only ? t.status === only : t.status !== "DONE"));

  return (
    <Section title="Задачи брокера">
      {!hubOk ? (
        <Empty>Задачи живут на хабе — без него не видны.</Empty>
      ) : tasks.length === 0 ? (
        <Empty>Задач нет.</Empty>
      ) : (
        <>
          <div className="mb-2 flex flex-wrap gap-1.5" role="group" aria-label="Задачи по состояниям">
            {STATUSES.map((st) => (
              <button
                key={st}
                type="button"
                aria-pressed={only === st}
                onClick={() => setOnly(only === st ? null : st)}
                className={cn(
                  "rounded-full border px-2 py-0.5 text-xs outline-none focus-visible:ring-3 focus-visible:ring-ring/60",
                  only === st ? "border-foreground bg-foreground text-background" : "hover:bg-muted",
                  st === "FAILED" && count(st) > 0 && only !== st && "border-root/50 text-root-fg",
                )}
              >
                {TASK_LABEL[st]} <span className="font-semibold">{count(st)}</span>
              </button>
            ))}
          </div>
          {shown.length === 0 ? (
            <Empty>{only ? "Таких задач нет." : "Незакрытых задач нет — только готовые."}</Empty>
          ) : (
            <ul className="space-y-1.5">
              {shown.map((t) => (
                <TaskRow key={t.id} t={t} byId={byId} now={now} />
              ))}
            </ul>
          )}
        </>
      )}
    </Section>
  );
}

function TaskRow({ t, byId, now }: { t: FleetTask; byId: Lookup; now: number }) {
  let meta: ReactNode;
  if (t.status === "IN_PROGRESS")
    meta = (
      <>
        {t.session ? <SessionRef id={t.session} byId={byId} now={now} /> : (t.machine ?? "исполнитель неизвестен")}
        {t.node && ` · узел ${t.node}`}
        {t.startedAt && ` · идёт ${fmtDuration(now - new Date(t.startedAt).getTime())}`}
      </>
    );
  else if (t.status === "TODO") meta = `ждёт ${t.createdAt ? fmtDuration(now - new Date(t.createdAt).getTime()) : ""}${t.node ? ` · для узла ${t.node}` : ""}`;
  else meta = [t.finishedAt && ago(t.finishedAt, now), t.attempts > 1 && `попыток ${t.attempts}`, t.note].filter(Boolean).join(" · ");

  return (
    <li className={cn("text-xs", t.status === "FAILED" && "rounded-md border border-root/40 bg-root-bg/50 px-2 py-1")}>
      <p className="flex items-baseline gap-2">
        <code className="shrink-0 font-mono text-[11px] text-muted-foreground">{t.id}</code>
        <span className="min-w-0 truncate text-sm">{t.title}</span>
        {t.priority !== "NORMAL" && <span className="shrink-0 rounded border px-1 text-[10px] font-medium">{t.priority}</span>}
        {t.executor && t.executor !== "claude" && <span className="shrink-0 text-[10px] text-muted-foreground">{t.executor}</span>}
      </p>
      <p className={cn("truncate", t.status === "FAILED" ? "text-root-fg" : "text-muted-foreground")}>
        <span className="font-medium">{TASK_STATE[t.status]}</span>
        {meta ? " · " : ""}
        {meta}
      </p>
    </li>
  );
}

// ───────────── Мелочи ─────────────

function Section({ title, note, children }: { title: string; note?: string; children: ReactNode }) {
  return (
    <section className="min-w-0">
      <h3 className="mb-1 text-[13px] font-semibold">{title}</h3>
      {note && <p className="mb-2 text-xs text-muted-foreground">{note}</p>}
      {children}
    </section>
  );
}

function Empty({ children }: { children: ReactNode }) {
  return <p className="rounded-md border border-dashed px-3 py-2 text-xs text-muted-foreground">{children}</p>;
}
