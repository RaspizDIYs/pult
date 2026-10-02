import { info, warn } from "@tauri-apps/plugin-log";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { useCallback, useEffect, useState } from "react";
import { pult } from "./pult";

const CHECK_INTERVAL_MS = 6 * 60 * 60 * 1000;

// Сырой текст ошибки замены бандла («Cross-device link») человеку ничего не говорит —
// добавляем, что делать. Та же фраза — в src-tauri/src/lib.rs для флага --apply-update.
export function explainUpdateError(e: string): string {
  const hint = /Cross-device link|os error 18/.test(e)
    ? "не удалось заменить приложение на месте: оно запущено не из «Программ» (из образа диска или копией с карантином). Перенеси Пульт в «Программы», открой оттуда и обнови снова"
    : /Read-only file system|os error 30/.test(e)
      ? "приложение лежит на диске только для чтения (например, в открытом образе .dmg): перенеси Пульт в «Программы»"
      : /Permission denied|os error 13/.test(e)
        ? "нет прав заменить файлы приложения: проверь, что Пульт лежит в «Программах» и принадлежит тебе"
        : null;
  return hint ? `${hint} (${e})` : e;
}

const logInfo = (text: string) => void info(text).catch(() => {});

// Одна проверка за раз на всё окно: двойной запуск эффекта (StrictMode) или щелчок по кнопке
// во время проверки не плодят вторую. Строка в логе пишется здесь — один раз и с причиной запуска.
let checking: Promise<Update | null> | null = null;
function checkOnce(reason: string) {
  checking ??= check()
    .then((update) => {
      logInfo(`проверка обновлений (${reason}): ${update ? `доступна ${update.version}` : "установлена последняя"}`);
      return update;
    })
    .catch((e) => {
      // Без сети или VPN проверка падает регулярно — это не повод мешать работе окном.
      void warn(`проверка обновлений (${reason}): ${String(e)}`).catch(() => {});
      throw e;
    })
    .finally(() => (checking = null));
  return checking;
}

export type UpdaterState = {
  update: Update | null;
  checkedAt: Date | null;
  /** Ошибка проверки. Ошибка установки — отдельно: это другое действие и другое лечение. */
  error: string | null;
  installError: { at: Date; text: string } | null;
  installing: boolean;
  /** Почему обновление не поставить (спрашиваем у ядра), или null. */
  blocker: string | null;
};

export function useUpdater() {
  const [state, setState] = useState<UpdaterState>({
    update: null,
    checkedAt: null,
    error: null,
    installError: null,
    installing: false,
    blocker: null,
  });

  const runCheck = useCallback(async (reason = "вручную") => {
    try {
      const update = await checkOnce(reason);
      // Обновления больше нет (поставили руками) — прежняя ошибка установки уже не про нас.
      setState((s) => ({ ...s, update, checkedAt: new Date(), error: null, installError: update ? s.installError : null }));
    } catch (e) {
      setState((s) => ({ ...s, checkedAt: new Date(), error: String(e) }));
    }
  }, []);

  useEffect(() => {
    void runCheck("при запуске");
    const id = setInterval(() => void runCheck("по расписанию"), CHECK_INTERVAL_MS);
    return () => clearInterval(id);
  }, [runCheck]);

  useEffect(() => {
    // Ядро решает по пути запуска; в браузере (макет) и при отказе команды считаем, что можно.
    pult.getUpdateBlocker().then(
      (blocker) => setState((s) => ({ ...s, blocker })),
      () => {},
    );
  }, []);

  const install = useCallback(async () => {
    if (!state.update || state.blocker) return;
    setState((s) => ({ ...s, installing: true, installError: null }));
    try {
      // На Windows установщик сам закрывает и перезапускает приложение, до relaunch дело не дойдёт.
      logInfo(`установка обновления ${state.update.version}: скачиваю и ставлю`);
      await state.update.downloadAndInstall();
      logInfo(`установка обновления ${state.update.version}: готово, перезапуск`);
      await relaunch();
    } catch (e) {
      const text = explainUpdateError(String(e));
      void warn(`установка обновления: ${text}`).catch(() => {});
      setState((s) => ({ ...s, installing: false, installError: { at: new Date(), text } }));
    }
  }, [state.update, state.blocker]);

  return { ...state, runCheck, install };
}
