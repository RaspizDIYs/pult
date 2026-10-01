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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeView {
    pub id: String,
    pub title: String,
    /// Значение `вид` из инвентаря как есть: «хост», «контейнер»…
    pub kind: NodeKind,
    pub group: Option<String>,
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

type Core<'a> = State<'a, Arc<Monitor>>;

#[tauri::command]
pub fn get_snapshot(core: Core<'_>) -> Snapshot {
    core.snapshot()
}

/// Внеочередной цикл; результат придёт событием `pult://states`. Цикл всегда полный:
/// локальные проверки дешёвые, а `id` пригодится, когда появится сбор по ssh.
#[tauri::command]
pub fn recheck(core: Core<'_>, id: Option<String>) {
    let _ = id;
    core.recheck();
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

const LOGS_LATER: &str = "логи контейнеров появятся вместе со сбором по ssh";

#[tauri::command]
pub fn open_logs(id: String, tail: Option<u32>) -> Result<LogStream, String> {
    let _ = (id, tail);
    Err(LOGS_LATER.into())
}

#[tauri::command]
pub fn close_logs(stream_id: String) -> Result<(), String> {
    let _ = stream_id;
    Err(LOGS_LATER.into())
}
