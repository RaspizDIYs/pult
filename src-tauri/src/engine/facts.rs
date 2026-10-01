//! Входные факты движка. Локальные проверки пишет планировщик, остальное — сборщик
//! по ssh (`collect/`, стыкуется тонким адаптером): ему нужны только эти структуры.

use serde::Serialize;
use std::collections::HashMap;
use std::time::Duration;
use time::OffsetDateTime;

/// Проверка в инвентаре: id узла и номер в его списке `проверки` (с нуля).
pub type CheckKey = (String, usize);

/// Результат одной проверки — так же он уходит в интерфейс (`CheckResult` контракта).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckResult {
    pub kind: ResultKind,
    /// Что проверяли, по-человечески.
    pub target: String,
    /// id узла, на котором выполнена; None — с машины пользователя.
    pub from: Option<String>,
    /// None — выполнить не удалось, узнать нечего.
    pub ok: Option<bool>,
    /// Наблюдаемый факт: «порт 22: таймаут 3 с».
    pub fact: String,
    pub latency_ms: Option<u64>,
    #[serde(with = "time::serde::rfc3339")]
    pub measured_at: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ResultKind {
    Tcp,
    Http,
    Container,
    Vm,
    Collect,
}

/// Только эти поля берутся из `docker inspect`; окружение не читается вовсе.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerFacts {
    /// running | exited | restarting | created | paused | dead
    pub state: String,
    pub exit_code: Option<i64>,
    pub oom_killed: Option<bool>,
    /// healthy | unhealthy | starting | None — healthcheck не задан
    pub health: Option<String>,
    pub restart_count: Option<u64>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub image: Option<String>,
}

/// Всё, что движок знает о мире к моменту оценки.
#[derive(Debug, Default)]
pub struct Facts {
    /// Проверки без `откуда`, выполненные с этой машины.
    pub local: HashMap<CheckKey, CheckResult>,
    /// Интервал локальных проверок: срок годности их результатов — три интервала.
    pub local_interval: Duration,
    /// Результат сбора по id узла со `сбор`.
    pub hosts: HashMap<String, HostFacts>,
    /// Когда машина проснулась: всё, что измерено раньше, считается устаревшим.
    pub woke_at: Option<OffsetDateTime>,
}

/// Итог одного сеанса сбора с узла.
#[derive(Debug, Clone)]
pub struct HostFacts {
    pub measured_at: OffsetDateTime,
    /// Интервал сбора: срок годности — три интервала.
    pub interval: Duration,
    /// Err — сбор с узла не удался (текст причины). Тогда факты о контейнерах и ВМ
    /// на узле и проверки `откуда` этого узла — unknown, а не fail.
    pub result: Result<Collected, String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // строит сборщик; до стыковки — только тесты
pub struct Collected {
    /// Секция docker: Err — команда не удалась («узнать не удалось», а не «контейнеров
    /// нет»); Ok — полный список, включая остановленные (`docker ps -a`).
    pub containers: Result<Vec<Container>, String>,
    /// Секция proxmox (`qm list` + `pct list`), так же.
    pub vms: Result<Vec<Vm>, String>,
    /// Проверки, у которых `откуда` — этот узел.
    pub checks: HashMap<CheckKey, CheckResult>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // строит сборщик; до стыковки — только тесты
pub struct Container {
    pub name: String,
    /// Метки com.docker.compose.project / com.docker.compose.service.
    pub compose_project: Option<String>,
    pub compose_service: Option<String>,
    pub facts: ContainerFacts,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // строит сборщик; до стыковки — только тесты
pub struct Vm {
    /// Номер ВМ или LXC — то, что в инвентаре в поле `вм`.
    pub id: u32,
    /// Как отдаёт гипервизор: running | stopped | paused …
    pub status: String,
}

/// Отбрасывает ответы уже завершённого цикла: опоздавший ответ старого цикла не должен
/// перетереть свежий. У каждого источника (локальные проверки, сбор) — свой.
#[derive(Debug, Default)]
pub struct CycleGate {
    finished: u64,
}

impl CycleGate {
    pub fn accepts(&self, cycle: u64) -> bool {
        cycle > self.finished
    }

    pub fn finish(&mut self, cycle: u64) {
        self.finished = self.finished.max(cycle);
    }
}
