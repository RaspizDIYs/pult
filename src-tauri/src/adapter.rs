//! Стыковка сборщика (`collect/`) с инвентарём и движком: инвентарь → планы цепочек,
//! отчёт сбора → факты движка, неописанные контейнеры, цепочка ssh для логов.
//! Сети здесь нет, поэтому всё проверяется на фикстурах.

use crate::collect::{self, Hop, HostPlan, HostReport, Outcome, PlanError, RemoteCheck};
use crate::engine::container_matches;
use crate::engine::facts::{CheckKey, CheckResult, Collected, Facts, HostFacts};
use crate::inventory::{Check, CheckKind, Collect, Collector, Expected, Inventory, Node, NodeKind};
use crate::probes;
use std::time::Duration;
use time::OffsetDateTime;

/// Планы по одному на внешний хост (`сбор` без `через`); вложенные — внутри.
pub fn plans(inv: &Inventory) -> Vec<HostPlan> {
    inv.nodes
        .iter()
        .filter(|n| n.collect.as_ref().is_some_and(|c| c.via.is_none()))
        .map(|n| plan(inv, n))
        .collect()
}

fn plan(inv: &Inventory, node: &Node) -> HostPlan {
    let c = node.collect.as_ref().expect("план строится только для узла со сбор");
    HostPlan {
        id: node.id.clone(),
        hop: hop(c),
        collectors: c
            .what
            .iter()
            .map(|w| match w {
                Collector::Docker => collect::Collector::Docker,
                Collector::Wireguard => collect::Collector::Wireguard,
                Collector::Proxmox => collect::Collector::Proxmox,
            })
            .collect(),
        checks: remote_checks(inv, &node.id).into_iter().map(|(_, check)| remote(check)).collect(),
        // Петель по `через` нет: их отклоняет проверка инвентаря.
        nested: inv
            .nodes
            .iter()
            .filter(|n| n.collect.as_ref().and_then(|c| c.via.as_deref()) == Some(node.id.as_str()))
            .map(|n| plan(inv, n))
            .collect(),
    }
}

fn hop(c: &Collect) -> Hop {
    Hop { target: c.ssh.clone(), key: c.key.clone() }
}

/// Проверки, у которых `откуда` — этот узел, в порядке инвентаря. По этому же порядку
/// сборщик возвращает результаты, так что ключ проверки восстанавливается однозначно.
fn remote_checks<'a>(inv: &'a Inventory, host: &str) -> Vec<(CheckKey, &'a Check)> {
    inv.nodes
        .iter()
        .flat_map(|n| n.checks.iter().enumerate().map(move |(i, c)| ((n.id.clone(), i), c)))
        .filter(|(_, c)| c.from.as_deref() == Some(host))
        .collect()
}

fn remote(check: &Check) -> RemoteCheck {
    let timeout_ms = probes::limit_ms(check) as u32;
    match check.kind {
        CheckKind::Tcp => {
            // Формат хост:порт уже проверен при загрузке инвентаря.
            let (host, port) = check.address.as_deref().and_then(|a| a.rsplit_once(':')).unwrap_or_default();
            RemoteCheck::Tcp { host: host.to_string(), port: port.parse().unwrap_or_default(), timeout_ms }
        }
        CheckKind::Http => RemoteCheck::Http { url: check.target(), timeout_ms },
    }
}

/// Узел и его вложенные — все, о ком этот план.
fn plan_ids(plan: &HostPlan, out: &mut Vec<String>) {
    out.push(plan.id.clone());
    for n in &plan.nested {
        plan_ids(n, out);
    }
}

/// План узла не изменился (те же цель, сборщики и проверки в том же порядке): его факты
/// можно оставить при смене инвентаря, а не ждать следующего сбора.
pub fn same_plan(old: &Inventory, new: &Inventory, host: &str) -> bool {
    let of = |inv: &Inventory| {
        let node = inv.nodes.iter().find(|n| n.id == host && n.collect.is_some())?;
        let keys: Vec<CheckKey> = remote_checks(inv, host).into_iter().map(|(k, _)| k).collect();
        Some((plan(inv, node), keys, node.collect.as_ref().and_then(|c| c.via.clone())))
    };
    of(old).is_some_and(|p| Some(p) == of(new))
}

/// Ответ сбора по цепочке → факты движка по каждому её узлу.
pub fn host_facts(
    inv: &Inventory,
    plan: &HostPlan,
    result: Result<Vec<HostReport>, PlanError>,
    now: OffsetDateTime,
    interval: Duration,
) -> Vec<(String, HostFacts)> {
    let facts = |result| HostFacts { measured_at: now, interval, result };
    match result {
        Err(e) => {
            let mut ids = Vec::new();
            plan_ids(plan, &mut ids);
            ids.into_iter().map(|id| (id, facts(Err(format!("план сбора не собран: {e}"))))).collect()
        }
        Ok(reports) => reports.into_iter().map(|r| (r.id.clone(), facts(collected(inv, r, now)))).collect(),
    }
}

fn collected(inv: &Inventory, r: HostReport, now: OffsetDateTime) -> Result<Collected, String> {
    match r.ssh {
        Outcome::Ok(()) => {}
        Outcome::Failed { error, .. } => return Err(last_line(&error, "ssh не удался")),
        Outcome::Missing => return Err("сеанс сбора не дошёл до узла".into()),
    }
    let checks = remote_checks(inv, &r.id)
        .into_iter()
        .zip(r.checks)
        .map(|((key, check), outcome)| (key, remote_result(&r.id, check, outcome, now)))
        .collect();
    Ok(Collected {
        containers: section(r.docker, "docker"),
        vms: section(r.proxmox, "proxmox"),
        peers: section(r.wireguard, "wireguard"),
        checks,
    })
}

/// `Failed` и `Missing` — «узнать не удалось», а не «пусто»: движок покажет unknown.
fn section<T>(o: Option<Outcome<T>>, name: &str) -> Result<T, String> {
    match o {
        Some(Outcome::Ok(v)) => Ok(v),
        Some(Outcome::Failed { rc, error }) => Err(format!("{} (код {rc})", last_line(&error, "ошибка"))),
        Some(Outcome::Missing) => Err("секция не получена: предел времени или обрыв сеанса".into()),
        None => Err(format!("{name} не собирается: нет в «что»")),
    }
}

/// stderr ssh бывает многострочным (предупреждения о ключах хоста); суть — в последней строке.
fn last_line(text: &str, fallback: &str) -> String {
    let line = text.lines().rev().map(str::trim).find(|l| !l.is_empty()).unwrap_or(fallback);
    line.chars().take(300).collect()
}

fn remote_result(host: &str, check: &Check, outcome: Outcome<collect::ProbeResult>, now: OffsetDateTime) -> CheckResult {
    let (ok, fact, latency_ms) = match outcome {
        Outcome::Ok(p) => match check.kind {
            CheckKind::Tcp => {
                let port = probes::port_of(check.address.as_deref().unwrap_or_default());
                let fact = if p.connected { format!("порт {port}: открыт") } else { format!("порт {port}: {}", p.fact) };
                (Some(p.connected), fact, p.latency_ms)
            }
            CheckKind::Http => {
                let path = probes::path_of(&check.target());
                match p.http_code.filter(|_| p.connected) {
                    Some(code) => {
                        let (ok, fact) = probes::http_verdict(&path, code, &probes::codes(check));
                        (Some(ok), fact, p.latency_ms)
                    }
                    None => (Some(false), format!("GET {path}: {}", p.fact), None),
                }
            }
        },
        Outcome::Failed { error, .. } => (None, format!("проверка на {host} не выполнилась: {}", last_line(&error, "ошибка")), None),
        Outcome::Missing => (None, format!("результат проверки с {host} не получен"), None),
    };
    CheckResult {
        kind: probes::result_kind(check),
        target: check.target(),
        from: Some(host.to_string()),
        ok,
        fact,
        latency_ms,
        measured_at: now,
    }
}

/// Контейнеры, которые сбор увидел, а инвентарь не описывает, — узлами на своём хосте.
/// `ожидается: любое`: что с ними должно быть, неизвестно, и красными они не станут.
pub fn undeclared(inv: &Inventory, facts: &Facts) -> Vec<Node> {
    let mut out = Vec::new();
    for host in inv.nodes.iter().filter(|n| n.collect.is_some()) {
        let Some(Ok(c)) = facts.hosts.get(&host.id).map(|h| &h.result) else { continue };
        let Ok(list) = &c.containers else { continue };
        for container in list {
            let declared = inv
                .nodes
                .iter()
                .any(|n| n.on.as_deref() == Some(host.id.as_str()) && container_matches(n, container));
            let id = format!("{}/{}", host.id, container.name);
            if declared || inv.nodes.iter().any(|n| n.id == id) {
                continue;
            }
            out.push(Node {
                id,
                title: container.name.clone(),
                kind: NodeKind::Container,
                group: host.group.clone(),
                project: None,
                hidden: false,
                on: Some(host.id.clone()),
                depends_on: Vec::new(),
                checks: Vec::new(),
                collect: None,
                container: Some(container.name.clone()),
                compose: None,
                vm: None,
                expected: Expected::Any,
                access: None,
                links: Vec::new(),
            });
        }
    }
    out
}

/// Цепочка ssh до хоста контейнера и точное имя контейнера — для `docker logs`.
pub fn log_target(inv: &Inventory, facts: &Facts, id: &str) -> Result<(Vec<Hop>, String), String> {
    let node = inv.nodes.iter().find(|n| n.id == id).ok_or_else(|| format!("узла «{id}» нет на карте"))?;
    let on = node.on.as_deref().filter(|_| node.container.is_some() || node.compose.is_some());
    let on = on.ok_or_else(|| format!("«{}» — не контейнер: логов нет", node.title))?;
    let name = match &node.container {
        Some(name) => name.clone(),
        // Имя у compose меняется между выкатами — берём из последнего сбора.
        None => {
            let Some(Ok(c)) = facts.hosts.get(on).map(|h| &h.result) else {
                return Err(format!("нет данных сбора с {on}: имя контейнера неизвестно"));
            };
            let list = c.containers.as_ref().map_err(|e| format!("docker на {on}: {e}"))?;
            let found: Vec<_> = list.iter().filter(|c| container_matches(node, c)).collect();
            // Тот же выбор, что у движка: исправный экземпляр, затем работающий, затем любой.
            let running = |c: &&&crate::engine::facts::Container| c.facts.state == "running";
            let healthy = |c: &&&crate::engine::facts::Container| running(c) && c.facts.health.as_deref() != Some("unhealthy");
            let pick = found.iter().find(healthy).or_else(|| found.iter().find(running)).or(found.first());
            pick.map(|c| c.name.clone()).ok_or_else(|| format!("контейнер «{}» не найден на {on}", node.title))?
        }
    };
    let mut hops = Vec::new();
    let mut cur = Some(on);
    while let Some(id) = cur {
        let host = inv.nodes.iter().find(|n| n.id == id);
        let c = host.and_then(|h| h.collect.as_ref()).ok_or_else(|| format!("с узла {id} ничего не собирается: зайти не через что"))?;
        hops.push(hop(c));
        cur = c.via.as_deref();
    }
    hops.reverse();
    Ok((hops, name))
}

#[cfg(test)]
mod tests;
