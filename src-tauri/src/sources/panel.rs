//! Панель задач («каны»): только чтение, с локальным кэшем. Контракт — docs/контракт.md, раздел 6.
//!
//! Панель живёт на домашнем сервере и пропадает вместе со светом или интернетом. Поэтому
//! каждый удачный ответ сохраняется в папку данных, а при ошибке отдаётся сохранённое
//! с `stale` и фактом ошибки: интерфейс пишет «данные от 14:32, панель недоступна: …»,
//! а не показывает пустую доску.
//!
//! Токен — только в системной связке ключей и в памяти процесса. Ни одна структура, которая
//! уходит в интерфейс или в файл, его не содержит; в лог он не пишется.

use crate::store::write_atomic;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;
use tauri::State;
use time::OffsetDateTime;

/// Как у CLI панели: стенд за WireGuard при обрыве держит соединение минутами.
const TIMEOUT: Duration = Duration::from_secs(8);
const CONFIG_FILE: &str = "panel.json";
const CACHE_DIR: &str = "panel-cache";
const KEYRING_SERVICE: &str = "ru.raspizdiys.pult";
const KEYRING_USER: &str = "panel-token";

// ───────────── Ответы панели ─────────────
// Разбираем только то, что показываем: незнакомые поля serde пропускает, и новая версия
// панели не ломает Пульт. Имена в JSON для интерфейса — camelCase, как во всём контракте.

/// Состояние — символ из markdown хранилища: ' ', '/', '?', 'x', '-'.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskState {
    #[serde(rename(deserialize = " "))]
    Todo,
    #[serde(rename(deserialize = "/"))]
    Doing,
    #[serde(rename(deserialize = "?"))]
    Review,
    #[serde(rename(deserialize = "x"))]
    Done,
    #[serde(rename(deserialize = "-"))]
    Cancelled,
    /// Новое состояние панели не должно ронять весь список.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all(serialize = "camelCase"))]
pub struct Task {
    /// У старых задач хранилища ключа нет.
    pub key: Option<String>,
    pub key_num: Option<i64>,
    /// Родитель — номер задачи того же проекта (эпик или обычная задача).
    pub epic_num: Option<i64>,
    pub state: TaskState,
    pub title: String,
    pub priority: Option<i64>,
    pub kind: Option<String>,
    #[serde(default, deserialize_with = "flag")]
    pub is_epic: bool,
    pub who: Option<String>,
    pub project: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub created: Option<String>,
    pub updated: Option<String>,
    pub taken_at: Option<String>,
    pub done_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRef {
    pub key: Option<String>,
    pub title: String,
    pub state: TaskState,
    pub priority: Option<i64>,
    pub who: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDetail {
    #[serde(flatten)]
    pub task: Task,
    pub body: Option<String>,
    #[serde(default)]
    pub journal: Vec<String>,
    pub parent: Option<TaskRef>,
    #[serde(default)]
    pub kids: Vec<TaskRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all(serialize = "camelCase"))]
pub struct Epic {
    pub key: Option<String>,
    pub key_num: Option<i64>,
    pub title: String,
    pub state: TaskState,
    pub project: String,
    #[serde(default, rename(deserialize = "всего"))]
    pub total: i64,
    #[serde(default, rename(deserialize = "готово"))]
    pub done: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StateCounts {
    #[serde(default, rename(deserialize = " "))]
    pub todo: i64,
    #[serde(default, rename(deserialize = "/"))]
    pub doing: i64,
    #[serde(default, rename(deserialize = "?"))]
    pub review: i64,
    #[serde(default, rename(deserialize = "x"))]
    pub done: i64,
    #[serde(default, rename(deserialize = "-"))]
    pub cancelled: i64,
}

/// Сводка панели — без людей и теплокарты: Пульту нужны только счётчики.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all(serialize = "camelCase"))]
pub struct Overview {
    #[serde(rename(deserialize = "открыто"))]
    pub open: i64,
    #[serde(rename(deserialize = "всего"))]
    pub total: i64,
    #[serde(rename(deserialize = "закрытоЗаНеделю"))]
    pub closed_week: i64,
    #[serde(rename(deserialize = "закрытоЗаМесяц"))]
    pub closed_month: i64,
    #[serde(default, rename(deserialize = "счёт"))]
    pub by_state: StateCounts,
}

#[derive(Debug, Clone, Deserialize)]
struct Me {
    login: Option<String>,
    name: Option<String>,
}

impl Me {
    fn name(&self) -> Option<String> {
        self.name.clone().or_else(|| self.login.clone()).filter(|s| !s.is_empty())
    }
}

/// SQLite отдаёт флаги числом 0/1.
fn flag<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::Bool(b) => b,
        serde_json::Value::Number(n) => n.as_i64() != Some(0),
        _ => false,
    })
}

// ───────────── Ответ с кэшем ─────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Problem {
    NoUrl,
    NoToken,
    /// 401/403. Отдельно от недоступности: это чинится не ожиданием, а новым токеном.
    Auth,
    /// Нет сети, таймаут, 5xx, не тот ответ.
    Unavailable,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Fetched<T> {
    /// `None` — ни свежего ответа, ни сохранённого.
    pub data: Option<T>,
    /// Когда получены `data`: сейчас или при последнем удачном запросе.
    #[serde(with = "time::serde::rfc3339::option")]
    pub fetched_at: Option<OffsetDateTime>,
    /// `data` из кэша, свежий запрос не удался.
    pub stale: bool,
    pub problem: Option<Problem>,
    /// Наблюдаемый факт: «нет ответа за 8 с», «панель ответила 502».
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
struct Failure {
    problem: Problem,
    fact: String,
}

impl Failure {
    fn new(problem: Problem, fact: impl Into<String>) -> Self {
        Self { problem, fact: fact.into() }
    }
}

/// Адрес и токен, либо почему идти некуда.
type Target = Result<(String, String), Failure>;

// ponytail: файлы JSON, SQLite когда понадобятся запросы по кэшу
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Saved {
    #[serde(with = "time::serde::rfc3339")]
    fetched_at: OffsetDateTime,
    /// Ответ панели как есть: разбирается заново при чтении, так что правка разбора
    /// не требует сбрасывать кэш.
    body: serde_json::Value,
}

fn cache_file(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.json"))
}

/// Битый или старого формата файл — как будто кэша нет: запрос из-за него не падает.
fn read_cache<T: DeserializeOwned>(dir: &Path, key: &str) -> Option<(T, OffsetDateTime)> {
    let bytes = std::fs::read(cache_file(dir, key)).ok()?;
    let parsed = serde_json::from_slice::<Saved>(&bytes)
        .and_then(|s| serde_json::from_value::<T>(s.body).map(|d| (d, s.fetched_at)));
    match parsed {
        Ok(v) => Some(v),
        Err(e) => {
            log::warn!("панель: сохранённый ответ «{key}» не читается, пропускаю: {e}");
            None
        }
    }
}

fn write_cache(dir: &Path, key: &str, saved: &Saved) {
    let res = std::fs::create_dir_all(dir)
        .and_then(|_| serde_json::to_vec(saved).map_err(std::io::Error::other))
        .and_then(|bytes| write_atomic(&cache_file(dir, key), &bytes));
    if let Err(e) = res {
        log::warn!("панель: ответ «{key}» не сохранился: {e}");
    }
}

fn from_cache<T: DeserializeOwned>(dir: &Path, key: &str, f: Failure) -> Fetched<T> {
    let saved = read_cache::<T>(dir, key);
    Fetched {
        stale: saved.is_some(),
        fetched_at: saved.as_ref().map(|s| s.1),
        data: saved.map(|s| s.0),
        problem: Some(f.problem),
        error: Some(f.fact),
    }
}

/// GET к панели: свежий ответ сохраняется, ошибка отдаёт сохранённое.
async fn get<T: DeserializeOwned>(target: &Target, dir: &Path, key: &str, segments: &[&str], query: &[(&str, &str)]) -> Fetched<T> {
    let fresh = match target {
        Ok((url, token)) => request(url, token, segments, query)
            .await
            .and_then(|body| match serde_json::from_value::<T>(body.clone()) {
                Ok(data) => Ok((data, body)),
                Err(e) => Err(Failure::new(Problem::Unavailable, format!("ответ панели не разобран: {e}"))),
            })
            // Текст ошибки показывают и сохраняют в лог: токена в нём быть не должно, даже
            // если прокси его повторил.
            .map_err(|f| Failure { fact: f.fact.replace(token.as_str(), "***"), ..f }),
        Err(f) => Err(f.clone()),
    };
    match fresh {
        Ok((data, body)) => {
            let at = crate::engine::now();
            write_cache(dir, key, &Saved { fetched_at: at, body });
            Fetched { data: Some(data), fetched_at: Some(at), stale: false, problem: None, error: None }
        }
        Err(f) => {
            if f.problem != Problem::NoUrl && f.problem != Problem::NoToken {
                log::info!("панель: «{key}» не получен: {}", f.fact);
            }
            from_cache(dir, key, f)
        }
    }
}

async fn request(base: &str, token: &str, segments: &[&str], query: &[(&str, &str)]) -> Result<serde_json::Value, Failure> {
    let down = |fact: String| Failure::new(Problem::Unavailable, fact);
    let mut url = reqwest::Url::parse(base).map_err(|e| down(format!("адрес панели не разобран: {e}")))?;
    // Сегменты кодируются по одному: в ключах бывает кириллица (СК-7).
    url.path_segments_mut().map_err(|_| down("адрес панели не годится для запросов".into()))?.pop_if_empty().extend(segments);
    if !query.is_empty() {
        url.query_pairs_mut().extend_pairs(query);
    }
    let mut auth = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| Failure::new(Problem::Auth, "в токене недопустимые символы"))?;
    // Чувствительный заголовок не попадает в Debug запроса.
    auth.set_sensitive(true);

    // updater ставит тот же провайдер; кто первый — неважно, второй вызов ничего не делает.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = reqwest::Client::builder().timeout(TIMEOUT).build().map_err(|e| down(crate::probes::innermost(&e)))?;
    let sent = |e: reqwest::Error| {
        if e.is_timeout() {
            down(format!("нет ответа за {} с", TIMEOUT.as_secs()))
        } else {
            down(crate::probes::innermost(&e))
        }
    };
    let resp = client
        .get(url)
        .header(reqwest::header::AUTHORIZATION, auth)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(sent)?;
    let status = resp.status().as_u16();
    let bytes = resp.bytes().await.map_err(sent)?;
    let body = serde_json::from_slice::<serde_json::Value>(&bytes).ok();
    // Панель объясняет отказ полем error: «неизвестный токен», «KAN-1 не найден».
    let said = body.as_ref().and_then(|b| b.get("error")?.as_str()).map(|s| format!(": {s}")).unwrap_or_default();
    match status {
        200..=299 => body.ok_or_else(|| down(format!("панель ответила не JSON ({status})"))),
        401 | 403 => Err(Failure::new(Problem::Auth, format!("панель ответила {status}{said}"))),
        // Traefik на неподнятом сервисе отвечает HTML 502 — это тоже «недоступна».
        _ => Err(down(format!("панель ответила {status}{said}"))),
    }
}

// ───────────── Настройки и токен ─────────────

#[derive(Serialize, Deserialize, Default)]
struct ConfigFile {
    url: Option<String>,
}

/// Что интерфейс знает о доступе: адрес и факт наличия токена, но не сам токен.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelConfig {
    pub url: Option<String>,
    pub token_set: bool,
    /// Связка ключей не отдала токен (запрет доступа, заблокирована).
    pub token_error: Option<String>,
}

fn keyring_entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER).map_err(|e| format!("связка ключей недоступна: {e}"))
}

fn keyring_read() -> Result<Option<String>, String> {
    match keyring_entry()?.get_password() {
        Ok(t) => Ok(Some(t)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("связка ключей не отдала токен: {e}")),
    }
}

pub struct Panel {
    dir: PathBuf,
    url: Mutex<Option<String>>,
    /// Токен читается из связки один раз за запуск: на маке каждое чтение может спросить
    /// разрешение. `None` — ещё не читали, `Some(None)` — токена нет.
    token: Mutex<Option<Option<String>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Panel {
    /// `dir` — папка данных приложения: там `panel.json` с адресом и `panel-cache/`.
    pub fn new(dir: PathBuf) -> Self {
        let url = std::fs::read(dir.join(CONFIG_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice::<ConfigFile>(&b).ok())
            .and_then(|c| c.url);
        Self { dir, url: Mutex::new(url), token: Mutex::new(None) }
    }

    fn cache(&self) -> PathBuf {
        self.dir.join(CACHE_DIR)
    }

    fn token(&self) -> Result<Option<String>, String> {
        let mut slot = lock(&self.token);
        if let Some(t) = slot.as_ref() {
            return Ok(t.clone());
        }
        // Ошибку не запоминаем: человек мог отказать в доступе случайно и нажать «Обновить».
        let t = keyring_read()?;
        *slot = Some(t.clone());
        Ok(t)
    }

    fn target(&self) -> Target {
        let url = lock(&self.url).clone().ok_or_else(|| Failure::new(Problem::NoUrl, "адрес панели не задан"))?;
        match self.token() {
            Ok(Some(token)) => Ok((url, token)),
            Ok(None) => Err(Failure::new(Problem::NoToken, "токен не задан")),
            Err(e) => Err(Failure::new(Problem::NoToken, e)),
        }
    }

    pub fn config(&self) -> PanelConfig {
        let (token_set, token_error) = match self.token() {
            Ok(t) => (t.is_some(), None),
            Err(e) => (false, Some(e)),
        };
        PanelConfig { url: lock(&self.url).clone(), token_set, token_error }
    }

    pub fn set_url(&self, url: Option<String>) -> Result<PanelConfig, String> {
        let url = url.map(|u| u.trim().trim_end_matches('/').to_string()).filter(|u| !u.is_empty());
        if let Some(u) = &url {
            match reqwest::Url::parse(u) {
                Ok(p) if (p.scheme() == "http" || p.scheme() == "https") && p.host_str().is_some() => {}
                _ => return Err(format!("нужен адрес вида https://tasks.example.com, а не «{u}»")),
            }
        }
        let bytes = serde_json::to_vec_pretty(&ConfigFile { url: url.clone() }).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&self.dir).and_then(|_| write_atomic(&self.dir.join(CONFIG_FILE), &bytes)).map_err(|e| format!("адрес не сохранился: {e}"))?;
        let mut cur = lock(&self.url);
        if *cur != url {
            // Другой адрес — другая панель: её задачи нельзя выдавать за «данные от 14:32» прежней.
            match std::fs::remove_dir_all(self.cache()) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => log::warn!("панель: старый кэш не удалился: {e}"),
                _ => {}
            }
        }
        *cur = url;
        drop(cur);
        Ok(self.config())
    }

    /// `None` — удалить токен из связки.
    pub fn set_token(&self, token: Option<String>) -> Result<PanelConfig, String> {
        let token = token.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        let entry = keyring_entry()?;
        match &token {
            // Токен уезжает в HTTP-заголовок: кириллица и пробелы там не проходят.
            Some(t) if !t.chars().all(|c| c.is_ascii_graphic()) => {
                return Err("токен — латиница, цифры и знаки, без пробелов".into());
            }
            Some(t) => entry.set_password(t).map_err(|e| format!("токен не сохранился в связке ключей: {e}"))?,
            None => match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => {}
                Err(e) => return Err(format!("токен не удалился из связки ключей: {e}")),
            },
        }
        *lock(&self.token) = Some(token);
        Ok(self.config())
    }

    async fn tasks(&self, mine: bool) -> Fetched<Vec<Task>> {
        let (target, dir) = (self.target(), self.cache());
        if !mine {
            return get(&target, &dir, "tasks", &["api", "tasks"], &[]).await;
        }
        // «Мои» — фильтром самой панели: она сводит разные написания одного имени в поле «кто».
        let me: Fetched<Me> = get(&target, &dir, "me", &["api", "me"], &[]).await;
        match me.data.as_ref().and_then(Me::name) {
            Some(name) => get(&target, &dir, "tasks-mine", &["api", "tasks"], &[("who", name.as_str())]).await,
            None => {
                let f = match (me.problem, me.error) {
                    (Some(p), Some(e)) => Failure::new(p, e),
                    _ => Failure::new(Problem::Unavailable, "панель не назвала, чей это токен"),
                };
                from_cache(&dir, "tasks-mine", f)
            }
        }
    }
}

/// Ключ задачи из интерфейса идёт в путь запроса и в имя файла кэша: только буквы, цифры и дефис.
fn valid_key(key: &str) -> bool {
    !key.is_empty() && key.chars().count() <= 40 && key.chars().all(|c| c.is_alphanumeric() || c == '-')
}

// ───────────── Команды ─────────────
// Асинхронные: связка ключей на маке может держать вызов, пока человек отвечает на запрос
// доступа, а синхронные команды идут на главном потоке и заморозили бы окно.

type Core<'a> = State<'a, Panel>;

#[tauri::command]
pub async fn panel_config(panel: Core<'_>) -> Result<PanelConfig, String> {
    Ok(panel.config())
}

#[tauri::command]
pub async fn panel_set_url(panel: Core<'_>, url: Option<String>) -> Result<PanelConfig, String> {
    panel.set_url(url)
}

#[tauri::command]
pub async fn panel_set_token(panel: Core<'_>, token: Option<String>) -> Result<PanelConfig, String> {
    panel.set_token(token)
}

#[tauri::command]
pub async fn panel_tasks(panel: Core<'_>, mine: bool) -> Result<Fetched<Vec<Task>>, String> {
    Ok(panel.tasks(mine).await)
}

#[tauri::command]
pub async fn panel_task(panel: Core<'_>, key: String) -> Result<Fetched<TaskDetail>, String> {
    if !valid_key(&key) {
        return Err(format!("не похоже на ключ задачи: «{key}»"));
    }
    Ok(get(&panel.target(), &panel.cache(), &format!("task-{key}"), &["api", "task", key.as_str()], &[]).await)
}

#[tauri::command]
pub async fn panel_projects(panel: Core<'_>) -> Result<Fetched<Vec<String>>, String> {
    Ok(get(&panel.target(), &panel.cache(), "projects", &["api", "projects"], &[]).await)
}

#[tauri::command]
pub async fn panel_epics(panel: Core<'_>) -> Result<Fetched<Vec<Epic>>, String> {
    Ok(get(&panel.target(), &panel.cache(), "epics", &["api", "epics"], &[]).await)
}

#[tauri::command]
pub async fn panel_overview(panel: Core<'_>) -> Result<Fetched<Overview>, String> {
    Ok(get(&panel.target(), &panel.cache(), "overview", &["api", "overview"], &[]).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const TOKEN: &str = "test-token-Zq9";

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
    }

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!("{}/src/sources/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pult-test-panel-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// Отвечает по очереди заготовленными ответами и запоминает заголовки запросов.
    async fn serve(replies: Vec<(u16, String)>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let mut seen = Vec::new();
            for (status, body) in replies {
                let (mut s, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 8192];
                let n = s.read(&mut buf).await.unwrap();
                seen.push(String::from_utf8_lossy(&buf[..n]).to_string());
                let reply = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                s.write_all(reply.as_bytes()).await.unwrap();
            }
            seen
        });
        (url, handle)
    }

    fn target(url: &str) -> Target {
        Ok((url.to_string(), TOKEN.to_string()))
    }

    #[test]
    fn parses_panel_fixtures() {
        let tasks: Vec<Task> = serde_json::from_str(&fixture("panel-tasks.json")).unwrap();
        assert_eq!(tasks.len(), 7);
        let states: Vec<TaskState> = tasks.iter().map(|t| t.state).collect();
        use TaskState::*;
        assert_eq!(states, [Doing, Doing, Review, Done, Cancelled, Todo, Unknown]);
        assert!(tasks[1].is_epic && !tasks[0].is_epic, "флаг 0/1 из SQLite");
        assert_eq!((tasks[0].key.as_deref(), tasks[0].epic_num, tasks[0].priority), (Some("SHOP-12"), Some(10), Some(1)));
        assert_eq!(tasks[0].tags, ["касса"]);
        assert!(tasks[5].key.is_none() && tasks[5].tags.is_empty(), "старая задача без ключа и без тегов");

        let d: TaskDetail = serde_json::from_str(&fixture("panel-task.json")).unwrap();
        assert_eq!(d.task.key.as_deref(), Some("SHOP-12"));
        assert_eq!(d.journal.len(), 2);
        assert_eq!(d.parent.as_ref().map(|p| (p.key.as_deref(), p.state)), Some((Some("SHOP-10"), Doing)));
        assert!(d.body.unwrap().starts_with("## Что видно"));

        let e: Vec<Epic> = serde_json::from_str(&fixture("panel-epics.json")).unwrap();
        assert_eq!((e[0].total, e[0].done), (4, 1));
        let o: Overview = serde_json::from_str(&fixture("panel-overview.json")).unwrap();
        assert_eq!((o.open, o.total, o.closed_week, o.closed_month), (9, 31, 4, 11));
        assert_eq!((o.by_state.doing, o.by_state.review, o.by_state.cancelled), (3, 1, 2));
        let me: Me = serde_json::from_str(&fixture("panel-me.json")).unwrap();
        assert_eq!(me.name().as_deref(), Some("алиса"));
        let p: Vec<String> = serde_json::from_str(&fixture("panel-projects.json")).unwrap();
        assert_eq!(p.len(), 3);

        // Интерфейсу — camelCase и слова вместо символов.
        let ui = serde_json::to_value(&tasks[0]).unwrap();
        assert_eq!((ui["state"].as_str(), ui["epicNum"].as_i64(), ui["isEpic"].as_bool()), (Some("doing"), Some(10), Some(false)));
        let ui = serde_json::to_value(&o).unwrap();
        assert_eq!((ui["closedWeek"].as_i64(), ui["byState"]["review"].as_i64()), (Some(4), Some(1)));
    }

    #[test]
    fn fresh_answer_is_saved_and_served_stale_on_error() {
        let dir = temp_dir("stale");
        block_on(async {
            let (url, server) = serve(vec![(200, fixture("panel-tasks.json")), (502, "<html>Bad Gateway</html>".into())]).await;
            let fresh: Fetched<Vec<Task>> = get(&target(&url), &dir, "tasks", &["api", "tasks"], &[]).await;
            assert_eq!((fresh.data.as_ref().map(Vec::len), fresh.stale, fresh.problem), (Some(7), false, None));
            assert!(cache_file(&dir, "tasks").exists());
            let at = fresh.fetched_at.unwrap();

            let stale: Fetched<Vec<Task>> = get(&target(&url), &dir, "tasks", &["api", "tasks"], &[]).await;
            assert_eq!((stale.data.as_ref().map(Vec::len), stale.stale, stale.problem), (Some(7), true, Some(Problem::Unavailable)));
            assert_eq!(stale.fetched_at, Some(at), "время — от удачного ответа, а не от ошибки");
            assert_eq!(stale.error.as_deref(), Some("панель ответила 502"));

            let seen = server.await.unwrap();
            assert!(seen[0].starts_with("GET /api/tasks HTTP/1.1"), "{}", seen[0]);
            assert!(seen[0].to_lowercase().contains(&format!("authorization: bearer {}", TOKEN.to_lowercase())));

            // Сервер выключен — соединение отклонено: тоже сохранённое.
            let down: Fetched<Vec<Task>> = get(&target(&url), &dir, "tasks", &["api", "tasks"], &[]).await;
            assert_eq!((down.stale, down.problem, down.fetched_at), (true, Some(Problem::Unavailable), Some(at)));
            assert!(down.error.unwrap().to_lowercase().contains("refused"), "факт ошибки соединения");
        });
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unauthorized_is_its_own_state_and_shows_cache() {
        let dir = temp_dir("auth");
        block_on(async {
            let echo = format!(r#"{{"ok":false,"error":"неизвестный токен {TOKEN}"}}"#);
            let (url, _server) = serve(vec![(200, fixture("panel-projects.json")), (401, echo)]).await;
            let _: Fetched<Vec<String>> = get(&target(&url), &dir, "projects", &["api", "projects"], &[]).await;
            let r: Fetched<Vec<String>> = get(&target(&url), &dir, "projects", &["api", "projects"], &[]).await;
            assert_eq!((r.problem, r.stale, r.data.as_ref().map(Vec::len)), (Some(Problem::Auth), true, Some(3)));
            assert_eq!(r.error.as_deref(), Some("панель ответила 401: неизвестный токен ***"));
            let json = serde_json::to_string(&r).unwrap();
            assert!(!json.contains(TOKEN), "токен в ответе интерфейсу: {json}");
            assert!(json.contains(r#""problem":"auth""#));
        });
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn broken_cache_file_does_not_break_request() {
        let dir = temp_dir("broken");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(cache_file(&dir, "overview"), b"{\"fetchedAt\": \"\xff").unwrap();
        block_on(async {
            let down: Fetched<Overview> = get(&target("http://127.0.0.1:1"), &dir, "overview", &["api", "overview"], &[]).await;
            assert_eq!((down.data.is_none(), down.stale, down.problem), (true, false, Some(Problem::Unavailable)));
            let (url, _server) = serve(vec![(200, fixture("panel-overview.json"))]).await;
            let ok: Fetched<Overview> = get(&target(&url), &dir, "overview", &["api", "overview"], &[]).await;
            assert_eq!(ok.data.map(|o| o.open), Some(9));
            assert!(read_cache::<Overview>(&dir, "overview").is_some(), "битый файл перезаписан");
        });
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn no_url_or_token_answers_without_network() {
        let dir = temp_dir("access");
        let panel = Panel { dir: dir.clone(), url: Mutex::new(None), token: Mutex::new(Some(Some(TOKEN.into()))) };
        let r: Fetched<Vec<String>> = block_on(get(&panel.target(), &panel.cache(), "projects", &["api", "projects"], &[]));
        assert_eq!((r.problem, r.error.as_deref()), (Some(Problem::NoUrl), Some("адрес панели не задан")));

        *lock(&panel.url) = Some("https://tasks.example.com".into());
        *lock(&panel.token) = Some(None);
        let r: Fetched<Vec<String>> = block_on(get(&panel.target(), &panel.cache(), "projects", &["api", "projects"], &[]));
        assert_eq!(r.problem, Some(Problem::NoToken));

        *lock(&panel.token) = Some(Some(TOKEN.into()));
        let cfg = serde_json::to_string(&panel.config()).unwrap();
        assert_eq!(cfg, r#"{"url":"https://tasks.example.com","tokenSet":true,"tokenError":null}"#);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn mine_asks_panel_who_and_filters_by_name() {
        let dir = temp_dir("mine");
        block_on(async {
            let (url, server) = serve(vec![(200, fixture("panel-me.json")), (200, fixture("panel-tasks.json"))]).await;
            let panel = Panel { dir: dir.clone(), url: Mutex::new(Some(url)), token: Mutex::new(Some(Some(TOKEN.into()))) };
            let r = panel.tasks(true).await;
            assert_eq!(r.data.map(|t| t.len()), Some(7));
            let seen = server.await.unwrap();
            assert!(seen[0].starts_with("GET /api/me "));
            // Кириллица в имени — кодированная, ключ кэша от имени не зависит.
            assert!(seen[1].starts_with("GET /api/tasks?who=%D0%B0%D0%BB%D0%B8%D1%81%D0%B0 "), "{}", seen[1]);
            assert!(cache_file(&panel.cache(), "tasks-mine").exists());
        });
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn task_key_must_be_plain() {
        assert!(valid_key("KAN-1223") && valid_key("СК-7") && valid_key("42"));
        assert!(!valid_key("../settings") && !valid_key("") && !valid_key("KAN 1") && !valid_key("a/b"));
    }

    /// Живая панель, только чтение: `PANEL_URL=… PANEL_TOKEN=… cargo test live_panel -- --ignored --nocapture`.
    /// Печатает только числа: названия задач и имена в вывод не попадают.
    #[test]
    #[ignore = "нужна живая панель: PANEL_URL и PANEL_TOKEN"]
    fn live_panel() {
        let (url, token) = (std::env::var("PANEL_URL").expect("PANEL_URL"), std::env::var("PANEL_TOKEN").expect("PANEL_TOKEN"));
        let dir = temp_dir("live");
        let t: Target = Ok((url.clone(), token.clone()));
        block_on(async {
            let me: Fetched<Me> = get(&t, &dir, "me", &["api", "me"], &[]).await;
            assert_eq!(me.problem, None, "{:?}", me.error);
            let name = me.data.and_then(|m| m.name()).expect("панель не назвала имя");

            let raw = request(&url, &token, &["api", "tasks"], &[]).await.map_err(|f| f.fact).unwrap();
            let raw = raw.as_array().expect("список задач — массив");
            let tasks: Fetched<Vec<Task>> = get(&t, &dir, "tasks", &["api", "tasks"], &[]).await;
            assert_eq!(tasks.problem, None, "{:?}", tasks.error);
            let tasks = tasks.data.unwrap();
            assert_eq!(tasks.len(), raw.len());
            // Сверка поле в поле с сырым ответом, а не только «разобралось».
            for (t, r) in tasks.iter().zip(raw) {
                assert_eq!(t.key.as_deref(), r["key"].as_str());
                assert_eq!(t.title, r["title"].as_str().unwrap());
                assert_eq!(t.project, r["project"].as_str().unwrap());
                assert_eq!(t.priority, r["priority"].as_i64());
                assert_eq!(t.epic_num, r["epic_num"].as_i64());
                assert_eq!(t.who.as_deref(), r["who"].as_str());
                assert_eq!(t.is_epic, r["is_epic"].as_i64() == Some(1));
            }
            let unknown = tasks.iter().filter(|t| t.state == TaskState::Unknown).count();
            let count = |s: TaskState| tasks.iter().filter(|t| t.state == s).count();
            let projects: Fetched<Vec<String>> = get(&t, &dir, "projects", &["api", "projects"], &[]).await;
            let epics: Fetched<Vec<Epic>> = get(&t, &dir, "epics", &["api", "epics"], &[]).await;
            let overview: Fetched<Overview> = get(&t, &dir, "overview", &["api", "overview"], &[]).await;
            let mine: Fetched<Vec<Task>> = get(&t, &dir, "tasks-mine", &["api", "tasks"], &[("who", name.as_str())]).await;
            let with_key = tasks.iter().find(|t| t.key.is_some() && t.epic_num.is_some()).and_then(|t| t.key.clone()).unwrap();
            let detail: Fetched<TaskDetail> = get(&t, &dir, "task-x", &["api", "task", with_key.as_str()], &[]).await;
            for (what, p, e) in [
                ("проекты", projects.problem, &projects.error),
                ("эпики", epics.problem, &epics.error),
                ("сводка", overview.problem, &overview.error),
                ("мои", mine.problem, &mine.error),
                ("задача", detail.problem, &detail.error),
            ] {
                assert_eq!(p, None, "{what}: {e:?}");
            }
            let o = overview.data.unwrap();
            let d = detail.data.unwrap();
            println!(
                "задач {} (разобрано {}), без ключа {}, неизвестных состояний {unknown}; \
                 не начаты {}, в работе {}, на ревью {}, готовы {}, отменены {}; \
                 проектов {}, эпиков {}; сводка: открыто {} из {}; моих {}; \
                 карточка: журнал {} строк, родитель {}, детей {}",
                raw.len(),
                tasks.len(),
                tasks.iter().filter(|t| t.key.is_none()).count(),
                count(TaskState::Todo),
                count(TaskState::Doing),
                count(TaskState::Review),
                count(TaskState::Done),
                count(TaskState::Cancelled),
                projects.data.map_or(0, |p| p.len()),
                epics.data.map_or(0, |e| e.len()),
                o.open,
                o.total,
                mine.data.map_or(0, |m| m.len()),
                d.journal.len(),
                if d.parent.is_some() { "есть" } else { "нет" },
                d.kids.len(),
            );
        });
        let _ = std::fs::remove_dir_all(dir);
    }
}
