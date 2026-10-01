import { info, warn } from "@tauri-apps/plugin-log";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { useCallback, useEffect, useState } from "react";

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

export type UpdaterState = {
  update: Update | null;
  checkedAt: Date | null;
  error: string | null;
  installing: boolean;
};

export function useUpdater() {
  const [state, setState] = useState<UpdaterState>({
    update: null,
    checkedAt: null,
    error: null,
    installing: false,
  });

  const runCheck = useCallback(async () => {
    try {
      const update = await check();
      logInfo(update ? `проверка обновлений: доступна ${update.version}` : "проверка обновлений: установлена последняя");
      setState((s) => ({ ...s, update, checkedAt: new Date(), error: null }));
    } catch (e) {
      // Без сети или VPN проверка падает регулярно — это не повод мешать работе окном.
      const error = String(e);
      void warn(`проверка обновлений: ${error}`).catch(() => {});
      setState((s) => ({ ...s, checkedAt: new Date(), error }));
    }
  }, []);

  useEffect(() => {
    void runCheck();
    const id = setInterval(runCheck, CHECK_INTERVAL_MS);
    return () => clearInterval(id);
  }, [runCheck]);

  const install = useCallback(async () => {
    if (!state.update) return;
    setState((s) => ({ ...s, installing: true }));
    try {
      // На Windows установщик сам закрывает и перезапускает приложение, до relaunch дело не дойдёт.
      logInfo(`установка обновления ${state.update.version}: скачиваю и ставлю`);
      await state.update.downloadAndInstall();
      logInfo(`установка обновления ${state.update.version}: готово, перезапуск`);
      await relaunch();
    } catch (e) {
      const error = explainUpdateError(String(e));
      void warn(`установка обновления: ${error}`).catch(() => {});
      setState((s) => ({ ...s, installing: false, error }));
    }
  }, [state.update]);

  return { ...state, runCheck, install };
}
