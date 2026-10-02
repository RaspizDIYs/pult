import { CloudOff, KeyRound, LoaderCircle, RefreshCw, Search, Settings as SettingsIcon, X } from "lucide-react";
import { useEffect, useMemo, useState, type KeyboardEvent, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { Switch } from "@/components/ui/switch";
import { fmtDayTime, fmtTime, TONE, type Tone } from "@/lib/model";
import { errText } from "@/lib/pult";
import { cn } from "@/lib/utils";
import { CANCELLED, COLUMNS, OTHER, panel, refKey, STATE_LABEL, useTasks, type Fetched, type Task, type TaskDetail, type TaskRef, type TaskState } from "./api";
import { PanelSettings } from "./panel-settings";

// Готовых у команды под тысячу: рисовать все — тормозить на каждой букве поиска.
const DONE_LIMIT = 50;

/** «14:32» сегодня, «30.09, 14:32» — раньше: «данные от 14:32» позавчерашние вводили бы в заблуждение. */
function stamp(iso: string, now: number): string {
  return new Date(iso).toDateString() === new Date(now).toDateString() ? fmtTime(iso) : fmtDayTime(iso);
}

const upper = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);

interface Status {
  tone: Tone;
  text: string;
  /** Чинится в настройках, а не ожиданием. */
  settings: boolean;
}

/** Плашка источника: откуда данные на доске и насколько им верить. */
export function sourceStatus(f: Fetched<unknown>, now: number): Status {
  const when = f.fetchedAt ? stamp(f.fetchedAt, now) : null;
  const shown = f.data !== null && when ? `показаны данные от ${when}` : "сохранённых данных нет";
  switch (f.problem) {
    case null:
      return { tone: "ok", text: when && now - Date.parse(f.fetchedAt!) < 60_000 ? "Обновлено только что" : `Обновлено в ${when}`, settings: false };
    case "unavailable":
      return {
        tone: f.data ? "stale" : "unknown",
        text: f.data ? `Данные от ${when}, панель недоступна: ${f.error}` : `Панель недоступна: ${f.error}. Сохранённых данных нет`,
        settings: false,
      };
    case "auth":
      return { tone: "root", text: `Токен не подошёл (${f.error}) · ${shown}`, settings: true };
    case "noToken":
      return { tone: "unknown", text: `${upper(f.error ?? "токен не задан")} · ${shown}`, settings: true };
    case "noUrl":
      return { tone: "unknown", text: `Адрес панели не задан · ${shown}`, settings: true };
  }
}

export function TasksScreen({ active, now, onSettings }: { active: boolean; now: number; onSettings: () => void }) {
  const { tasks, projects, epics, overview, loading, error, mine, setMine, reload } = useTasks(active);
  const [project, setProject] = useState("");
  const [query, setQuery] = useState("");
  const [showCancelled, setShowCancelled] = useState(false);
  // Сама задача, а не ключ: после обновления списка или смены фильтра карточка остаётся —
  // человек её читает. Закрывается только явно.
  const [selected, setSelected] = useState<Task | null>(null);

  const all = tasks?.data ?? null;

  // Родитель — номер в том же проекте. Эпики знает отдельный список: при «моих» эпика в задачах может не быть.
  const parents = useMemo(() => {
    const m = new Map<string, { key: string | null; title: string }>();
    for (const t of all ?? []) if (t.keyNum !== null) m.set(refKey(t.project, t.keyNum), t);
    for (const e of epics?.data ?? []) if (e.keyNum !== null) m.set(refKey(e.project, e.keyNum), e);
    return m;
  }, [all, epics]);

  const projectNames = useMemo(() => projects?.data ?? [...new Set((all ?? []).map((t) => t.project))].sort(), [projects, all]);

  const visible = useMemo(() => {
    const q = query.trim().toLocaleLowerCase("ru");
    return (all ?? []).filter(
      (t) => (!project || t.project === project) && (!q || t.title.toLocaleLowerCase("ru").includes(q) || (t.key ?? "").toLocaleLowerCase("ru").includes(q)),
    );
  }, [all, project, query]);

  const byState = useMemo(() => {
    const m = new Map<TaskState, Task[]>();
    for (const t of visible) m.set(t.state, [...(m.get(t.state) ?? []), t]);
    // Готовые — свежие сверху: старые закрытые никто не ищет глазами.
    m.get("done")?.sort((a, b) => (b.doneAt ?? "").localeCompare(a.doneAt ?? ""));
    return m;
  }, [visible]);

  const columns = [...COLUMNS, ...(showCancelled ? [CANCELLED] : []), ...(byState.has("unknown") ? [OTHER] : [])];
  const cancelled = byState.get("cancelled")?.length ?? 0;

  const status = tasks ? sourceStatus(tasks, now) : null;

  let body: ReactNode;
  if (!tasks) {
    body = error ? (
      <Center icon={<CloudOff className="size-6" />} title="Ядро не ответило">
        <p className="rounded-md border border-root/50 bg-root-bg px-3 py-2 font-mono text-xs break-words text-root-fg">{error}</p>
        <div className="text-center">
          <Button onClick={() => void reload()}>Попробовать ещё раз</Button>
        </div>
      </Center>
    ) : (
      <div className="flex flex-1 items-center justify-center gap-2 text-sm text-muted-foreground" role="status">
        <LoaderCircle className="size-4 animate-spin" aria-hidden /> Загружаю задачи…
      </div>
    );
  } else if (!all && (tasks.problem === "noUrl" || tasks.problem === "noToken")) {
    body = (
      <Center icon={<KeyRound className="size-6" />} title="Панель задач не настроена">
        <p className="text-center">Пульт показывает каны из панели задач и помнит последнее, что видел, — на случай, когда панель недоступна. Нужны адрес панели и твой токен.</p>
        <PanelSettings heading={false} />
      </Center>
    );
  } else if (!all) {
    body = (
      <Center icon={<CloudOff className="size-6" />} title={tasks.problem === "auth" ? "Токен не подошёл" : "Панель недоступна"}>
        <p className="text-center">Свежих задач нет, а сохранённых ещё не было: Пульт ни разу не получил ответа от этой панели.</p>
        <p className="rounded-md border border-root/50 bg-root-bg px-3 py-2 font-mono text-xs break-words text-root-fg">{tasks.error}</p>
        <div className="flex justify-center gap-2">
          <Button onClick={() => void reload()} disabled={loading}>
            <RefreshCw className={cn(loading && "animate-spin")} /> Обновить
          </Button>
          <Button variant="outline" onClick={onSettings}>
            <SettingsIcon /> Настройки
          </Button>
        </div>
      </Center>
    );
  } else {
    body = (
      <>
        <div className="flex flex-wrap items-center gap-x-4 gap-y-2 border-b px-4 py-2">
          <div className="flex items-center gap-2">
            <Label htmlFor="task-project" className="text-xs text-muted-foreground">
              Проект
            </Label>
            <NativeSelect id="task-project" size="sm" value={project} onChange={(e) => setProject(e.target.value)}>
              <NativeSelectOption value="">все проекты</NativeSelectOption>
              {projectNames.map((p) => (
                <NativeSelectOption key={p} value={p}>
                  {p}
                </NativeSelectOption>
              ))}
            </NativeSelect>
          </div>
          <div className="flex items-center gap-2">
            <Switch id="task-mine" checked={mine} onCheckedChange={setMine} size="sm" />
            <Label htmlFor="task-mine" className="text-sm">
              Только мои
            </Label>
          </div>
          <div className="relative w-64 max-w-full">
            <Search className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" aria-hidden />
            <Input
              type="search"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="Поиск по ключу и названию"
              aria-label="Поиск по ключу и названию"
              className="h-7 pl-8"
            />
          </div>
          <div className="flex items-center gap-2">
            <Switch id="task-cancelled" checked={showCancelled} onCheckedChange={setShowCancelled} size="sm" />
            <Label htmlFor="task-cancelled" className="text-sm">
              Отменённые{cancelled ? ` (${cancelled})` : ""}
            </Label>
          </div>
          <span className="flex-1" />
          {overview?.data && (
            <span className="text-xs text-muted-foreground" title="Вся панель, без фильтров">
              У команды: открыто {overview.data.open} · в работе {overview.data.byState.doing} · на ревью {overview.data.byState.review} · закрыто за неделю {overview.data.closedWeek}
            </span>
          )}
        </div>

        <div className="flex min-h-0 flex-1 flex-col lg:flex-row">
          {selected && (
            <TaskPanel
              task={selected}
              parentOf={(t) => (t.epicNum !== null ? (parents.get(refKey(t.project, t.epicNum)) ?? null) : null)}
              now={now}
              onOpen={(key) => setSelected(all.find((t) => t.key === key) ?? { ...selected, key, title: "загружаю…", epicNum: null, priority: null, who: null, tags: [] })}
              onClose={() => setSelected(null)}
              className="order-2 shrink-0 grow-0 basis-[56%] lg:w-[26rem] lg:basis-auto xl:w-[30rem]"
            />
          )}
          <main className="order-1 min-h-0 min-w-0 flex-1 overflow-x-auto">
            <div className="grid h-full auto-cols-[minmax(15rem,1fr)] grid-flow-col gap-3 p-3">
              {columns.map((c) => {
                const list = byState.get(c.state) ?? [];
                const shown = c.state === "done" ? list.slice(0, DONE_LIMIT) : list;
                return (
                  <section key={c.state} aria-label={`${c.label}: ${list.length}`} className="flex min-h-0 flex-col rounded-xl bg-muted/60">
                    <h2 className="flex items-baseline gap-2 px-3 pt-2.5 pb-1.5 text-[13px] font-semibold">
                      {c.label}
                      <span className="text-xs font-normal text-muted-foreground">{list.length}</span>
                    </h2>
                    <ul className="min-h-0 flex-1 space-y-1.5 overflow-y-auto px-2 pb-2">
                      {shown.map((t, i) => (
                        <li key={t.key ?? `${t.project}-${i}`}>
                          <TaskCard
                            task={t}
                            parent={t.epicNum !== null ? (parents.get(refKey(t.project, t.epicNum)) ?? null) : null}
                            selected={selected === t || (!!t.key && selected?.key === t.key)}
                            onSelect={() => setSelected(t)}
                          />
                        </li>
                      ))}
                      {!list.length && <li className="px-1 py-2 text-xs text-muted-foreground">{query || project ? "ничего не нашлось" : "пусто"}</li>}
                      {shown.length < list.length && <li className="px-1 py-1 text-xs text-muted-foreground">…и ещё {list.length - shown.length} раньше</li>}
                    </ul>
                  </section>
                );
              })}
            </div>
          </main>
        </div>
      </>
    );
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className={cn("flex flex-wrap items-center gap-x-3 gap-y-1 border-b px-4 py-1.5 text-sm", status ? TONE[status.tone].box : "text-muted-foreground")} role="status">
        {status ? (
          <>
            <StatusIcon tone={status.tone} />
            <span className="min-w-0 flex-1 break-words">{status.text}</span>
            {status.settings && (
              <Button size="xs" variant="outline" onClick={onSettings} className="bg-background/60">
                <SettingsIcon /> Настройки
              </Button>
            )}
          </>
        ) : (
          <span className="flex-1">Панель задач</span>
        )}
        <Button size="xs" variant="outline" onClick={() => void reload()} disabled={loading} className="bg-background/60">
          <RefreshCw className={cn(loading && "animate-spin")} /> {loading ? "Обновляю…" : "Обновить"}
        </Button>
      </div>
      {body}
    </div>
  );
}

function StatusIcon({ tone }: { tone: Tone }) {
  const { Icon } = TONE[tone];
  return <Icon className="size-4 shrink-0" aria-hidden />;
}

function Center({ icon, title, children }: { icon: ReactNode; title: string; children?: ReactNode }) {
  return (
    <div className="flex flex-1 items-center justify-center overflow-y-auto p-6">
      <div className="w-full max-w-lg space-y-4 text-center">
        <div className="mx-auto grid size-12 place-items-center rounded-full bg-muted text-muted-foreground">{icon}</div>
        <h2 className="text-lg font-semibold">{title}</h2>
        <div className="space-y-3 text-left text-sm text-muted-foreground">{children}</div>
      </div>
    </div>
  );
}

const PRIORITY_STYLE = ["border-root/60 bg-root text-white", "border-warn/60 bg-warn-bg text-warn-fg", "border-border text-foreground/80", "border-border text-muted-foreground"];

function Priority({ p }: { p: number | null }) {
  if (p === null) return null;
  return (
    <span className={cn("inline-flex h-4 shrink-0 items-center rounded border px-1 font-mono text-[10px] font-semibold", PRIORITY_STYLE[p] ?? PRIORITY_STYLE[3])} title={`приоритет ${p}`}>
      p{p}
    </span>
  );
}

/** Ключ всегда рядом с названием: по номерам задачи не помнят. */
function Keyed({ k, title, className }: { k: string | null; title: string; className?: string }) {
  return (
    <span className={className}>
      <span className="mr-1.5 font-mono text-[0.85em] font-normal text-muted-foreground">{k ?? "без ключа"}</span>
      {title}
    </span>
  );
}

function TaskCard({ task: t, parent, selected, onSelect }: { task: Task; parent: { key: string | null; title: string } | null; selected: boolean; onSelect: () => void }) {
  return (
    <button
      type="button"
      onClick={onSelect}
      aria-pressed={selected}
      className={cn(
        "w-full rounded-lg border bg-card px-2.5 py-2 text-left outline-none hover:border-foreground/25 focus-visible:ring-3 focus-visible:ring-ring/60",
        selected && "ring-2 ring-foreground/70",
      )}
    >
      <div className="flex items-start gap-1.5">
        <Priority p={t.priority} />
        <Keyed k={t.key} title={t.title} className="min-w-0 flex-1 text-[13px] leading-snug font-medium" />
      </div>
      <div className="mt-1 flex flex-wrap gap-x-2 gap-y-0.5 text-[11px] text-muted-foreground">
        <span>{t.project}</span>
        <span>{t.who ?? "без исполнителя"}</span>
        {t.isEpic && <span className="font-medium text-foreground/70">эпик</span>}
      </div>
      {t.epicNum !== null && (
        <p className="mt-0.5 truncate text-[11px] text-muted-foreground" title={parent ? `${parent.key ?? ""} ${parent.title}` : undefined}>
          ↳ {parent ? <Keyed k={parent.key} title={parent.title} /> : `родитель № ${t.epicNum}`}
        </p>
      )}
    </button>
  );
}

function TaskPanel({
  task,
  parentOf,
  now,
  onOpen,
  onClose,
  className,
}: {
  task: Task;
  parentOf: (t: Task) => { key: string | null; title: string } | null;
  now: number;
  onOpen: (key: string) => void;
  onClose: () => void;
  className?: string;
}) {
  const [detail, setDetail] = useState<Fetched<TaskDetail> | null>(null);
  const [error, setError] = useState<string | null>(null);
  const key = task.key;

  useEffect(() => {
    setDetail(null);
    setError(null);
    if (!key) return;
    let live = true;
    panel.task(key).then(
      (d) => live && setDetail(d),
      (e) => live && setError(errText(e)),
    );
    return () => {
      live = false;
    };
  }, [key]);

  // Подробности не пришли и не сохранены — показываем то, что было в списке.
  const t: Task = detail?.data ?? task;
  const d = detail?.data ?? null;
  const parent: TaskRef | { key: string | null; title: string } | null = d?.parent ?? parentOf(task);

  const onKeyDown = (e: KeyboardEvent<HTMLElement>) => {
    if (e.key === "Escape") {
      e.stopPropagation();
      onClose();
    }
  };

  let note: string | null = null;
  if (!key) note = "У этой задачи нет ключа — подробностей панель по ней не отдаёт.";
  else if (error) note = `Подробности не загрузились: ${error}`;
  else if (detail?.problem && detail.data) note = `Подробности от ${stamp(detail.fetchedAt!, now)}: ${detail.error}`;
  else if (detail?.problem) note = `Подробностей нет — панель недоступна, а сохранённых нет: ${detail.error}`;

  return (
    <aside aria-label={`Задача ${t.key ?? ""} «${t.title}»`} onKeyDown={onKeyDown} className={cn("flex min-h-0 flex-col border-t bg-background lg:border-t-0 lg:border-l", className)}>
      <header className="flex items-start gap-3 border-b px-4 py-3">
        <div className="min-w-0 flex-1">
          <h2 className="text-base leading-snug font-semibold">
            <Keyed k={t.key} title={t.title} />
          </h2>
          <p className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted-foreground">
            <span className="rounded-full border px-1.5 leading-4 text-foreground/80">{STATE_LABEL[t.state]}</span>
            <Priority p={t.priority} />
            <span>{t.project}</span>
            <span>{t.who ?? "без исполнителя"}</span>
            {t.kind && <span>{t.kind}</span>}
          </p>
        </div>
        <Button size="icon-sm" variant="ghost" aria-label="Закрыть (Esc)" title="Закрыть (Esc)" onClick={onClose}>
          <X />
        </Button>
      </header>

      <div className="min-h-0 flex-1 space-y-4 overflow-y-auto px-4 py-3 text-sm">
        {note && <p className={cn("rounded-md border px-2.5 py-1.5 text-xs", TONE.stale.box)}>{note}</p>}
        {key && !detail && !error && (
          <p className="flex items-center gap-2 text-xs text-muted-foreground" role="status">
            <LoaderCircle className="size-3.5 animate-spin" aria-hidden /> Загружаю подробности…
          </p>
        )}

        {parent && (
          <Block title="Родитель">
            {parent.key ? (
              <button type="button" onClick={() => onOpen(parent.key!)} className="text-left hover:underline">
                <Keyed k={parent.key} title={parent.title} />
              </button>
            ) : (
              <Keyed k={null} title={parent.title} />
            )}
          </Block>
        )}

        {d && (
          <Block title="Описание">
            {d.body?.trim() ? (
              // ponytail: markdown показан как текст; рендер, если описания станут нечитаемыми
              <div className="font-sans leading-relaxed break-words whitespace-pre-wrap">{d.body.trim()}</div>
            ) : (
              <p className="text-muted-foreground">описания нет</p>
            )}
          </Block>
        )}

        {d && d.kids.length > 0 && (
          <Block title={`Подзадачи · ${d.kids.length}`}>
            <ul className="space-y-1">
              {d.kids.map((k, i) => (
                <li key={k.key ?? i} className="flex items-start gap-1.5">
                  <span className="mt-px w-20 shrink-0 text-xs text-muted-foreground">{STATE_LABEL[k.state]}</span>
                  {k.key ? (
                    <button type="button" onClick={() => onOpen(k.key!)} className="min-w-0 text-left hover:underline">
                      <Keyed k={k.key} title={k.title} />
                    </button>
                  ) : (
                    <Keyed k={null} title={k.title} />
                  )}
                </li>
              ))}
            </ul>
          </Block>
        )}

        {d && (
          <Block title="Журнал">
            {d.journal.length ? (
              <ul className="space-y-1.5 border-l pl-3">
                {d.journal.map((line, i) => (
                  <li key={i} className="break-words">
                    {line}
                  </li>
                ))}
              </ul>
            ) : (
              <p className="text-muted-foreground">записей нет</p>
            )}
          </Block>
        )}

        <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-0.5 text-xs">
          {(
            [
              ["Заведена", t.created],
              ["Взята", t.takenAt],
              ["Сделана", t.doneAt],
              ["Теги", t.tags.length ? t.tags.map((x) => `#${x}`).join(" ") : null],
            ] as const
          )
            .filter(([, v]) => v)
            .map(([k, v]) => (
              <div key={k} className="contents">
                <dt className="text-muted-foreground">{k}</dt>
                <dd>{v}</dd>
              </div>
            ))}
        </dl>
      </div>
    </aside>
  );
}

function Block({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section>
      <h3 className="mb-1.5 text-[13px] font-semibold">{title}</h3>
      {children}
    </section>
  );
}
