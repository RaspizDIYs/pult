//! Адаптер на фикстурах: инвентарь → план, отчёт сбора → факты → состояния движка.

use super::*;
use crate::collect::{GuestKind, ProbeResult};
use crate::engine::facts::{Container, ContainerFacts, Guest};
use crate::engine::{evaluate, OwnStatus};
use crate::inventory::{parse, validate};
use std::collections::HashMap;
use time::macros::datetime;

const NOW: OffsetDateTime = datetime!(2026-10-02 12:00:00 UTC);

const INV: &str = r#"
версия_схемы: 1
узлы:
  - id: хост-а
    название: Хост А
    вид: хост
    сбор: {ssh: "deploy@a.example.com", ключ: "~/.ssh/id_a", что: [docker, wireguard]}
  - id: гипервизор
    название: Гипервизор
    вид: хост
    сбор: {ssh: "root@10.0.0.3", через: хост-а, ключ: "/root/.ssh/id_home", что: [proxmox]}
  - id: хост-б
    название: Хост Б
    вид: вм
    на: гипервизор
    вм: 100
    сбор: {ssh: "user@10.0.0.4", через: гипервизор, что: [docker]}
  - id: туннель
    название: Туннель
    вид: туннель
    зависит_от: [хост-а]
    проверки:
      - {вид: tcp, адрес: "10.0.0.2:22", откуда: хост-а}
      - {вид: http, url: "http://10.0.0.2:8080/health", откуда: хост-а, ожидать: [200, 204]}
  - {id: вм-сборки, название: Сборки, вид: вм, на: гипервизор, вм: 101}
  - {id: api, название: API, вид: контейнер, на: хост-б, compose: {проект: app, сервис: backend}}
  - {id: база, название: База, вид: контейнер, на: хост-а, контейнер: db}
"#;

fn inv() -> Inventory {
    let (inv, warnings) = parse(INV).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    validate(&inv).unwrap();
    inv
}

fn container(name: &str, service: Option<&str>, state: &str, health: Option<&str>) -> Container {
    Container {
        name: name.into(),
        compose_project: service.map(|_| "app".into()),
        compose_service: service.map(String::from),
        facts: ContainerFacts {
            state: state.into(),
            exit_code: Some(0),
            oom_killed: Some(false),
            health: health.map(String::from),
            restart_count: Some(0),
            started_at: None,
            finished_at: None,
            image: None,
        },
    }
}

fn report(id: &str) -> HostReport {
    HostReport { id: id.into(), ssh: Outcome::Ok(()), docker: None, wireguard: None, proxmox: None, checks: vec![] }
}

fn states(inv: &Inventory, facts: &Facts) -> HashMap<String, crate::engine::NodeState> {
    let mut full = inv.clone();
    full.nodes.extend(undeclared(inv, facts));
    evaluate(&full, facts, &HashMap::new(), 1, NOW).into_iter().map(|s| (s.id.clone(), s)).collect()
}

fn facts_from(inv: &Inventory, reports: Vec<HostReport>) -> Facts {
    let plan = &plans(inv)[0];
    let mut facts = Facts { local_interval: Duration::from_secs(30), ..Facts::default() };
    facts.hosts.extend(host_facts(inv, plan, Ok(reports), NOW, Duration::from_secs(60)));
    facts
}

#[test]
fn inventory_becomes_nested_plan_with_remote_checks_and_keys() {
    let plans = plans(&inv());
    assert_eq!(plans.len(), 1, "внешний хост один, остальные — через него");
    let a = &plans[0];
    assert_eq!(a.hop, Hop { target: "deploy@a.example.com".into(), key: Some("~/.ssh/id_a".into()) });
    assert_eq!(a.collectors, [collect::Collector::Docker, collect::Collector::Wireguard]);
    assert_eq!(
        a.checks,
        [
            RemoteCheck::Tcp { host: "10.0.0.2".into(), port: 22, timeout_ms: 3000 },
            RemoteCheck::Http { url: "http://10.0.0.2:8080/health".into(), timeout_ms: 5000 },
        ],
        "проверки «откуда: хост-а» выполняются в сеансе хоста А"
    );
    let hyper = &a.nested[0];
    assert_eq!((hyper.id.as_str(), hyper.hop.key.as_deref()), ("гипервизор", Some("/root/.ssh/id_home")));
    assert_eq!(hyper.nested[0].id, "хост-б", "через гипервизор — ещё уровнем ниже");
    assert!(hyper.nested[0].nested.is_empty());
}

#[test]
fn missing_section_is_unknown_and_remote_checks_map_back() {
    let inv = inv();
    let mut a = report("хост-а");
    a.docker = Some(Outcome::Missing);
    a.wireguard = Some(Outcome::Failed { rc: 1, error: "wg: permission denied".into() });
    a.checks = vec![
        Outcome::Ok(ProbeResult { connected: false, http_code: None, latency_ms: None, fact: "таймаут".into() }),
        Outcome::Ok(ProbeResult { connected: true, http_code: Some(204), latency_ms: Some(12), fact: "HTTP 204".into() }),
    ];
    let s = states(&inv, &facts_from(&inv, vec![a]));
    assert_eq!(s["база"].own, OwnStatus::Unknown, "секция не получена — не «контейнера нет»");
    assert!(s["база"].fact.contains("секция не получена"), "{}", s["база"].fact);
    assert_eq!(s["туннель"].own, OwnStatus::Fail);
    assert_eq!(s["туннель"].checks[0].fact, "порт 22: таймаут");
    assert_eq!(s["туннель"].checks[0].from.as_deref(), Some("хост-а"));
    assert_eq!((s["туннель"].checks[1].ok, s["туннель"].checks[1].fact.as_str()), (Some(true), "GET /health → 204"));
}

#[test]
fn compose_service_with_two_instances_undeclared_container_and_stopped_vm() {
    let inv = inv();
    let mut a = report("хост-а");
    a.docker = Some(Outcome::Ok(vec![container("db", None, "running", None), container("grafana-old", None, "exited", None)]));
    let mut hyper = report("гипервизор");
    hyper.proxmox = Some(Outcome::Ok(vec![
        Guest { vmid: 100, name: "b".into(), status: "running".into(), kind: GuestKind::Vm },
        Guest { vmid: 101, name: "build".into(), status: "stopped".into(), kind: GuestKind::Vm },
    ]));
    let mut b = report("хост-б");
    b.docker = Some(Outcome::Ok(vec![
        container("app-backend-1", Some("backend"), "running", Some("unhealthy")),
        container("app-backend-2", Some("backend"), "running", Some("healthy")),
    ]));
    let facts = facts_from(&inv, vec![a, hyper, b]);
    let s = states(&inv, &facts);

    assert_eq!(s["api"].own, OwnStatus::Ok, "работает хоть один исправный экземпляр");
    assert_eq!(s["вм-сборки"].own, OwnStatus::Fail);
    assert_eq!(s["вм-сборки"].fact, "ВМ 101: stopped");
    assert_eq!(s["хост-б"].own, OwnStatus::Ok);

    let extra = undeclared(&inv, &facts);
    assert_eq!(extra.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), ["хост-а/grafana-old"]);
    let ghost = &s["хост-а/grafana-old"];
    assert_eq!((ghost.own, ghost.fact.as_str()), (OwnStatus::Ok, "контейнер остановлен, код 0"));

    let (hops, name) = log_target(&inv, &facts, "api").unwrap();
    assert_eq!(name, "app-backend-2");
    let targets: Vec<&str> = hops.iter().map(|h| h.target.as_str()).collect();
    assert_eq!(targets, ["deploy@a.example.com", "root@10.0.0.3", "user@10.0.0.4"]);
}

#[test]
fn ssh_failure_and_bad_plan_make_every_host_of_chain_unknown() {
    let inv = inv();
    let mut a = report("хост-а");
    a.ssh = Outcome::Failed { rc: 255, error: "Warning: something\nssh: connect to host a.example.com port 22: Connection timed out".into() };
    let mut nested = report("хост-б");
    nested.ssh = Outcome::Missing;
    let s = states(&inv, &facts_from(&inv, vec![a, nested]));
    assert_eq!(s["база"].fact, "нет данных с хост-а: ssh: connect to host a.example.com port 22: Connection timed out");
    assert_eq!(s["api"].fact, "нет данных с хост-б: сеанс сбора не дошёл до узла");

    let plan = &plans(&inv)[0];
    let bad = host_facts(&inv, plan, Err(PlanError("недопустимое значение".into())), NOW, Duration::from_secs(60));
    assert_eq!(bad.len(), 3);
    assert!(bad.iter().all(|(_, f)| f.result.is_err()));
}
