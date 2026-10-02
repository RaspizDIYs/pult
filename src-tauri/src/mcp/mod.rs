//! MCP-серверы этой машины: Пульт находит их сам в настройках Claude и показывает на карте
//! отдельной площадкой «Эта машина». Контракт: docs/контракт.md, раздел 7.
//!
//! Это локальные настройки каждого человека, поэтому в общий инвентарь узлы не пишутся и через
//! движок не проходят: состояние собирается здесь же.
//!
//! **Секреты.** В настройках рядом с именем сервера лежат токены: значения `env`, заголовки,
//! аргументы командной строки. Они живут только в `Server` (в памяти, для настоящей проверки) и
//! никуда больше не уходят: в интерфейс идёт `McpInfo`, в лог — имена. У `Server` нарочно нет
//! `Debug`: его нельзя случайно напечатать через `{:?}`.

pub mod handshake;

use crate::commands::NodeView;
use crate::engine::facts::{CheckResult, ResultKind};
use crate::engine::{NodeState, OwnStatus};
use crate::inventory::NodeKind;
use crate::store::Transition;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use time::OffsetDateTime;

/// Папка на карте, в которой лежат серверы.
const FOLDER: &str = "MCP";
/// Приставка id: в инвентаре id с «#» не бывает, поэтому с его узлами не пересечётся.
const ID_PREFIX: &str = "#mcp/";
const NOT_RUNNING: &str = "сейчас не запущен: стартует вместе с сессией Claude";

/// Где лежат настройки Claude. Пути приходят аргументом, а не берутся из окружения на месте:
/// тесты читают выдуманные каталоги, а окно разработки запускается с чужим `HOME`.
pub struct Paths {
    pub home: PathBuf,
    /// `claude_desktop_config.json` приложения Claude Desktop.
    pub desktop: PathBuf,
}

impl Paths {
    pub fn of_user() -> Option<Self> {
        let home = PathBuf::from(std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })?);
        Some(Self::in_home(home))
    }

    pub fn in_home(home: PathBuf) -> Self {
        let app_data = if cfg!(windows) {
            std::env::var_os("APPDATA").map_or_else(|| home.join("AppData").join("Roaming"), PathBuf::from)
        } else {
            home.join("Library").join("Application Support")
        };
        Self { desktop: app_data.join("Claude").join("claude_desktop_config.json"), home }
    }
}

/// Сервер как он описан в настройках — вместе с секретами. Наружу не отдаётся.
#[derive(Clone, PartialEq)]
pub struct Server {
    pub name: String,
    /// Откуда взят; одинаковые описания из разных мест — один сервер.
    pub sources: Vec<String>,
    /// Каталог проекта: от него считаются относительные пути в аргументах.
    pub cwd: Option<PathBuf>,
    pub transport: Transport,
}

#[derive(Clone, PartialEq)]
pub enum Transport {
    Stdio { command: String, args: Vec<String>, env: BTreeMap<String, String> },
    /// `sse` — прежний транспорт с потоком событий, иначе Streamable HTTP.
    Remote { sse: bool, url: String, headers: BTreeMap<String, String> },
}

/// Всё, что о сервере видит интерфейс. Значений `env`, заголовков и аргументов здесь нет.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpInfo {
    /// stdio | http | sse
    pub transport: &'static str,
    pub sources: Vec<String>,
    /// Имя исполняемого файла (stdio).
    pub command: Option<String>,
    /// Путь к скрипту, если он есть среди аргументов (stdio).
    pub script: Option<String>,
    /// Схема и хост адреса, без пути и запроса: в них бывают токены (http, sse).
    pub host: Option<String>,
    pub env_names: Vec<String>,
    pub header_names: Vec<String>,
}

// ───────────── Обнаружение ─────────────

/// Все серверы из настроек Claude Code и Claude Desktop, в постоянном порядке. Битый или
/// отсутствующий файл — не ошибка: остальные читаются; причины — вторым значением.
pub fn discover(paths: &Paths) -> (Vec<Server>, Vec<String>) {
    let mut found: Vec<Server> = Vec::new();
    let mut errors = Vec::new();
    let home = &paths.home;
    let tilde = |p: &Path| match p.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    };
    let mut add = |servers: Option<&Value>, source: String, cwd: &Path, off: &[String]| {
        let Some(map) = servers.and_then(Value::as_object) else { return };
        for (name, raw) in map {
            let Some(transport) = transport(raw) else { continue };
            let source = if off.contains(name) { format!("{source}, выключен в настройках Claude") } else { source.clone() };
            match found.iter_mut().find(|s| &s.name == name && s.transport == transport) {
                Some(same) => same.sources.push(source),
                None => found.push(Server { name: name.clone(), sources: vec![source], cwd: Some(cwd.to_path_buf()), transport }),
            }
        }
    };

    let main = read(&home.join(".claude.json"), &mut errors).unwrap_or_default();
    add(main.get("mcpServers"), "Claude Code: все проекты".into(), home, &[]);
    // Домашний каталог — тоже проект (`~/.mcp.json`), даже если Claude его таким не записал.
    let mut projects: Vec<(PathBuf, Value)> = vec![(home.clone(), Value::Null)];
    for (path, project) in main.get("projects").and_then(Value::as_object).into_iter().flatten() {
        match projects.iter_mut().find(|(p, _)| p == Path::new(path)) {
            Some(slot) => slot.1 = project.clone(),
            None => projects.push((PathBuf::from(path), project.clone())),
        }
    }
    for (dir, project) in &projects {
        add(project.get("mcpServers"), format!("Claude Code: проект {}", tilde(dir)), dir, &[]);
        let file = dir.join(".mcp.json");
        if !file.is_file() {
            continue;
        }
        let off: Vec<String> = project
            .get("disabledMcpjsonServers")
            .and_then(Value::as_array)
            .map(|list| list.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let shared = read(&file, &mut errors).unwrap_or_default();
        add(shared.get("mcpServers"), tilde(&file), dir, &off);
    }
    if paths.desktop.is_file() {
        let desktop = read(&paths.desktop, &mut errors).unwrap_or_default();
        add(desktop.get("mcpServers"), "Claude Desktop".into(), home, &[]);
    }
    (found, errors)
}

fn read(path: &Path, errors: &mut Vec<String>) -> Option<Value> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            errors.push(format!("{}: {e}", path.display()));
            return None;
        }
    };
    serde_json::from_slice(&bytes).map_err(|e| errors.push(format!("{}: не разобран ({e})", path.display()))).ok()
}

/// Запись настроек → транспорт. Незнакомый вид (или запись без команды и адреса) пропускается:
/// новый транспорт Claude не должен ронять обнаружение остальных.
fn transport(raw: &Value) -> Option<Transport> {
    let text = |key: &str| raw.get(key).and_then(Value::as_str).map(String::from);
    // Значения бывают числами и флагами — процессу и серверу они уходят строками.
    let string = |v: &Value| v.as_str().map_or_else(|| v.to_string(), String::from);
    let map = |key: &str| -> BTreeMap<String, String> {
        raw.get(key).and_then(Value::as_object).into_iter().flatten().map(|(k, v)| (k.clone(), string(v))).collect()
    };
    match (text("type").as_deref(), text("command"), text("url")) {
        (None | Some("stdio"), Some(command), _) => Some(Transport::Stdio {
            command,
            args: raw.get("args").and_then(Value::as_array).into_iter().flatten().map(string).collect(),
            env: map("env"),
        }),
        (kind @ (None | Some("http") | Some("sse")), None, Some(url)) => {
            Some(Transport::Remote { sse: kind == Some("sse"), url, headers: map("headers") })
        }
        _ => None,
    }
}

impl Server {
    /// Безопасная для показа выжимка. `home` — чтобы путь к скрипту читался как `~/…`.
    pub fn info(&self, home: &Path) -> McpInfo {
        let mut info = McpInfo {
            transport: "stdio",
            sources: self.sources.clone(),
            command: None,
            script: None,
            host: None,
            env_names: Vec::new(),
            header_names: Vec::new(),
        };
        match &self.transport {
            Transport::Stdio { command, args, env } => {
                info.command = Some(Path::new(command).file_name().map_or_else(|| command.clone(), |f| f.to_string_lossy().into_owned()));
                // Аргумент показываем, только если это существующий файл: путь к скрипту — не
                // секрет, а ключ, переданный аргументом, файлом не окажется.
                info.script = args.iter().find(|a| self.resolve(a).is_file()).map(|a| {
                    let path = self.resolve(a);
                    path.strip_prefix(home).map_or_else(|_| path.display().to_string(), |rest| format!("~/{}", rest.display()))
                });
                info.env_names = env.keys().cloned().collect();
            }
            Transport::Remote { sse, url, headers } => {
                info.transport = if *sse { "sse" } else { "http" };
                info.host = reqwest::Url::parse(url).ok().and_then(|u| {
                    let port = u.port().map(|p| format!(":{p}")).unwrap_or_default();
                    Some(format!("{}://{}{port}", u.scheme(), u.host_str()?))
                });
                info.header_names = headers.keys().cloned().collect();
            }
        }
        info
    }

    fn resolve(&self, arg: &str) -> PathBuf {
        match &self.cwd {
            Some(cwd) => cwd.join(arg), // абсолютный путь `join` оставляет как есть
            None => PathBuf::from(arg),
        }
    }
}

// ───────────── Пассивное состояние ─────────────

/// Что видно без побочных эффектов, по одному результату на сервер: у stdio — есть ли сейчас
/// его процесс, у http и sse — открыт ли порт адреса (без запроса и без заголовков авторизации).
pub async fn scan(servers: &[Server]) -> Vec<CheckResult> {
    let measured_at = crate::engine::now();
    let result = |kind, target: &str, ok, fact: String, latency_ms| CheckResult {
        kind,
        target: target.to_string(),
        from: None,
        ok,
        fact,
        latency_ms,
        measured_at,
        models: Vec::new(),
    };
    let processes = if servers.iter().any(|s| matches!(s.transport, Transport::Stdio { .. })) { processes().await } else { None };
    let mut out: Vec<Option<CheckResult>> = Vec::new();
    // Порты — параллельно: десяток недоступных адресов по три секунды не должен растянуть обход.
    let mut ports = tokio::task::JoinSet::new();
    for (i, server) in servers.iter().enumerate() {
        out.push(match &server.transport {
            Transport::Stdio { command, args, .. } => {
                let (ok, fact) = match processes.as_ref().map(|list| list.iter().filter(|line| is_process_of(line, command, args)).count()) {
                    None => (None, "список процессов получить не удалось".to_string()),
                    // Не запущен — не отказ: сервер stdio живёт, только пока открыта сессия Claude.
                    Some(0) => (None, NOT_RUNNING.to_string()),
                    Some(n) => (Some(true), format!("запущен, процессов: {n}")),
                };
                Some(result(ResultKind::Process, "процесс сервера", ok, fact, None))
            }
            Transport::Remote { url, .. } => {
                let address = reqwest::Url::parse(url).ok().and_then(|u| Some(format!("{}:{}", u.host_str()?, u.port_or_known_default()?)));
                match address {
                    None => Some(result(ResultKind::Tcp, "адрес сервера", Some(false), "адрес в настройках не разбирается".into(), None)),
                    Some(address) => {
                        ports.spawn(async move {
                            let started = std::time::Instant::now();
                            let (ok, fact, connected) = crate::probes::tcp(&address, Duration::from_secs(3)).await;
                            (i, address, ok, fact, connected.then(|| started.elapsed().as_millis() as u64))
                        });
                        None
                    }
                }
            }
        });
    }
    while let Some(done) = ports.join_next().await {
        if let Ok((i, address, ok, fact, latency_ms)) = done {
            out[i] = Some(result(ResultKind::Tcp, &address, Some(ok), fact, latency_ms));
        }
    }
    out.into_iter().map(|r| r.unwrap_or_else(|| result(ResultKind::Tcp, "адрес сервера", None, "проверка порта не выполнилась".into(), None))).collect()
}

/// Командные строки всех процессов машины. Нужны только для сопоставления: не хранятся и не
/// печатаются — в чужих командных строках тоже бывают секреты.
async fn processes() -> Option<Vec<String>> {
    let mut cmd = if cfg!(windows) {
        let mut cmd = tokio::process::Command::new("powershell");
        cmd.args(["-NoProfile", "-NonInteractive", "-Command", "Get-CimInstance Win32_Process | ForEach-Object { $_.CommandLine }"]);
        cmd
    } else {
        let mut cmd = tokio::process::Command::new("ps");
        cmd.args(["-axww", "-o", "command="]);
        cmd
    };
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let out = tokio::time::timeout(Duration::from_secs(10), cmd.output()).await.ok()?.ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).lines().map(String::from).collect())
}

/// Процесс этого сервера: в его командной строке есть все аргументы-значения из настроек
/// (путь к скрипту, имя пакета или модуля). Флаги не сравниваются: запускалки (`npx`, `uvx`)
/// переставляют их и подменяют саму команду. Без таких аргументов — по имени команды.
// ponytail: сопоставление по вхождению строк, без дерева процессов; два сервера с одним
// скриптом и разным окружением неразличимы. Точнее — когда Claude начнёт отдавать pid серверов.
fn is_process_of(line: &str, command: &str, args: &[String]) -> bool {
    let mut values = args.iter().filter(|a| !a.starts_with('-')).peekable();
    if values.peek().is_some() {
        return values.all(|a| line.contains(a.as_str()));
    }
    let name = Path::new(command).file_name().map_or(command.into(), |f| f.to_string_lossy());
    line.split_whitespace().next().is_some_and(|first| Path::new(first).file_name().is_some_and(|f| f.to_string_lossy() == name))
}

// ───────────── Состояние на карте ─────────────

/// Серверы этой машины и всё, что о них известно.
#[derive(Default)]
pub struct Local {
    home: PathBuf,
    servers: Vec<(String, Server)>,
    passive: HashMap<String, CheckResult>,
    /// Итог «Проверить по-настоящему». Держится до следующего нажатия: пассивное наблюдение
    /// его ни подтвердить, ни опровергнуть не может (процесс жив — не значит, что отвечает).
    /// Снимается сам, только если описание сервера в настройках изменилось.
    real: HashMap<String, CheckResult>,
    states: HashMap<String, NodeState>,
    errors: Vec<String>,
}

/// Что изменилось после обновления — монитору для событий и истории.
#[derive(Default)]
pub struct Update {
    pub list_changed: bool,
    pub states: Vec<NodeState>,
    pub transitions: Vec<Transition>,
}

impl Local {
    pub fn states(&self) -> &HashMap<String, NodeState> {
        &self.states
    }

    pub fn server(&self, id: &str) -> Option<Server> {
        self.servers.iter().find(|(i, _)| i == id).map(|(_, s)| s.clone())
    }

    pub fn views(&self) -> Vec<NodeView> {
        self.servers
            .iter()
            .map(|(id, s)| NodeView {
                id: id.clone(),
                title: s.name.clone(),
                kind: NodeKind::Mcp,
                group: None,
                project: Some(FOLDER.into()),
                on: None,
                depends_on: Vec::new(),
                access: None,
                links: Vec::new(),
                undeclared: false,
                has_logs: false,
                mcp: Some(s.info(&self.home)),
            })
            .collect()
    }

    /// Новый обход: список серверов и их пассивное состояние (в том же порядке).
    pub fn update(&mut self, paths: &Paths, servers: Vec<Server>, errors: Vec<String>, passive: Vec<CheckResult>, now: OffsetDateTime) -> Update {
        if self.errors != errors {
            for e in &errors {
                log::warn!("настройки Claude: {e}");
            }
            self.errors = errors;
        }
        self.home = paths.home.clone();
        // Одноимённые, но разные описания (в двух проектах) — разные узлы: «имя», «имя/2»…
        let mut seen: HashMap<String, usize> = HashMap::new();
        let servers: Vec<(String, Server)> = servers
            .into_iter()
            .map(|s| {
                let n = seen.entry(s.name.clone()).or_default();
                *n += 1;
                let id = if *n == 1 { format!("{ID_PREFIX}{}", s.name) } else { format!("{ID_PREFIX}{}/{n}", s.name) };
                (id, s)
            })
            .collect();
        let was = |id: &str| self.servers.iter().find(|(i, _)| i == id).map(|(_, s)| s);
        // Вид узла для интерфейса зависит и от источников, поэтому сравниваем сервер целиком.
        let list_changed = servers.len() != self.servers.len() || servers.iter().any(|(id, s)| was(id) != Some(s));
        self.real.retain(|id, _| servers.iter().any(|(i, s)| i == id && was(id).is_some_and(|old| old.transport == s.transport)));
        self.passive = servers.iter().map(|(id, _)| id.clone()).zip(passive).collect();
        self.servers = servers;
        let mut update = self.evaluate(now);
        update.list_changed = list_changed;
        if list_changed {
            log::info!("MCP: серверов в настройках Claude — {}", self.servers.len());
        }
        update
    }

    /// Итог настоящей проверки. `server` — описание, которое проверяли: если настройки за это
    /// время сменились, итог относится к другому серверу и не записывается.
    pub fn set_real(&mut self, id: &str, server: &Server, result: CheckResult, now: OffsetDateTime) -> Update {
        if self.server(id).is_some_and(|s| s.transport == server.transport) {
            self.real.insert(id.to_string(), result);
        }
        self.evaluate(now)
    }

    fn evaluate(&mut self, now: OffsetDateTime) -> Update {
        let mut update = Update::default();
        let mut states = HashMap::new();
        for (id, _) in &self.servers {
            let prev = self.states.get(id);
            let state = node_state(id, self.passive.get(id), self.real.get(id), prev, now);
            // Первое состояние — не переход, как и у узлов инвентаря.
            if prev.is_some_and(|p| p.own != state.own) {
                update.transitions.push(Transition { id: id.clone(), at: now, own: state.own, fact: state.fact.clone() });
            }
            if prev != Some(&state) {
                update.states.push(state.clone());
            }
            states.insert(id.clone(), state);
        }
        self.states = states;
        update
    }
}

/// Состояние узла. Отказ настоящей проверки — корень (красная карточка, счётчик в трее); всё,
/// что видно пассивно, корнем не бывает: закрытый порт чужого сервера или не запущенный процесс —
/// не повод для тревоги.
fn node_state(id: &str, passive: Option<&CheckResult>, real: Option<&CheckResult>, prev: Option<&NodeState>, now: OffsetDateTime) -> NodeState {
    let failed = real.filter(|r| r.ok == Some(false));
    let (own, fact) = match (failed, passive) {
        (Some(r), _) => (OwnStatus::Fail, r.fact.clone()),
        (None, Some(p)) => match p.ok {
            Some(true) => (OwnStatus::Ok, p.fact.clone()),
            Some(false) => (OwnStatus::Fail, p.fact.clone()),
            // «Не запущен» — нейтральное состояние, а не «неизвестно»: наблюдение состоялось.
            None => (OwnStatus::Unchecked, p.fact.clone()),
        },
        (None, None) => (OwnStatus::Unknown, "измерений ещё не было".to_string()),
    };
    NodeState {
        id: id.to_string(),
        own,
        // Подтверждать нечем и незачем: уведомлений по этим узлам нет.
        confirmed: true,
        fact,
        hints: Vec::new(),
        is_root: failed.is_some(),
        blocked_by: Vec::new(),
        checks: passive.into_iter().chain(real).cloned().collect(),
        container: None,
        measured_at: passive.map(|p| p.measured_at),
        since: prev.filter(|p| p.own == own).map_or(Some(now), |p| p.since),
        since_cycle: 0,
        last_measured: None,
        since_measured: None,
    }
}

#[cfg(test)]
mod tests;
