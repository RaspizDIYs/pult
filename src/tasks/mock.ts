// Макет панели задач для браузера без Tauri. Всё выдумано: репозиторий публичный.
//
// Параметр адреса ?tasks= выбирает сценарий, чтобы его можно было рассматривать и снимать:
//   fresh (по умолчанию) — панель отвечает, «обновлено только что»;
//   stale     — панель недоступна, показаны данные от последнего удачного запроса;
//   auth      — панель отвергла токен (401), показаны сохранённые данные;
//   no-token  — токен удалён, показаны сохранённые данные;
//   first-run — адрес и токен не заданы, сохранённого нет: экран настройки;
//   empty     — панель недоступна, сохранённого нет;
//   loading   — ядро не отвечает на первый запрос.
import type { Epic, Fetched, Overview, PanelConfig, Problem, Task, TaskDetail, TaskRef, TaskState } from "./api";

const MIN = 60_000;
const DAY = 24 * 60 * MIN;
const t0 = Date.now();
const day = (n: number) => new Date(t0 - n * DAY).toISOString().slice(0, 10);

type Def = [key: string, state: TaskState, title: string, priority: number | null, who: string | null, extra?: Partial<Task>];

const PROJECTS: Record<string, string> = { SHOP: "Магазин", SITE: "Сайт", BOT: "Бот", INFRA: "Инфра" };
const ME = "алиса";

// Порядок — как отдаёт панель: в порядке хранилища, задачи сгруппированы по эпикам.
const DEFS: Def[] = [
  ["SHOP-10", "doing", "Новая корзина", 1, "боб", { isEpic: true, kind: "Эпик" }],
  ["SHOP-12", "doing", "fix(cart): корзина теряет промокод после входа", 0, "алиса", { epicNum: 10, kind: "Баг", tags: ["касса"] }],
  ["SHOP-13", "review", "feat(cart): сохранять корзину между устройствами", 1, "боб", { epicNum: 10 }],
  ["SHOP-14", "todo", "Пересчитывать доставку при смене адреса", 2, null, { epicNum: 10 }],
  ["SHOP-15", "done", "Кнопка «повторить заказ»", 2, "вера", { epicNum: 10, doneAt: day(2) }],
  ["SHOP-16", "todo", "Скидка не применяется к товарам из подборки", 1, "алиса", { kind: "Баг" }],
  ["SHOP-17", "cancelled", "Корзина в виде боковой панели", null, null, { epicNum: 10 }],
  ["SHOP-18", "done", "fix(cart): двойное списание при медленной сети", 0, "алиса", { kind: "Баг", doneAt: day(1) }],
  ["SITE-30", "doing", "Переезд сайта на новый движок", 2, "вера", { isEpic: true, kind: "Эпик" }],
  ["SITE-31", "doing", "Перенести блог со старыми адресами", 2, "вера", { epicNum: 30 }],
  ["SITE-32", "todo", "Редиректы со старых страниц", 2, null, { epicNum: 30 }],
  ["SITE-33", "review", "Страница цен: новая сетка тарифов", 1, "алиса", { epicNum: 30 }],
  ["SITE-34", "done", "Сжать картинки на главной", 3, "боб", { epicNum: 30, doneAt: day(4) }],
  ["SITE-35", "todo", "Тёмная тема для документации", 3, null, {}],
  ["SITE-36", "cancelled", "Анимация логотипа при загрузке", null, "вера", {}],
  ["SITE-37", "done", "Форма обратной связи теряет вложения", 1, "вера", { kind: "Баг", doneAt: day(6) }],
  ["BOT-7", "doing", "Бот отвечает дважды на одно сообщение", 0, "боб", { kind: "Баг", tags: ["срочно"] }],
  ["BOT-8", "todo", "Команда /отчёт за неделю", 2, "алиса", {}],
  ["BOT-9", "todo", "Напоминание о забытых задачах раз в день", null, null, {}],
  ["BOT-10", "done", "Обновить библиотеку бота до новой версии", 2, "боб", { doneAt: day(3) }],
  ["BOT-11", "review", "Справка по командам", 3, "вера", {}],
  ["INFRA-3", "todo", "Резервная копия базы на второй диск", 1, "боб", {}],
  ["INFRA-4", "doing", "Мониторинг места на дисках", 2, "алиса", {}],
  ["INFRA-5", "done", "Продлить сертификат тестового стенда", 1, "боб", { doneAt: day(9) }],
  ["INFRA-6", "cancelled", "Перейти на другой хостинг", null, null, {}],
];

const TASKS: Task[] = DEFS.map(([key, state, title, priority, who, extra = {}]) => {
  const [prefix, num] = key.split("-");
  return {
    key,
    keyNum: Number(num),
    epicNum: null,
    state,
    title,
    priority,
    kind: "Задача",
    isEpic: false,
    who,
    project: PROJECTS[prefix],
    tags: [],
    created: day(20),
    updated: day(1),
    takenAt: state === "todo" ? null : day(8),
    doneAt: null,
    ...extra,
  };
});
// Старая задача хранилища без ключа: такие у панели есть, подробностей по ним не получить.
TASKS.push({ ...TASKS[13], key: null, keyNum: null, state: "todo", title: "Разобрать старые заметки по сайту", priority: null, who: null });

const ref = (t: Task): TaskRef => ({ key: t.key, title: t.title, state: t.state, priority: t.priority, who: t.who });

function detail(key: string): TaskDetail | null {
  const t = TASKS.find((x) => x.key === key);
  if (!t) return null;
  const parent = t.epicNum ? TASKS.find((x) => x.project === t.project && x.keyNum === t.epicNum) : undefined;
  const kids = TASKS.filter((x) => x.project === t.project && x.epicNum === t.keyNum && t.keyNum !== null);
  const rich = key === "SHOP-12";
  return {
    ...t,
    body: rich
      ? "## Что видно\n\nПосле входа в аккаунт промокод пропадает из корзины, хотя скидка уже показана.\n\n## Как повторить\n\n1. Добавить товар без входа\n2. Ввести промокод\n3. Войти — промокода нет\n\n## Где смотреть\n\nСессия гостя не переносится в сессию пользователя."
      : t.state === "todo"
        ? null
        : "Короткое описание задачи из панели.",
    journal: rich
      ? [`${day(3)} алиса: взяла, воспроизводится на тесте`, `${day(2)} алиса: причина в сбросе сессии при входе`, `${day(1)} боб: посмотрел — похоже на то же, что SHOP-18`]
      : t.state === "todo"
        ? []
        : [`${day(5)} ${t.who ?? "кто-то"}: взято в работу`],
    parent: parent ? ref(parent) : null,
    kids: kids.map(ref),
  };
}

const EPICS: Epic[] = TASKS.filter((t) => t.isEpic).map((e) => {
  const kids = TASKS.filter((t) => t.project === e.project && t.epicNum === e.keyNum);
  return { key: e.key, keyNum: e.keyNum, title: e.title, state: e.state, project: e.project, total: kids.length, done: kids.filter((k) => k.state === "done").length };
});

const count = (s: TaskState) => TASKS.filter((t) => t.state === s).length;
const OVERVIEW: Overview = {
  open: count("todo") + count("doing") + count("review"),
  total: TASKS.length,
  closedWeek: TASKS.filter((t) => t.doneAt && t.doneAt >= day(7)).length,
  closedMonth: count("done"),
  byState: { todo: count("todo"), doing: count("doing"), review: count("review"), done: count("done"), cancelled: count("cancelled") },
};

// ───────────── Сценарии ─────────────

type Scenario = "fresh" | "stale" | "auth" | "no-token" | "first-run" | "empty" | "loading";
const param = typeof location !== "undefined" ? new URLSearchParams(location.search).get("tasks") : null;
let scenario: Scenario = (["fresh", "stale", "auth", "no-token", "first-run", "empty", "loading"] as const).find((s) => s === param) ?? "fresh";

let config: PanelConfig = {
  url: scenario === "first-run" ? null : "https://tasks.example.com",
  tokenSet: scenario !== "first-run" && scenario !== "no-token",
  tokenError: null,
};

/** Когда был последний удачный ответ: «данные от …» должно быть заметно не «только что». */
const savedAt = new Date(t0 - 47 * MIN).toISOString();

const FAILURE: Record<Exclude<Scenario, "fresh" | "loading">, [Problem, string]> = {
  stale: ["unavailable", "нет ответа за 8 с"],
  auth: ["auth", "панель ответила 401: неизвестный токен"],
  "no-token": ["noToken", "токен не задан"],
  "first-run": ["noUrl", "адрес панели не задан"],
  empty: ["unavailable", "имя tasks.example.com не разрешается"],
};
// Сохранённое бывает только после удачного ответа — как в ядре.
let cached = scenario !== "first-run" && scenario !== "empty";

function answer<T>(data: T | null): Fetched<T> {
  if (scenario === "fresh") {
    cached = true;
    return { data, fetchedAt: new Date().toISOString(), stale: false, problem: null, error: null };
  }
  const [problem, error] = FAILURE[scenario as keyof typeof FAILURE];
  return cached && data !== null
    ? { data, fetchedAt: savedAt, stale: true, problem, error }
    : { data: null, fetchedAt: null, stale: false, problem, error };
}

export async function call(cmd: string, args: Record<string, unknown>): Promise<unknown> {
  await new Promise((r) => setTimeout(r, 150)); // как настоящий вызов: не мгновенно
  if (scenario === "loading") return new Promise(() => {});
  switch (cmd) {
    case "panel_config":
      return config;
    case "panel_set_url":
      config = { ...config, url: (args.url as string | null) || null };
      break;
    case "panel_set_token":
      config = { ...config, tokenSet: !!args.token };
      break;
    case "panel_tasks":
      return answer(args.mine ? TASKS.filter((t) => t.who === ME) : TASKS);
    case "panel_task":
      return answer(detail(args.key as string));
    case "panel_projects":
      return answer(Object.values(PROJECTS).sort());
    case "panel_epics":
      return answer(EPICS);
    case "panel_overview":
      return answer(OVERVIEW);
    default:
      throw `Command ${cmd} not found`;
  }
  // Задали адрес и токен — панель «ответила».
  if (config.url && config.tokenSet) scenario = "fresh";
  else if (!config.url) scenario = "first-run";
  else scenario = "no-token";
  return config;
}
