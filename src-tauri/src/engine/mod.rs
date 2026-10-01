//! Движок: чистая функция «инвентарь + факты + прошлые состояния → состояния»
//! (docs/контракт.md, раздел 2). Сеть, файлы и время сюда не заходят — всё приходит
//! аргументами, поэтому движок проверяется на фикстурах.

pub mod facts;

use crate::inventory::{Expected, Inventory, Node};
use facts::{CheckResult, Container, ContainerFacts, Facts, HostFacts, ResultKind, Vm};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use time::OffsetDateTime;

/// Сейчас, с точностью до секунды: дробные секунды в ISO не всякий движок JS разберёт.
pub fn now() -> OffsetDateTime {
    let t = OffsetDateTime::now_utc();
    t.replace_nanosecond(0).unwrap_or(t)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OwnStatus {
    Ok,
    Fail,
    Unknown,
    Stale,
    Unchecked,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeState {
    pub id: String,
    pub own: OwnStatus,
    pub confirmed: bool,
    pub fact: String,
    pub hints: Vec<String>,
    pub is_root: bool,
    pub blocked_by: Vec<String>,
    pub checks: Vec<CheckResult>,
    pub container: Option<ContainerFacts>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub measured_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub since: Option<OffsetDateTime>,
    /// Цикл, с которого `own` не менялся: по нему считается `confirmed`.
    #[serde(skip)]
    pub since_cycle: u64,
}

/// Оценка всех узлов в порядке инвентаря. `prev` — состояния прошлой оценки,
/// `cycle` — номер текущего цикла (повторная оценка в том же цикле ничего не подтверждает).
pub fn evaluate(
    inv: &Inventory,
    facts: &Facts,
    prev: &HashMap<String, NodeState>,
    cycle: u64,
    now: OffsetDateTime,
) -> Vec<NodeState> {
    let mut states: Vec<NodeState> = inv.nodes.iter().map(|n| own_state(n, facts, now)).collect();

    let index: HashMap<&str, usize> = inv.nodes.iter().enumerate().map(|(i, n)| (n.id.as_str(), i)).collect();
    let failed: Vec<bool> = states.iter().map(|s| s.own == OwnStatus::Fail).collect();
    // ponytail: предки обходом для каждого узла, O(n²); хватит на сотни узлов
    let ancestors: Vec<Vec<usize>> = inv.nodes.iter().map(|n| ancestors(n, inv, &index)).collect();
    let is_root: Vec<bool> = (0..states.len())
        .map(|i| failed[i] && !ancestors[i].iter().any(|&a| failed[a]))
        .collect();

    for (i, s) in states.iter_mut().enumerate() {
        s.is_root = is_root[i];
        if s.own != OwnStatus::Ok {
            s.blocked_by = ancestors[i].iter().filter(|&&a| is_root[a]).map(|&a| inv.nodes[a].id.clone()).collect();
        }
        match prev.get(&s.id) {
            Some(p) if p.own == s.own => {
                s.since = p.since;
                s.since_cycle = p.since_cycle;
            }
            _ => {
                s.since = Some(now);
                s.since_cycle = cycle;
            }
        }
        s.confirmed = cycle > s.since_cycle;
    }
    states
}

/// Обязательные предки: `на` и `зависит_от`, транзитивно; в порядке инвентаря.
fn ancestors(node: &Node, inv: &Inventory, index: &HashMap<&str, usize>) -> Vec<usize> {
    let mut seen = vec![false; inv.nodes.len()];
    let mut stack: Vec<&Node> = vec![node];
    while let Some(n) = stack.pop() {
        for id in n.on.iter().chain(&n.depends_on) {
            if let Some(&i) = index.get(id.as_str()) {
                if !seen[i] {
                    seen[i] = true;
                    stack.push(&inv.nodes[i]);
                }
            }
        }
    }
    (0..seen.len()).filter(|&i| seen[i]).collect()
}

/// Собственный результат узла — без оглядки на предков.
fn own_state(node: &Node, facts: &Facts, now: OffsetDateTime) -> NodeState {
    // Наблюдения узла и интервал, от которого считается их срок годности.
    let mut seen: Vec<(CheckResult, Duration)> = Vec::new();
    let mut hints = Vec::new();
    let mut container = None;

    // Факты о контейнере или ВМ идут первыми: «контейнер остановлен» объясняет
    // карточку лучше, чем «/health не отвечает».
    if let (true, Some(on)) = (node.is_bound(), &node.on) {
        if let Some(host) = facts.hosts.get(on) {
            let (r, h, c) = bound_fact(node, on, host);
            seen.push((r, host.interval));
            hints.extend(h);
            container = c;
        }
    }

    for (i, check) in node.checks.iter().enumerate() {
        let key = (node.id.clone(), i);
        match &check.from {
            None => {
                if let Some(r) = facts.local.get(&key) {
                    seen.push((r.clone(), facts.local_interval));
                }
            }
            Some(from) => {
                let Some(host) = facts.hosts.get(from) else { continue };
                let unknown = |fact: String| CheckResult {
                    kind: result_kind(check),
                    target: check.target(),
                    from: Some(from.clone()),
                    ok: None,
                    fact,
                    latency_ms: None,
                    measured_at: host.measured_at,
                };
                let r = match &host.result {
                    Err(e) => unknown(format!("нет данных с {from}: {e}")),
                    Ok(c) => c.checks.get(&key).cloned().unwrap_or_else(|| unknown(format!("сбор с {from} не вернул эту проверку"))),
                };
                seen.push((r, host.interval));
            }
        }
    }

    if let (Some(collect), Some(host)) = (&node.collect, facts.hosts.get(&node.id)) {
        let (ok, fact) = match &host.result {
            Ok(_) => (Some(true), "сбор выполнен".to_string()),
            Err(e) => (None, format!("сбор не удался: {e}")),
        };
        let r = CheckResult {
            kind: ResultKind::Collect,
            target: format!("ssh {}", collect.ssh),
            from: collect.via.clone(),
            ok,
            fact,
            latency_ms: None,
            measured_at: host.measured_at,
        };
        seen.push((r, host.interval));
    }

    // Устаревает только то, что что-то утверждало; «узнать не удалось» и так unknown.
    let stale = |r: &CheckResult, interval: Duration| {
        r.ok.is_some() && (facts.woke_at.is_some_and(|w| r.measured_at < w) || now - r.measured_at > interval * 3)
    };
    let first = |want: fn(&CheckResult, bool) -> bool| {
        seen.iter().find(|(r, iv)| want(r, stale(r, *iv))).map(|(r, _)| r.fact.clone())
    };
    // Свежий отказ — доказательство; иначе свежий ok. Смешанный случай (часть в порядке,
    // часть узнать не удалось) контракт не определяет — считаем ok: известное в порядке,
    // неизвестное видно в checks.
    let (own, fact) = if let Some(f) = first(|r, old| r.ok == Some(false) && !old) {
        (OwnStatus::Fail, f)
    } else if let Some(f) = first(|r, old| r.ok == Some(true) && !old) {
        (OwnStatus::Ok, f)
    } else if let Some(f) = first(|_, old| old) {
        (OwnStatus::Stale, f)
    } else if let Some(f) = first(|r, _| r.ok.is_none()) {
        (OwnStatus::Unknown, f)
    } else if node.checks.is_empty() && !node.is_bound() && node.collect.is_none() {
        (OwnStatus::Unchecked, "не проверяется".to_string())
    } else {
        (OwnStatus::Unknown, "измерений ещё не было".to_string())
    };

    NodeState {
        id: node.id.clone(),
        own,
        confirmed: false,
        fact,
        hints,
        is_root: false,
        blocked_by: Vec::new(),
        measured_at: seen.iter().map(|(r, _)| r.measured_at).min(),
        checks: seen.into_iter().map(|(r, _)| r).collect(),
        container,
        since: None,
        since_cycle: 0,
    }
}

fn result_kind(check: &crate::inventory::Check) -> ResultKind {
    match check.kind {
        crate::inventory::CheckKind::Tcp => ResultKind::Tcp,
        crate::inventory::CheckKind::Http => ResultKind::Http,
    }
}

/// Факт о контейнере или ВМ узла по данным сбора с узла `on`.
fn bound_fact(node: &Node, on: &str, host: &HostFacts) -> (CheckResult, Vec<String>, Option<ContainerFacts>) {
    let (kind, target) = match (&node.container, &node.compose, node.vm) {
        (Some(name), _, _) => (ResultKind::Container, name.clone()),
        (None, Some(c), _) => (ResultKind::Container, format!("compose {}/{}", c.project, c.service)),
        _ => (ResultKind::Vm, format!("ВМ {}", node.vm.unwrap_or_default())),
    };
    let section = match (&host.result, kind) {
        (Err(e), _) => Err(format!("нет данных с {on}: {e}")),
        (Ok(c), ResultKind::Container) => c.containers.as_ref().map(|list| container_fact(node, list)).map_err(|e| format!("docker на {on}: {e}")),
        (Ok(c), _) => c.vms.as_ref().map(|list| vm_fact(node, list)).map_err(|e| format!("гипервизор на {on}: {e}")),
    };
    // Не удался сбор или его секция — это «узнать не удалось», а не отказ.
    let (ok, fact, hints, facts) = section.unwrap_or_else(|e| (None, e, Vec::new(), None));
    let r = CheckResult {
        kind,
        target,
        from: Some(on.to_string()),
        ok,
        fact,
        latency_ms: None,
        measured_at: host.measured_at,
    };
    (r, hints, facts)
}

type Fact = (Option<bool>, String, Vec<String>, Option<ContainerFacts>);

fn container_fact(node: &Node, list: &[Container]) -> Fact {
    let matches: Vec<&Container> = list
        .iter()
        .filter(|c| match (&node.container, &node.compose) {
            (Some(name), _) => &c.name == name,
            (None, Some(cmp)) => {
                c.compose_project.as_deref() == Some(cmp.project.as_str())
                    && c.compose_service.as_deref() == Some(cmp.service.as_str())
            }
            _ => false,
        })
        .collect();
    // Несколько экземпляров сервиса compose — один узел: работает, если работает хоть один.
    let Some(c) = matches.iter().find(|c| c.facts.state == "running").or(matches.first()) else {
        // Список полный (сбор удался), значит контейнера действительно нет.
        return match node.expected {
            Expected::Running => (Some(false), "контейнер не найден".into(), Vec::new(), None),
            _ => (Some(true), "контейнера нет, так и должно быть".into(), Vec::new(), None),
        };
    };
    let f = &c.facts;
    let running = f.state == "running";
    let unhealthy = f.health.as_deref() == Some("unhealthy");
    let fact = match f.state.as_str() {
        "running" if node.expected == Expected::Stopped => "сейчас запущен".to_string(),
        "running" if unhealthy => "контейнер работает, healthcheck: unhealthy".to_string(),
        "running" => "контейнер работает".to_string(),
        "exited" => match f.exit_code {
            Some(code) => format!("контейнер остановлен, код {code}"),
            None => "контейнер остановлен".to_string(),
        },
        "restarting" => match f.restart_count {
            Some(n) => format!("контейнер перезапускается, перезапусков: {n}"),
            None => "контейнер перезапускается".to_string(),
        },
        "created" => "контейнер создан, но не запущен".to_string(),
        "paused" => "контейнер приостановлен".to_string(),
        other => format!("контейнер: {other}"),
    };
    let ok = match node.expected {
        Expected::Running => running && !unhealthy,
        Expected::Stopped | Expected::Any => true,
    };
    (Some(ok), fact, container_hints(f), Some(f.clone()))
}

/// Возможные причины — только предположения, заголовок карточки остаётся фактом.
fn container_hints(f: &ContainerFacts) -> Vec<String> {
    let mut hints = Vec::new();
    let stopped = f.state != "running";
    if f.oom_killed == Some(true) {
        hints.push("нехватка памяти: процесс убит по OOM".to_string());
    } else if stopped && f.exit_code == Some(137) {
        // 137 без OOMKilled — не обязательно память: так же выглядит docker kill.
        hints.push("остановлен принудительно (SIGKILL)".to_string());
    }
    if stopped && f.exit_code == Some(143) {
        hints.push("остановлен сигналом SIGTERM: штатная остановка или выкатка".to_string());
    }
    if f.state == "restarting" {
        hints.push("процесс падает при запуске, и docker перезапускает его по кругу".to_string());
    }
    if f.health.as_deref() == Some("unhealthy") {
        hints.push("процесс жив, но встроенная проверка здоровья не проходит".to_string());
    }
    hints
}

fn vm_fact(node: &Node, list: &[Vm]) -> Fact {
    let id = node.vm.unwrap_or_default();
    let Some(vm) = list.iter().find(|v| v.id == id) else {
        return match node.expected {
            Expected::Running => (Some(false), format!("ВМ {id} не найдена"), Vec::new(), None),
            _ => (Some(true), format!("ВМ {id} нет, так и должно быть"), Vec::new(), None),
        };
    };
    let running = vm.status == "running";
    let fact = match (running, node.expected) {
        (true, Expected::Stopped) => "сейчас запущена".to_string(),
        (true, _) => format!("ВМ {id} работает"),
        (false, _) => format!("ВМ {id}: {}", vm.status),
    };
    (Some(running || node.expected != Expected::Running), fact, Vec::new(), None)
}

#[cfg(test)]
mod tests;
