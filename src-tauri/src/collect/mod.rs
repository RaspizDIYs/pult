//! Сбор фактов по ssh: план → скрипт POSIX sh → один сеанс ssh на цепочку → разобранные факты.
//! Контракт: docs/контракт.md, раздел 4. Типы инвентаря сюда не тянем намеренно:
//! план строит адаптер (`crate::adapter`); факты о контейнерах — сразу типы движка.

mod parse;
mod run;
mod script;

pub use run::{collect, stream_logs, LogStream};
pub(crate) use script::{is_plain, is_url};

/// Контейнеры, пиры и гостевые системы — те же типы, что читает движок: второй копии
/// с другими числами нет, адаптеру нечего переводить.
pub use crate::engine::facts::{Container, ContainerFacts, Guest, GuestKind, WgPeer};

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
