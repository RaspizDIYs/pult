//! Рой агентов: кто в сети, кто что держит, кто чего ждёт. Только чтение — ни замков,
//! ни писем, ни отметок присутствия: Пульт смотрит на рой, а не участвует в нём.
//!
//! Два источника, и второй не зависит от первого:
//! - хаб роя (`GET /state` и поток `GET /events`) — общие замки, сессии всех машин, доска,
//!   заявки на файлы, задачи брокера;
//! - файлы локального диспетчера (`~/.claude/orchestrator`) — ёмкость этой машины, идущие
//!   прогоны, локальные замки и очереди, имена сессий. Их показываем и при мёртвом хабе:
//!   именно тогда вопрос «кто что держит» встаёт острее всего.
//!
//! Адрес и токен хаба берём из того же `fleet.json`, что читает CLI роя: второй источник
//! настроек рано или поздно разошёлся бы с первым. Токен не пишется в лог и не уходит
//! интерфейсу — у конфига нет даже `Debug`.

use crate::probes::innermost;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, State};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

pub const EVENT_FLEET: &str = "pult://fleet";

/// Сессия в сети, пока о ней было слышно за этот срок. Тот же порог, что у хаба
/// (`SESSION_TTL_MS`): второго определения «жив» в рою быть не должно.
const ONLINE_MS: i64 = 60 * 60_000;
/// «Давно молчит». Клиент роя отмечается не чаще раза в 10 минут и только при вызове
/// Bash, так что 20 минут — это две пропущенные отметки подряд, а не пауза на раздумье.
const SILENT_MS: i64 = 20 * 60_000;
/// Доска и заявки локального диспетчера живут 90 минут (coord.mjs) — старше не показываем.
const FRESH_MS: i64 = 90 * 60_000;
/// Талон локальной очереди без подтверждения дольше — брошен (queue.mjs).
const TICKET_TTL_MS: i64 = 15 * 60_000;

const STATE_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Хаб шлёт пульс каждые 20 с. Минута тишины — поток мёртв, даже если сокет «открыт»:
/// после смены VPN туннель именно так и висит (fleet/README.md).
const STREAM_IDLE: Duration = Duration::from_secs(60);
/// Снимок перечитываем и без событий: сессии уходят из сети молча, по порогу.
const REFRESH: Duration = Duration::from_secs(30);
/// Хаб не настроен или конфиг битый — перечитываем реже: это меняет человек, а не сеть.
const CONFIG_RETRY: Duration = Duration::from_secs(30);
const LOCAL_POLL: Duration = Duration::from_secs(5);
/// Кадр SSE без конца дольше этого — не поток, а мусор; память не должна расти без предела.
const FRAME_MAX: usize = 16 * 1024 * 1024;

// ───────────── Что получает интерфейс (docs/контракт.md, раздел 5) ─────────────

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FleetView {
    pub taken_at: String,
    pub hub: HubInfo,
    /// Имя этой машины в рою; None — локальные файлы его ни разу не назвали.
    pub machine: Option<String>,
    pub sessions: Vec<SessionView>,
    pub locks: Vec<LockView>,
    /// Общие ресурсы хаба — чтобы показать и свободные, а не только занятые.
    pub fleet_resources: Vec<ResourceView>,
    pub claims: Vec<ClaimView>,
    pub board: Vec<NoteView>,
    pub tasks: Vec<TaskView>,
    pub local: LocalView,
    /// Замки с проблемой плюс файлы, которые правят двое.
    pub problems: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HubState {
    /// На этой машине рой не настроен (нет `fleet.json`).
    Off,
    Connecting,
    Ok,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HubInfo {
    pub state: HubState,
    /// `хост:порт` — без пути и учётных данных.
    pub address: Option<String>,
    pub error: Option<String>,
    pub last_ok_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Presence {
    Online,
    Offline,
    /// Хаб недоступен, а локальной отметки нет — сказать нечего.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    pub id: String,
    pub name: Option<String>,
    pub machine: Option<String>,
    /// Сессия этой машины.
    pub local: bool,
    pub presence: Presence,
    /// В сети, но молчит дольше SILENT_MS.
    pub silent: bool,
    pub last_seen_at: Option<String>,
    pub cwd: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Fleet,
    Local,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Problem {
    /// Держатель не в сети: замок висит на ушедшей сессии.
    Offline,
    /// Держит дольше срока ресурса.
    Overdue,
    /// Держатель в сети, но давно молчит.
    Silent,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LockView {
    pub key: String,
    pub resource: String,
    pub scope: Scope,
    pub repo: Option<String>,
    /// None — замок свободен, но к нему стоит очередь.
    pub session: Option<String>,
    pub since: Option<String>,
    pub ttl_min: Option<i64>,
    /// Сколько единиц ёмкости держит (полный прогон занимает все).
    pub units: u32,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub queue: Vec<TicketView>,
    pub problem: Option<Problem>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TicketView {
    pub session: String,
    pub since: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceView {
    pub name: String,
    pub label: Option<String>,
    pub capacity: u32,
    pub used: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimView {
    pub file: String,
    pub repo: Option<String>,
    pub sessions: Vec<ClaimHolder>,
    /// Файл заявили две и больше сессий, которые не ушли из сети.
    pub overlap: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimHolder {
    pub session: String,
    pub since: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteView {
    pub session: String,
    pub text: String,
    pub task: Option<String>,
    pub cwd: Option<String>,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskView {
    pub id: String,
    pub title: String,
    /// TODO | IN_PROGRESS | DONE | FAILED — как в брокере.
    pub status: String,
    pub priority: String,
    pub executor: Option<String>,
    pub machine: Option<String>,
    pub session: Option<String>,
    pub node: Option<String>,
    pub attempts: u32,
    pub created_at: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalView {
    pub dir: String,
    /// Каталог диспетчера существует: без него «ничего не идёт» значило бы «не знаю».
    pub found: bool,
    pub resources: Vec<ResourceView>,
    pub runs: Vec<RunView>,
    /// Файлы, которые не прочитались. Остальное при этом показано.
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunView {
    pub resource: String,
    pub session: Option<String>,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub started_at: Option<String>,
    pub background: bool,
}

// ───────────── Разобранный источник (хаб или локальные файлы) ─────────────

/// Хаб и локальный диспетчер описывают одно и то же разными файлами; сводим к общему
/// виду, чтобы склейка и правила «держатель не в сети» были одни на оба источника.
#[derive(Debug, Clone, Default)]
struct Part {
    found: bool,
    machine: Option<String>,
    /// Хаб: сессии в сети. Локально: отметки присутствия `fleet-beat-*.json`.
    seen: Vec<Seen>,
    names: HashMap<String, String>,
    locks: Vec<Lock>,
    tickets: Vec<Ticket>,
    notes: Vec<Note>,
    claims: Vec<Claim>,
    tasks: Vec<TaskView>,
    resources: Vec<ResourceView>,
    runs: Vec<RunView>,
    errors: Vec<String>,
}

#[derive(Debug, Clone)]
struct Seen {
    id: String,
    machine: Option<String>,
    name: Option<String>,
    at: i64,
}

#[derive(Debug, Clone)]
struct Lock {
    key: String,
    resource: String,
    repo: Option<String>,
    session: String,
    machine: Option<String>,
    since: i64,
    ttl_min: Option<i64>,
    units: u32,
    command: Option<String>,
    cwd: Option<String>,
}

#[derive(Debug, Clone)]
struct Ticket {
    key: String,
    session: String,
    machine: Option<String>,
    at: i64,
    reason: Option<String>,
    cwd: Option<String>,
}

#[derive(Debug, Clone)]
struct Note {
    session: String,
    machine: Option<String>,
    text: String,
    task: Option<String>,
    cwd: Option<String>,
    at: i64,
}

#[derive(Debug, Clone)]
struct Claim {
    file: String,
    repo: Option<String>,
    session: String,
    machine: Option<String>,
    cwd: Option<String>,
    at: i64,
}

// ───────────── Время и чужой JSON ─────────────

fn now_ms() -> i64 {
    (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

fn iso(ms: i64) -> String {
    OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000)
        .ok()
        .and_then(|t| t.format(&Rfc3339).ok())
        .unwrap_or_default()
}

/// Непустая строка поля. Чужой JSON пишут разные версии клиента: поле бывает и `null`, и `""`.
fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key)?.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// Время в миллисекундах. Хаб пишет числа, локальный диспетчер — ISO-строки; читаем оба.
fn time_of(v: &Value, key: &str) -> Option<i64> {
    match v.get(key)? {
        Value::Number(n) => n.as_f64().map(|f| f as i64),
        Value::String(s) => OffsetDateTime::parse(s, &Rfc3339).ok().map(|t| (t.unix_timestamp_nanos() / 1_000_000) as i64),
        _ => None,
    }
}

fn list<'a>(v: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    v.get(key).and_then(Value::as_array).into_iter().flatten()
}

fn entries<'a>(v: &'a Value, key: &str) -> impl Iterator<Item = (&'a String, &'a Value)> {
    v.get(key).and_then(Value::as_object).into_iter().flatten()
}

/// Домашний каталог — тильдой: путь короче, а имя пользователя не мелькает на снимке экрана.
fn tilde(path: String) -> String {
    let Some(home) = std::env::home_dir().and_then(|h| h.to_str().map(str::to_string)) else { return path };
    match path.strip_prefix(&home) {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => format!("~{rest}"),
        _ => path,
    }
}

fn note_of(x: &Value, at: Option<i64>) -> Option<Note> {
    Some(Note {
        session: text(x, "session")?,
        machine: text(x, "machine"),
        text: text(x, "text").unwrap_or_default(),
        task: text(x, "task"),
        cwd: text(x, "cwd").map(tilde),
        at: at?,
    })
}

fn claim_of(x: &Value, at: Option<i64>) -> Option<Claim> {
    Some(Claim {
        file: text(x, "file")?,
        repo: text(x, "repo"),
        session: text(x, "session")?,
        machine: text(x, "machine"),
        cwd: text(x, "cwd").map(tilde),
        at: at?,
    })
}

// ───────────── Хаб ─────────────

/// Снимок `GET /state` (он же — данные кадра `hello` потока `/events`).
fn parse_hub(v: &Value, now: i64) -> Part {
    // Время хаба переводим в часы этой машины. Возраст считаем по часам хаба: иначе
    // расхождение часов между машинами выдало бы «держит −3 мин» или лишний час молчания.
    let skew = time_of(v, "now").map_or(0, |hub_now| now - hub_now);
    let at = |x: &Value, key: &str| time_of(x, key).map(|t| t + skew);
    let mut p = Part { found: true, ..Part::default() };

    let mut ttl = HashMap::new();
    for (name, r) in entries(v, "resources") {
        if let Some(t) = r.get("ttlMin").and_then(Value::as_i64) {
            ttl.insert(name.clone(), t);
        }
        let capacity = r.get("capacity").and_then(Value::as_u64).unwrap_or(1) as u32;
        p.resources.push(ResourceView { name: name.clone(), label: text(r, "label"), capacity, used: 0 });
    }
    for x in list(v, "sessions") {
        let Some(id) = text(x, "session") else { continue };
        let idle = x.get("idleSec").and_then(Value::as_i64).unwrap_or(0);
        p.seen.push(Seen { id, machine: text(x, "machine"), name: text(x, "name"), at: now - idle * 1000 });
    }
    for x in list(v, "locks") {
        let (Some(session), Some(resource)) = (text(x, "session"), text(x, "resource")) else { continue };
        p.locks.push(Lock {
            key: text(x, "key").unwrap_or_else(|| resource.clone()),
            ttl_min: ttl.get(&resource).copied(),
            // Ключ хаба — `репозиторий::ресурс`, а `-` значит «вне репозитория».
            repo: text(x, "repo").filter(|r| r != "-"),
            machine: text(x, "machine"),
            since: at(x, "since").unwrap_or(now),
            command: text(x, "command").or_else(|| text(x, "reason")),
            cwd: None,
            units: 1,
            session,
            resource,
        });
    }
    for (key, tickets) in entries(v, "queue") {
        for x in tickets.as_array().into_iter().flatten() {
            let Some(session) = text(x, "session") else { continue };
            let ticket = Ticket { key: key.clone(), session, machine: text(x, "machine"), at: at(x, "at").unwrap_or(now), reason: text(x, "reason"), cwd: None };
            p.tickets.push(ticket);
        }
    }
    p.notes = list(v, "board").filter_map(|x| note_of(x, at(x, "at"))).collect();
    p.claims = list(v, "claims").filter_map(|x| claim_of(x, at(x, "at"))).collect();
    for x in list(v, "tasks") {
        let Some(id) = text(x, "task_id") else { continue };
        p.tasks.push(TaskView {
            id,
            title: text(x, "title").unwrap_or_default(),
            status: text(x, "status").unwrap_or_else(|| "TODO".into()),
            priority: text(x, "priority").unwrap_or_else(|| "NORMAL".into()),
            executor: text(x, "executor"),
            machine: text(x, "machine"),
            session: text(x, "session"),
            node: text(x, "node").or_else(|| text(x, "target_node")),
            attempts: x.get("attempts").and_then(Value::as_u64).unwrap_or(0) as u32,
            created_at: at(x, "created_at").map(iso),
            started_at: at(x, "started_at").map(iso),
            finished_at: at(x, "finished_at").map(iso),
            note: text(x, "note"),
        });
    }
    p
}

/// Адрес хаба и токен. Без `Debug` намеренно: токен не должен попасть в лог даже случайно.
struct HubConfig {
    url: String,
    token: String,
}

enum Config {
    Off,
    Broken(String),
    Hub(HubConfig),
}

/// Каталог диспетчера: `AGENT_ORCH_DIR` или `~/.claude/orchestrator`, как в lib/paths.mjs.
fn orchestrator_dir() -> PathBuf {
    match std::env::var_os("AGENT_ORCH_DIR").filter(|d| !d.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => std::env::home_dir().unwrap_or_default().join(".claude").join("orchestrator"),
    }
}

fn read_config(dir: &Path) -> Config {
    let saved = match std::fs::read(dir.join("fleet.json")) {
        Ok(raw) => match serde_json::from_slice::<Value>(&raw) {
            Ok(v) => v,
            Err(e) => return Config::Broken(format!("fleet.json не читается: битый JSON ({e})")),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Value::Null,
        Err(e) => return Config::Broken(format!("fleet.json не читается: {e}")),
    };
    // Переменные окружения главнее файла — как в CLI роя (lib/fleet.mjs).
    let env = |k: &str| std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let Some(url) = env("FLEET_URL").or_else(|| text(&saved, "url")) else { return Config::Off };
    let token = env("FLEET_TOKEN").or_else(|| text(&saved, "token")).unwrap_or_default();
    Config::Hub(HubConfig { url: url.trim_end_matches('/').to_string(), token })
}

fn address_of(url: &str) -> Option<String> {
    let u = reqwest::Url::parse(url).ok()?;
    Some(format!("{}:{}", u.host_str()?, u.port_or_known_default()?))
}

fn net_error(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        return format!("не ответил за {} с", STATE_TIMEOUT.as_secs());
    }
    let why = innermost(e);
    if why.contains("Connection refused") || why.contains("os error 61") || why.contains("os error 111") || why.contains("os error 10061") {
        "соединение отклонено — туннель к хабу не поднят?".into()
    } else {
        why
    }
}

fn hub_client() -> Result<reqwest::Client, String> {
    // reqwest без провайдера rustls не строит клиента даже для http; второй вызов
    // ничего не делает (как в probes).
    let _ = rustls::crypto::ring::default_provider().install_default();
    // Без системного прокси: хаб — за ssh-туннелем на петлевом адресе, а прокси,
    // пускающий только внешние домены, молча отрезал бы рой (fleet/README.md, NO_PROXY).
    reqwest::Client::builder().no_proxy().connect_timeout(CONNECT_TIMEOUT).build().map_err(|e| innermost(&e))
}

fn get(client: &reqwest::Client, cfg: &HubConfig, route: &str) -> reqwest::RequestBuilder {
    let req = client.get(format!("{}{route}", cfg.url));
    if cfg.token.is_empty() {
        req
    } else {
        req.bearer_auth(&cfg.token)
    }
}

fn check_status(resp: &reqwest::Response) -> Result<(), String> {
    match resp.status().as_u16() {
        200..=299 => Ok(()),
        401 | 403 => Err("хаб отказал в доступе: токен в fleet.json не подходит".into()),
        code => Err(format!("хаб ответил {code}")),
    }
}

async fn fetch_state(client: &reqwest::Client, cfg: &HubConfig) -> Result<Value, String> {
    let resp = get(client, cfg, "/state").timeout(STATE_TIMEOUT).send().await.map_err(|e| net_error(&e))?;
    check_status(&resp)?;
    let body = resp.bytes().await.map_err(|e| net_error(&e))?;
    serde_json::from_slice(&body).map_err(|e| format!("хаб прислал не JSON: {e}"))
}

/// Вынимает из буфера законченные кадры SSE: `(событие, данные)`. Пульс (`: ping`) и
/// кадры без данных пропускаются.
fn take_frames(buf: &mut Vec<u8>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    while let Some(end) = buf.windows(2).position(|w| w == b"\n\n") {
        let frame: Vec<u8> = buf.drain(..end + 2).collect();
        let frame = String::from_utf8_lossy(&frame);
        let mut event = "message".to_string();
        let mut data = Vec::new();
        for line in frame.lines() {
            if let Some(v) = line.strip_prefix("event:") {
                event = v.trim().to_string();
            } else if let Some(v) = line.strip_prefix("data:") {
                data.push(v.strip_prefix(' ').unwrap_or(v));
            }
        }
        if !data.is_empty() {
            out.push((event, data.join("\n")));
        }
    }
    out
}

// ───────────── Локальный диспетчер ─────────────

/// Ресурсы диспетчера по умолчанию (lib/paths.mjs, DEFAULT_CONFIG). Он не пишет их
/// в config.json — там только переопределения, — а «занято 1 из ?» ничего не говорит.
const DEFAULT_SPECS: [(&str, u32, i64, bool, &str); 7] = [
    ("frontend-check", 2, 30, false, "фронт: tsc, eslint, vitest, build"),
    ("backend-build", 2, 30, false, "бэк: dotnet, cargo, gradle, mvn, pytest, go"),
    ("e2e", 1, 45, false, "браузерные тесты: Playwright, Cypress"),
    ("heavy-misc", 2, 30, false, "прочее тяжёлое: docker build, make, кодогенерация"),
    ("db-migrate", 1, 20, true, "миграции БД"),
    ("deploy", 1, 40, true, "деплой"),
    ("push", 1, 5, true, "push в общую ветку"),
];

#[derive(Debug, Clone)]
struct Spec {
    name: String,
    capacity: u32,
    ttl_min: i64,
    project: bool,
    label: Option<String>,
}

fn specs(config: Option<&Value>) -> Vec<Spec> {
    let mut out: Vec<Spec> = DEFAULT_SPECS
        .iter()
        .map(|&(name, capacity, ttl_min, project, label)| Spec { name: name.into(), capacity, ttl_min, project, label: Some(label.into()) })
        .collect();
    for (name, o) in config.map(|c| entries(c, "resources")).into_iter().flatten() {
        let i = out.iter().position(|s| &s.name == name).unwrap_or_else(|| {
            out.push(Spec { name: name.clone(), capacity: 2, ttl_min: 30, project: false, label: None });
            out.len() - 1
        });
        let s = &mut out[i];
        s.capacity = o.get("capacity").and_then(Value::as_u64).map_or(s.capacity, |c| c as u32);
        s.ttl_min = o.get("ttlMin").and_then(Value::as_i64).unwrap_or(s.ttl_min);
        s.project = text(o, "scope").map_or(s.project, |sc| sc == "project");
        s.label = text(o, "label").or(s.label.take());
    }
    out
}

/// Чтение каталога диспетчера: каждый файл сам по себе. Отсутствующий файл — норма
/// (диспетчер создаёт их по мере надобности), битый — строка в `errors`, а не пустой экран:
/// недописанный board.json не должен прятать замки.
struct Reader<'a> {
    dir: &'a Path,
    errors: Vec<String>,
}

impl Reader<'_> {
    fn json(&mut self, rel: &str) -> Option<Value> {
        let raw = match std::fs::read(self.dir.join(rel)) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                self.errors.push(format!("{rel}: {e}"));
                return None;
            }
        };
        match serde_json::from_slice(&raw) {
            Ok(v) => Some(v),
            Err(e) => {
                self.errors.push(format!("{rel}: битый JSON ({e})"));
                None
            }
        }
    }

    /// Имена в подкаталоге по алфавиту; нет подкаталога — пусто.
    fn names(&mut self, rel: &str) -> Vec<String> {
        match std::fs::read_dir(self.dir.join(rel)) {
            Ok(rd) => {
                let mut names: Vec<String> = rd.filter_map(|e| e.ok()?.file_name().into_string().ok()).collect();
                names.sort();
                names
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                self.errors.push(format!("{}: {e}", if rel.is_empty() { "." } else { rel }));
                Vec::new()
            }
        }
    }
}

fn read_local(dir: &Path, now: i64) -> Part {
    let mut p = Part { found: dir.is_dir(), ..Part::default() };
    if !p.found {
        return p;
    }
    let mut r = Reader { dir, errors: Vec::new() };
    let specs = specs(r.json("config.json").as_ref());
    let spec_of = |name: &str| specs.iter().find(|s| s.name == name).cloned();

    // Замок — каталог `<ключ>#<единица>` с holder.json. Единицы одного прогона — одна строка.
    for entry in r.names("locks") {
        let Some(h) = r.json(&format!("locks/{entry}/holder.json")) else { continue };
        let (Some(session), Some(resource), Some(since)) = (text(&h, "session"), text(&h, "resource"), time_of(&h, "since")) else { continue };
        let key = entry.rsplit_once('#').map_or(entry.as_str(), |(k, _)| k).to_string();
        if p.machine.is_none() {
            p.machine = text(&h, "machine");
        }
        if let Some(l) = p.locks.iter_mut().find(|l| l.key == key && l.session == session) {
            l.units += 1;
            continue;
        }
        p.locks.push(Lock {
            repo: key.strip_suffix(&format!("__{resource}")).map(str::to_string),
            ttl_min: Some(spec_of(&resource).map_or(30, |s| s.ttl_min)),
            machine: text(&h, "machine"),
            command: text(&h, "command"),
            cwd: text(&h, "cwd").map(tilde),
            units: 1,
            since,
            key,
            session,
            resource,
        });
    }

    for dir_name in r.names("queue") {
        for name in r.names(&format!("queue/{dir_name}")) {
            if !(name.starts_with("t-") && name.ends_with(".json")) {
                continue;
            }
            let Some(t) = r.json(&format!("queue/{dir_name}/{name}")) else { continue };
            // Брошенный талон диспетчер удалит при следующем обращении; показывать его незачем.
            if time_of(&t, "lastSeen").is_none_or(|seen| now - seen >= TICKET_TTL_MS) {
                continue;
            }
            let Some(session) = text(&t, "session") else { continue };
            p.tickets.push(Ticket {
                key: text(&t, "key").unwrap_or_else(|| dir_name.clone()),
                session,
                machine: None,
                at: time_of(&t, "at").unwrap_or(now),
                reason: text(&t, "reason"),
                cwd: text(&t, "cwd").map(tilde),
            });
        }
    }

    for name in r.names("runs") {
        if !name.ends_with(".json") {
            continue;
        }
        let Some(run) = r.json(&format!("runs/{name}")) else { continue };
        if p.machine.is_none() {
            p.machine = text(&run, "machine");
        }
        if text(&run, "status").as_deref() != Some("running") {
            continue;
        }
        p.runs.push(RunView {
            resource: text(&run, "resource").unwrap_or_default(),
            session: text(&run, "session"),
            command: text(&run, "command"),
            cwd: text(&run, "cwd").map(tilde),
            started_at: time_of(&run, "startedAt").map(iso),
            background: run.get("background").and_then(Value::as_bool).unwrap_or(false),
        });
    }

    let fresh = |x: &Value| time_of(x, "at").filter(|t| now - t < FRESH_MS);
    if let Some(v) = r.json("board.json") {
        p.notes = v.as_array().into_iter().flatten().filter_map(|x| note_of(x, fresh(x))).collect();
    }
    if let Some(v) = r.json("claims.json") {
        p.claims = v.as_array().into_iter().flatten().filter_map(|x| claim_of(x, fresh(x))).collect();
    }
    if let Some(v) = r.json("fleet-names.json") {
        for (id, name) in v.as_object().into_iter().flatten() {
            if let Some(name) = name.as_str().map(str::trim).filter(|n| !n.is_empty()) {
                p.names.insert(id.clone(), name.to_string());
            }
        }
    }
    for name in r.names("") {
        let Some(id) = name.strip_prefix("fleet-beat-").and_then(|n| n.strip_suffix(".json")) else { continue };
        if let Some(at) = r.json(&name).as_ref().and_then(|b| time_of(b, "at")) {
            p.seen.push(Seen { id: id.to_string(), machine: None, name: None, at });
        }
    }

    for s in specs.iter().filter(|s| !s.project) {
        let used = p
            .locks
            .iter()
            .filter(|l| l.resource == s.name && now - l.since < s.ttl_min * 60_000)
            .map(|l| l.units)
            .sum();
        p.resources.push(ResourceView { name: s.name.clone(), label: s.label.clone(), capacity: s.capacity, used });
    }
    p.errors = r.errors;
    p
}

// ───────────── Склейка ─────────────

/// Проблема замка. Порядок — от причины к симптому: ушедший держатель объясняет и то,
/// что замок держится дольше срока, поэтому «не в сети» главнее «просрочен».
fn lock_problem(lock: &Lock, holder: Option<&SessionView>, now: i64) -> Option<Problem> {
    if holder.is_some_and(|s| s.presence == Presence::Offline) {
        return Some(Problem::Offline);
    }
    if lock.ttl_min.is_some_and(|ttl| now - lock.since >= ttl * 60_000) {
        return Some(Problem::Overdue);
    }
    holder.is_some_and(|s| s.silent).then_some(Problem::Silent)
}

#[derive(Default)]
struct Who {
    machine: Option<String>,
    local: bool,
    hub_seen: Option<i64>,
    cwd: Option<String>,
    note: Option<String>,
}

fn resource_of_key(key: &str) -> String {
    key.rsplit_once("::").or_else(|| key.rsplit_once("__")).map_or(key, |(_, r)| r).to_string()
}

fn build(hub: Option<&Part>, info: HubInfo, local: &Part, now: i64) -> FleetView {
    let none = Part::default();
    let h = hub.unwrap_or(&none);
    let mut who: BTreeMap<String, Who> = BTreeMap::new();
    for s in &h.seen {
        let w = who.entry(s.id.clone()).or_default();
        w.machine = s.machine.clone();
        w.hub_seen = Some(s.at);
    }

    // Доска: одна заметка на сессию, самая свежая — из любого источника.
    let mut notes: Vec<(&Note, bool)> = h.notes.iter().map(|n| (n, false)).chain(local.notes.iter().map(|n| (n, true))).collect();
    notes.sort_by_key(|(n, _)| -n.at);
    let mut board: Vec<NoteView> = Vec::new();
    let mut touch = |id: &str, machine: &Option<String>, cwd: &Option<String>, is_local: bool| {
        let w = who.entry(id.to_string()).or_default();
        w.machine = w.machine.take().or_else(|| machine.clone());
        w.cwd = w.cwd.take().or_else(|| cwd.clone());
        w.local |= is_local;
    };
    for (n, is_local) in notes {
        if board.iter().any(|b| b.session == n.session) {
            continue;
        }
        touch(&n.session, &n.machine, &n.cwd, is_local);
        board.push(NoteView { session: n.session.clone(), text: n.text.clone(), task: n.task.clone(), cwd: n.cwd.clone(), at: iso(n.at) });
    }
    for (part, is_local) in [(h, false), (local, true)] {
        part.locks.iter().for_each(|l| touch(&l.session, &l.machine, &l.cwd, is_local));
        part.tickets.iter().for_each(|t| touch(&t.session, &t.machine, &t.cwd, is_local));
        part.claims.iter().for_each(|c| touch(&c.session, &c.machine, &c.cwd, is_local));
        for r in part.runs.iter() {
            if let Some(s) = &r.session {
                touch(s, &None, &r.cwd, is_local);
            }
        }
    }
    for t in h.tasks.iter().filter(|t| t.status == "IN_PROGRESS") {
        if let Some(s) = &t.session {
            touch(s, &t.machine, &None, false);
        }
    }
    for b in &board {
        if let Some(w) = who.get_mut(&b.session) {
            w.note = Some(b.text.clone()).filter(|t| !t.is_empty());
        }
    }

    let beats: HashMap<&str, i64> = local.seen.iter().map(|s| (s.id.as_str(), s.at)).collect();
    let hub_names: HashMap<&str, &String> = h.seen.iter().filter_map(|s| Some((s.id.as_str(), s.name.as_ref()?))).collect();
    let mut sessions: Vec<SessionView> = who
        .into_iter()
        .map(|(id, w)| {
            let beat = beats.get(id.as_str()).copied();
            let last = w.hub_seen.or(beat);
            // Хаб на связи — правда за ним: кого нет в его списке, тот не в сети. Без хаба
            // остаются локальные отметки, и то только для сессий этой машины.
            let presence = match (hub.is_some(), w.hub_seen, beat) {
                (true, Some(_), _) => Presence::Online,
                (true, None, _) => Presence::Offline,
                (false, _, Some(t)) if now - t < ONLINE_MS => Presence::Online,
                (false, _, Some(_)) => Presence::Offline,
                (false, _, None) => Presence::Unknown,
            };
            let machine = w.machine.or_else(|| w.local.then(|| local.machine.clone()).flatten());
            SessionView {
                name: hub_names.get(id.as_str()).map(|n| n.to_string()).or_else(|| local.names.get(&id).cloned()),
                local: w.local || (machine.is_some() && machine == local.machine),
                silent: presence == Presence::Online && last.is_some_and(|t| now - t >= SILENT_MS),
                last_seen_at: last.map(iso),
                cwd: w.cwd,
                note: w.note,
                machine,
                presence,
                id,
            }
        })
        .collect();
    let by_id: HashMap<String, SessionView> = sessions.iter().map(|s| (s.id.clone(), s.clone())).collect();

    let mut locks = Vec::new();
    for (part, scope) in [(h, Scope::Fleet), (local, Scope::Local)] {
        let mut queues: BTreeMap<&str, Vec<&Ticket>> = BTreeMap::new();
        for t in &part.tickets {
            queues.entry(t.key.as_str()).or_default().push(t);
        }
        let tickets = |q: Vec<&Ticket>| {
            let mut q: Vec<TicketView> = q.into_iter().map(|t| TicketView { session: t.session.clone(), since: iso(t.at), reason: t.reason.clone() }).collect();
            q.sort_by(|a, b| a.since.cmp(&b.since));
            q
        };
        for l in &part.locks {
            locks.push(LockView {
                key: l.key.clone(),
                resource: l.resource.clone(),
                scope,
                repo: l.repo.clone(),
                session: Some(l.session.clone()),
                since: Some(iso(l.since)),
                ttl_min: l.ttl_min,
                units: l.units,
                command: l.command.clone(),
                cwd: l.cwd.clone(),
                queue: tickets(queues.remove(l.key.as_str()).unwrap_or_default()),
                problem: lock_problem(l, by_id.get(&l.session), now),
            });
        }
        // Очередь при свободном замке — тоже картина: так выглядел талон, который не снимался (KAN-1399).
        for (key, q) in queues {
            let resource = resource_of_key(key);
            let repo = key.strip_suffix(&format!("::{resource}")).or_else(|| key.strip_suffix(&format!("__{resource}"))).map(str::to_string);
            locks.push(LockView {
                key: key.to_string(),
                resource,
                scope,
                repo: repo.filter(|r| r != "-"),
                session: None,
                since: None,
                ttl_min: None,
                units: 0,
                command: None,
                cwd: None,
                queue: tickets(q),
                problem: None,
            });
        }
    }
    locks.sort_by_key(|l| (l.problem.is_none(), l.session.is_none(), l.scope == Scope::Local, l.since.clone()));

    // Заявки: файл в репозитории — одна строка, на ней все, кто его заявил. Пересечение
    // считаем только среди тех, кто не ушёл, как и хаб: предупреждение о призраке приучает
    // пропускать предупреждения не глядя.
    let mut all: Vec<&Claim> = h.claims.iter().chain(&local.claims).collect();
    all.sort_by_key(|c| -c.at);
    let mut claims: Vec<ClaimView> = Vec::new();
    for c in all {
        let same = |f: &&mut ClaimView| f.file.to_lowercase() == c.file.to_lowercase() && f.repo == c.repo;
        let row = match claims.iter_mut().find(same) {
            Some(row) => row,
            None => {
                claims.push(ClaimView { file: c.file.clone(), repo: c.repo.clone(), sessions: Vec::new(), overlap: false });
                claims.last_mut().unwrap()
            }
        };
        if !row.sessions.iter().any(|s| s.session == c.session) {
            row.sessions.push(ClaimHolder { session: c.session.clone(), since: iso(c.at) });
        }
    }
    for row in &mut claims {
        let present = row.sessions.iter().filter(|s| by_id.get(&s.session).is_none_or(|p| p.presence != Presence::Offline)).count();
        row.overlap = present >= 2;
    }
    claims.sort_by_key(|c| !c.overlap);

    let fleet_resources = h
        .resources
        .iter()
        .map(|r| ResourceView { used: locks.iter().filter(|l| l.scope == Scope::Fleet && l.session.is_some() && l.resource == r.name).count() as u32, ..r.clone() })
        .collect();

    let order = |s: &str| ["IN_PROGRESS", "TODO", "FAILED", "DONE"].iter().position(|x| *x == s).unwrap_or(4);
    let priority = |p: &str| ["URGENT", "HIGH", "NORMAL", "LOW"].iter().position(|x| *x == p).unwrap_or(2);
    let mut tasks = h.tasks.clone();
    tasks.sort_by(|a, b| {
        (order(&a.status), priority(&a.priority))
            .cmp(&(order(&b.status), priority(&b.priority)))
            // Закрытые — свежие сверху, остальные — в порядке заведения.
            .then_with(|| if a.status == "DONE" { b.finished_at.cmp(&a.finished_at) } else { a.created_at.cmp(&b.created_at) })
    });

    sessions.sort_by(|a, b| {
        (!a.local, &a.machine, a.presence != Presence::Online)
            .cmp(&(!b.local, &b.machine, b.presence != Presence::Online))
            .then_with(|| b.last_seen_at.cmp(&a.last_seen_at))
    });

    FleetView {
        taken_at: iso(now),
        hub: info,
        machine: local.machine.clone(),
        problems: locks.iter().filter(|l| l.problem.is_some()).count() + claims.iter().filter(|c| c.overlap).count(),
        sessions,
        locks,
        fleet_resources,
        claims,
        board,
        tasks,
        local: LocalView {
            dir: String::new(),
            found: local.found,
            resources: local.resources.clone(),
            runs: local.runs.clone(),
            errors: local.errors.clone(),
        },
    }
}

// ───────────── Жизненный цикл ─────────────

pub struct Fleet {
    app: AppHandle,
    dir: PathBuf,
    inner: Mutex<Inner>,
}

struct Inner {
    hub: HubInfo,
    hub_part: Option<Part>,
    local: Part,
    /// Последнее отправленное интерфейсу — чтобы не слать одно и то же каждые 5 секунд.
    sent: Option<FleetView>,
}

impl Fleet {
    pub fn start(app: AppHandle) -> Arc<Self> {
        let dir = orchestrator_dir();
        let local = read_local(&dir, now_ms());
        let hub = HubInfo { state: HubState::Connecting, address: None, error: None, last_ok_at: None };
        let fleet = Arc::new(Self { app, dir, inner: Mutex::new(Inner { hub, hub_part: None, local, sent: None }) });
        tauri::async_runtime::spawn(fleet.clone().hub_loop());
        tauri::async_runtime::spawn(fleet.clone().local_loop());
        fleet
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn view(&self) -> FleetView {
        let s = self.lock();
        self.view_of(&s)
    }

    fn view_of(&self, s: &Inner) -> FleetView {
        let mut v = build(s.hub_part.as_ref(), s.hub.clone(), &s.local, now_ms());
        v.local.dir = tilde(self.dir.to_string_lossy().into_owned());
        v
    }

    fn publish(&self, change: impl FnOnce(&mut Inner)) {
        let mut s = self.lock();
        change(&mut s);
        let view = self.view_of(&s);
        // Время снимка меняется всегда; рассылаем, только если изменилось что-то ещё.
        if s.sent.as_ref().is_some_and(|prev| *prev == FleetView { taken_at: prev.taken_at.clone(), ..view.clone() }) {
            return;
        }
        s.sent = Some(view.clone());
        drop(s);
        if let Err(e) = self.app.emit(EVENT_FLEET, view) {
            log::warn!("{EVENT_FLEET} не отправлен: {e}");
        }
    }

    async fn local_loop(self: Arc<Self>) {
        loop {
            tokio::time::sleep(LOCAL_POLL).await;
            let local = read_local(&self.dir, now_ms());
            self.publish(|s| s.local = local);
        }
    }

    fn hub_ok(&self, v: &Value, address: &Option<String>) {
        let part = parse_hub(v, now_ms());
        // В лог — только возвращение хаба, а не каждый снимок: по логу видно, когда рой был на связи.
        let summary = format!("сессий в сети {}, общих замков {}", part.seen.len(), part.locks.len());
        let mut came_back = false;
        self.publish(|s| {
            came_back = s.hub.state != HubState::Ok;
            s.hub_part = Some(part);
            s.hub = HubInfo { state: HubState::Ok, address: address.clone(), error: None, last_ok_at: Some(iso(now_ms())) };
        });
        if came_back {
            log::info!("рой: хаб на связи ({}): {summary}", address.as_deref().unwrap_or("?"));
        }
    }

    /// Хаб пропал — его данные убираем, а не показываем застывшими: старый снимок замков
    /// читался бы как текущий. Время последнего ответа остаётся, чтобы было видно, с каких пор.
    fn hub_down(&self, state: HubState, address: Option<String>, error: Option<String>) {
        self.publish(|s| {
            s.hub_part = None;
            s.hub = HubInfo { state, address, error, last_ok_at: s.hub.last_ok_at.take() };
        });
    }

    async fn hub_loop(self: Arc<Self>) {
        let mut failures = 0u32;
        // Хаб, лежащий всю ночь, не должен писать в лог каждые 30 секунд: пишем смену причины.
        let mut logged: Option<String> = None;
        loop {
            let wait = match read_config(&self.dir) {
                Config::Off => {
                    self.hub_down(HubState::Off, None, None);
                    CONFIG_RETRY
                }
                Config::Broken(e) => {
                    if logged.as_ref() != Some(&e) {
                        log::warn!("рой: {e}");
                        logged = Some(e.clone());
                    }
                    self.hub_down(HubState::Unavailable, None, Some(e));
                    CONFIG_RETRY
                }
                Config::Hub(cfg) => {
                    let address = address_of(&cfg.url);
                    let (connected, err) = self.follow(&cfg, &address).await;
                    if connected {
                        failures = 0;
                        logged = None;
                    }
                    if logged.as_ref() != Some(&err) {
                        log::info!("рой: хаб недоступен: {err}");
                        logged = Some(err.clone());
                    }
                    self.hub_down(HubState::Unavailable, address, Some(err));
                    // Как у CLI роя: 2, 4, 8, 16, 30 секунд.
                    let wait = Duration::from_secs((2u64 << failures.min(4)).min(30));
                    failures += 1;
                    wait
                }
            };
            tokio::time::sleep(wait).await;
        }
    }

    /// Снимок и затем поток событий, пока он жив. Возвращает, удалось ли подключиться,
    /// и почему всё закончилось.
    async fn follow(&self, cfg: &HubConfig, address: &Option<String>) -> (bool, String) {
        let client = match hub_client() {
            Ok(c) => c,
            Err(e) => return (false, e),
        };
        // Сначала обычный снимок: так быстрее видна понятная ошибка (401, нет туннеля).
        match fetch_state(&client, cfg).await {
            Ok(v) => self.hub_ok(&v, address),
            Err(e) => return (false, e),
        }
        let send = get(&client, cfg, "/events").send();
        let mut resp = match tokio::time::timeout(STATE_TIMEOUT, send).await {
            Err(_) => return (true, format!("поток событий не открылся за {} с", STATE_TIMEOUT.as_secs())),
            Ok(Err(e)) => return (true, net_error(&e)),
            Ok(Ok(r)) => r,
        };
        if let Err(e) = check_status(&resp) {
            return (true, e);
        }

        let mut buf = Vec::new();
        let mut heard = Instant::now();
        let mut fetched = Instant::now();
        let mut dirty = false;
        loop {
            match tokio::time::timeout(Duration::from_secs(2), resp.chunk()).await {
                Ok(Ok(Some(bytes))) => {
                    heard = Instant::now();
                    buf.extend_from_slice(&bytes);
                    if buf.len() > FRAME_MAX {
                        return (true, "хаб прислал слишком длинный кадр".into());
                    }
                    for (event, data) in take_frames(&mut buf) {
                        match event.as_str() {
                            // Первый кадр — полный снимок, отдельный запрос не нужен.
                            "hello" => match serde_json::from_str::<Value>(&data) {
                                Ok(v) => {
                                    self.hub_ok(&v, address);
                                    fetched = Instant::now();
                                }
                                Err(_) => dirty = true,
                            },
                            // Письма снимка не меняют, а их текст не наше дело — даже в памяти.
                            "mail" => {}
                            _ => dirty = true,
                        }
                    }
                }
                Ok(Ok(None)) => return (true, "хаб закрыл поток событий".into()),
                Ok(Err(e)) => return (true, net_error(&e)),
                Err(_) if heard.elapsed() > STREAM_IDLE => {
                    return (true, format!("хаб молчит дольше {} с — туннель повис?", STREAM_IDLE.as_secs()));
                }
                Err(_) => {}
            }
            // События — только повод перечитать снимок: собирать состояние хаба из событий
            // значило бы повторить его логику и разойтись с ней на первой же правке хаба.
            if (dirty && fetched.elapsed() >= Duration::from_secs(1)) || fetched.elapsed() >= REFRESH {
                match fetch_state(&client, cfg).await {
                    Ok(v) => self.hub_ok(&v, address),
                    Err(e) => return (true, e),
                }
                dirty = false;
                fetched = Instant::now();
            }
        }
    }
}

#[tauri::command]
pub fn get_fleet(fleet: State<'_, Arc<Fleet>>) -> FleetView {
    let view = fleet.view();
    // Зовётся раз на открытие окна — строка в логе не шумит, а отвечает, дошло ли окно до роя.
    log::info!("рой: get_fleet — хаб {:?}, сессий {}, проблем {}", view.hub.state, view.sessions.len(), view.problems);
    view
}

#[cfg(test)]
mod tests {
    use super::*;

    /// «Сейчас» для всех тестов: 2026-01-15 12:00:00 UTC.
    const NOW: i64 = 1_768_478_400_000;
    const MIN: i64 = 60_000;

    fn hub_info() -> HubInfo {
        HubInfo { state: HubState::Ok, address: None, error: None, last_ok_at: None }
    }

    fn fixture() -> Value {
        serde_json::from_str(include_str!("fixtures/hub-state.json")).unwrap()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pult-fleet-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn lock_of<'a>(v: &'a FleetView, resource: &str) -> &'a LockView {
        v.locks.iter().find(|l| l.resource == resource && l.session.is_some()).unwrap_or_else(|| panic!("нет замка {resource}"))
    }

    fn session<'a>(v: &'a FleetView, id: &str) -> &'a SessionView {
        v.sessions.iter().find(|s| s.id == id).unwrap_or_else(|| panic!("нет сессии {id}"))
    }

    /// Часы хаба в фикстуре на 5 минут впереди: все времена должны сдвинуться к нашим.
    #[test]
    fn hub_snapshot_parses_fixture() {
        let hub = parse_hub(&fixture(), NOW);
        let v = build(Some(&hub), hub_info(), &Part::default(), NOW);

        let online: Vec<&str> = v.sessions.iter().filter(|s| s.presence == Presence::Online).map(|s| s.id.as_str()).collect();
        assert_eq!(online.len(), 4, "{online:?}");
        let machines: std::collections::HashSet<_> = v.sessions.iter().filter(|s| s.presence == Presence::Online).filter_map(|s| s.machine.as_deref()).collect();
        assert_eq!(machines.len(), 3);

        let named = session(&v, "aaaa1111-0000-4000-8000-000000000001");
        assert_eq!(named.name.as_deref(), Some("карта"));
        assert_eq!(named.cwd.as_deref(), Some("/work/pult"), "папка — из заметки на доске");
        assert_eq!(named.note.as_deref(), Some("правлю экран роя"));
        // idleSec = 40 → последний признак жизни 40 секунд назад по нашим часам.
        assert_eq!(named.last_seen_at.as_deref(), Some(iso(NOW - 40_000).as_str()));

        // Держатель deploy в списке сессий хаба отсутствует — значит, не в сети.
        let deploy = lock_of(&v, "deploy");
        assert_eq!(deploy.problem, Some(Problem::Offline));
        assert_eq!(deploy.repo.as_deref(), Some("shop-1a2b3c4d"));
        assert_eq!(deploy.since.as_deref(), Some(iso(NOW - 180 * MIN).as_str()), "сдвиг часов хаба учтён");
        assert_eq!(deploy.queue.len(), 1);
        assert_eq!(deploy.queue[0].session, "aaaa1111-0000-4000-8000-000000000001");
        assert_eq!(session(&v, "dddd4444-0000-4000-8000-000000000004").presence, Presence::Offline);

        // push держит сессия в сети, но молчащая полчаса.
        assert_eq!(lock_of(&v, "push").problem, Some(Problem::Silent));
        assert_eq!(lock_of(&v, "db-migrate").problem, None);
        assert_eq!(v.fleet_resources.iter().find(|r| r.name == "test-stand").map(|r| r.used), Some(0));
        assert_eq!(v.fleet_resources.iter().find(|r| r.name == "deploy").and_then(|r| r.label.as_deref()), Some("деплой общего окружения"));

        // src/App.tsx заявили двое живых — пересечение; docs/контракт.md — живой и ушедший, нет.
        let app = v.claims.iter().find(|c| c.file == "src/App.tsx").unwrap();
        assert!(app.overlap);
        assert_eq!(app.sessions.len(), 2);
        assert!(!v.claims.iter().find(|c| c.file == "docs/контракт.md").unwrap().overlap);

        let count = |st: &str| v.tasks.iter().filter(|t| t.status == st).count();
        assert_eq!((count("TODO"), count("IN_PROGRESS"), count("DONE"), count("FAILED")), (2, 1, 2, 1));
        assert_eq!(v.tasks[0].status, "IN_PROGRESS", "в работе — первыми");
        assert_eq!(v.tasks[1].priority, "HIGH", "среди ждущих срочное вперёд");

        assert_eq!(v.board.len(), 2);
        assert_eq!(v.problems, 3, "deploy без хозяина, молчащий push, пересечение по файлу");
    }

    /// Хаб недоступен: живость сессий этой машины — по локальным отметкам присутствия.
    #[test]
    fn lock_holder_presence_without_hub() {
        let dir = temp_dir("presence");
        let holder = |session: &str, resource: &str, since_min: i64| {
            format!(r#"{{"session":"{session}","resource":"{resource}","machine":"мак-а","cwd":"/work/x","since":"{}"}}"#, iso(NOW - since_min * MIN))
        };
        // Ушла три часа назад, замок на деплой висит с тех пор.
        write(&dir, "locks/shop-1a2b__deploy#1/holder.json", &holder("gone", "deploy", 180));
        write(&dir, "fleet-beat-gone.json", &format!(r#"{{"at":{}}}"#, NOW - 170 * MIN));
        // Жива, отмечалась 3 минуты назад, полный прогон на две единицы.
        write(&dir, "locks/frontend-check#1/holder.json", &holder("alive", "frontend-check", 4));
        write(&dir, "locks/frontend-check#2/holder.json", &holder("alive", "frontend-check", 4));
        write(&dir, "fleet-beat-alive.json", &format!(r#"{{"at":{}}}"#, NOW - 3 * MIN));
        // Отметки нет вовсе, но замок старше срока ресурса.
        write(&dir, "locks/heavy-misc#1/holder.json", &holder("mute", "heavy-misc", 50));
        // В сети, но молчит 25 минут.
        write(&dir, "locks/e2e#1/holder.json", &holder("quiet", "e2e", 10));
        write(&dir, "fleet-beat-quiet.json", &format!(r#"{{"at":{}}}"#, NOW - 25 * MIN));

        let local = read_local(&dir, NOW);
        let v = build(None, hub_info(), &local, NOW);
        assert_eq!(lock_of(&v, "deploy").problem, Some(Problem::Offline));
        assert_eq!(lock_of(&v, "deploy").repo.as_deref(), Some("shop-1a2b"));
        assert_eq!(lock_of(&v, "frontend-check").problem, None);
        assert_eq!(lock_of(&v, "frontend-check").units, 2);
        assert_eq!(lock_of(&v, "heavy-misc").problem, Some(Problem::Overdue));
        assert_eq!(session(&v, "mute").presence, Presence::Unknown);
        assert_eq!(lock_of(&v, "e2e").problem, Some(Problem::Silent));
        assert_eq!(v.problems, 3);
        assert_eq!(v.machine.as_deref(), Some("мак-а"));
        assert!(session(&v, "alive").local);
        let fc = v.local.resources.iter().find(|r| r.name == "frontend-check").unwrap();
        assert_eq!((fc.used, fc.capacity), (2, 2));

        // Тот же замок при живом хабе: сессии нет в его списке — не в сети, какой бы свежей
        // ни была локальная отметка.
        let hub = parse_hub(&serde_json::json!({ "now": NOW, "sessions": [] }), NOW);
        let v = build(Some(&hub), hub_info(), &local, NOW);
        assert_eq!(lock_of(&v, "frontend-check").problem, Some(Problem::Offline));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_files_missing_and_broken() {
        // Каталога нет вовсе: пусто и без ошибок, но видно, что каталог не найден.
        let missing = read_local(&std::env::temp_dir().join("pult-fleet-no-such-dir"), NOW);
        assert!(!missing.found && missing.errors.is_empty() && missing.locks.is_empty());

        // Каталог есть, файлов нет: ёмкость — по умолчанию диспетчера, ошибок нет.
        let dir = temp_dir("broken");
        let empty = read_local(&dir, NOW);
        assert!(empty.found && empty.errors.is_empty());
        assert_eq!(empty.resources.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["frontend-check", "backend-build", "e2e", "heavy-misc"]);

        let at = iso(NOW - 5 * MIN);
        write(&dir, "config.json", r#"{"resources":{"frontend-check":{"capacity":1}}}"#);
        write(&dir, "board.json", "[{\"session\":\"s1\",\"text\":\"недопис");
        write(&dir, "claims.json", &format!(r#"[{{"file":"src/a.ts","session":"s1","cwd":"/w","at":"{at}"}},{{"file":"src/old.ts","session":"s1","at":"{}"}}]"#, iso(NOW - 200 * MIN)));
        write(&dir, "fleet-names.json", r#"{"s1":"карта"}"#);
        write(&dir, "locks/backend-build#1/holder.json", &format!(r#"{{"session":"s1","resource":"backend-build","since":"{at}"}}"#));
        write(&dir, "locks/e2e#1/holder.json", "не json");
        write(&dir, "runs/a__backend-build.json", &format!(r#"{{"resource":"backend-build","session":"s1","status":"running","command":"cargo test","startedAt":"{at}"}}"#));
        write(&dir, "runs/b__e2e.json", "{");
        write(&dir, "runs/c__e2e.json", r#"{"resource":"e2e","status":"passed"}"#);
        write(&dir, "runs/a__backend-build.log", "не json, но и не .json");
        write(&dir, "queue/backend-build/t-s2.json", &format!(r#"{{"key":"backend-build","session":"s2","at":{},"lastSeen":{}}}"#, NOW - MIN, NOW - MIN));
        write(&dir, "queue/backend-build/t-old.json", &format!(r#"{{"key":"backend-build","session":"old","at":{},"lastSeen":{}}}"#, NOW - 60 * MIN, NOW - 30 * MIN));
        write(&dir, "fleet-beat-s1.json", "]]");

        let p = read_local(&dir, NOW);
        let mut errors = p.errors.clone();
        errors.sort();
        assert_eq!(errors.len(), 4, "{errors:?}");
        for (e, file) in errors.iter().zip(["board.json", "fleet-beat-s1.json", "locks/e2e#1/holder.json", "runs/b__e2e.json"]) {
            assert!(e.starts_with(file) && e.contains("битый JSON"), "{e}");
        }
        // Всё остальное прочитано.
        assert_eq!(p.claims.len(), 1, "заявка старше 90 минут отброшена");
        assert_eq!(p.names.get("s1").map(String::as_str), Some("карта"));
        assert_eq!(p.locks.len(), 1);
        assert_eq!(p.runs.len(), 1);
        assert_eq!(p.runs[0].command.as_deref(), Some("cargo test"));
        assert_eq!(p.tickets.iter().map(|t| t.session.as_str()).collect::<Vec<_>>(), ["s2"], "брошенный талон не показан");
        assert_eq!(p.resources.iter().find(|r| r.name == "frontend-check").map(|r| r.capacity), Some(1));
        assert!(p.notes.is_empty());

        let v = build(None, hub_info(), &p, NOW);
        assert_eq!(session(&v, "s1").name.as_deref(), Some("карта"));
        assert_eq!(lock_of(&v, "backend-build").queue.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sse_frames() {
        let mut buf = b": ping\n\nevent: hello\ndata: {\"a\":1}\n\nevent: lease.acquired\nid: 7\ndata: {}\n\nevent: board\ndata: {\"x".to_vec();
        let frames = take_frames(&mut buf);
        assert_eq!(frames, [("hello".to_string(), "{\"a\":1}".to_string()), ("lease.acquired".to_string(), "{}".to_string())]);
        assert_eq!(buf, b"event: board\ndata: {\"x", "недописанный кадр ждёт продолжения");
    }

    #[test]
    fn view_json_field_names() {
        let hub = parse_hub(&fixture(), NOW);
        let json = serde_json::to_value(build(Some(&hub), hub_info(), &Part::default(), NOW)).unwrap();
        for key in ["takenAt", "hub", "machine", "sessions", "locks", "fleetResources", "claims", "board", "tasks", "local", "problems"] {
            assert!(json.get(key).is_some(), "{key}");
        }
        assert_eq!(json["hub"]["state"], "ok");
        assert!(json["sessions"][0].get("lastSeenAt").is_some());
        assert_eq!(json["locks"][0]["problem"], "offline");
        assert!(json["locks"][0].get("ttlMin").is_some());
    }

    /// Живой хаб этой машины, только чтение. Запуск: `cargo test live_hub -- --ignored --nocapture`.
    /// Печатает одни числа: имена, пути и токен в вывод не попадают.
    #[test]
    #[ignore]
    fn live_hub() {
        let Config::Hub(cfg) = read_config(&orchestrator_dir()) else { panic!("рой на этой машине не настроен") };
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let client = hub_client().unwrap();
        let state = rt.block_on(fetch_state(&client, &cfg)).expect("хаб не ответил");
        let now = now_ms();
        let v = build(Some(&parse_hub(&state, now)), hub_info(), &read_local(&orchestrator_dir(), now), now);
        let online: Vec<_> = v.sessions.iter().filter(|s| s.presence == Presence::Online).collect();
        let machines: std::collections::HashSet<_> = online.iter().filter_map(|s| s.machine.as_ref()).collect();
        let held = |scope| v.locks.iter().filter(|l| l.scope == scope && l.session.is_some()).count();
        println!("сессий в сети {} (с именем {}), машин {}", online.len(), online.iter().filter(|s| s.name.is_some()).count(), machines.len());
        println!("сессий всего в картине {}, не в сети {}", v.sessions.len(), v.sessions.iter().filter(|s| s.presence == Presence::Offline).count());
        println!("замков хаба {}, локальных {}, проблемных {}", held(Scope::Fleet), held(Scope::Local), v.locks.iter().filter(|l| l.problem.is_some()).count());
        println!("заявок на файлы {} (пересечений {}), заметок {}", v.claims.len(), v.claims.iter().filter(|c| c.overlap).count(), v.board.len());
        let count = |st: &str| v.tasks.iter().filter(|t| t.status == st).count();
        println!("задач TODO {} IN_PROGRESS {} DONE {} FAILED {}", count("TODO"), count("IN_PROGRESS"), count("DONE"), count("FAILED"));
        println!("локально: прогонов идёт {}, ошибок чтения {}; проблем всего {}", v.local.runs.len(), v.local.errors.len(), v.problems);
        let raw = |k: &str| state.get(k).and_then(Value::as_array).map_or(0, Vec::len);
        println!("сырой снимок: sessions {} locks {} claims {} board {} tasks {}", raw("sessions"), raw("locks"), raw("claims"), raw("board"), raw("tasks"));
    }
}
