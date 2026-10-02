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
        Outcome::Ok(ProbeResult { fact: "таймаут".into(), ..ProbeResult::default() }),
        Outcome::Ok(ProbeResult { connected: true, http_code: Some(204), latency_ms: Some(12), fact: "HTTP 204".into(), bodies: vec![] }),
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

const LLM: &str = r#"
версия_схемы: 1
узлы:
  - {id: хост-а, название: Хост А, вид: хост, сбор: {ssh: "deploy@a.example.com", что: [docker]}}
  - {id: хост-б, название: Хост Б, вид: хост, сбор: {ssh: "user@10.0.0.4", через: хост-а, ключ: "/home/deploy/.ssh/id_home", что: []}}
  - id: модели
    название: Локальные модели
    вид: сервис
    проверки:
      - {вид: ollama, url: "http://10.0.0.5:11434", откуда: хост-б, ожидать_модели: [qwen3]}
  - {id: проброс, название: Проброс, вид: сервис, проверки: [{вид: ollama, url: "http://10.0.0.4:11434/", откуда: хост-а}]}
"#;

fn llm() -> Inventory {
    let (inv, warnings) = parse(LLM).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    validate(&inv).unwrap();
    inv
}

fn body(bodies: &[&str]) -> Outcome<ProbeResult> {
    Outcome::Ok(ProbeResult { connected: true, http_code: Some(200), latency_ms: Some(40), fact: "HTTP 200".into(), bodies: bodies.iter().map(|b| b.to_string()).collect() })
}

#[test]
fn remote_ollama_bodies_are_parsed_like_local_ones() {
    let inv = llm();
    let plan = &plans(&inv)[0];
    assert_eq!(plan.checks, [RemoteCheck::Ollama { url: "http://10.0.0.4:11434".into(), timeout_ms: 5000 }], "хвостовой «/» снят");
    assert_eq!(plan.nested[0].checks, [RemoteCheck::Ollama { url: "http://10.0.0.5:11434".into(), timeout_ms: 5000 }]);

    let tags = r#"{"models":[{"name":"qwen3:8b","size":5},{"name":"embed:latest","size":1}]}"#;
    let ps = r#"{"models":[{"name":"qwen3:8b","size":10,"size_vram":10}]}"#;
    let mut a = report("хост-а");
    a.checks = vec![Outcome::Failed { rc: 0, error: "tags: ответ длиннее 256 КБ, обрезан".into() }];
    let mut b = report("хост-б");
    b.checks = vec![body(&[tags, ps])];
    let s = states(&inv, &facts_from(&inv, vec![a, b]));
    let check = &s["модели"].checks[0];
    assert_eq!(s["модели"].own, OwnStatus::Ok);
    assert_eq!(check.fact, "отвечает за 40 мс · моделей: 2 · в памяти: qwen3:8b (GPU)");
    assert_eq!((check.models.as_slice(), check.from.as_deref(), check.latency_ms), (&["qwen3:8b".to_string(), "embed:latest".to_string()][..], Some("хост-б"), Some(40)));
    let json = serde_json::to_value(check).unwrap();
    assert_eq!((json["kind"].as_str(), json["models"][1].as_str()), (Some("ollama"), Some("embed:latest")));
    // Обрезанный ответ — «узнать не удалось», а не отказ сервера.
    assert_eq!(s["проброс"].own, OwnStatus::Unknown);
    assert_eq!(s["проброс"].fact, "проверка на хост-а не выполнилась: tags: ответ длиннее 256 КБ, обрезан");

    // Сервер жив, но нужной модели нет; сервер не отвечает; в ответе мусор.
    let outcome = |o: Outcome<ProbeResult>| {
        let mut b = report("хост-б");
        b.checks = vec![o];
        let s = states(&inv, &facts_from(&inv, vec![report("хост-а"), b]));
        (s["модели"].own, s["модели"].fact.clone())
    };
    assert_eq!(outcome(body(&[r#"{"models":[{"name":"embed:latest"}]}"#])), (OwnStatus::Fail, "нет модели qwen3".into()));
    let refused = Outcome::Ok(ProbeResult { fact: "соединение отклонено".into(), ..ProbeResult::default() });
    assert_eq!(outcome(refused), (OwnStatus::Fail, "не отвечает: соединение отклонено".into()));
    assert_eq!(outcome(body(&["<html>502</html>"])).1, "не отвечает: в ответе не список моделей Ollama: <html>502</html>");
}

#[test]
fn ask_plan_follows_the_chain_and_answer_names_the_broken_hop() {
    let inv = llm();
    let plan = ask_plan(&inv, "хост-б", "http://10.0.0.5:11434", "qwen3:8b").unwrap();
    assert_eq!((plan.id.as_str(), plan.collectors.len(), plan.checks.len()), ("хост-а", 0, 0), "по пути ничего не собирается");
    let leaf = &plan.nested[0];
    assert_eq!(leaf.hop, Hop { target: "user@10.0.0.4".into(), key: Some("/home/deploy/.ssh/id_home".into()) });
    assert_eq!(leaf.checks, [RemoteCheck::OllamaAsk { url: "http://10.0.0.5:11434".into(), model: "qwen3:8b".into(), timeout_ms: 60_000 }]);
    assert!(ask_plan(&inv, "модели", "http://10.0.0.5:11434", "qwen3:8b").is_err(), "с узла без «сбор» спросить нельзя");
    assert_eq!(ollama_check(&inv.nodes[2]).map(|c| c.target()), Some("http://10.0.0.5:11434".into()));
    assert!(ollama_check(&inv.nodes[0]).is_none());

    let answered = |o: Outcome<ProbeResult>| {
        let mut b = report("хост-б");
        b.checks = vec![o];
        ask_answer(Ok(vec![report("хост-а"), b]))
    };
    let mut done = body(&[r#"{"response":"","done":true,"load_duration":2500000000}"#]);
    if let Outcome::Ok(p) = &mut done {
        p.latency_ms = Some(3100);
    }
    assert_eq!(answered(done), ollama::Answer { ok: true, fact: "ответила за 3.1 с, из них загрузка в память — 2.5 с".into(), seconds: Some(3.1) });
    let mut missing = body(&[r#"{"error":"model 'qwen3:8b' not found"}"#]);
    if let Outcome::Ok(p) = &mut missing {
        p.http_code = Some(404);
    }
    assert_eq!(answered(missing).fact, "model 'qwen3:8b' not found");
    assert_eq!(answered(Outcome::Ok(ProbeResult { fact: "таймаут".into(), ..ProbeResult::default() })).fact, "таймаут");
    assert_eq!(answered(Outcome::Missing).fact, "ответ не получен: предел времени или обрыв сеанса");

    let mut down = report("хост-б");
    down.ssh = Outcome::Failed { rc: 255, error: "ssh: connect to host 10.0.0.4 port 22: Connection timed out".into() };
    let got = ask_answer(Ok(vec![report("хост-а"), down]));
    assert_eq!((got.ok, got.fact.as_str()), (false, "до узла хост-б не дойти: ssh: connect to host 10.0.0.4 port 22: Connection timed out"));
    assert!(!ask_answer(Err(PlanError("недопустимое значение".into()))).ok);
}

/// Живые серверы Ollama из своего инвентаря — тем же путём, что у приложения: цепочка ssh,
/// скрипт сбора, разбор. Только `/api/tags` и `/api/ps`:
/// `PULT_INVENTORY=каталог cargo test live_ollama -- --ignored --nocapture`
/// С `PULT_LIVE_ASK=<модель>` — ещё и одна генерация этой моделью на первом сервере (путь кнопки
/// «Спросить модель»). Она грузит модель в память и может вытеснить рабочую, поэтому модель
/// называется явно: лучше всего ту, что уже в памяти.
#[tokio::test]
#[ignore = "ходит на свои серверы: PULT_INVENTORY"]
async fn live_ollama() {
    let dir = std::path::PathBuf::from(std::env::var("PULT_INVENTORY").expect("задай PULT_INVENTORY"));
    let (inv, _) = parse(&std::fs::read_to_string(dir.join(crate::inventory::FILE_NAME)).unwrap()).unwrap();
    validate(&inv).unwrap();
    let ssh = crate::system::find("ssh");
    let mut ask = std::env::var("PULT_LIVE_ASK").ok();
    for node in &inv.nodes {
        for check in node.checks.iter().filter(|c| c.kind == CheckKind::Ollama) {
            let Some(from) = &check.from else {
                let r = probes::run(check).await;
                println!("{}: с этой машины · ok={:?} · {} · моделей {}", node.id, r.ok, r.fact, r.models.len());
                continue;
            };
            let plan = chain_plan(&inv, from, remote(check)).unwrap();
            let reports = collect::collect(ssh.as_deref(), &plan).await.unwrap();
            for r in &reports {
                println!("  ssh {}: {:?}", r.id, r.ssh);
            }
            let outcome = reports.last().unwrap().checks[0].clone();
            let sizes: Vec<(String, u64)> = match &outcome {
                Outcome::Ok(p) => {
                    println!("  тел в ответе: {}, байт: {:?}", p.bodies.len(), p.bodies.iter().map(String::len).collect::<Vec<_>>());
                    let tags: serde_json::Value = serde_json::from_str(p.bodies.first().map_or("", String::as_str)).unwrap_or_default();
                    let models = tags["models"].as_array().cloned().unwrap_or_default();
                    models.iter().map(|m| (m["name"].as_str().unwrap_or_default().to_string(), m["size"].as_u64().unwrap_or_default() >> 20)).collect()
                }
                other => {
                    println!("  секция: {other:?}");
                    Vec::new()
                }
            };
            let r = remote_result(from, check, outcome, NOW);
            println!("{}: с узла {from} · ok={:?} · {} · моделей {}", node.id, r.ok, r.fact, r.models.len());
            println!("  модели и размеры, МБ: {sizes:?}");
            if let Some(model) = ask.take() {
                assert!(r.models.contains(&model), "модели {model} нет в списке сервера");
                let plan = ask_plan(&inv, from, &check.target(), &model).unwrap();
                let started = std::time::Instant::now();
                let answer = ask_answer(collect::collect_within(ssh.as_deref(), &plan, ollama::ASK_LIMIT + Duration::from_secs(30)).await);
                println!("  вопрос модели {model}: ok={} · {} · весь путь {:.1} с", answer.ok, answer.fact, started.elapsed().as_secs_f64());
            }
        }
    }
}
