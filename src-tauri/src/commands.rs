//! Команды и события для интерфейса — ровно раздел 3 docs/контракт.md.
//! Поля в JSON — camelCase, как в TS-типах контракта.

use crate::engine::{NodeState, OwnStatus};
use crate::inventory::{Collector, Inventory, Node, NodeKind};
use crate::monitor::Monitor;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tauri::State;
use time::OffsetDateTime;

pub const EVENT_SNAPSHOT: &str = "pult://snapshot";
pub const EVENT_STATES: &str = "pult://states";
pub const EVENT_LOG: &str = "pult://log";
pub const EVENT_LOG_END: &str = "pult://log-end";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeView {
    pub id: String,
    pub title: String,
    /// Значение `вид` из инвентаря как есть: «хост», «контейнер»…
    pub kind: NodeKind,
    pub group: Option<String>,
    pub project: Option<String>,
    pub on: Option<String>,
    pub depends_on: Vec<String>,
    pub access: Option<AccessView>,
    pub links: Vec<LinkView>,
    pub undeclared: bool,
    pub has_logs: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AccessView {
    pub how: Option<String>,
    pub secret: Option<String>,
    pub who: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LinkView {
    pub title: String,
    pub url: String,
}

impl NodeView {
    pub fn new(n: &Node, inv: &Inventory) -> Self {
        // Логи — это docker logs на узле размещения: нужен контейнер и сбор docker там.
        let docker_on_host = n.on.as_ref().and_then(|on| inv.nodes.iter().find(|h| &h.id == on)).is_some_and(|h| {
            h.collect.as_ref().is_some_and(|c| c.what.contains(&Collector::Docker))
        });
        Self {
            id: n.id.clone(),
            title: n.title.clone(),
            kind: n.kind,
            group: n.group.clone(),
            project: n.project.clone(),
            on: n.on.clone(),
            depends_on: n.depends_on.clone(),
            access: n.access.as_ref().map(|a| AccessView { how: a.how.clone(), secret: a.secret.clone(), who: a.who.clone() }),
            links: n.links.iter().map(|l| LinkView { title: l.title.clone(), url: l.url.clone() }).collect(),
            undeclared: false,
            has_logs: (n.container.is_some() || n.compose.is_some()) && docker_on_host,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryInfo {
    pub path: Option<String>,
    pub commit: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub loaded_at: Option<OffsetDateTime>,
    pub error: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub cycle: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub taken_at: OffsetDateTime,
    pub inventory: InventoryInfo,
    pub nodes: Vec<NodeView>,
    pub states: HashMap<String, NodeState>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatesEvent {
    pub cycle: u64,
    pub states: Vec<NodeState>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub inventory_path: Option<String>,
    pub notifications: bool,
    pub autostart: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            // В режиме разработки сразу видна карта из примера, а не пустой экран.
            inventory_path: cfg!(debug_assertions)
                .then(|| concat!(env!("CARGO_MANIFEST_DIR"), "/../examples").to_string()),
            notifications: true,
            autostart: false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EnvCheck {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HistoryEntry {
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub own: OwnStatus,
    pub fact: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogStream {
    pub stream_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogLines {
    pub stream_id: String,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEnd {
    pub stream_id: String,
    pub error: Option<String>,
}

type Core<'a> = State<'a, Arc<Monitor>>;

#[tauri::command]
pub fn get_snapshot(core: Core<'_>) -> Snapshot {
    core.snapshot()
}

/// Внеочередная проверка; результат придёт событием `pult://states`. С `id` —
/// сразу локальные проверки этого узла, без — полный цикл.
#[tauri::command]
pub fn recheck(core: Core<'_>, id: Option<String>) {
    core.recheck(id);
}

/// Ссылка из инвентаря — в системный браузер. Инвентарь — данные из репозитория,
/// поэтому схему проверяем и здесь, а не только в интерфейсе: только http и https.
#[tauri::command]
pub fn open_url(app: tauri::AppHandle, url: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    match reqwest::Url::parse(&url).map(|u| u.scheme().to_string()) {
        Ok(scheme) if scheme == "http" || scheme == "https" => {}
        _ => return Err(format!("открываются только ссылки http и https: {url}")),
    }
    app.opener().open_url(url, None::<&str>).map_err(|e| format!("ссылка не открылась: {e}"))
}

#[tauri::command]
pub fn get_history(core: Core<'_>, id: String, limit: Option<usize>) -> Result<Vec<HistoryEntry>, String> {
    core.history(&id, limit.unwrap_or(50))
}

#[tauri::command]
pub fn get_settings(core: Core<'_>) -> Settings {
    core.settings()
}

#[tauri::command]
pub fn set_settings(core: Core<'_>, settings: Settings) -> Result<Settings, String> {
    core.set_settings(settings)
}

#[tauri::command]
pub fn check_environment(core: Core<'_>) -> Vec<EnvCheck> {
    core.check_environment()
}

/// Текст «почему обновление не поставить» или `None`, если можно. Решает ядро: интерфейс
/// не знает, из какого пути запущен.
#[tauri::command]
pub fn get_update_blocker() -> Option<String> {
    crate::update_blocker()
}

/// Асинхронная: поток логов запускается задачами tokio, а синхронные команды идут
/// на главном потоке, вне рантайма.
#[tauri::command]
pub async fn open_logs(core: Core<'_>, id: String, tail: Option<u32>) -> Result<LogStream, String> {
    let stream_id = core.open_logs(&id, tail.unwrap_or(200).min(5000))?;
    Ok(LogStream { stream_id })
}

#[tauri::command]
pub fn close_logs(core: Core<'_>, stream_id: String) {
    core.close_logs(&stream_id);
}
