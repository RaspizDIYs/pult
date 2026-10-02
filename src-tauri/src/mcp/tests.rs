//! Обнаружение на выдуманных настройках Claude и состояние узлов. Все имена, пути и «секреты»
//! здесь придуманы; настоящие настройки читает только `live_discovery` и печатает одни числа.

use super::*;
use serde_json::json;
use time::macros::datetime;

const NOW: OffsetDateTime = datetime!(2026-10-02 12:00:00 UTC);
/// Каждое значение, которое не должно покинуть ядро, содержит эту метку.
const SECRET: &str = "SECRET";

/// Домашний каталог с настройками во всех местах, где их держит Claude.
fn home(name: &str) -> Paths {
    let home = std::env::temp_dir().join(format!("pult-mcp-home-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let (shop, blog, gone) = (home.join("work/shop"), home.join("work/blog"), home.join("work/gone"));
    for dir in [&shop, &blog] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(shop.join("server.js"), "").unwrap();
    let write = |path: PathBuf, value: Value| std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();

    write(
        home.join(".claude.json"),
        json!({
            "numStartups": 7,
            "mcpServers": {
                "wiki": {"type": "stdio", "command": "/opt/tools/bin/python", "args": ["-m", "wiki_mcp", "--api-key", "SECRET-arg-wiki"], "env": {"WIKI_TOKEN": "SECRET-env-wiki", "RETRIES": 3}}
            },
            "projects": {
                shop.to_str().unwrap(): {
                    "mcpServers": {"db": {"command": "node", "args": ["server.js", "SECRET-arg-db"], "env": {}}},
                    "enabledMcpjsonServers": ["tracker"],
                    "disabledMcpjsonServers": ["legacy"]
                },
                blog.to_str().unwrap(): {"mcpServers": {}, "disabledMcpjsonServers": []},
                // Каталога уже нет: проект остался в настройках, читать в нём нечего.
                gone.to_str().unwrap(): {"mcpServers": {"db": {"command": "node", "args": ["other.js"]}}}
            }
        }),
    );
    write(
        home.join(".mcp.json"),
        json!({"mcpServers": {
            "search": {"command": "uvx", "args": ["search-mcp"], "env": {"SEARCH_KEY": "SECRET-env-search"}},
            // То же описание, что в общих настройках: один сервер, два источника.
            "wiki": {"type": "stdio", "command": "/opt/tools/bin/python", "args": ["-m", "wiki_mcp", "--api-key", "SECRET-arg-wiki"], "env": {"WIKI_TOKEN": "SECRET-env-wiki", "RETRIES": 3}}
        }}),
    );
    write(
        shop.join(".mcp.json"),
        json!({"mcpServers": {
            "tracker": {"type": "http", "url": "https://mcp.example.com/v1/SECRET-path?key=SECRET-query", "headers": {"Authorization": "Bearer SECRET-header"}},
            "feed": {"type": "sse", "url": "http://10.0.0.7:8811/sse"},
            "legacy": {"command": "legacy-mcp"},
            "future": {"type": "quantum", "endpoint": "SECRET-unknown"},
            "broken": "SECRET-not-an-object"
        }}),
    );
    std::fs::write(blog.join(".mcp.json"), "{\"mcpServers\": {\"x\": {\"command\": \"SECRET-broken").unwrap();
    let paths = Paths::in_home(home);
    std::fs::create_dir_all(paths.desktop.parent().unwrap()).unwrap();
    write(paths.desktop.clone(), json!({"preferences": {}, "mcpServers": {"files": {"command": "npx", "args": ["-y", "files-mcp", "/tmp"]}}}));
    paths
}

fn passive(servers: &[Server], running: &[&str]) -> Vec<CheckResult> {
    servers
        .iter()
        .map(|s| {
            let (kind, ok, fact) = match (&s.transport, running.contains(&s.name.as_str())) {
                (Transport::Stdio { .. }, true) => (ResultKind::Process, Some(true), "запущен, процессов: 2"),
                (Transport::Stdio { .. }, false) => (ResultKind::Process, None, NOT_RUNNING),
                (Transport::Remote { .. }, true) => (ResultKind::Tcp, Some(true), "порт 443: открыт"),
                (Transport::Remote { .. }, false) => (ResultKind::Tcp, Some(false), "порт 443: таймаут 3 с"),
            };
            CheckResult { kind, target: "x".into(), from: None, ok, fact: fact.into(), latency_ms: None, measured_at: NOW, models: vec![] }
        })
        .collect()
}

fn real(ok: bool, fact: &str) -> CheckResult {
    CheckResult { kind: ResultKind::Mcp, target: "initialize и tools/list".into(), from: None, ok: Some(ok), fact: fact.into(), latency_ms: None, measured_at: NOW, models: vec![] }
}

#[test]
fn servers_are_found_in_every_place_claude_keeps_them() {
    let paths = home("discover");
    let (servers, errors) = discover(&paths);
    let found: Vec<(&str, &str, Vec<&str>)> = servers
        .iter()
        .map(|s| {
            let kind = match &s.transport {
                Transport::Stdio { .. } => "stdio",
                Transport::Remote { sse: true, .. } => "sse",
                Transport::Remote { sse: false, .. } => "http",
            };
            (s.name.as_str(), kind, s.sources.iter().map(String::as_str).collect())
        })
        .collect();
    assert_eq!(
        found,
        [
            ("wiki", "stdio", vec!["Claude Code: все проекты", "~/.mcp.json"]),
            ("search", "stdio", vec!["~/.mcp.json"]),
            ("db", "stdio", vec!["Claude Code: проект ~/work/gone"]),
            ("db", "stdio", vec!["Claude Code: проект ~/work/shop"]),
            ("feed", "sse", vec!["~/work/shop/.mcp.json"]),
            ("legacy", "stdio", vec!["~/work/shop/.mcp.json, выключен в настройках Claude"]),
            ("tracker", "http", vec!["~/work/shop/.mcp.json"]),
            ("files", "stdio", vec!["Claude Desktop"]),
        ],
        "незнакомый вид и запись-строка пропущены, остальное найдено"
    );
    // Битый файл одного проекта не мешает остальным; значения из него в ошибку не попадают.
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("blog") && errors[0].contains("не разобран") && !errors[0].contains(SECRET), "{errors:?}");
    // Значения не-строки доходят до процесса строками.
    let Transport::Stdio { env, .. } = &servers[0].transport else { panic!() };
    assert_eq!(env["RETRIES"], "3");
    let _ = std::fs::remove_dir_all(&paths.home);
}

#[test]
fn nothing_secret_reaches_the_interface() {
    let paths = home("secrets");
    let (servers, errors) = discover(&paths);
    let seen = passive(&servers, &["wiki", "tracker"]);
    let mut local = Local::default();
    let update = local.update(&paths, servers, errors, seen, NOW);
    assert!(update.list_changed);

    // Всё, что уходит интерфейсу: узлы, состояния, изменения.
    let sent = serde_json::to_string(&(local.views(), local.states(), &update.states)).unwrap();
    assert!(!sent.contains(SECRET), "секрет в снимке: {sent}");
    // При этом сами настройки прочитаны целиком — иначе проверка выше ничего не доказывает.
    let Some(Server { transport: Transport::Stdio { args, env, .. }, .. }) = local.server("#mcp/wiki") else { panic!() };
    assert!(args.contains(&"SECRET-arg-wiki".to_string()) && env["WIKI_TOKEN"] == "SECRET-env-wiki");

    let views = local.views();
    let info = |id: &str| views.iter().find(|v| v.id == id).unwrap_or_else(|| panic!("{id}")).mcp.clone().unwrap();
    let wiki = info("#mcp/wiki");
    assert_eq!((wiki.transport, wiki.command.as_deref(), wiki.script), ("stdio", Some("python"), None));
    assert_eq!(wiki.env_names, ["RETRIES", "WIKI_TOKEN"]);
    // Аргумент показывается, только если это существующий файл; путь — от каталога проекта.
    assert_eq!(info("#mcp/db/2").script.as_deref(), Some("~/work/shop/server.js"));
    assert_eq!(info("#mcp/db").script, None);
    let tracker = info("#mcp/tracker");
    assert_eq!((tracker.transport, tracker.host.as_deref()), ("http", Some("https://mcp.example.com")));
    assert_eq!(tracker.header_names, ["Authorization"]);
    assert_eq!(info("#mcp/feed").host.as_deref(), Some("http://10.0.0.7:8811"));

    let view = serde_json::to_value(&views[0]).unwrap();
    assert_eq!((view["kind"].as_str(), view["project"].as_str(), view["on"].is_null()), (Some("mcp"), Some("MCP"), true));
    for key in ["transport", "sources", "command", "script", "host", "envNames", "headerNames"] {
        assert!(view["mcp"].get(key).is_some(), "mcp.{key}");
    }
    let _ = std::fs::remove_dir_all(&paths.home);
}

#[test]
fn not_running_is_neutral_and_only_a_real_failure_is_a_root() {
    let paths = home("states");
    let (servers, _) = discover(&paths);
    let mut local = Local::default();
    local.update(&paths, servers.clone(), vec![], passive(&servers, &["wiki"]), NOW);
    let state = |local: &Local, id: &str| {
        let s = &local.states()[id];
        (s.own, s.is_root, s.fact.clone())
    };
    assert_eq!(state(&local, "#mcp/wiki"), (OwnStatus::Ok, false, "запущен, процессов: 2".into()));
    assert_eq!(state(&local, "#mcp/search"), (OwnStatus::Unchecked, false, NOT_RUNNING.into()), "не запущен — не отказ");
    assert_eq!(state(&local, "#mcp/tracker"), (OwnStatus::Fail, false, "порт 443: таймаут 3 с".into()), "закрытый порт — отказ, но не корень");
    assert!(local.states().values().all(|s| s.confirmed && !s.is_root));

    // Настоящая проверка не прошла: корень — и остаётся им, что бы ни показывал обход.
    let search = local.server("#mcp/search").unwrap();
    let update = local.set_real("#mcp/search", &search, real(false, "не запустился: команда «uvx» не найдена"), NOW);
    assert_eq!(update.states.len(), 1);
    assert_eq!(update.transitions.len(), 1, "переход попадает в историю");
    assert_eq!(state(&local, "#mcp/search"), (OwnStatus::Fail, true, "не запустился: команда «uvx» не найдена".into()));
    let later = NOW + Duration::from_secs(30);
    let update = local.update(&paths, servers.clone(), vec![], passive(&servers, &["wiki", "search"]), later);
    assert!(!update.list_changed);
    assert_eq!(state(&local, "#mcp/search").0, OwnStatus::Fail, "«процесс запущен» отказ рукопожатия не опровергает");
    assert_eq!(local.states()["#mcp/search"].since, Some(NOW));
    assert_eq!(local.states()["#mcp/search"].checks.len(), 2, "обе строки видны в панели");

    // Следующее нажатие прошло — отказ снят; карточка снова показывает то, что видно сейчас.
    local.set_real("#mcp/search", &search, real(true, "отвечает · инструментов: 4"), later);
    assert_eq!(state(&local, "#mcp/search"), (OwnStatus::Ok, false, "запущен, процессов: 2".into()));

    // Описание сервера в настройках изменилось — прежний итог относится к другому серверу.
    local.set_real("#mcp/search", &search, real(false, "нет ответа на initialize за 20 с"), later);
    let mut edited = servers.clone();
    let i = edited.iter().position(|s| s.name == "search").unwrap();
    edited[i].transport = Transport::Stdio { command: "uvx".into(), args: vec!["search-mcp@2".into()], env: BTreeMap::new() };
    let update = local.update(&paths, edited.clone(), vec![], passive(&edited, &[]), later);
    assert!(update.list_changed);
    assert_eq!(state(&local, "#mcp/search"), (OwnStatus::Unchecked, false, NOT_RUNNING.into()));
    // Итог проверки сервера, которого за время проверки не стало или который сменился, не пишется.
    local.set_real("#mcp/search", &search, real(false, "устаревший итог"), later);
    assert_eq!(state(&local, "#mcp/search").0, OwnStatus::Unchecked);
    let _ = std::fs::remove_dir_all(&paths.home);
}

#[test]
fn process_is_matched_by_value_arguments() {
    let args = |list: &[&str]| list.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    let node = args(&["/srv/mcp/tracker/index.js"]);
    assert!(is_process_of("/opt/homebrew/bin/node /srv/mcp/tracker/index.js", "node", &node));
    assert!(!is_process_of("node /srv/mcp/other/index.js", "node", &node));
    // Запускалка подменяет команду и флаги, значение остаётся.
    assert!(is_process_of("npm exec files-mcp /tmp", "npx", &args(&["-y", "files-mcp", "/tmp"])));
    assert!(is_process_of("/opt/tools/bin/python3.13 -m wiki_mcp", "python", &args(&["-m", "wiki_mcp"])));
    assert!(!is_process_of("python -m other_mcp", "python", &args(&["-m", "wiki_mcp"])));
    // Без аргументов-значений — по имени самой команды.
    assert!(is_process_of("/usr/local/bin/legacy-mcp --stdio", "legacy-mcp", &args(&["--stdio"])));
    assert!(!is_process_of("vim legacy-mcp.md", "legacy-mcp", &[]));
}

/// Настоящие настройки этой машины: печатает только числа. Каталог задаётся явно, потому что
/// окно разработки и тесты работают с чужим `HOME`.
/// `PULT_LIVE_HOME=$HOME cargo test live_discovery -- --ignored --nocapture`
#[tokio::test]
#[ignore = "читает настоящие настройки Claude"]
async fn live_discovery() {
    let home = std::env::var_os("PULT_LIVE_HOME").expect("задай PULT_LIVE_HOME");
    let paths = Paths::in_home(PathBuf::from(home));
    let (servers, errors) = discover(&paths);
    let seen = scan(&servers).await;
    let count = |kind: &str| servers.iter().filter(|s| s.info(&paths.home).transport == kind).count();
    println!("серверов: {} (stdio {}, http {}, sse {}), файлов с ошибкой: {}", servers.len(), count("stdio"), count("http"), count("sse"), errors.len());
    let stdio_up = seen.iter().filter(|r| r.kind == ResultKind::Process && r.ok == Some(true)).count();
    let ports = seen.iter().filter(|r| r.kind == ResultKind::Tcp).count();
    let ports_up = seen.iter().filter(|r| r.kind == ResultKind::Tcp && r.ok == Some(true)).count();
    println!("stdio сейчас запущено: {stdio_up}; адресов http/sse доступно: {ports_up} из {ports}");
    // По серверам — без имён: сколько источников слилось в один узел и что видно пассивно.
    let facts: Vec<String> = servers.iter().zip(&seen).map(|(s, r)| format!("{}×{}", s.sources.len(), r.fact.rsplit(' ').next().unwrap_or_default())).collect();
    println!("источников × последнее слово факта: {facts:?}");
    let mut local = Local::default();
    local.update(&paths, servers, errors, seen, crate::engine::now());
    let own = |own| local.states().values().filter(|s| s.own == own).count();
    println!("узлов: {}; ok {}, не запущен {}, отказ {}", local.views().len(), own(OwnStatus::Ok), own(OwnStatus::Unchecked), own(OwnStatus::Fail));
}

/// «Проверить по-настоящему» на настоящих серверах этой машины, названных явно (запускается
/// вторая копия сервера — выбирай те, у кого нет внешних побочных эффектов):
/// `PULT_LIVE_HOME=$HOME PULT_LIVE_PROBE=имя,имя cargo test live_probe -- --ignored --nocapture`
#[tokio::test]
#[ignore = "запускает настоящие MCP-серверы: PULT_LIVE_PROBE"]
async fn live_probe() {
    let paths = Paths::in_home(PathBuf::from(std::env::var_os("PULT_LIVE_HOME").expect("задай PULT_LIVE_HOME")));
    let names = std::env::var("PULT_LIVE_PROBE").expect("задай PULT_LIVE_PROBE");
    let (servers, _) = discover(&paths);
    let running = |seen: &[CheckResult]| seen.iter().map(|r| r.fact.rsplit(' ').next().unwrap_or_default().to_string()).collect::<Vec<_>>();
    let before = running(&scan(&servers).await);
    for (i, name) in names.split(',').enumerate() {
        let server = servers.iter().find(|s| s.name == name).expect("сервера с таким именем в настройках нет");
        let started = std::time::Instant::now();
        let probe = handshake::probe(server, handshake::LIMIT).await;
        println!("сервер {} ({}): ok={} · {} · {:.1} с", i + 1, server.info(&paths.home).transport, probe.ok, probe.fact, started.elapsed().as_secs_f64());
    }
    // Вторая копия не должна остаться жить: число процессов то же, что до проверки.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let after = running(&scan(&servers).await);
    println!("процессов до и после: {}", if before == after { "без изменений".to_string() } else { format!("{before:?} → {after:?}") });
}
