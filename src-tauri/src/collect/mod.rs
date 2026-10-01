//! Сбор фактов по ssh: план → скрипт POSIX sh → один сеанс ssh на цепочку → разобранные факты.
//! Контракт: docs/контракт.md, раздел 4. Типы инвентаря сюда не тянем намеренно:
//! план строит адаптер на стороне движка.

// Движок подключит модуль следующим шагом; до тех пор публичное API никем не вызывается.
#![allow(dead_code, unused_imports)]

mod parse;
mod run;
mod script;

pub use parse::parse_output;
pub use run::{collect, stream_logs, LogStream, COLLECT_LIMIT};
pub use script::build_script;

use serde::Serialize;

/// Встроенные сборщики. Произвольных команд нет: инвентарь не должен быть источником кода.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Collector {
    Docker,
    Wireguard,
    Proxmox,
}

/// Один переход ssh: цель и ключ на той машине, которая этот ssh выполняет.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hop {
    /// Алиас из ~/.ssh/config или user@host.
    pub target: String,
    /// Передаётся как `-i`.
    pub key: Option<String>,
}

/// Проверка, которая выполняется на хосте внутри сеанса сбора (`откуда:` в инвентаре).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteCheck {
    Tcp { host: String, port: u16, timeout_ms: u32 },
    Http { url: String, timeout_ms: u32 },
}

/// Хост цепочки. Корень плана — внешний хост, `nested` выполняются с него (`через:`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPlan {
    /// id узла: им подписаны секции вывода.
    pub id: String,
    pub hop: Hop,
    pub collectors: Vec<Collector>,
    pub checks: Vec<RemoteCheck>,
    pub nested: Vec<HostPlan>,
}

/// План нельзя превратить в скрипт: значение не прошло белый список.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanError(pub String);

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PlanError {}

/// Итог одной секции. `Failed` — «узнать не удалось», а не «объектов нет».
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome<T> {
    Ok(T),
    Failed { rc: i32, error: String },
    /// Секция не пришла: истёк предел времени, оборвался вывод или хост недоступен.
    Missing,
}

/// Разобранный ответ одного хоста цепочки.
#[derive(Debug, Clone, PartialEq)]
pub struct HostReport {
    pub id: String,
    /// Удалось ли зайти на хост.
    pub ssh: Outcome<()>,
    /// `None` — сборщик не заказан в плане.
    pub docker: Option<Outcome<Vec<Container>>>,
    pub wireguard: Option<Outcome<Vec<WgPeer>>>,
    pub proxmox: Option<Outcome<Vec<Guest>>>,
    /// В порядке `HostPlan::checks`.
    pub checks: Vec<Outcome<ProbeResult>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Container {
    pub name: String,
    pub compose_project: Option<String>,
    pub compose_service: Option<String>,
    pub facts: ContainerFacts,
}

/// Тип из контракта (раздел 2), уходит в интерфейс как есть.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerFacts {
    pub state: String,
    pub exit_code: Option<i32>,
    pub oom_killed: Option<bool>,
    pub health: Option<String>,
    pub restart_count: Option<u32>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub image: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WgPeer {
    pub interface: String,
    pub public_key: String,
    pub allowed_ips: Vec<String>,
    /// По часам самого хоста; `None` — handshake не было ни разу.
    pub handshake_age_secs: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestKind {
    Vm,
    Lxc,
}

/// Гостевая система Proxmox: номера ВМ и LXC общие.
#[derive(Debug, Clone, PartialEq)]
pub struct Guest {
    pub vmid: u32,
    pub name: String,
    pub status: String,
    pub kind: GuestKind,
}

/// Результат удалённой tcp/http-проверки. Успех по списку ожидаемых кодов решает движок.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeResult {
    /// tcp: соединение установлено; http: получен ответ.
    pub connected: bool,
    pub http_code: Option<u16>,
    pub latency_ms: Option<u64>,
    /// Наблюдаемый факт для карточки: «HTTP 502», «соединение отклонено».
    pub fact: String,
}
