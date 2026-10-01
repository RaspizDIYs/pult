import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { error as logError, warn as logWarn } from "@tauri-apps/plugin-log";
import App from "./App";
import { errText, isTauri } from "./lib/pult";
import "./index.css";

// Ошибки веб-окна — в лог приложения: иначе после запуска из «Программ» их не увидеть.
if (isTauri) {
  const send = (write: typeof logError, text: string) => void write(`окно: ${text}`).catch(() => {});
  for (const [level, write] of [["error", logError], ["warn", logWarn]] as const) {
    const original = console[level].bind(console);
    console[level] = (...args: unknown[]) => {
      original(...args);
      send(write, args.map((a) => (typeof a === "string" ? a : errText(a))).join(" "));
    };
  }
  window.addEventListener("error", (e) => send(logError, `${e.message} (${e.filename}:${e.lineno})`));
  window.addEventListener("unhandledrejection", (e) => send(logError, `необработанный отказ: ${errText(e.reason)}`));
}

// Тема — как в системе, и следует за ней без перезапуска (ночью macOS переключается сама).
const systemDark = matchMedia("(prefers-color-scheme: dark)");
const applyTheme = () => document.documentElement.classList.toggle("dark", systemDark.matches);
applyTheme();
systemDark.addEventListener("change", applyTheme);

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
