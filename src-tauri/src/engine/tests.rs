//! Движок на фикстурах: правила раздела 2 контракта.

use super::facts::*;
use super::*;
use crate::inventory::{parse, validate};
use time::macros::datetime;

const NOW: OffsetDateTime = datetime!(2026-10-02 12:00:00 UTC);
const INTERVAL: Duration = Duration::from_secs(30);

fn inv(yaml: &str) -> Inventory {
    let (inv, warnings) = parse(&format!("версия_схемы: 1\nузлы:\n{yaml}")).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    validate(&inv).unwrap();
    inv
}

fn result(ok: Option<bool>, fact: &str, measured_at: OffsetDateTime) -> CheckResult {
    CheckResult {
        kind: ResultKind::Tcp,
        target: "x".into(),
        from: None,
        ok,
        fact: fact.into(),
        latency_ms: None,
        measured_at,
    }
}

fn facts(local: &[(&str, Option<bool>, &str)]) -> Facts {
    Facts {
        local: local.iter().map(|(id, ok, fact)| ((id.to_string(), 0), result(*ok, fact, NOW))).collect(),
        local_interval: INTERVAL,
        ..Facts::default()
    }
}

fn host(result: Result<Collected, String>) -> HostFacts {
    HostFacts { measured_at: NOW, interval: Duration::from_secs(60), result }
}

fn container(name: &str, state: &str, exit_code: Option<i64>, oom: Option<bool>) -> Container {
    Container {
        name: name.into(),
        compose_project: None,
        compose_service: None,
        facts: ContainerFacts {
            state: state.into(),
            exit_code,
            oom_killed: oom,
            health: None,
            restart_count: None,
            started_at: None,
            finished_at: None,
            image: None,
        },
    }
}

fn collected(containers: Vec<Container>) -> Result<Collected, String> {
    Ok(Collected { containers: Ok(containers), vms: Err("не собиралось".into()), peers: Err("не собиралось".into()), checks: HashMap::new() })
}

fn eval(inv: &Inventory, facts: &Facts) -> HashMap<String, NodeState> {
    evaluate(inv, facts, &HashMap::new(), 1, NOW).into_iter().map(|s| (s.id.clone(), s)).collect()
}

const CHAIN: &str = r#"
  - {id: хост-а, название: А, вид: хост, проверки: [{вид: tcp, адрес: "a.example.com:22"}],
     сбор: {ssh: host-a, что: [docker]}}
  - {id: панель, название: Панель, вид: контейнер, на: хост-а, контейнер: panel,
     проверки: [{вид: http, url: "https://panel.example.com/health"}]}
  - {id: виджет, название: Виджет, вид: сервис, зависит_от: [панель], проверки: [{вид: tcp, адрес: "10.0.0.5:80"}]}
"#;

#[test]
fn broken_parent_and_child_both_fail_with_one_root() {
    let inv = inv(CHAIN);
    let s = eval(&inv, &facts(&[("хост-а", Some(false), "порт 22: таймаут 3 с"), ("панель", Some(false), "GET /health: таймаут 5 с"), ("виджет", Some(false), "порт 80: таймаут 3 с")]));
    assert_eq!(s["хост-а"].own, OwnStatus::Fail);
    assert!(s["хост-а"].is_root);
    assert!(s["хост-а"].blocked_by.is_empty());
    for child in ["панель", "виджет"] {
        assert_eq!(s[child].own, OwnStatus::Fail, "отказ ребёнка не скрывается");
        assert!(!s[child].is_root);
        assert_eq!(s[child].blocked_by, vec!["хост-а"], "причина одна — корень");
    }
    assert_eq!(s["панель"].fact, "GET /health: таймаут 5 с", "заголовок — наблюдаемый факт");
}

#[test]
fn two_independent_roots() {
    let inv = inv(r#"
  - {id: а, название: А, вид: хост, проверки: [{вид: tcp, адрес: "a.example.com:22"}]}
  - {id: б, название: Б, вид: хост, проверки: [{вид: tcp, адрес: "b.example.com:22"}]}
  - {id: в, название: В, вид: сервис, зависит_от: [а, б], проверки: [{вид: tcp, адрес: "c.example.com:80"}]}
"#);
    let s = eval(&inv, &facts(&[("а", Some(false), "нет"), ("б", Some(false), "нет"), ("в", None, "не выполнилась")]));
    assert!(s["а"].is_root && s["б"].is_root);
    assert_eq!(s["в"].own, OwnStatus::Unknown);
    assert_eq!(s["в"].blocked_by, vec!["а", "б"]);
}

#[test]
fn failed_collection_makes_containers_unknown_not_fail() {
    let inv = inv(r#"
  - {id: хост-а, название: А, вид: хост, сбор: {ssh: host-a, что: [docker]}}
  - {id: база, название: База, вид: контейнер, на: хост-а, контейнер: db}
  - {id: панель, название: Панель, вид: контейнер, на: хост-а, контейнер: panel,
     проверки: [{вид: http, url: "https://panel.example.com/health"}]}
"#);
    let mut f = facts(&[("панель", Some(false), "GET /health → 502, ожидался 200")]);
    f.hosts.insert("хост-а".into(), host(Err("ssh: таймаут 8 с".into())));
    let s = eval(&inv, &f);
    assert_eq!(s["хост-а"].own, OwnStatus::Unknown);
    assert_eq!(s["база"].own, OwnStatus::Unknown, "сбор не удался — не значит «контейнера нет»");
    assert_eq!(s["база"].fact, "нет данных с хост-а: ssh: таймаут 8 с");
    assert_eq!(s["база"].checks[0].ok, None);
    // Собственная сетевая проверка контейнера учитывается как обычно.
    assert_eq!(s["панель"].own, OwnStatus::Fail);
    assert!(s["панель"].is_root, "хост не отказал — он неизвестен, корень здесь");

    // Отказ одной секции сбора — тоже «узнать не удалось».
    f.hosts.insert(
        "хост-а".into(),
        host(Ok(Collected { containers: Err("rc=1".into()), vms: Err("не собиралось".into()), peers: Err("не собиралось".into()), checks: HashMap::new() })),
    );
    let s = eval(&inv, &f);
    assert_eq!(s["хост-а"].own, OwnStatus::Ok);
    assert_eq!(s["база"].own, OwnStatus::Unknown);
}

#[test]
fn old_measurement_is_stale() {
    let inv = inv(CHAIN);
    let mut f = facts(&[]);
    f.local.insert(("хост-а".into(), 0), result(Some(true), "порт 22: открыт", NOW - Duration::from_secs(91)));
    f.local.insert(("панель".into(), 0), result(Some(true), "GET /health → 200", NOW - Duration::from_secs(60)));
    let s = eval(&inv, &f);
    assert_eq!(s["хост-а"].own, OwnStatus::Stale, "старше трёх интервалов");
    assert_eq!(s["хост-а"].measured_at, Some(NOW - Duration::from_secs(91)));
    assert_eq!(s["панель"].own, OwnStatus::Ok);

    // После сна устаревает всё, что измерено до пробуждения, даже минуту назад.
    f.woke_at = Some(NOW - Duration::from_secs(1));
    let s = eval(&inv, &f);
    assert_eq!(s["панель"].own, OwnStatus::Stale);
}

#[test]
fn flapping_is_confirmed_only_after_two_cycles_in_a_row() {
    let inv = inv(CHAIN);
    let mut prev = HashMap::new();
    let mut confirmed = Vec::new();
    for (cycle, ok) in [true, false, true, false, false, false].into_iter().enumerate() {
        let at = NOW + Duration::from_secs(30 * cycle as u64);
        let mut f = facts(&[]);
        f.local.insert(("хост-а".into(), 0), result(Some(ok), "порт 22", at));
        let states = evaluate(&inv, &f, &prev, cycle as u64 + 1, at);
        prev = states.into_iter().map(|s| (s.id.clone(), s)).collect();
        confirmed.push(prev["хост-а"].confirmed);
    }
    assert_eq!(confirmed, [false, false, false, false, true, true]);
    assert_eq!(prev["хост-а"].since, Some(NOW + Duration::from_secs(90)), "с какого цикла держится fail");

    // Повторная оценка в том же цикле (сменился инвентарь) ничего не подтверждает.
    let f = facts(&[("хост-а", Some(true), "порт 22")]);
    let s1 = evaluate(&inv, &f, &prev, 7, NOW);
    let again: HashMap<_, _> = s1.into_iter().map(|s| (s.id.clone(), s)).collect();
    assert!(!evaluate(&inv, &f, &again, 7, NOW)[0].confirmed);
}

#[test]
fn node_without_checks_is_unchecked() {
    let inv = inv("  - {id: копии, название: Копии, вид: сервис}\n  - {id: а, название: А, вид: хост, проверки: [{вид: tcp, адрес: \"a:1\"}]}\n");
    let s = eval(&inv, &facts(&[]));
    assert_eq!(s["копии"].own, OwnStatus::Unchecked);
    assert_eq!(s["а"].own, OwnStatus::Unknown);
    assert_eq!(s["а"].fact, "измерений ещё не было");
}

#[test]
fn answer_from_finished_cycle_is_dropped() {
    // Так планировщик и сборщик складывают ответы: через шлюз своего источника.
    let mut gate = CycleGate::default();
    let mut f = facts(&[]);
    let record = |gate: &CycleGate, f: &mut Facts, cycle: u64, ok: bool| {
        if gate.accepts(cycle) {
            f.local.insert(("хост-а".into(), 0), result(Some(ok), if ok { "открыт" } else { "таймаут" }, NOW));
        }
    };
    record(&gate, &mut f, 1, false); // цикл 1 начался, ответ ещё в пути
    gate.finish(1); // цикл 1 закрыт по пределу времени
    record(&gate, &mut f, 2, true); // свежий ответ цикла 2
    record(&gate, &mut f, 1, false); // опоздавший ответ цикла 1
    gate.finish(2);
    assert_eq!(eval(&inv(CHAIN), &f)["хост-а"].own, OwnStatus::Ok);
    assert!(!gate.accepts(2));
}

#[test]
fn expected_state_of_container() {
    let inv = inv(r#"
  - {id: хост-а, название: А, вид: хост, сбор: {ssh: host-a, что: [docker]}}
  - {id: мигратор, название: Мигратор, вид: контейнер, на: хост-а, контейнер: migrate, ожидается: остановлен}
  - {id: мигратор-2, название: Мигратор 2, вид: контейнер, на: хост-а, контейнер: migrate-2, ожидается: остановлен}
  - {id: удалён, название: Удалён после запуска, вид: контейнер, на: хост-а, контейнер: gone, ожидается: остановлен}
  - {id: что-угодно, название: Любое, вид: контейнер, на: хост-а, контейнер: gone-too, ожидается: любое}
  - {id: панель, название: Панель, вид: контейнер, на: хост-а, контейнер: panel}
  - {id: база, название: База, вид: контейнер, на: хост-а, контейнер: db}
  - {id: кэш, название: Кэш, вид: контейнер, на: хост-а, контейнер: cache}
"#);
    let mut f = facts(&[]);
    f.hosts.insert(
        "хост-а".into(),
        host(collected(vec![
            container("migrate", "exited", Some(0), Some(false)),
            container("migrate-2", "running", None, None),
            container("db", "exited", Some(137), Some(true)),
            container("cache", "exited", Some(137), Some(false)),
        ])),
    );
    let s = eval(&inv, &f);
    assert_eq!((s["мигратор"].own, s["мигратор"].fact.as_str()), (OwnStatus::Ok, "контейнер остановлен, код 0"));
    assert_eq!((s["мигратор-2"].own, s["мигратор-2"].fact.as_str()), (OwnStatus::Ok, "сейчас запущен"));
    for id in ["удалён", "что-угодно"] {
        assert_eq!((s[id].own, s[id].fact.as_str()), (OwnStatus::Ok, "контейнера нет, так и должно быть"));
    }
    assert_eq!((s["панель"].own, s["панель"].fact.as_str()), (OwnStatus::Fail, "контейнер не найден"));
    assert_eq!(s["хост-а"].own, OwnStatus::Ok);

    // 137 — факт; «нехватка памяти» — только при OOMKilled и только в подсказках.
    assert_eq!(s["база"].fact, "контейнер остановлен, код 137");
    assert_eq!(s["база"].hints, vec!["нехватка памяти: процесс убит по OOM"]);
    assert_eq!(s["кэш"].hints, vec!["остановлен принудительно (SIGKILL)"]);
    assert_eq!(s["кэш"].container.as_ref().unwrap().exit_code, Some(137));
}

#[test]
fn remote_check_comes_from_its_host_collection() {
    let inv = inv(r#"
  - {id: хост-а, название: А, вид: хост, сбор: {ssh: host-a}}
  - {id: туннель, название: Туннель, вид: туннель, зависит_от: [хост-а],
     проверки: [{вид: tcp, адрес: "10.0.0.2:22", откуда: хост-а}]}
"#);
    let mut f = facts(&[]);
    f.hosts.insert("хост-а".into(), host(Err("сбор ещё не подключён".into())));
    let s = eval(&inv, &f);
    assert_eq!(s["туннель"].own, OwnStatus::Unknown);
    assert_eq!(s["туннель"].fact, "нет данных с хост-а: сбор ещё не подключён");

    let mut checks = HashMap::new();
    checks.insert(("туннель".to_string(), 0), result(Some(false), "порт 22: таймаут 3 с", NOW));
    f.hosts.insert("хост-а".into(), host(Ok(Collected { containers: Ok(vec![]), vms: Ok(vec![]), peers: Ok(vec![]), checks })));
    let s = eval(&inv, &f);
    assert_eq!(s["туннель"].own, OwnStatus::Fail);
    assert!(s["туннель"].is_root);
}

/// Находка ревью 5: у compose-сервиса из двух экземпляров исход зависел от порядка в docker ps.
#[test]
fn compose_service_is_ok_when_any_instance_fully_healthy() {
    let inv = inv(r#"
  - {id: хост-а, название: А, вид: хост, сбор: {ssh: host-a, что: [docker]}}
  - {id: api, название: API, вид: контейнер, на: хост-а, compose: {проект: app, сервис: backend}}
"#);
    let instance = |name: &str, health: &str| {
        let mut c = container(name, "running", None, None);
        c.compose_project = Some("app".into());
        c.compose_service = Some("backend".into());
        c.facts.health = Some(health.into());
        c
    };
    for order in [["unhealthy", "healthy"], ["healthy", "unhealthy"]] {
        let mut f = facts(&[]);
        f.hosts.insert("хост-а".into(), host(collected(vec![instance("app-backend-1", order[0]), instance("app-backend-2", order[1])])));
        assert_eq!(eval(&inv, &f)["api"].own, OwnStatus::Ok, "{order:?}");
    }
}

/// Правило ревью 8: свежий ok при недополученной части данных — ok, но неполнота видна.
#[test]
fn partial_ok_shows_what_is_missing_and_fresh_fail_still_wins() {
    let inv = inv(r#"
  - {id: хост-а, название: А, вид: хост, проверки: [{вид: tcp, адрес: "a.example.com:22"}], сбор: {ssh: host-a, что: [docker]}}
"#);
    let mut f = facts(&[("хост-а", Some(true), "порт 22: открыт")]);
    f.hosts.insert("хост-а".into(), host(Err("ssh: таймаут 8 с".into())));
    let s = &eval(&inv, &f)["хост-а"];
    assert_eq!(s.own, OwnStatus::Ok);
    assert_eq!(s.hints, ["часть данных не получена: сбор не удался: ssh: таймаут 8 с"]);
    assert!(s.checks.iter().any(|c| c.ok.is_none()), "строка с ok: null в checks");

    let mut f = facts(&[("хост-а", Some(false), "порт 22: таймаут 3 с")]);
    f.hosts.insert("хост-а".into(), host(collected(vec![])));
    assert_eq!(eval(&inv, &f)["хост-а"].own, OwnStatus::Fail, "свежий отказ любого измерения — fail");
}

/// Подтверждение — только новым измерением: сбор раз в минуту, а оценка — после каждой
/// локальной проверки, и одно старое измерение не должно подтверждать само себя.
#[test]
fn reevaluating_old_collection_does_not_confirm() {
    let inv = inv(r#"
  - {id: хост-а, название: А, вид: хост, сбор: {ssh: host-a, что: [docker]}}
  - {id: база, название: База, вид: контейнер, на: хост-а, контейнер: db}
"#);
    let mut f = facts(&[]);
    f.hosts.insert("хост-а".into(), host(collected(vec![container("db", "exited", Some(1), None)])));
    let first: HashMap<_, _> = evaluate(&inv, &f, &HashMap::new(), 1, NOW).into_iter().map(|s| (s.id.clone(), s)).collect();
    let again: HashMap<_, _> = evaluate(&inv, &f, &first, 2, NOW + Duration::from_secs(30)).into_iter().map(|s| (s.id.clone(), s)).collect();
    assert!(!again["база"].confirmed, "тот же сбор, другой цикл — не подтверждение");
    let mut next = host(collected(vec![container("db", "exited", Some(1), None)]));
    next.measured_at = NOW + Duration::from_secs(60);
    f.hosts.insert("хост-а".into(), next);
    let third = evaluate(&inv, &f, &again, 3, NOW + Duration::from_secs(60));
    assert!(third.iter().find(|s| s.id == "база").unwrap().confirmed, "новый сбор с тем же итогом — подтверждение");
}

#[test]
fn hidden_node_is_never_a_root_or_a_cause() {
    let inv = inv(r#"
  - {id: хост-а, название: А, вид: хост, скрыть: true, проверки: [{вид: tcp, адрес: "a.example.com:22"}]}
  - {id: сайт, название: Сайт, вид: сервис, зависит_от: [хост-а], проверки: [{вид: tcp, адрес: "site.example.com:443"}]}
"#);
    let s = eval(&inv, &facts(&[("хост-а", Some(false), "порт 22: таймаут 3 с"), ("сайт", Some(false), "порт 443: таймаут 3 с")]));
    assert_eq!(s["хост-а"].own, OwnStatus::Fail, "скрытый проверяется как обычно");
    assert!(!s["хост-а"].is_root);
    assert!(s["сайт"].is_root, "видимый отказ под скрытым — сам корень");
    assert!(s["сайт"].blocked_by.is_empty());
}
