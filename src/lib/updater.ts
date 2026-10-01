import { warn } from "@tauri-apps/plugin-log";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { useCallback, useEffect, useState } from "react";

const CHECK_INTERVAL_MS = 6 * 60 * 60 * 1000;

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
      setState((s) => ({ ...s, update, checkedAt: new Date(), error: null }));
    } catch (e) {
      // Без сети или VPN проверка падает регулярно — это не повод мешать работе окном.
      const error = String(e);
      void warn(`проверка обновлений: ${error}`);
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
      await state.update.downloadAndInstall();
      await relaunch();
    } catch (e) {
      const error = String(e);
      void warn(`установка обновления: ${error}`);
      setState((s) => ({ ...s, installing: false, error }));
    }
  }, [state.update]);

  return { ...state, runCheck, install };
}
