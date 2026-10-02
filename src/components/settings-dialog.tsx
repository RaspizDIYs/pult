import { open as pickDirectory } from "@tauri-apps/plugin-dialog";
import { CircleCheck, CircleX, FolderOpen, FolderSearch, TriangleAlert } from "lucide-react";
import { useCallback, useEffect, useState, type FormEvent } from "react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { fmtDayTime, fmtTime } from "@/lib/model";
import { errText, isTauri, pult, type EnvCheck, type InventoryInfo, type Settings } from "@/lib/pult";
import type { useUpdater } from "@/lib/updater";
import { PanelSettings } from "@/tasks/panel-settings";

export function useSettings(active = true) {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    if (!active) return;
    let live = true;
    pult.getSettings().then(
      (s) => {
        if (!live) return;
        setSettings(s);
        setError(null);
      },
      (e) => live && setError(errText(e)),
    );
    return () => {
      live = false;
    };
  }, [active]);

  // set_settings принимает настройки целиком, поэтому меняем поле поверх последних прочитанных.
  const save = useCallback(
    async (patch: Partial<Settings>) => {
      if (!settings) return false;
      setSaving(true);
      try {
        setSettings(await pult.setSettings({ ...settings, ...patch }));
        setError(null);
        return true;
      } catch (e) {
        setError(errText(e));
        return false;
      } finally {
        setSaving(false);
      }
    },
    [settings],
  );

  return { settings, error, saving, save };
}

// Хранилище настроек общее для всей формы: set_settings принимает настройки целиком, и два
// независимых экземпляра перезаписывали бы друг другу устаревшие значения.
export function InventoryPathForm({ store, compact = false }: { store: ReturnType<typeof useSettings>; compact?: boolean }) {
  const { settings, error, saving, save } = store;
  const [path, setPath] = useState("");
  useEffect(() => setPath(settings?.inventoryPath ?? ""), [settings?.inventoryPath]);

  async function submit(e: FormEvent) {
    e.preventDefault();
    await save({ inventoryPath: path.trim() || null });
  }

  // Выбранный каталог сохраняем сразу: человек уже показал, что хочет именно его.
  async function choose() {
    const dir = await pickDirectory({ directory: true, multiple: false, title: "Каталог с инвентарь.yaml", defaultPath: path || undefined });
    if (typeof dir !== "string") return;
    setPath(dir);
    await save({ inventoryPath: dir });
  }

  return (
    <form onSubmit={submit} className="space-y-1.5">
      <Label htmlFor="inventory-path">Каталог с инвентарём</Label>
      <div className="flex gap-2">
        <Input
          id="inventory-path"
          value={path}
          onChange={(e) => setPath(e.target.value)}
          placeholder="/Users/ты/pult-inventory"
          spellCheck={false}
          autoComplete="off"
          className="font-mono"
        />
        {isTauri && (
          <Button type="button" variant="outline" onClick={() => void choose()} disabled={!settings || saving}>
            <FolderSearch /> Выбрать…
          </Button>
        )}
        <Button type="submit" disabled={!settings || saving || path.trim() === (settings.inventoryPath ?? "")}>
          <FolderOpen /> {compact ? "Сохранить" : "Сохранить путь"}
        </Button>
      </div>
      <p className="text-xs text-muted-foreground">Каталог, где лежит файл инвентарь.yaml. Если это git-клон, Пульт сам подтягивает свежее.</p>
      {error && <p className="text-xs text-root-fg">{error}</p>}
    </form>
  );
}

function Environment() {
  const [res, setRes] = useState<EnvCheck[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function run() {
    setBusy(true);
    setError(null);
    try {
      setRes(await pult.checkEnvironment());
    } catch (e) {
      setError(errText(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="space-y-2">
      <div className="flex items-center justify-between gap-2">
        <div>
          <h3 className="text-sm font-semibold">Проверка окружения</h3>
          <p className="text-xs text-muted-foreground">Есть ли на этой машине всё, без чего проверки не заработают: ssh, git, ключ, доступ к инвентарю.</p>
        </div>
        <Button size="sm" variant="outline" onClick={run} disabled={busy}>
          {busy ? "Проверяю…" : "Проверить"}
        </Button>
      </div>
      {error && <p className="text-xs text-root-fg">{error}</p>}
      {res && (
        <ul className="divide-y rounded-md border text-sm">
          {res.map((c) => (
            <li key={c.name} className="flex items-start gap-2 px-2.5 py-1.5">
              {c.ok ? <CircleCheck className="mt-0.5 size-4 shrink-0 text-ok" aria-label="в порядке" /> : <CircleX className="mt-0.5 size-4 shrink-0 text-root" aria-label="не в порядке" />}
              <span className="min-w-0">
                <span className="font-medium">{c.name}</span>
                <span className="block text-xs break-words text-muted-foreground">{c.detail}</span>
              </span>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

function InventoryState({ inv }: { inv: InventoryInfo | null }) {
  if (!inv) return null;
  const rows: [string, string][] = [
    ["Путь", inv.path ?? "не задан"],
    ["Версия (коммит)", inv.commit ?? "—"],
    ["Загружен", inv.loadedAt ? fmtDayTime(inv.loadedAt) : "—"],
  ];
  return (
    <section className="space-y-2">
      <h3 className="text-sm font-semibold">Состояние инвентаря</h3>
      <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
        {rows.map(([k, v]) => (
          <div key={k} className="contents">
            <dt className="text-muted-foreground">{k}</dt>
            <dd className="min-w-0 font-mono text-xs break-all">{v}</dd>
          </div>
        ))}
      </dl>
      {inv.error && (
        <p className="rounded-md border border-root/50 bg-root-bg px-2.5 py-1.5 text-xs text-root-fg">
          <strong>Ошибка:</strong> {inv.error}
        </p>
      )}
      {inv.warnings.length > 0 && (
        <div className="rounded-md border border-warn/50 bg-warn-bg px-2.5 py-1.5 text-xs text-warn-fg">
          <p className="mb-1 flex items-center gap-1 font-semibold">
            <TriangleAlert className="size-3.5" aria-hidden /> Предупреждения ({inv.warnings.length})
          </p>
          <ul className="list-disc space-y-0.5 pl-4">
            {inv.warnings.map((w) => (
              <li key={w}>{w}</li>
            ))}
          </ul>
        </div>
      )}
    </section>
  );
}

function UpdatesBlock({ updater, version }: { updater: ReturnType<typeof useUpdater>; version: string }) {
  const { update, checkedAt, error } = updater;
  const when = checkedAt ? fmtTime(checkedAt.toISOString()) : "";
  const status = !isTauri
    ? "обновления проверяются только в самом приложении, не в браузере."
    : !checkedAt
    ? "проверяю…"
    : error
      ? `не удалось проверить (${when}): ${error}`
      : update
        ? `доступна ${update.version} (проверено в ${when})`
        : `установлена последняя (проверено в ${when})`;
  return (
    <section className="space-y-2">
      <h3 className="text-sm font-semibold">Обновления</h3>
      <div className="flex items-center gap-3 text-sm">
        <span className="min-w-0 flex-1 break-words text-muted-foreground">
          <span className="text-foreground">Версия {version}.</span> {status}
        </span>
        <Button size="sm" variant="outline" onClick={updater.runCheck} disabled={!isTauri}>
          Проверить сейчас
        </Button>
      </div>
    </section>
  );
}

export function SettingsDialog({
  open,
  onOpenChange,
  inventory,
  updater,
  version,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  inventory: InventoryInfo | null;
  updater: ReturnType<typeof useUpdater>;
  version: string;
}) {
  const store = useSettings(open);
  const { settings, saving, save } = store;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[88vh] gap-5 overflow-y-auto sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>Настройки</DialogTitle>
          <DialogDescription>Всё хранится на этой машине. Сервера у Пульта нет.</DialogDescription>
        </DialogHeader>

        <InventoryPathForm store={store} compact />
        <InventoryState inv={inventory} />

        <section className="space-y-3">
          <h3 className="text-sm font-semibold">Поведение</h3>
          <div className="flex items-center justify-between gap-3">
            <Label htmlFor="notifications" className="flex-col items-start gap-0.5">
              <span>Уведомления</span>
              <span className="text-xs font-normal text-muted-foreground">Только про подтверждённые поломки, которые начались с корня.</span>
            </Label>
            <Switch id="notifications" checked={settings?.notifications ?? false} disabled={!settings || saving} onCheckedChange={(v) => void save({ notifications: v })} />
          </div>
          <div className="flex items-center justify-between gap-3">
            <Label htmlFor="autostart" className="flex-col items-start gap-0.5">
              <span>Запускать при входе в систему</span>
              <span className="text-xs font-normal text-muted-foreground">Пульт сидит в трее и следит, пока компьютер включён.</span>
            </Label>
            <Switch id="autostart" checked={settings?.autostart ?? false} disabled={!settings || saving} onCheckedChange={(v) => void save({ autostart: v })} />
          </div>
        </section>

        <PanelSettings />
        <Environment />
        <UpdatesBlock updater={updater} version={version} />
      </DialogContent>
    </Dialog>
  );
}
