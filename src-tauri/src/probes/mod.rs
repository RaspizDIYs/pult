//! Проверки с машины пользователя: tcp, http и ollama. Проверки с `откуда` выполняет сбор на
//! узле, здесь их нет.

pub mod ollama;

use crate::engine::facts::{CheckKey, CheckResult, ResultKind};
use crate::engine::now;
use crate::inventory::{Check, CheckKind, Node};
use std::time::{Duration, Instant};
use tokio::net::{lookup_host, TcpStream};
use tokio::task::JoinSet;
use tokio::time::timeout;

pub const INTERVAL: Duration = Duration::from_secs(30);
const TCP_TIMEOUT_MS: u64 = 3000;
const HTTP_TIMEOUT_MS: u64 = 5000;
/// Цикл не должен растягиваться дольше интервала из-за одного таймаута из инвентаря.
const MAX_TIMEOUT_MS: u64 = 20_000;

/// Все локальные проверки узлов параллельно; каждая ограничена своим таймаутом,
/// поэтому цикл длится не дольше самой долгой из них.
pub async fn run_local(nodes: &[Node]) -> Vec<(CheckKey, CheckResult)> {
    let mut set = JoinSet::new();
    for node in nodes {
        for (i, check) in node.checks.iter().enumerate().filter(|(_, c)| c.from.is_none()) {
            let key = (node.id.clone(), i);
            let check = check.clone();
            set.spawn(async move { (key, run(&check).await) });
        }
    }
    let mut out = Vec::new();
    while let Some(done) = set.join_next().await {
        match done {
            Ok(r) => out.push(r),
            Err(e) => log::warn!("проверка упала: {e}"),
        }
    }
    out
}

/// Предел проверки: из инвентаря или по умолчанию, не больше MAX_TIMEOUT_MS. Общий для
/// проверок с этой машины и с узла (`откуда`).
pub fn limit_ms(check: &Check) -> u64 {
    let default = match check.kind {
        CheckKind::Tcp => TCP_TIMEOUT_MS,
        CheckKind::Http | CheckKind::Ollama => HTTP_TIMEOUT_MS,
    };
    check.timeout_ms.unwrap_or(default).min(MAX_TIMEOUT_MS)
}

pub fn codes(check: &Check) -> Vec<u16> {
    check.expect.as_ref().map_or(vec![200], |c| c.list())
}

pub fn result_kind(check: &Check) -> ResultKind {
    match check.kind {
        CheckKind::Tcp => ResultKind::Tcp,
        CheckKind::Http => ResultKind::Http,
        CheckKind::Ollama => ResultKind::Ollama,
    }
}

pub fn port_of(addr: &str) -> &str {
    addr.rsplit_once(':').map_or("", |(_, p)| p)
}

pub fn path_of(url: &str) -> String {
    reqwest::Url::parse(url).map_or_else(|_| url.to_string(), |u| u.path().to_string())
}

/// Код ответа против `ожидать` — одинаково для проверок отсюда и с узла.
pub fn http_verdict(path: &str, code: u16, codes: &[u16]) -> (bool, String) {
    if codes.contains(&code) {
        (true, format!("GET {path} → {code}"))
    } else {
        let want: Vec<String> = codes.iter().map(u16::to_string).collect();
        (false, format!("GET {path} → {code}, ожидался {}", want.join(" или ")))
    }
}

pub async fn run(check: &Check) -> CheckResult {
    let measured_at = now();
    let target = check.target();
    let limit = Duration::from_millis(limit_ms(check));
    let start = Instant::now();
    let mut models = Vec::new();
    let (ok, fact, latency_ms) = match check.kind {
        CheckKind::Tcp | CheckKind::Http => {
            let (ok, fact, connected) = match check.kind {
                CheckKind::Tcp => tcp(&target, limit).await,
                _ => http(&target, &codes(check), limit).await,
            };
            (ok, fact, connected.then(|| start.elapsed().as_millis() as u64))
        }
        CheckKind::Ollama => {
            let v = ollama::check(&target, &check.expect_models, limit).await;
            models = v.models;
            (v.ok, v.fact, v.latency_ms)
        }
    };
    CheckResult { kind: result_kind(check), target, from: None, ok: Some(ok), fact, latency_ms, measured_at, models }
}

fn secs(d: Duration) -> String {
    format!("{} с", d.as_millis() as f64 / 1000.0)
}

/// (успех, факт, соединение установлено — тогда время ответа осмысленно).
pub(crate) async fn tcp(addr: &str, limit: Duration) -> (bool, String, bool) {
    let (host, port) = (addr.rsplit_once(':').map_or(addr, |(h, _)| h), port_of(addr));
    let attempt = async {
        // Имя разрешаем отдельно: «имя не разрешается» и «порт закрыт» — разные факты.
        let addrs: Vec<_> = match lookup_host(addr).await {
            Ok(a) => a.collect(),
            Err(_) => return (false, format!("имя {host} не разрешается"), false),
        };
        match TcpStream::connect(&addrs[..]).await {
            Ok(_) => (true, format!("порт {port}: открыт"), true),
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                (false, format!("порт {port}: соединение отклонено"), false)
            }
            Err(e) => (false, format!("порт {port}: {e}"), false),
        }
    };
    timeout(limit, attempt)
        .await
        .unwrap_or_else(|_| (false, format!("порт {port}: таймаут {}", secs(limit)), false))
}

async fn http(url: &str, codes: &[u16], limit: Duration) -> (bool, String, bool) {
    let path = path_of(url);
    let client = match client(limit) {
        Ok(c) => c,
        Err(e) => return (false, format!("GET {path}: {e}"), false),
    };
    match client.get(url).send().await {
        Ok(resp) => {
            let (ok, fact) = http_verdict(&path, resp.status().as_u16(), codes);
            (ok, fact, true)
        }
        Err(e) if chain(&e).any(|m| m.starts_with("dns error")) => (false, net_error(&e, url, limit), false),
        Err(e) => (false, format!("GET {path}: {}", net_error(&e, url, limit)), false),
    }
}

/// Свой клиент на каждую проверку: пул соединений не должен прятать упавший сервер.
/// Редиректы не следуем: проверяется ответ этого адреса, нужный код пишется в `ожидать`.
pub(crate) fn client(limit: Duration) -> Result<reqwest::Client, String> {
    // updater ставит тот же провайдер; кто первый — неважно, второй вызов ничего не делает.
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .timeout(limit)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| innermost(&e))
}

/// Почему запрос не дошёл — словами, одинаково для http, ollama и MCP по http.
pub(crate) fn net_error(e: &reqwest::Error, url: &str, limit: Duration) -> String {
    if e.is_timeout() {
        format!("нет ответа, таймаут {}", secs(limit))
    } else if chain(e).any(|m| m.starts_with("dns error")) {
        let host = reqwest::Url::parse(url).ok().and_then(|u| u.host_str().map(String::from)).unwrap_or_default();
        format!("имя {host} не разрешается")
    } else {
        innermost(e)
    }
}

fn chain<'a>(e: &'a (dyn std::error::Error + 'static)) -> impl Iterator<Item = String> + 'a {
    std::iter::successors(Some(e), |e| e.source()).map(|e| e.to_string())
}

/// У reqwest верхний текст ошибки общий («error sending request»), суть — в самом
/// глубоком источнике: «connection refused», «invalid peer certificate» и т. п.
pub(crate) fn innermost(e: &(dyn std::error::Error + 'static)) -> String {
    chain(e).last().unwrap_or_default()
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Успешный результат проверки — для тестов, которым сеть не нужна.
    pub fn ok_result(check: &Check) -> CheckResult {
        CheckResult {
            kind: ResultKind::Tcp,
            target: check.target(),
            from: None,
            ok: Some(true),
            fact: "открыт".into(),
            latency_ms: Some(1),
            measured_at: now(),
            models: Vec::new(),
        }
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
    }

    #[test]
    fn tcp_open_and_closed_port_on_loopback() {
        block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let open = listener.local_addr().unwrap();
            let (ok, fact, _) = tcp(&open.to_string(), Duration::from_secs(1)).await;
            assert!(ok, "{fact}");
            assert_eq!(fact, format!("порт {}: открыт", open.port()));
            // Закрытый порт — 1, а не только что освобождённый: его успевал занять соседний тест.
            let (ok, fact, _) = tcp("127.0.0.1:1", Duration::from_secs(1)).await;
            assert!(!ok);
            assert_eq!(fact, "порт 1: соединение отклонено");
        });
    }

    #[test]
    fn http_status_against_expected_codes() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/health", listener.local_addr().unwrap());
            tokio::spawn(async move {
                for status in ["502 Bad Gateway", "204 No Content"] {
                    let (mut s, _) = listener.accept().await.unwrap();
                    let mut buf = [0u8; 1024];
                    let _ = s.read(&mut buf).await;
                    let reply = format!("HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
                    s.write_all(reply.as_bytes()).await.unwrap();
                }
            });
            let (ok, fact, _) = http(&url, &[200], Duration::from_secs(2)).await;
            assert_eq!((ok, fact.as_str()), (false, "GET /health → 502, ожидался 200"));
            let (ok, fact, _) = http(&url, &[200, 204], Duration::from_secs(2)).await;
            assert_eq!((ok, fact.as_str()), (true, "GET /health → 204"));
        });
    }
}
