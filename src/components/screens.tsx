import { CircleX, LoaderCircle, MapPinned, PackageOpen, PlugZap, TriangleAlert } from "lucide-react";
import type { ReactNode } from "react";
import { InventoryPathForm, useSettings } from "@/components/settings-dialog";
import { Button } from "@/components/ui/button";

// Экраны без карты. Каждый отвечает на вопрос «что происходит и что мне сделать».

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

export function LoadingScreen() {
  return (
    <div className="flex flex-1 items-center justify-center gap-2 text-sm text-muted-foreground" role="status">
      <LoaderCircle className="size-4 animate-spin" aria-hidden /> Загружаю карту…
    </div>
  );
}

export function LoadErrorScreen({ error, onRetry }: { error: string; onRetry: () => void }) {
  return (
    <Center icon={<PlugZap className="size-6" />} title="Ядро не отвечает">
      <p className="text-center">Пульт не смог получить карту. Приложение при этом работает: можно открыть настройки и проверить окружение.</p>
      <p className="rounded-md border border-root/50 bg-root-bg px-3 py-2 font-mono text-xs break-words text-root-fg">{error}</p>
      <div className="text-center">
        <Button onClick={onRetry}>Попробовать ещё раз</Button>
      </div>
    </Center>
  );
}

export function NoInventoryScreen() {
  const store = useSettings();
  return (
    <Center icon={<MapPinned className="size-6" />} title="Инвентарь не задан">
      <p className="text-center">
        Карта строится по инвентарю — это файл, где описано, что у тебя есть и от чего зависит. Укажи каталог, в котором он лежит.
      </p>
      <InventoryPathForm store={store} />
    </Center>
  );
}

export function EmptyInventoryScreen({ path, onSettings }: { path: string | null; onSettings: () => void }) {
  return (
    <Center icon={<PackageOpen className="size-6" />} title="В инвентаре нет узлов">
      <p className="text-center">
        Инвентарь прочитан{path ? <> (<code className="font-mono text-xs">{path}</code>)</> : ""}, но в нём пусто. Добавь узлы в инвентарь.yaml — карта появится сама.
      </p>
      <div className="text-center">
        <Button variant="outline" onClick={onSettings}>
          Открыть настройки
        </Button>
      </div>
    </Center>
  );
}

export function InventoryErrorScreen({ error, onSettings }: { error: string; onSettings: () => void }) {
  return (
    <Center icon={<CircleX className="size-6 text-root" />} title="Инвентарь не принят">
      <p className="text-center">Ошибка в инвентаре, а принятой раньше копии нет — показывать пока нечего.</p>
      <p className="rounded-md border border-root/50 bg-root-bg px-3 py-2 font-mono text-xs break-words text-root-fg">{error}</p>
      <div className="text-center">
        <Button variant="outline" onClick={onSettings}>
          Открыть настройки
        </Button>
      </div>
    </Center>
  );
}

/** Инвентарь отклонён, но есть последний принятый: карта работает, ошибка висит над ней. */
export function InventoryBanner({ error, onSettings }: { error: string; onSettings: () => void }) {
  return (
    <div role="alert" className="flex items-start gap-2 border-b border-root/40 bg-root-bg px-4 py-2 text-sm text-root-fg">
      <TriangleAlert className="mt-0.5 size-4 shrink-0" aria-hidden />
      <p className="min-w-0 flex-1 break-words">
        <strong>Инвентарь не принят, карта показана по последнему принятому.</strong> {error}
      </p>
      <Button size="xs" variant="outline" onClick={onSettings}>
        Подробнее
      </Button>
    </div>
  );
}
