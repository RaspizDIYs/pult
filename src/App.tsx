import { getVersion } from "@tauri-apps/api/app";
import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import { UpdateBanner } from "@/components/update-banner";
import { useUpdater } from "@/lib/updater";

function formatTime(date: Date) {
  return date.toLocaleTimeString("ru-RU", { hour: "2-digit", minute: "2-digit" });
}

export default function App() {
  const updater = useUpdater();
  const [version, setVersion] = useState("…");

  useEffect(() => {
    getVersion().then(setVersion, () => setVersion("—"));
  }, []);

  const { update, checkedAt, error } = updater;
  const status = !checkedAt
    ? "проверяю…"
    : error
      ? `не удалось проверить (${formatTime(checkedAt)}): ${error}`
      : update
        ? `доступна ${update.version} (проверено в ${formatTime(checkedAt)})`
        : `установлена последняя (проверено в ${formatTime(checkedAt)})`;

  return (
    <div className="flex h-screen flex-col">
      <header className="flex items-baseline gap-2 border-b px-4 py-2">
        <h1 className="text-base font-semibold">Пульт</h1>
        <span className="text-xs text-muted-foreground">{version}</span>
      </header>
      <UpdateBanner updater={updater} />
      <main className="m-4 flex flex-1 items-center justify-center rounded-lg border border-dashed text-muted-foreground">
        Здесь будет карта
      </main>
      <section className="flex items-center gap-3 border-t px-4 py-2 text-xs text-muted-foreground">
        <span className="font-medium text-foreground">Настройки · Обновления:</span>
        <span className="min-w-0 flex-1 truncate" title={status}>
          {status}
        </span>
        <Button size="xs" variant="outline" onClick={updater.runCheck}>
          Проверить сейчас
        </Button>
      </section>
    </div>
  );
}
