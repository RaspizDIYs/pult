// Состояние интерфейса: снимок от ядра плюс поток изменений. Подписываемся раньше, чем
// просим снимок, а устаревшие по номеру цикла события отбрасываем (контракт, правило 6).
import { useCallback, useEffect, useState } from "react";
import { errText, pult, type Snapshot } from "./pult";

export function usePult() {
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [cycleAt, setCycleAt] = useState<number | null>(null);

  const load = useCallback(async () => {
    try {
      const s = await pult.getSnapshot();
      setSnapshot(s);
      setCycleAt(new Date(s.takenAt).getTime());
      setError(null);
    } catch (e) {
      setError(errText(e));
    }
  }, []);

  useEffect(() => {
    let dead = false;
    const offs: (() => void)[] = [];
    const keep = (p: Promise<() => void>) =>
      p.then(
        (off) => (dead ? off() : offs.push(off)),
        (e) => setError((prev) => prev ?? `не удалось подписаться на события ядра: ${errText(e)}`),
      );

    void keep(
      pult.onSnapshot((s) => {
        setSnapshot(s);
        setCycleAt(Date.now());
        setError(null);
      }),
    );
    void keep(
      pult.onStates(({ cycle, states }) => {
        setCycleAt(Date.now());
        setSnapshot((prev) => {
          if (!prev || cycle < prev.cycle) return prev;
          const merged = { ...prev.states };
          for (const s of states) merged[s.id] = s;
          return { ...prev, cycle, states: merged };
        });
      }),
    );
    void load();

    return () => {
      dead = true;
      offs.forEach((off) => off());
    };
  }, [load]);

  return { snapshot, error, cycleAt, reload: load };
}

/** Текущее время для подписей «5 мин назад» — обновляется само, без новых данных от ядра. */
export function useNow(intervalMs = 30_000): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), intervalMs);
    return () => clearInterval(id);
  }, [intervalMs]);
  return now;
}
