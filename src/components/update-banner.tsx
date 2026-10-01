import { useState } from "react";
import { Button } from "@/components/ui/button";
import type { useUpdater } from "@/lib/updater";

export function UpdateBanner({ updater }: { updater: ReturnType<typeof useUpdater> }) {
  const [hiddenVersion, setHiddenVersion] = useState<string | null>(null);
  const { update, installing, install } = updater;
  if (!update || update.version === hiddenVersion) return null;

  return (
    <div className="flex items-start gap-3 border-b bg-muted/60 px-4 py-2 text-sm">
      <div className="min-w-0 flex-1">
        <div>Доступна версия {update.version}</div>
        {update.body && (
          <div className="mt-1 whitespace-pre-line text-muted-foreground">{update.body}</div>
        )}
      </div>
      <Button size="sm" onClick={install} disabled={installing}>
        {installing ? "Устанавливаю…" : "Обновить и перезапустить"}
      </Button>
      <Button size="sm" variant="ghost" onClick={() => setHiddenVersion(update.version)} disabled={installing}>
        Позже
      </Button>
    </div>
  );
}
