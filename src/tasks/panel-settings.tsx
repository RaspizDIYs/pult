import { KeyRound, Link, Trash2 } from "lucide-react";
import { useEffect, useState, type FormEvent } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { errText } from "@/lib/pult";
import { panel, type PanelConfig } from "./api";

/**
 * Адрес панели и токен. Адрес — в настройках (репозиторий публичный, вшивать его нельзя),
 * токен — в системной связке ключей: интерфейс его не видит, только «задан» или «нет».
 */
export function PanelSettings({ heading = true }: { heading?: boolean }) {
  const [cfg, setCfg] = useState<PanelConfig | null>(null);
  const [url, setUrl] = useState("");
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    panel.config().then(
      (c) => {
        if (!live) return;
        setCfg(c);
        setUrl(c.url ?? "");
      },
      (e) => live && setError(errText(e)),
    );
    return () => {
      live = false;
    };
  }, []);

  async function run(action: () => Promise<PanelConfig>) {
    setBusy(true);
    setError(null);
    try {
      const c = await action();
      setCfg(c);
      setUrl(c.url ?? "");
      return true;
    } catch (e) {
      setError(errText(e));
      return false;
    } finally {
      setBusy(false);
    }
  }

  const saveUrl = (e: FormEvent) => {
    e.preventDefault();
    void run(() => panel.setUrl(url.trim() || null));
  };
  const saveToken = async (e: FormEvent) => {
    e.preventDefault();
    // Поле очищаем сразу после сохранения: токен не должен висеть в форме.
    if (await run(() => panel.setToken(token.trim()))) setToken("");
  };

  return (
    <section className="space-y-3">
      {heading && (
        <div>
          <h3 className="text-sm font-semibold">Панель задач</h3>
          <p className="text-xs text-muted-foreground">Откуда вкладка «Задачи» берёт каны. Пульт только читает: задачи здесь не меняются.</p>
        </div>
      )}

      <form onSubmit={saveUrl} className="space-y-1.5">
        <Label htmlFor="panel-url">Адрес панели</Label>
        <div className="flex gap-2">
          <Input id="panel-url" value={url} onChange={(e) => setUrl(e.target.value)} placeholder="https://tasks.example.com" spellCheck={false} autoComplete="off" className="font-mono" />
          <Button type="submit" variant="outline" disabled={!cfg || busy || url.trim() === (cfg.url ?? "")}>
            <Link /> Сохранить
          </Button>
        </div>
      </form>

      <form onSubmit={saveToken} className="space-y-1.5">
        <Label htmlFor="panel-token">Токен</Label>
        <p className="text-xs text-muted-foreground">
          {cfg?.tokenError
            ? cfg.tokenError
            : cfg?.tokenSet
              ? "Задан и хранится в связке ключей системы. Показать его нельзя — только заменить или удалить."
              : "Не задан. Токен выдаёт владелец панели; хранится в связке ключей системы, не в файлах."}
        </p>
        <div className="flex gap-2">
          <Input
            id="panel-token"
            type="password"
            value={token}
            onChange={(e) => setToken(e.target.value)}
            placeholder={cfg?.tokenSet ? "новый токен" : "вставь токен"}
            spellCheck={false}
            autoComplete="off"
            className="font-mono"
          />
          <Button type="submit" variant="outline" disabled={!cfg || busy || !token.trim()}>
            <KeyRound /> {cfg?.tokenSet ? "Заменить" : "Сохранить"}
          </Button>
          {cfg?.tokenSet && (
            <Button type="button" variant="ghost" disabled={busy} onClick={() => void run(() => panel.setToken(null))} aria-label="Удалить токен" title="Удалить токен">
              <Trash2 />
            </Button>
          )}
        </div>
      </form>
      {error && <p className="text-xs text-root-fg">{error}</p>}
    </section>
  );
}
