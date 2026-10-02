//! Сервер Ollama: `GET /api/tags` (какие модели есть) и `GET /api/ps` (что сейчас в памяти).
//! Генерацию цикл не запускает: она грузит модель в видеопамять и мешает рабочим запросам.
//! Настоящий запрос к модели — только по кнопке «Спросить модель» (`ask`).
//!
//! Разбор ответов — чистые функции: ими же разбираются тела, которые скрипт сбора принёс с узла.

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// Предел тела ответа: список в сотни моделей укладывается, а сошедший с ума сервер память не
/// съест. Тот же предел у скрипта сбора (`head -c`).
pub const MAX_BODY: usize = 256 << 10;
/// Предел одной генерации по кнопке: холодной модели нужно время на загрузку в память.
pub const ASK_LIMIT: Duration = Duration::from_secs(60);

#[derive(Deserialize)]
struct Models {
    models: Vec<Model>,
}

#[derive(Deserialize)]
struct Model {
    name: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    size_vram: u64,
}

#[derive(Deserialize)]
struct Generated {
    response: Option<String>,
    error: Option<String>,
    /// Наносекунды, как отдаёт сервер.
    #[serde(default)]
    load_duration: u64,
}

pub struct Verdict {
    pub ok: bool,
    pub fact: String,
    /// Имена моделей сервера; пусто, если список не получен.
    pub models: Vec<String>,
    /// Время ответа `/api/tags`; есть, только когда список разобран.
    pub latency_ms: Option<u64>,
}

/// Итог проверки. `tags` — код и тело `/api/tags` либо причина, по которой ответа нет;
/// `ps` — тело `/api/ps`, если оно получено: без него сервер всё равно «отвечает».
pub fn verdict(tags: Result<(u16, &str), String>, ps: Option<&str>, latency_ms: Option<u64>, expect: &[String]) -> Verdict {
    let fail = |fact: String| Verdict { ok: false, fact, models: Vec::new(), latency_ms: None };
    let (code, body) = match tags {
        Ok(reply) => reply,
        Err(e) => return fail(format!("не отвечает: {e}")),
    };
    if code != 200 {
        let detail = server_error(body).map(|e| format!(": {e}")).unwrap_or_default();
        return fail(format!("не отвечает: HTTP {code}{detail}"));
    }
    let Ok(tags) = serde_json::from_str::<Models>(body) else {
        return fail(format!("не отвечает: в ответе не список моделей Ollama: {}", clip(body, 80)));
    };
    let models: Vec<String> = tags.models.into_iter().map(|m| m.name).collect();
    let missing: Vec<&str> = expect.iter().filter(|want| !models.iter().any(|m| is_model(m, want))).map(String::as_str).collect();
    if !missing.is_empty() {
        let what = if missing.len() == 1 { "нет модели" } else { "нет моделей" };
        return Verdict { ok: false, fact: format!("{what} {}", missing.join(", ")), models, latency_ms };
    }
    let loaded = match ps.and_then(|body| serde_json::from_str::<Models>(body).ok()) {
        None => "неизвестно".to_string(),
        Some(ps) if ps.models.is_empty() => "ничего".to_string(),
        Some(ps) => ps.models.iter().map(|m| format!("{} ({})", m.name, placement(m))).collect::<Vec<_>>().join(", "),
    };
    let took = latency_ms.map(|ms| format!(" за {ms} мс")).unwrap_or_default();
    Verdict { ok: true, fact: format!("отвечает{took} · моделей: {} · в памяти: {loaded}", models.len()), models, latency_ms }
}

/// `qwen3` в `ожидать_модели` — любая метка этой модели (`qwen3:8b`); с меткой — точное имя.
fn is_model(name: &str, want: &str) -> bool {
    name == want || (!want.contains(':') && name.split(':').next() == Some(want))
}

/// Где модель лежит: целиком в видеопамяти, целиком в обычной или поделена.
fn placement(m: &Model) -> String {
    match (m.size, m.size_vram) {
        (_, 0) => "CPU".into(),
        (size, vram) if vram >= size => "GPU".into(),
        (size, vram) => format!("GPU {}%", vram * 100 / size),
    }
}

/// `{"error": "…"}` — текст сервера как есть: он и есть причина.
fn server_error(body: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Failure {
        error: String,
    }
    serde_json::from_str::<Failure>(body).ok().map(|f| clip(&f.error, 300))
}

/// Кусок чужого текста для строки факта: в одну строку и не длиннее `max` символов.
fn clip(text: &str, max: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match text.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text,
    }
}

/// Ответ кнопки «Спросить модель» — так же он уходит в интерфейс.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Answer {
    pub ok: bool,
    /// «ответила за 3.2 с» либо текст ошибки сервера как есть.
    pub fact: String,
    pub seconds: Option<f64>,
}

/// Разбор ответа `/api/generate`; `took_ms` — сколько длился запрос.
pub fn answer(reply: Result<(u16, &str), String>, took_ms: u64) -> Answer {
    let fail = |fact: String| Answer { ok: false, fact, seconds: None };
    let (code, body) = match reply {
        Ok(reply) => reply,
        Err(e) => return fail(e),
    };
    match serde_json::from_str::<Generated>(body) {
        Ok(Generated { error: Some(e), .. }) => fail(clip(&e, 300)),
        // Пустой `response` — тоже ответ: думающая модель тратит единственный токен на размышление.
        Ok(Generated { response: Some(_), load_duration, .. }) if code == 200 => {
            let seconds = took_ms as f64 / 1000.0;
            let load = load_duration as f64 / 1e9;
            let loading = if load >= 1.0 { format!(", из них загрузка в память — {load:.1} с") } else { String::new() };
            Answer { ok: true, fact: format!("ответила за {seconds:.1} с{loading}"), seconds: Some(seconds) }
        }
        _ => fail(format!("HTTP {code}: {}", clip(body, 300))),
    }
}

/// Самая короткая настоящая генерация: один токен, ответ целиком. `keep_alive` не задаём
/// намеренно: он сменил бы срок жизни модели, которой сейчас пользуются рабочие запросы.
pub fn ask_body(model: &str) -> String {
    serde_json::json!({"model": model, "prompt": "ping", "stream": false, "options": {"num_predict": 1}}).to_string()
}

async fn fetch(request: reqwest::RequestBuilder, url: &str, limit: Duration) -> Result<(u16, String), String> {
    let mut resp = request.send().await.map_err(|e| super::net_error(&e, url, limit))?;
    let code = resp.status().as_u16();
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| super::net_error(&e, url, limit))? {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_BODY {
            return Err(format!("ответ длиннее {} КБ", MAX_BODY >> 10));
        }
    }
    Ok((code, String::from_utf8_lossy(&body).into_owned()))
}

fn borrowed(reply: &Result<(u16, String), String>) -> Result<(u16, &str), String> {
    reply.as_ref().map(|(code, body)| (*code, body.as_str())).map_err(String::clone)
}

/// Проверка с этой машины.
pub async fn check(base: &str, expect: &[String], limit: Duration) -> Verdict {
    let client = match super::client(limit) {
        Ok(client) => client,
        Err(e) => return verdict(Err(e), None, None, expect),
    };
    let started = Instant::now();
    let tags = fetch(client.get(format!("{base}/api/tags")), base, limit).await;
    let latency = started.elapsed().as_millis() as u64;
    let ps = match &tags {
        Ok((200, _)) => fetch(client.get(format!("{base}/api/ps")), base, limit).await.ok().filter(|(code, _)| *code == 200),
        _ => None,
    };
    verdict(borrowed(&tags), ps.as_ref().map(|(_, body)| body.as_str()), Some(latency), expect)
}

/// «Спросить модель» с этой машины.
pub async fn ask(base: &str, model: &str) -> Answer {
    let client = match super::client(ASK_LIMIT) {
        Ok(client) => client,
        Err(e) => return answer(Err(e), 0),
    };
    let started = Instant::now();
    let request = client.post(format!("{base}/api/generate")).header("content-type", "application/json").body(ask_body(model));
    let reply = fetch(request, base, ASK_LIMIT).await;
    answer(borrowed(&reply), started.elapsed().as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Формат снят с настоящего сервера; имена и размеры выдуманы.
    const TAGS: &str = r#"{"models":[
      {"name":"qwen3:8b","model":"qwen3:8b","modified_at":"2026-09-01T10:00:00.123456789+03:00","size":5225388164,"digest":"aa11","details":{"parent_model":"","format":"gguf","family":"qwen3","families":["qwen3"],"parameter_size":"8.2B","quantization_level":"Q4_K_M"}},
      {"name":"nomic-embed-text:latest","model":"nomic-embed-text:latest","modified_at":"2026-08-01T10:00:00+03:00","size":274302450,"digest":"bb22","details":{"format":"gguf","family":"nomic-bert","families":["nomic-bert"],"parameter_size":"137M","quantization_level":"F16"}}
    ]}"#;
    const PS: &str = r#"{"models":[{"name":"qwen3:8b","model":"qwen3:8b","size":6654289920,"digest":"aa11","details":{"family":"qwen3"},"expires_at":"2026-10-02T12:05:00+03:00","size_vram":6654289920}]}"#;

    fn expect(models: &[&str]) -> Vec<String> {
        models.iter().map(|m| m.to_string()).collect()
    }

    #[test]
    fn answering_server_lists_models_and_what_is_loaded() {
        let v = verdict(Ok((200, TAGS)), Some(PS), Some(40), &[]);
        assert!(v.ok);
        assert_eq!(v.fact, "отвечает за 40 мс · моделей: 2 · в памяти: qwen3:8b (GPU)");
        assert_eq!(v.models, ["qwen3:8b", "nomic-embed-text:latest"]);

        let v = verdict(Ok((200, TAGS)), Some(r#"{"models":[]}"#), Some(7), &[]);
        assert_eq!(v.fact, "отвечает за 7 мс · моделей: 2 · в памяти: ничего");
        // /api/ps не получен или в нём мусор — сервер всё равно отвечает.
        for ps in [None, Some("<html>"), Some(&PS[..40])] {
            let v = verdict(Ok((200, TAGS)), ps, None, &[]);
            assert_eq!((v.ok, v.fact.as_str()), (true, "отвечает · моделей: 2 · в памяти: неизвестно"));
        }
        let split = r#"{"models":[{"name":"big:70b","size":1000,"size_vram":400},{"name":"small:1b","size":10,"size_vram":0}]}"#;
        assert!(verdict(Ok((200, TAGS)), Some(split), None, &[]).fact.ends_with("в памяти: big:70b (GPU 40%), small:1b (CPU)"));
    }

    #[test]
    fn garbage_truncated_body_and_errors_are_failures_with_the_reason() {
        let fact = |tags| verdict(tags, None, Some(1), &[]).fact;
        assert_eq!(fact(Err("соединение отклонено".into())), "не отвечает: соединение отклонено");
        assert_eq!(fact(Ok((502, "<html>Bad Gateway</html>"))), "не отвечает: HTTP 502");
        assert_eq!(fact(Ok((500, r#"{"error":"llama runner process has terminated"}"#))), "не отвечает: HTTP 500: llama runner process has terminated");
        assert_eq!(fact(Ok((200, "<html>это не Ollama</html>"))), "не отвечает: в ответе не список моделей Ollama: <html>это не Ollama</html>");
        // Обрезанное тело — не JSON: неполный список за целый не выдаётся.
        let cut = verdict(Ok((200, &TAGS[..TAGS.len() / 2])), Some(PS), Some(1), &expect(&["qwen3"]));
        assert!(!cut.ok && cut.models.is_empty() && cut.latency_ms.is_none());
        assert!(cut.fact.starts_with("не отвечает: в ответе не список моделей Ollama: {\"models\":["), "{}", cut.fact);
        assert!(cut.fact.chars().count() < 140, "длинное тело в факт не попадает: {}", cut.fact);
        assert!(!verdict(Ok((200, r#"{"models":null}"#)), None, None, &[]).ok);
    }

    #[test]
    fn expected_models() {
        // Без метки — любая метка этой модели; с меткой — точное имя.
        assert!(verdict(Ok((200, TAGS)), None, None, &expect(&["qwen3", "nomic-embed-text:latest"])).ok);
        let v = verdict(Ok((200, TAGS)), Some(PS), Some(40), &expect(&["qwen3:14b"]));
        assert_eq!((v.ok, v.fact.as_str()), (false, "нет модели qwen3:14b"));
        assert_eq!(v.models.len(), 2, "список при этом получен: из него выбирают модель для вопроса");
        let v = verdict(Ok((200, TAGS)), None, None, &expect(&["llama3", "qwen3", "phi"]));
        assert_eq!(v.fact, "нет моделей llama3, phi");
    }

    #[test]
    fn answer_of_generation() {
        let done = r#"{"model":"qwen3:8b","created_at":"2026-10-02T12:00:00Z","response":"","done":true,"done_reason":"length","total_duration":3200000000,"load_duration":2800000000,"eval_count":1}"#;
        assert_eq!(
            answer(Ok((200, done)), 3240),
            Answer { ok: true, fact: "ответила за 3.2 с, из них загрузка в память — 2.8 с".into(), seconds: Some(3.24) }
        );
        let warm = r#"{"response":"Hi","done":true,"load_duration":21000000}"#;
        assert_eq!(answer(Ok((200, warm)), 412).fact, "ответила за 0.4 с");
        // Ошибка сервера — его словами.
        let missing = r#"{"error":"model 'qwen3:99b' not found"}"#;
        assert_eq!(answer(Ok((404, missing)), 30), Answer { ok: false, fact: "model 'qwen3:99b' not found".into(), seconds: None });
        assert_eq!(answer(Err("нет ответа, таймаут 60 с".into()), 60_000).fact, "нет ответа, таймаут 60 с");
        assert_eq!(answer(Ok((502, "Bad Gateway")), 5).fact, "HTTP 502: Bad Gateway");
        assert_eq!(answer(Ok((200, &done[..30])), 5).fact, format!("HTTP 200: {}", &done[..30]));
    }

    #[test]
    fn generation_request_is_one_token_and_keeps_keep_alive() {
        let body: serde_json::Value = serde_json::from_str(&ask_body("qwen3:8b")).unwrap();
        assert_eq!(body["options"]["num_predict"], 1);
        assert_eq!(body["stream"], false);
        assert!(body.get("keep_alive").is_none());
    }

    /// Проверка и вопрос по настоящему http: подделка сервера на localhost.
    #[tokio::test]
    async fn check_and_ask_over_http() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            loop {
                let (mut s, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 4096];
                let n = s.read(&mut buf).await.unwrap();
                let head = String::from_utf8_lossy(&buf[..n]).into_owned();
                let body = match head.split_whitespace().nth(1) {
                    Some("/api/tags") => TAGS,
                    Some("/api/ps") => PS,
                    _ => r#"{"response":"p","done":true,"load_duration":0}"#,
                };
                let reply = format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
                s.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        let v = check(&base, &[], Duration::from_secs(5)).await;
        assert!(v.ok && v.fact.ends_with("моделей: 2 · в памяти: qwen3:8b (GPU)"), "{}", v.fact);
        assert!(ask(&base, "qwen3:8b").await.ok);
        let dead = check("http://127.0.0.1:1", &[], Duration::from_secs(5)).await;
        assert!(!dead.ok && dead.fact.starts_with("не отвечает: "), "{}", dead.fact);
    }
}
