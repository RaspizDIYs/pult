//! «Проверить по-настоящему» — рукопожатие MCP с сервером, только по кнопке.
//!
//! - stdio: сервер запускается так, как описан в настройках, получает `initialize`,
//!   `notifications/initialized` и `tools/list`, после чего гасится вместе с потомками.
//!   На эти секунды у сервера появляется вторая копия — поэтому цикл этого не делает.
//! - http: `initialize` по адресу с заголовками из настроек; sse: открывается ли поток событий.
//!
//! Всё, что сервер написал сам (stderr, посторонний вывод, тело ошибки), перед показом проходит
//! `mask`: значения `env`, заголовков и секретных флагов заменяются на `***`.

use super::{Server, Transport};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// Предел всего рукопожатия: `npx` и `uvx` при первом запуске ещё и качают пакет.
pub const LIMIT: Duration = Duration::from_secs(20);
/// Сколько ждать, пока сервер сам выйдет по концу ввода, прежде чем убить.
const GRACE: Duration = Duration::from_secs(2);
/// `tools/list` большого сервера — сотни килобайт одной строкой.
const MAX_LINE: u64 = 4 << 20;
const MAX_STDERR: usize = 64 << 10;

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"pult","version":"1"}}}"#;
const INITIALIZED: &str = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
const TOOLS: &str = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;

/// Итог проверки — так же он уходит в интерфейс.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Probe {
    pub ok: bool,
    /// «отвечает · инструментов: 12» либо понятная ошибка.
    pub fact: String,
}

pub async fn probe(server: &Server, limit: Duration) -> Probe {
    let result = match &server.transport {
        Transport::Stdio { command, args, env } => stdio(command, args, env, server.cwd.as_deref(), limit).await,
        Transport::Remote { sse, url, headers } => remote(*sse, url, headers, limit).await,
    };
    let secrets = secrets(server);
    match result {
        Ok(fact) => Probe { ok: true, fact: mask(&fact, &secrets) },
        Err(fact) => Probe { ok: false, fact: mask(&fact, &secrets) },
    }
}

/// Строки, которые нельзя показывать, длинные первыми: иначе секрет, входящий в другой, оставил
/// бы от того хвост. Короче восьми символов не маскируем: это «1», «true» и «Bearer», а не ключи.
fn secrets(server: &Server) -> Vec<String> {
    let mut out: Vec<String> = match &server.transport {
        Transport::Stdio { args, env, .. } => {
            // Значение флага с говорящим именем (`--api-key X`, `--token=X`). Прочие аргументы —
            // скрипт, пакет, модуль — остаются: без них ошибку запуска не понять.
            let secret_flag = |flag: &str| {
                let flag = flag.to_lowercase();
                flag.starts_with('-') && ["key", "token", "secret", "pass", "auth", "cred"].iter().any(|w| flag.contains(w))
            };
            let flags = args.iter().enumerate().filter_map(|(i, arg)| match arg.split_once('=') {
                Some((flag, value)) if secret_flag(flag) => Some(value.to_string()),
                _ if i > 0 && secret_flag(&args[i - 1]) && !args[i - 1].contains('=') => Some(arg.clone()),
                _ => None,
            });
            env.values().cloned().chain(flags).collect()
        }
        Transport::Remote { url, headers, .. } => {
            // «Bearer abc» — и целиком, и без слова «Bearer».
            let headers = headers.values().flat_map(|v| std::iter::once(v.as_str()).chain(v.split_whitespace())).map(String::from);
            let query = reqwest::Url::parse(url).map(|u| u.query_pairs().map(|(_, v)| v.into_owned()).collect::<Vec<_>>()).unwrap_or_default();
            headers.chain(query).collect()
        }
    };
    out.retain(|s| s.chars().count() >= 8);
    out.sort_by_key(|s| std::cmp::Reverse(s.len()));
    out
}

fn mask(text: &str, secrets: &[String]) -> String {
    secrets.iter().fold(text.to_string(), |text, secret| text.replace(secret.as_str(), "***"))
}

/// Кусок чужого вывода для строки факта: в одну строку, не длиннее `max` символов, с конца.
fn tail(text: &str, max: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let skip = text.chars().count().saturating_sub(max);
    if skip == 0 { text } else { format!("…{}", text.chars().skip(skip).collect::<String>()) }
}

// ───────────── stdio ─────────────

/// Чем кончился разговор, если не ответом.
enum Stop {
    /// Процесс закрыл вывод или не принял ввод.
    Gone,
    /// Сервер ответил ошибкой JSON-RPC.
    Rpc(String),
}

async fn stdio(command: &str, args: &[String], env: &BTreeMap<String, String>, cwd: Option<&Path>, limit: Duration) -> Result<String, String> {
    let name = Path::new(command).file_name().map_or(command.into(), |f| f.to_string_lossy());
    let mut cmd = Command::new(command);
    cmd.args(args).envs(env).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    if let Some(dir) = cwd.filter(|d| d.is_dir()) {
        cmd.current_dir(dir);
    }
    #[cfg(unix)]
    {
        // Своя группа процессов: по ней потом гасится сервер вместе с потомками.
        cmd.process_group(0);
        if !env.contains_key("PATH") {
            if let Some(path) = user_path().await {
                cmd.env("PATH", path);
            }
        }
    }
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let mut child = cmd.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!("не запустился: команда «{name}» не найдена"),
        _ => format!("не запустился: {e}"),
    })?;
    let pid = child.id();
    let mut stdin = child.stdin.take().expect("stdin задан как piped");
    let stdout = child.stdout.take().expect("piped");
    let stderr = Arc::new(Mutex::new(Vec::new()));
    let mut err_task = tokio::spawn(pump(child.stderr.take().expect("piped"), stderr.clone()));

    // Запросы пишутся в stdout сервера строками JSON; что туда попало кроме них и на каком шаге
    // встали, нужно и после предела времени — поэтому снаружи, а не в результате.
    let mut noise = None;
    let mut step = "initialize";
    let talked = tokio::time::timeout(limit, talk(&mut stdin, stdout, &mut noise, &mut step)).await;

    // Конец ввода — штатный сигнал серверу stdio. Не вышел сам — гасим; потомков гасим в любом
    // случае: запускалки (`npx`, `uvx`) оставляют настоящий сервер внуком.
    drop(stdin);
    let exit = tokio::time::timeout(GRACE, child.wait()).await.ok().and_then(Result::ok);
    kill_tree(&mut child, pid).await;
    if tokio::time::timeout(Duration::from_secs(1), &mut err_task).await.is_err() {
        err_task.abort();
    }

    let fact = match talked {
        Ok(Ok(fact)) => return Ok(fact),
        Ok(Err(Stop::Rpc(e))) => return Err(format!("{step}: сервер ответил ошибкой: {e}")),
        Ok(Err(Stop::Gone)) => {
            let code = exit.and_then(|s| s.code()).map(|c| format!(" с кодом {c}")).unwrap_or_default();
            format!("процесс завершился{code}, не ответив на {step}")
        }
        Err(_) => format!("нет ответа на {step} за {} с", limit.as_secs()),
    };
    let noise = noise.map(|n| format!("; в stdout не JSON-RPC: «{n}»")).unwrap_or_default();
    let stderr = String::from_utf8_lossy(&stderr.lock().unwrap_or_else(|e| e.into_inner())).into_owned();
    let stderr = if stderr.trim().is_empty() { String::new() } else { format!("; stderr: {}", tail(&stderr, 400)) };
    Err(format!("{fact}{noise}{stderr}"))
}

async fn talk(stdin: &mut ChildStdin, stdout: ChildStdout, noise: &mut Option<String>, step: &mut &'static str) -> Result<String, Stop> {
    let mut out = BufReader::new(stdout);
    send(stdin, INITIALIZE).await?;
    reply(&mut out, 1, noise).await?;
    *step = "tools/list";
    send(stdin, INITIALIZED).await?;
    send(stdin, TOOLS).await?;
    match reply(&mut out, 2, noise).await {
        Ok(result) => {
            let tools = result.get("tools").and_then(Value::as_array).map_or(0, Vec::len);
            // Список постраничный; вторую страницу не читаем — хватит знать, что их больше.
            let more = if result.get("nextCursor").is_some_and(|c| !c.is_null()) { "+" } else { "" };
            Ok(format!("отвечает · инструментов: {tools}{more}"))
        }
        // Сервер без инструментов (только ресурсы или подсказки) отвечает ошибкой — он при этом жив.
        Err(Stop::Rpc(e)) => Ok(format!("отвечает · инструментов нет ({e})")),
        Err(stop) => Err(stop),
    }
}

async fn send(stdin: &mut ChildStdin, message: &str) -> Result<(), Stop> {
    let write = async {
        stdin.write_all(message.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await
    };
    write.await.map_err(|_| Stop::Gone)
}

/// Ответ на запрос `id`. Уведомления и встречные запросы сервера пропускаются; строка, которая
/// вообще не JSON (логи, баннер), запоминается — это частая причина «Claude не видит сервер».
async fn reply(out: &mut (impl AsyncBufRead + Unpin), id: u64, noise: &mut Option<String>) -> Result<Value, Stop> {
    let mut line = Vec::new();
    loop {
        line.clear();
        // Предел длины: строка без перевода не должна съесть память.
        let read = (&mut *out).take(MAX_LINE).read_until(b'\n', &mut line).await;
        if !matches!(read, Ok(n) if n > 0) {
            return Err(Stop::Gone);
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim();
        match serde_json::from_str::<Value>(text) {
            Ok(message) if message.get("id").and_then(Value::as_u64) == Some(id) => {
                return match message.get("error") {
                    Some(e) => Err(Stop::Rpc(e.get("message").and_then(Value::as_str).unwrap_or("без текста").to_string())),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
            }
            Ok(message) if message.is_object() => {}
            _ if text.is_empty() => {}
            _ => {
                noise.get_or_insert_with(|| tail(text, 120));
            }
        }
    }
}

/// Читает поток до конца, но хранит не больше `MAX_STDERR` — последние байты: причина падения
/// обычно в конце.
async fn pump(mut src: impl tokio::io::AsyncRead + Unpin, sink: Arc<Mutex<Vec<u8>>>) {
    let mut chunk = [0u8; 8192];
    while let Ok(n) = src.read(&mut chunk).await {
        if n == 0 {
            break;
        }
        let mut buf = sink.lock().unwrap_or_else(|e| e.into_inner());
        buf.extend_from_slice(&chunk[..n]);
        let extra = buf.len().saturating_sub(MAX_STDERR);
        buf.drain(..extra);
    }
}

async fn kill_tree(child: &mut Child, pid: Option<u32>) {
    if let Some(pid) = pid {
        // Unix: сервер — лидер своей группы, сигнал идёт всей группе. Windows: дерево по pid.
        let mut killer = if cfg!(windows) {
            let mut c = Command::new("taskkill");
            c.args(["/PID", &pid.to_string(), "/T", "/F"]);
            c
        } else {
            let mut c = Command::new("kill");
            c.args(["-KILL", "--", &format!("-{pid}")]);
            c
        };
        killer.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        #[cfg(windows)]
        killer.creation_flags(0x0800_0000);
        let _ = killer.status().await;
    }
    // Заодно дожидается выхода: зомби не остаётся.
    let _ = child.kill().await;
}

/// `PATH` как в терминале пользователя. Приложение, открытое из Finder или автозапуском,
/// получает урезанный `PATH` без `node` и `uvx`: «команда не найдена» была бы ложным отказом —
/// Claude те же серверы запускает из терминала. Оболочку входа спрашиваем один раз.
// ponytail: fish печатает $PATH через пробел — у него останется PATH приложения; разбирать, когда понадобится
#[cfg(unix)]
async fn user_path() -> Option<&'static str> {
    static PATH: tokio::sync::OnceCell<Option<String>> = tokio::sync::OnceCell::const_new();
    let ask = || async {
        let mut cmd = Command::new(std::env::var_os("SHELL")?);
        cmd.args(["-ilc", r#"printf '\n@@path %s\n' "$PATH""#]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
        let out = tokio::time::timeout(Duration::from_secs(5), cmd.output()).await.ok()?.ok()?;
        String::from_utf8_lossy(&out.stdout).lines().find_map(|l| l.strip_prefix("@@path ").filter(|p| p.contains('/')).map(String::from))
    };
    PATH.get_or_init(ask).await.as_deref()
}

// ───────────── http и sse ─────────────

async fn remote(sse: bool, url: &str, headers: &BTreeMap<String, String>, limit: Duration) -> Result<String, String> {
    let client = crate::probes::client(limit)?;
    let mut request = match sse {
        true => client.get(url).header("accept", "text/event-stream"),
        false => client.post(url).header("accept", "application/json, text/event-stream").header("content-type", "application/json").body(INITIALIZE),
    };
    for (name, value) in headers {
        request = request.header(name.as_str(), value.as_str());
    }
    let mut resp = request.send().await.map_err(|e| crate::probes::net_error(&e, url, limit))?;
    let code = resp.status().as_u16();
    let events = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).is_some_and(|t| t.starts_with("text/event-stream"));
    // ponytail: у sse проверяется только то, что поток событий открылся; полное рукопожатие
    // (событие endpoint → POST) — когда появится сервер, которому этого мало
    if sse && code == 200 && events {
        return Ok("отвечает: поток событий открыт".into());
    }
    // Ответ — JSON целиком либо поток событий, который сервер держит открытым: читаем, пока не
    // увидим ответ на initialize, но не больше предела.
    let mut body = Vec::new();
    let mut found = None;
    while found.is_none() && body.len() < MAX_STDERR {
        match resp.chunk().await {
            Ok(Some(chunk)) => body.extend_from_slice(&chunk),
            _ => break,
        }
        found = rpc_reply(&String::from_utf8_lossy(&body));
    }
    let text = String::from_utf8_lossy(&body);
    let detail = if text.trim().is_empty() { String::new() } else { format!(": {}", tail(&text, 300)) };
    match found {
        Some(message) if !sse && code == 200 => match message.get("error") {
            Some(e) => Err(format!("initialize: сервер ответил ошибкой: {}", e.get("message").and_then(Value::as_str).unwrap_or("без текста"))),
            None => {
                let info = message.pointer("/result/serverInfo");
                let field = |key: &str| info.and_then(|i| i.get(key)).and_then(Value::as_str).unwrap_or_default().to_string();
                Ok(format!("отвечает · сервер {} {}", field("name"), field("version")).trim_end().to_string())
            }
        },
        _ => Err(format!("HTTP {code}{detail}")),
    }
}

/// Ответ на `initialize` (id 1) в теле: само тело, либо строка потока событий (`data: {…}`).
fn rpc_reply(body: &str) -> Option<Value> {
    std::iter::once(body)
        .chain(body.lines().map(|l| l.strip_prefix("data:").unwrap_or(l)))
        .filter_map(|text| serde_json::from_str::<Value>(text.trim()).ok())
        .find(|message| message.get("id").and_then(Value::as_u64) == Some(1))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const TOKEN: &str = "sk-test-SECRET-0123456789";

    /// Крошечный сервер MCP на sh; первый аргумент — как себя вести.
    const FAKE: &str = r#"#!/bin/sh
case $1 in
  crash) echo "fatal: no access with key $FAKE_KEY" >&2; exit 3 ;;
  garbage) echo "Server listening on stdio, key $FAKE_KEY" ;;
  silent) sleep 300 & echo $! > "$2"; wait ;;
esac
while IFS= read -r line; do
  case $1:$line in
    garbage:*) ;;
    *'"method":"initialize"'*) echo 'log line before the answer'; echo '{"jsonrpc":"2.0","method":"notifications/message","params":{}}'
      echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"1"}}}' ;;
    notools:*'"method":"tools/list"'*) echo '{"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"Method not found"}}' ;;
    *'"method":"tools/list"'*) echo '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"a","inputSchema":{}},{"name":"b","inputSchema":{}}]}}' ;;
  esac
done
"#;

    fn fake(name: &str, mode: &[&str]) -> (PathBuf, Server) {
        let dir = std::env::temp_dir().join(format!("pult-mcp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("server.sh"), FAKE).unwrap();
        let mut args = vec![dir.join("server.sh").display().to_string()];
        args.extend(mode.iter().map(|m| m.to_string()));
        // PATH задан явно: без него проверка спросила бы оболочку входа того, кто гоняет тесты.
        let env = BTreeMap::from([("FAKE_KEY".to_string(), TOKEN.to_string()), ("PATH".to_string(), std::env::var("PATH").unwrap_or_default())]);
        let transport = Transport::Stdio { command: "/bin/sh".into(), args, env };
        (dir.clone(), Server { name: "fake".into(), sources: vec![], cwd: Some(dir), transport })
    }

    fn alive(pid: &str) -> bool {
        std::process::Command::new("kill").args(["-0", pid]).stderr(Stdio::null()).status().unwrap().success()
    }

    #[tokio::test]
    async fn answering_server_reports_its_tools() {
        let (dir, server) = fake("ok", &["ok"]);
        // Строка лога и уведомление перед ответом рукопожатию не мешают.
        assert_eq!(probe(&server, LIMIT).await, Probe { ok: true, fact: "отвечает · инструментов: 2".into() });
        let (dir2, server) = fake("notools", &["notools"]);
        assert_eq!(probe(&server, LIMIT).await, Probe { ok: true, fact: "отвечает · инструментов нет (Method not found)".into() });
        let _ = (std::fs::remove_dir_all(dir), std::fs::remove_dir_all(dir2));
    }

    #[tokio::test]
    async fn silent_server_hits_the_limit_and_dies_with_its_children() {
        let (dir, _) = fake("silent", &[]);
        let pid_file = dir.join("child.pid");
        let (_, server) = fake("silent", &["silent", &pid_file.display().to_string()]);
        let got = probe(&server, Duration::from_secs(2)).await;
        assert_eq!(got, Probe { ok: false, fact: "нет ответа на initialize за 2 с".into() });
        // Потомок сервера жил бы ещё пять минут: его гасит сигнал группе.
        let child = std::fs::read_to_string(&pid_file).unwrap().trim().to_owned();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while alive(&child) && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(!alive(&child), "потомок сервера пережил проверку");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn garbage_and_crash_are_explained_without_secrets() {
        let (dir, server) = fake("garbage", &["garbage"]);
        let got = probe(&server, Duration::from_secs(2)).await;
        assert_eq!(got, Probe { ok: false, fact: "нет ответа на initialize за 2 с; в stdout не JSON-RPC: «Server listening on stdio, key ***»".into() });

        let (dir2, server) = fake("crash", &["crash"]);
        let got = probe(&server, LIMIT).await;
        assert_eq!(got, Probe { ok: false, fact: "процесс завершился с кодом 3, не ответив на initialize; stderr: fatal: no access with key ***".into() });

        let missing = Server { name: "x".into(), sources: vec![], cwd: None, transport: Transport::Stdio { command: "/nonexistent/pult-no-such-server".into(), args: vec![], env: BTreeMap::from([("PATH".into(), "/bin".into())]) } };
        assert_eq!(probe(&missing, LIMIT).await.fact, "не запустился: команда «pult-no-such-server» не найдена");
        let _ = (std::fs::remove_dir_all(dir), std::fs::remove_dir_all(dir2));
    }

    #[test]
    fn secret_flag_values_headers_and_query_are_masked() {
        let stdio = Server {
            name: "x".into(),
            sources: vec![],
            cwd: None,
            transport: Transport::Stdio {
                command: "node".into(),
                args: ["server.js", "--api-key", "key-AAAAAA", "--token=tok-BBBBBB", "--port", "808080"].map(String::from).to_vec(),
                env: BTreeMap::from([("A".into(), "env-CCCCCC".into()), ("DEBUG".into(), "1".into())]),
            },
        };
        let text = "node server.js 1 key-AAAAAA tok-BBBBBB env-CCCCCC 808080";
        assert_eq!(mask(text, &secrets(&stdio)), "node server.js 1 *** *** *** 808080");
        let remote = Server {
            name: "x".into(),
            sources: vec![],
            cwd: None,
            transport: Transport::Remote {
                sse: false,
                url: "https://mcp.example.com/mcp?api_key=qry-DDDDDD".into(),
                headers: BTreeMap::from([("Authorization".into(), "Bearer hdr-EEEEEE".into())]),
            },
        };
        assert_eq!(mask("bad token hdr-EEEEEE (Bearer hdr-EEEEEE) qry-DDDDDD", &secrets(&remote)), "bad token *** (***) ***");
    }

    /// http: заголовки из настроек доходят до сервера; ответ — JSON или поток событий.
    #[tokio::test]
    async fn http_initialize() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        tokio::spawn(async move {
            loop {
                let (mut s, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 8192];
                let n = s.read(&mut buf).await.unwrap();
                let head = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                let (status, kind, body) = if !head.contains("authorization: bearer hdr-eeeeee") {
                    ("401 Unauthorized", "application/json", r#"{"error":"invalid_token"}"#.to_string())
                } else if head.contains("x-mode: events") {
                    ("200 OK", "text/event-stream", format!("event: message\ndata: {}\n\n", r#"{"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"fake","version":"2.1"}}}"#))
                } else {
                    ("200 OK", "application/json", r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"wrong key hdr-EEEEEE"}}"#.to_string())
                };
                let reply = format!("HTTP/1.1 {status}\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
                s.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        let server = |headers: &[(&str, &str)]| Server {
            name: "x".into(),
            sources: vec![],
            cwd: None,
            transport: Transport::Remote { sse: false, url: url.clone(), headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() },
        };
        let auth = ("Authorization", "Bearer hdr-EEEEEE");
        assert_eq!(probe(&server(&[auth, ("X-Mode", "events")]), LIMIT).await, Probe { ok: true, fact: "отвечает · сервер fake 2.1".into() });
        assert_eq!(probe(&server(&[auth]), LIMIT).await, Probe { ok: false, fact: "initialize: сервер ответил ошибкой: wrong key ***".into() });
        assert_eq!(probe(&server(&[]), LIMIT).await, Probe { ok: false, fact: r#"HTTP 401: {"error":"invalid_token"}"#.into() });
    }
}
