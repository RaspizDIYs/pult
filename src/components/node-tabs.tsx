import { ArrowDown, Pause, Play } from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { StatePlaque } from "@/components/state-plaque";
import { Button } from "@/components/ui/button";
import { fmtDayTime, toneOfOwn } from "@/lib/model";
import { errText, pult, type HistoryEntry, type LogEndEvent, type LogEvent, type NodeState } from "@/lib/pult";
import { cn } from "@/lib/utils";

function Notice({ children }: { children: ReactNode }) {
  return <p className="rounded-md border border-dashed px-3 py-2 text-sm text-muted-foreground">{children}</p>;
}

// ───────────── История ─────────────

const OWN_LABEL = { ok: "Работает", fail: "Отказ", unknown: "Неизвестно", stale: "Устарело", unchecked: "Не проверяется" } as const;

export function HistoryTab({ id, state }: { id: string; state: NodeState | undefined }) {
  const [items, setItems] = useState<HistoryEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Новый переход приходит событием состояния — тогда же перечитываем историю.
  const stamp = `${state?.own}|${state?.fact}`;

  useEffect(() => {
    let live = true;
    pult.getHistory(id, 50).then(
      (r) => {
        if (!live) return;
        setItems([...r].sort((a, b) => new Date(b.at).getTime() - new Date(a.at).getTime()));
        setError(null);
      },
      (e) => live && setError(errText(e)),
    );
    return () => {
      live = false;
    };
  }, [id, stamp]);

  if (error) return <Notice>История пока недоступна: {error}</Notice>;
  if (!items) return <Notice>Загружаю историю…</Notice>;
  if (!items.length) return <Notice>Переходов состояния пока не было.</Notice>;
  return (
    <ol className="divide-y">
      {items.map((h, i) => (
        <li key={`${h.at}-${i}`} className="flex gap-3 py-2">
          <time dateTime={h.at} className="w-24 shrink-0 pt-0.5 text-xs text-muted-foreground tabular-nums">
            {fmtDayTime(h.at)}
          </time>
          <div className="min-w-0 space-y-1">
            <StatePlaque tone={toneOfOwn(h.own)} label={OWN_LABEL[h.own]} />
            <p className="text-sm break-words">{h.fact}</p>
          </div>
        </li>
      ))}
    </ol>
  );
}

// ───────────── Логи ─────────────

const MAX_LINES = 2000;
type Pending = { k: "log"; e: LogEvent } | { k: "end"; e: LogEndEvent };

export function LogsTab({ id }: { id: string }) {
  const [lines, setLines] = useState<string[]>([]);
  const [status, setStatus] = useState<"opening" | "live" | "ended">("opening");
  const [error, setError] = useState<string | null>(null);
  const [frozen, setFrozen] = useState<string[] | null>(null); // не null — стоит на паузе
  const [follow, setFollow] = useState(true);
  const box = useRef<HTMLDivElement>(null);

  // Вкладка открыта — поток идёт; закрыта или узел сменился — поток закрываем (контракт, п. 4).
  useEffect(() => {
    let dead = false;
    let streamId: string | null = null;
    const offs: (() => void)[] = [];
    // Первые строки могут прийти раньше, чем open_logs вернёт номер потока, — не теряем их.
    const early: Pending[] = [];
    setLines([]);
    setStatus("opening");
    setError(null);

    const handle = (p: Pending) => {
      if (p.e.streamId !== streamId) return;
      if (p.k === "log") setLines((prev) => (prev.length + p.e.lines.length > MAX_LINES ? [...prev, ...p.e.lines].slice(-MAX_LINES) : [...prev, ...p.e.lines]));
      else {
        setStatus("ended");
        if (p.e.error) setError(p.e.error);
      }
    };
    const sub = async (pending: Promise<() => void>) => {
      const off = await pending;
      if (dead) off();
      else offs.push(off);
    };

    void (async () => {
      try {
        await sub(pult.onLog((e) => (streamId === null ? early.push({ k: "log", e }) : handle({ k: "log", e }))));
        await sub(pult.onLogEnd((e) => (streamId === null ? early.push({ k: "end", e }) : handle({ k: "end", e }))));
        const r = await pult.openLogs(id, 200);
        if (dead) {
          void pult.closeLogs(r.streamId).catch(() => {});
          return;
        }
        streamId = r.streamId;
        setStatus((s) => (s === "opening" ? "live" : s));
        early.splice(0).forEach(handle);
      } catch (e) {
        if (!dead) {
          setError(errText(e));
          setStatus("ended");
        }
      }
    })();

    return () => {
      dead = true;
      offs.forEach((off) => off());
      if (streamId) void pult.closeLogs(streamId).catch(() => {});
    };
  }, [id]);

  const shown = frozen ?? lines;
  useLayoutEffect(() => {
    const el = box.current;
    if (el && follow && !frozen) el.scrollTop = el.scrollHeight;
  }, [shown, follow, frozen]);

  const onScroll = () => {
    const el = box.current;
    if (el) setFollow(el.scrollHeight - el.scrollTop - el.clientHeight < 24);
  };

  const missed = frozen ? lines.length - frozen.length : 0;
  return (
    <div className="flex h-full min-h-56 flex-col gap-2">
      <div className="flex items-center gap-2 text-xs text-muted-foreground">
        <span aria-live="polite">
          {status === "opening" ? "Подключаюсь к логам…" : status === "live" ? "Поток идёт" : "Поток закрыт"} · строк: {shown.length}
        </span>
        <span className="flex-1" />
        {!follow && !frozen && (
          <Button size="xs" variant="outline" onClick={() => setFollow(true)}>
            <ArrowDown /> К концу
          </Button>
        )}
        <Button
          size="xs"
          variant="outline"
          onClick={() => (frozen ? (setFrozen(null), setFollow(true)) : setFrozen(lines))}
          aria-pressed={!!frozen}
        >
          {frozen ? <Play /> : <Pause />}
          {frozen ? `Продолжить${missed ? ` (+${missed})` : ""}` : "Пауза"}
        </Button>
      </div>
      {error && <Notice>{error}</Notice>}
      <div
        ref={box}
        onScroll={onScroll}
        tabIndex={0}
        role="log"
        aria-label="Логи контейнера"
        className="min-h-40 flex-1 overflow-auto rounded-md border bg-muted/40 p-2 font-mono text-[11px] leading-relaxed"
      >
        {shown.map((l, i) => (
          <div key={i} className={cn("break-all whitespace-pre-wrap", /\b(ERROR|FATAL)\b/.test(l) && "text-root-fg", /\bWARN\b/.test(l) && "text-warn-fg")}>
            {l}
          </div>
        ))}
        {!shown.length && status !== "opening" && !error && <span className="text-muted-foreground">Пока ни одной строки.</span>}
      </div>
    </div>
  );
}
