//! Рой агентов: кто в сети, кто что держит, кто чего ждёт. Пульт смотрит на рой, а не
//! участвует в нём: ни своих замков, ни писем, ни отметок присутствия. Единственное действие —
//! снять замок, который уже считается проблемой, и только по кнопке с подтверждением.
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
use serde::{Deserialize, Serialize};
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

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
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
    /// Почему локальный замок отсюда не снять (нет `node` или диспетчера); None — можно.
    pub release_error: Option<String>,
}

/// Итог снятия замка — по перечитанному состоянию, а не по ответу «ок».
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseResult {
    pub released: bool,
    pub message: String,
    /// Вывод диспетчера (локальный замок): какие процессы погашены, какие нет.
    pub output: Option<String>,
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

async fn json_of(req: reqwest::RequestBuilder) -> Result<Value, String> {
    let resp = req.timeout(STATE_TIMEOUT).send().await.map_err(|e| net_error(&e))?;
    check_status(&resp)?;
    let body = resp.bytes().await.map_err(|e| net_error(&e))?;
    serde_json::from_slice(&body).map_err(|e| format!("хаб прислал не JSON: {e}"))
}

async fn fetch_state(client: &reqwest::Client, cfg: &HubConfig) -> Result<Value, String> {
    json_of(get(client, cfg, "/state")).await
}

/// Единственная запись в хаб — снятие замка. Тот же адрес и токен, что у чтения.
async fn post(client: &reqwest::Client, cfg: &HubConfig, route: &str, body: &Value) -> Result<Value, String> {
    let req = client.post(format!("{}{route}", cfg.url)).header(reqwest::header::CONTENT_TYPE, "application/json").body(body.to_string());
    json_of(if cfg.token.is_empty() { req } else { req.bearer_auth(&cfg.token) }).await
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
            release_error: None,
        },
    }
}

// ───────────── Названия чатов ─────────────

/// Данные приложения Claude Desktop: там лежат записи о чатах с их названиями.
fn desktop_chats_dir() -> Option<PathBuf> {
    let base = if cfg!(windows) { PathBuf::from(std::env::var_os("APPDATA")?) } else { std::env::home_dir()?.join("Library").join("Application Support") };
    Some(base.join("Claude").join("claude-code-sessions"))
}

fn paths_in(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir).into_iter().flatten().filter_map(|e| Some(e.ok()?.path())).collect()
}

/// Названия чатов Claude на этой машине: идентификатор сессии → название, каким чат виден
/// в списке приложения. «Чат 4b9c» никто не помнит, а название человек дал (или увидел) сам.
/// Источника два, оба — чужие файлы, поэтому всё нечитаемое молча пропускаем:
/// - Claude Desktop: `claude-code-sessions/<аккаунт>/<организация>/local_*.json`, поля
///   `cliSessionId` и `title`. Запись остаётся и после закрытия чата — а имя нужнее всего
///   именно ушедшему держателю замка;
/// - сам Claude Code: `~/.claude/sessions/<pid>.json`, поля `sessionId` и `name`. Только
///   запущенные сессии, зато и те, что открыты в терминале. Они свежее — кладём поверх.
// ponytail: читаем все записи чатов раз в минуту; счёт пойдёт на тысячи — кэш по времени изменения.
fn chat_titles(desktop: Option<&Path>, live: &Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut take = |file: &Path, id: &str, title: &str| {
        let Some(v) = std::fs::read(file).ok().and_then(|raw| serde_json::from_slice::<Value>(&raw).ok()) else { return };
        if let (Some(id), Some(title)) = (text(&v, id), text(&v, title)) {
            out.insert(id, title);
        }
    };
    for file in desktop.into_iter().flat_map(paths_in).flat_map(|account| paths_in(&account)).flat_map(|org| paths_in(&org)) {
        take(&file, "cliSessionId", "title");
    }
    for file in paths_in(live) {
        take(&file, "sessionId", "name");
    }
    out
}

fn titles() -> HashMap<String, String> {
    chat_titles(desktop_chats_dir().as_deref(), &std::env::home_dir().unwrap_or_default().join(".claude").join("sessions"))
}

/// Имя из роя главнее: им сессию зовут агенты, на него приходят письма. Название чата —
/// только когда в рою она не представилась.
fn fill_names(view: &mut FleetView, titles: &HashMap<String, String>) {
    for s in view.sessions.iter_mut().filter(|s| s.name.is_none()) {
        s.name = titles.get(&s.id).cloned();
    }
}

// ───────────── Действие: снять замок ─────────────

/// `mine` опрашивает процессы машины; на винде это секунды, а не миллисекунды.
const DISPATCHER_TIMEOUT: Duration = Duration::from_secs(30);

/// Приложение, открытое из Finder, получает PATH без Homebrew — два привычных места смотрим сами.
// ponytail: nvm, fnm и volta не ищем — кнопка честно скажет «не найден node»; понадобится — путь в настройки.
fn find_node() -> Option<PathBuf> {
    crate::system::find("node").or_else(|| ["/opt/homebrew/bin/node", "/usr/local/bin/node"].into_iter().map(PathBuf::from).find(|p| p.is_file()))
}

/// Установленный диспетчер этой машины: чем запускать и что. Ошибка — готовое объяснение для кнопки.
fn dispatcher(dir: &Path, node: Option<PathBuf>) -> Result<(PathBuf, PathBuf), String> {
    let script = dir.join("orch.mjs");
    if !script.is_file() {
        return Err(format!("диспетчер не установлен: нет {}", tilde(script.to_string_lossy().into_owned())));
    }
    Ok((node.ok_or("не найден node — запустить диспетчер нечем")?, script))
}

/// `node orch.mjs mine --session <id> [--kill]` — команда диспетчера про всё, что держит
/// сессия: без `--kill` только перечисляет замки, талоны и процессы, с ним — гасит процессы
/// и отпускает замки. Каталоги замков сами не трогаем: их формат и порядок уборки (процессы,
/// талоны, доска, хаб) знает диспетчер, и вторая реализация разошлась бы с первой.
async fn run_mine(dir: &Path, node: Option<PathBuf>, session: &str, kill: bool) -> Result<String, String> {
    let (node, script) = dispatcher(dir, node)?;
    // Идентификатор уходит аргументом; начинайся он с дефиса, диспетчер принял бы его за флаг.
    if session.is_empty() || session.starts_with('-') {
        return Err("у держателя странный идентификатор — диспетчеру его не передать".into());
    }
    let mut cmd = crate::system::command(&node);
    // Каталог состояния называем явно: диспетчер должен работать с теми же файлами, что читаем мы.
    cmd.arg(&script).args(["mine", "--session", session]).env("AGENT_ORCH_DIR", dir);
    if kill {
        cmd.arg("--kill");
    }
    let mut cmd = tokio::process::Command::from(cmd);
    cmd.kill_on_drop(true);
    let out = tokio::time::timeout(DISPATCHER_TIMEOUT, cmd.output())
        .await
        .map_err(|_| format!("диспетчер не ответил за {} с", DISPATCHER_TIMEOUT.as_secs()))?
        .map_err(|e| format!("диспетчер не запустился: {e}"))?;
    let said = |raw: &[u8]| String::from_utf8_lossy(raw).trim_end().to_string();
    if out.status.success() {
        Ok(said(&out.stdout))
    } else {
        Err(format!("диспетчер завершился с ошибкой: {}", [said(&out.stderr), said(&out.stdout)].join("\n").trim()))
    }
}

/// Замок, который можно снимать: он на месте, держит его та же сессия, и он всё ещё проблема.
/// Сверяем по свежей картине прямо перед действием: пока человек читал диалог, замок мог
/// перейти к живой сессии, и принудительный сброс выбил бы уже её.
fn releasable<'a>(view: &'a FleetView, scope: Scope, key: &str, session: &str) -> Result<&'a LockView, String> {
    let lock = view
        .locks
        .iter()
        .find(|l| l.scope == scope && l.key == key && l.session.as_deref() == Some(session))
        .ok_or("этого замка уже нет: держатель отпустил его сам или его сняла уборка")?;
    if lock.problem.is_none() {
        return Err("замок больше не проблема: держатель на связи и укладывается в срок — снимать не стал".into());
    }
    Ok(lock)
}

/// Тело `POST /lease/release`, как его читает хаб (fleet/server.mjs, `release`): ключ замка он
/// собирает сам из `repo` и `resource`, поэтому `repo` берём из ключа, а не из подписи.
/// `force` — потому что снимаем не от имени держателя; в журнале хаба это останется
/// «аварийным сбросом», а не «штатно».
fn release_body(lock: &LockView) -> Value {
    let repo = lock.key.strip_suffix(&format!("::{}", lock.resource)).unwrap_or("-");
    serde_json::json!({ "resource": lock.resource, "repo": repo, "session": lock.session, "force": true })
}

/// Ответ хаба: `{ok:true, freed}` или `{ok:false, error}`. `freed:false` — замка уже не было,
/// и это не ошибка: итог всё равно решает перечитанное состояние.
fn release_reply(v: &Value) -> Result<(), String> {
    match v.get("ok").and_then(Value::as_bool) {
        Some(true) => Ok(()),
        _ => Err(format!("хаб отказал: {}", text(v, "error").unwrap_or_else(|| "без объяснения".into()))),
    }
}

/// Свежая картина для проверки до и после действия: хаб (если настроен и отвечает) и файлы машины.
async fn picture(dir: &Path) -> (Result<Part, String>, Part) {
    let hub = match read_config(dir) {
        Config::Hub(cfg) => match hub_client() {
            Ok(client) => fetch_state(&client, &cfg).await.map(|v| parse_hub(&v, now_ms())),
            Err(e) => Err(e),
        },
        Config::Broken(e) => Err(e),
        Config::Off => Err("рой на этой машине не настроен".into()),
    };
    (hub, read_local(dir, now_ms()))
}

/// Снять замок и сказать, что вышло на самом деле. Возвращает и перечитанное состояние —
/// чтобы экран обновился сразу, а не через цикл опроса.
async fn release(dir: &Path, node: Option<PathBuf>, scope: Scope, key: &str, session: &str) -> (ReleaseResult, Option<Part>, Part) {
    let info = || HubInfo { state: HubState::Ok, address: None, error: None, last_ok_at: None };
    let fail = |message: String| ReleaseResult { released: false, message, output: None };

    let (hub, local) = picture(dir).await;
    if let (Scope::Fleet, Err(e)) = (scope, &hub) {
        return (fail(format!("хаб недоступен: {e}")), None, local);
    }
    let before = build(hub.as_ref().ok(), info(), &local, now_ms());
    let lock = match releasable(&before, scope, key, session) {
        Ok(lock) => lock,
        Err(e) => return (fail(e), hub.ok(), local),
    };

    let acted: Result<Option<String>, String> = match scope {
        Scope::Fleet => match (read_config(dir), hub_client()) {
            (Config::Hub(cfg), Ok(client)) => post(&client, &cfg, "/lease/release", &release_body(lock)).await.and_then(|v| release_reply(&v)).map(|_| None),
            (_, Err(e)) => Err(e),
            _ => Err("fleet.json изменился, пока шло действие".into()),
        },
        Scope::Local => run_mine(dir, node, session, true).await.map(Some),
    };

    // «Снят» — только если замка нет в перечитанном состоянии: «ок» в ответе бывает и там,
    // где ничего не произошло.
    let (hub, local) = picture(dir).await;
    if let (Scope::Fleet, Err(e)) = (scope, &hub) {
        return (fail(format!("хаб перестал отвечать ({e}) — снят ли замок, неизвестно")), None, local);
    }
    let after = build(hub.as_ref().ok(), info(), &local, now_ms());
    let still = after.locks.iter().any(|l| l.scope == scope && l.key == key && l.session.as_deref() == Some(session));
    let output = acted.as_ref().ok().cloned().flatten();
    let message = match (&acted, still) {
        (Ok(_), false) => "замок снят".to_string(),
        (Err(e), false) => format!("замок снят, хотя действие закончилось ошибкой: {e}"),
        (Ok(_), true) if scope == Scope::Fleet => "хаб ответил «ок», но замок на месте".to_string(),
        (Ok(_), true) => "диспетчер отработал, но замок на месте".to_string(),
        (Err(e), true) => e.clone(),
    };
    (ReleaseResult { released: !still, message, output }, hub.ok(), local)
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
    titles: HashMap<String, String>,
    /// Последнее отправленное интерфейсу — чтобы не слать одно и то же каждые 5 секунд.
    sent: Option<FleetView>,
}

impl Fleet {
    pub fn start(app: AppHandle) -> Arc<Self> {
        let dir = orchestrator_dir();
        let local = read_local(&dir, now_ms());
        let hub = HubInfo { state: HubState::Connecting, address: None, error: None, last_ok_at: None };
        let fleet = Arc::new(Self { app, dir, inner: Mutex::new(Inner { hub, hub_part: None, local, titles: titles(), sent: None }) });
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
        v.local.release_error = dispatcher(&self.dir, find_node()).err();
        fill_names(&mut v, &s.titles);
        v
    }

    /// Сухой прогон диспетчера для диалога: что он знает о сессии-держателе и что погасит.
    async fn release_preview(&self, key: &str, session: &str) -> Result<String, String> {
        releasable(&self.view(), Scope::Local, key, session)?;
        run_mine(&self.dir, find_node(), session, false).await
    }

    async fn release_lock(&self, scope: Scope, key: &str, session: &str) -> ReleaseResult {
        let (result, hub, local) = release(&self.dir, find_node(), scope, key, session).await;
        self.publish(|s| {
            s.local = local;
            // Хаб в беде — его снимок не подменяем: плашка «недоступен» с живыми замками под ней врала бы.
            if let (Some(part), HubState::Ok) = (hub, s.hub.state) {
                s.hub_part = Some(part);
            }
        });
        result
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
        for tick in 1u32.. {
            tokio::time::sleep(LOCAL_POLL).await;
            let local = read_local(&self.dir, now_ms());
            // Названия меняются редко, а записей о чатах сотни — их перечитываем раз в минуту.
            let titles = (tick % 12 == 0).then(titles);
            self.publish(|s| {
                s.local = local;
                if let Some(titles) = titles {
                    s.titles = titles;
                }
            });
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

/// Что диспетчер знает о держателе локального замка — показать до подтверждения.
#[tauri::command]
pub async fn fleet_release_preview(fleet: State<'_, Arc<Fleet>>, key: String, session: String) -> Result<String, String> {
    fleet.release_preview(&key, &session).await
}

/// Снять проблемный замок. Ошибкой не отвечает: отказ — тоже итог, и его показывают тем же текстом.
#[tauri::command]
pub async fn fleet_release_lock(fleet: State<'_, Arc<Fleet>>, scope: Scope, key: String, session: String) -> Result<ReleaseResult, ()> {
    let before = fleet.view();
    let holder = before.sessions.iter().find(|s| s.id == session).and_then(|s| s.name.clone()).unwrap_or_else(|| "без имени".into());
    let result = fleet.release_lock(scope, &key, &session).await;
    // Действие меняет чужую работу — в логе остаётся, с какой машины, что и чем кончилось.
    log::info!(
        "рой: снятие замка с машины {}: {key} ({}), держатель «{holder}» ({session}) — {}: {}{}",
        before.machine.as_deref().unwrap_or("?"),
        if scope == Scope::Fleet { "общий, хаб" } else { "локальный, диспетчер" },
        if result.released { "снят" } else { "НЕ снят" },
        result.message,
        result.output.as_deref().map(|o| format!(" | {}", o.replace('\n', " | "))).unwrap_or_default(),
    );
    Ok(result)
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

    const GONE: &str = "dddd4444-0000-4000-8000-000000000004";

    /// Кнопка есть только у проблемного замка, и снимается он только у того же держателя.
    #[test]
    fn release_only_for_problem_lock() {
        let hub = parse_hub(&fixture(), NOW);
        let v = build(Some(&hub), hub_info(), &Part::default(), NOW);
        let deploy = releasable(&v, Scope::Fleet, "shop-1a2b3c4d::deploy", GONE).expect("держатель не в сети — снимать можно");
        assert_eq!(release_body(deploy), serde_json::json!({ "resource": "deploy", "repo": "shop-1a2b3c4d", "session": GONE, "force": true }));

        let healthy = releasable(&v, Scope::Fleet, "pult-9f8e7d6c::db-migrate", "cccc3333-0000-4000-8000-000000000003");
        assert!(healthy.unwrap_err().contains("больше не проблема"));
        // Замок успел перейти к другой сессии — прежнего держателя с этим ключом уже нет.
        assert!(releasable(&v, Scope::Fleet, "shop-1a2b3c4d::deploy", "cccc3333-0000-4000-8000-000000000003").unwrap_err().contains("уже нет"));
        assert!(releasable(&v, Scope::Local, "shop-1a2b3c4d::deploy", GONE).is_err(), "общий замок — не локальный");

        // Замок вне репозитория: хаб хранит его под ключом `-::ресурс` и ждёт тот же `repo`.
        let bare = LockView { key: "-::push".into(), resource: "push".into(), repo: None, ..deploy.clone() };
        assert_eq!(release_body(&bare)["repo"], "-");
    }

    #[test]
    fn release_reply_is_read_as_hub_writes_it() {
        assert!(release_reply(&serde_json::json!({ "ok": true, "freed": true, "next": null })).is_ok());
        assert!(release_reply(&serde_json::json!({ "ok": true, "freed": false })).is_ok(), "замка уже не было — не отказ");
        let refused = release_reply(&serde_json::json!({ "ok": false, "error": "замок держит другая сессия" })).unwrap_err();
        assert_eq!(refused, "хаб отказал: замок держит другая сессия");
        assert!(release_reply(&serde_json::json!({})).is_err());
    }

    #[test]
    fn dispatcher_missing_is_explained() {
        let dir = temp_dir("dispatcher");
        let node = Some(PathBuf::from("node"));
        assert!(dispatcher(&dir, node.clone()).unwrap_err().starts_with("диспетчер не установлен"));
        write(&dir, "orch.mjs", "");
        assert!(dispatcher(&dir, None).unwrap_err().contains("не найден node"));
        assert!(dispatcher(&dir, node).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Название чата — имя только для сессии, которая в рою не представилась.
    #[test]
    fn chat_title_names_session_without_fleet_name() {
        let dir = temp_dir("titles");
        let (named, unnamed, terminal) = ("aaaa1111-0000-4000-8000-000000000001", GONE, "ffff6666-0000-4000-8000-000000000006");
        write(&dir, "desktop/acc/org/local_1.json", &format!(r#"{{"cliSessionId":"{named}","title":"Экран роя"}}"#));
        write(&dir, "desktop/acc/org/local_2.json", &format!(r#"{{"cliSessionId":"{unnamed}","title":"Выкатка 2.14"}}"#));
        write(&dir, "desktop/acc/org/local_3.json", r#"{"title":"чат без сессии"}"#);
        write(&dir, "desktop/acc/org/local_4.json", "не json");
        write(&dir, "live/101.json", &format!(r#"{{"sessionId":"{terminal}","name":"Чиню сборку"}}"#));
        write(&dir, "live/102.json", &format!(r#"{{"sessionId":"{unnamed}","name":"Выкатка 2.15"}}"#));
        write(&dir, "live/103.json", r#"{"sessionId":"без-названия","name":""}"#);

        let titles = chat_titles(Some(&dir.join("desktop")), &dir.join("live"));
        assert_eq!(titles.len(), 3, "{titles:?}");
        assert_eq!(titles[unnamed], "Выкатка 2.15", "запись запущенной сессии свежее записи чата");
        assert_eq!(titles[terminal], "Чиню сборку");
        assert!(chat_titles(None, &dir.join("нет-такого")).is_empty());

        let hub = parse_hub(&fixture(), NOW);
        let mut v = build(Some(&hub), hub_info(), &Part::default(), NOW);
        assert_eq!(session(&v, unnamed).name, None);
        fill_names(&mut v, &titles);
        assert_eq!(session(&v, unnamed).name.as_deref(), Some("Выкатка 2.15"));
        assert_eq!(session(&v, named).name.as_deref(), Some("карта"), "имя из роя главнее названия чата");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── Снятие замка на настоящих хабе и диспетчере роя ──
    //
    // Нужны `node` и репозиторий agent-orchestrator: рядом с Пультом или в `PULT_FLEET_REPO`.
    // Нет их — тесты пропускаются. Хаб поднимается свой, на свободном порту и во временном
    // каталоге; рабочий хаб машины не участвует ни в одном запросе.

    fn fleet_repo() -> Option<(PathBuf, PathBuf)> {
        let repo = std::env::var_os("PULT_FLEET_REPO").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../agent-orchestrator"));
        // FLEET_URL в окружении главнее fleet.json — с ним тест пошёл бы в чужой хаб.
        let usable = repo.join("fleet/server.mjs").is_file() && repo.join("orch.mjs").is_file() && std::env::var_os("FLEET_URL").is_none();
        match (find_node(), usable) {
            (Some(node), true) => Some((node, repo)),
            _ => {
                eprintln!("пропуск: нет node или репозитория роя ({})", repo.display());
                None
            }
        }
    }

    struct Hub(std::process::Child);
    impl Drop for Hub {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// Общий замок: держатель молчит 25 минут, второй ждёт в очереди. Проверяется и отказ
    /// (чужой токен), и то, что здоровый замок кодом Пульта не снимается.
    #[test]
    fn releases_fleet_lock_on_local_hub() {
        let Some((node, repo)) = fleet_repo() else { return };
        let dir = temp_dir("hub");
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let url = format!("http://127.0.0.1:{port}");
        let now = now_ms();
        // Замолчавшего держателя запросами не получить: хаб считает молчание от последнего
        // запроса, а 20 минут тест ждать не может. Поэтому держатель и его замок — в снимке,
        // с которого хаб стартует; всё остальное — настоящими запросами.
        let seeded = serde_json::json!({
            "sessions": { "quiet": { "machine": "ноут-б", "name": "", "at": now - 25 * MIN } },
            "locks": { "shop-1a2b::deploy": { "resource": "deploy", "repo": "shop-1a2b", "session": "quiet", "machine": "ноут-б", "command": "./deploy.sh", "reason": "", "since": now - 25 * MIN } },
        });
        write(&dir, "state/state.json", &seeded.to_string());
        let _hub = Hub(
            crate::system::command(&node)
                .arg(repo.join("fleet/server.mjs"))
                .envs([("FLEET_PORT", port.to_string()), ("FLEET_TOKEN", "test-token".into()), ("FLEET_STATE", dir.join("state").to_string_lossy().into_owned())])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
        write(&dir, "fleet.json", &serde_json::json!({ "url": url, "token": "test-token" }).to_string());

        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let Config::Hub(cfg) = read_config(&dir) else { panic!("конфиг не прочитан") };
            let client = hub_client().unwrap();
            for _ in 0..50 {
                if fetch_state(&client, &cfg).await.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            let acquire = |resource: &'static str, session: &'static str| {
                let body = serde_json::json!({ "resource": resource, "repo": "shop-1a2b", "session": session, "machine": "мак-а" });
                let (client, cfg) = (&client, &cfg);
                async move { post(client, cfg, "/lease/acquire", &body).await.unwrap() }
            };
            assert_eq!(acquire("deploy", "waiting").await["ok"], false, "второй встаёт в очередь");
            assert_eq!(acquire("push", "waiting").await["ok"], true);

            // Здоровый замок: держатель только что отметился — снимать нечего.
            let (r, _, _) = release(&dir, None, Scope::Fleet, "shop-1a2b::push", "waiting").await;
            assert!(!r.released && r.message.contains("больше не проблема"), "{r:?}");

            // Чужой токен: хаб отказывает, замок остаётся.
            write(&dir, "fleet.json", &serde_json::json!({ "url": url, "token": "чужой" }).to_string());
            let (r, _, _) = release(&dir, None, Scope::Fleet, "shop-1a2b::deploy", "quiet").await;
            assert!(!r.released && r.message.contains("отказал в доступе"), "{r:?}");
            write(&dir, "fleet.json", &serde_json::json!({ "url": url, "token": "test-token" }).to_string());

            let before = build(Some(&parse_hub(&fetch_state(&client, &cfg).await.unwrap(), now_ms())), hub_info(), &Part::default(), now_ms());
            let lock = releasable(&before, Scope::Fleet, "shop-1a2b::deploy", "quiet").unwrap();
            assert_eq!(lock.problem, Some(Problem::Silent));
            assert_eq!(lock.queue.len(), 1);

            let (r, hub, _) = release(&dir, None, Scope::Fleet, "shop-1a2b::deploy", "quiet").await;
            assert_eq!(r, ReleaseResult { released: true, message: "замок снят".into(), output: None });
            assert!(hub.unwrap().locks.iter().all(|l| l.session != "quiet"));
            // Очередь двинулась: ждавший берёт освободившийся замок.
            assert_eq!(acquire("deploy", "waiting").await["ok"], true);
            // Повтор по уже снятому замку — отказ, а не второй сброс по новому держателю.
            let (r, _, _) = release(&dir, None, Scope::Fleet, "shop-1a2b::deploy", "quiet").await;
            assert!(!r.released && r.message.contains("уже нет"), "{r:?}");
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Локальный замок ушедшей сессии снимает диспетчер: сухой прогон называет процесс,
    /// настоящий — гасит его и убирает замок.
    #[cfg(unix)]
    #[test]
    fn releases_local_lock_with_dispatcher() {
        let Some((node, repo)) = fleet_repo() else { return };
        let dir = temp_dir("mine");
        std::fs::copy(repo.join("orch.mjs"), dir.join("orch.mjs")).unwrap();
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        for file in paths_in(&repo.join("lib")) {
            std::fs::copy(&file, dir.join("lib").join(file.file_name().unwrap())).unwrap();
        }
        let now = now_ms();
        // Полминуты, а не час: если тест упадёт раньше, чем диспетчер погасит процесс, тот уйдёт сам.
        let mut stuck = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let pid = stuck.id();
        // Завершившегося потомка надо забрать, иначе он остаётся зомби и для диспетчера «жив».
        let reaped = std::thread::spawn(move || stuck.wait());
        // Ресурс машины, а не проекта: ключ проектного диспетчер считает из папки держателя.
        write(&dir, "locks/heavy-misc#1/holder.json", &format!(r#"{{"session":"gone","resource":"heavy-misc","machine":"мак-а","cwd":"/work/shop","command":"docker build .","since":"{}"}}"#, iso(now - 180 * MIN)));
        write(&dir, "fleet-beat-gone.json", &format!(r#"{{"at":{}}}"#, now - 170 * MIN));
        write(&dir, "owned-procs.json", &format!(r#"[{{"pid":{pid},"session":"gone","resource":"heavy-misc","cmd":"sleep 30","verified":true}}]"#));

        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let dry = run_mine(&dir, Some(node.clone()), "gone", false).await.unwrap();
            assert!(dry.contains("heavy-misc") && dry.contains(&pid.to_string()), "{dry}");
            assert!(dir.join("locks/heavy-misc#1").exists(), "сухой прогон ничего не снимает");

            // Без node действие не начинается, и замок на месте.
            let (r, _, _) = release(&dir, None, Scope::Local, "heavy-misc", "gone").await;
            assert!(!r.released && r.message.contains("не найден node"), "{r:?}");

            let (r, _, local) = release(&dir, Some(node), Scope::Local, "heavy-misc", "gone").await;
            assert!(r.released && r.message == "замок снят", "{r:?}");
            assert!(r.output.unwrap().contains(&pid.to_string()), "вывод диспетчера называет погашенный процесс");
            assert!(local.locks.is_empty());
        });
        assert!(reaped.join().unwrap().is_ok_and(|status| !status.success()), "процесс держателя погашен, а не доработал сам");
        let _ = std::fs::remove_dir_all(&dir);
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
        let mut v = build(Some(&parse_hub(&state, now)), hub_info(), &read_local(&orchestrator_dir(), now), now);
        let unnamed = v.sessions.iter().filter(|s| s.name.is_none()).count();
        let titles = titles();
        fill_names(&mut v, &titles);
        println!("названий чатов на машине {}, сессий без имени {unnamed} → после названий {}", titles.len(), v.sessions.iter().filter(|s| s.name.is_none()).count());
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
