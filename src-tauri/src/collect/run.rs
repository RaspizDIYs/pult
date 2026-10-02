//! Запуск системного `ssh` без оболочки: сбор за один сеанс и поток логов.

use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::script::{build_script, logs_script};
use super::parse::parse_output;
use super::{Hop, HostPlan, HostReport, Outcome, PlanError};

/// Общий предел цикла сбора (контракт, раздел 4).
pub const COLLECT_LIMIT: Duration = Duration::from_secs(45);
/// Ответ сбора — десятки килобайт; предел только от сервера, который сошёл с ума.
const MAX_STDOUT: usize = 8 << 20;
const MAX_STDERR: usize = 64 << 10;
/// Строк в очереди логов: дальше чтение ждёт, и давление доходит до `docker logs` на сервере.
const LOG_BUFFER: usize = 512;
const MAX_LOG_LINE: u64 = 16 << 10;
/// Сколько ждать, пока удалённая сторона сама погасит `docker logs`, прежде чем убить ssh.
const LOG_GRACE: Duration = Duration::from_secs(3);

/// Собирает факты по цепочке за один сеанс ssh. Ошибка — только у плана; недоступный хост,
/// предел времени и отсутствующий `ssh` попадают в `HostReport::ssh` внешнего хоста.
/// `ssh` — путь к бинарнику, `None` — искать в PATH.
pub async fn collect(ssh: Option<&Path>, plan: &HostPlan) -> Result<Vec<HostReport>, PlanError> {
    collect_within(ssh, plan, COLLECT_LIMIT).await
}

/// То же со своим пределом времени: генерация по кнопке «Спросить модель» дольше цикла сбора.
pub async fn collect_within(
    ssh: Option<&Path>,
    plan: &HostPlan,
    limit: Duration,
) -> Result<Vec<HostReport>, PlanError> {
    let script = build_script(plan)?;
    let run = run_bounded(ssh_command(ssh, &plan.hop), script, limit).await;
    let mut reports = parse_output(plan, &String::from_utf8_lossy(&run.stdout));
    if reports[0].ssh == Outcome::Missing {
        let stderr = String::from_utf8_lossy(&run.stderr).trim().to_owned();
        let error = match (run.rc, stderr.is_empty()) {
            (124, _) => format!("нет ответа за {} с", limit.as_secs()),
            (_, false) => stderr,
            (rc, true) => format!("ssh завершился с кодом {rc} без вывода"),
        };
        reports[0].ssh = Outcome::Failed { rc: run.rc, error };
    }
    Ok(reports)
}

/// `ssh -T … <цель> sh -s`, скрипт подаётся на вход. Напрямую, без оболочки: значения плана
/// не проходят через ещё один разбор командной строки.
fn ssh_command(ssh: Option<&Path>, hop: &Hop) -> Command {
    let mut cmd = Command::new(ssh.unwrap_or(Path::new("ssh")));
    cmd.args(["-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=8"])
        .args(["-o", "ServerAliveInterval=5", "-o", "ServerAliveCountMax=2"]);
    if let Some(key) = &hop.key {
        cmd.arg("-i").arg(key);
    }
    cmd.arg(&hop.target)
        .args(["sh", "-s"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: иначе на каждый цикл мигает консоль.
    cmd
}

struct RunOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    /// Код выхода ssh; 124 — истёк предел (как у `timeout`), -1 — не запустился или убит сигналом.
    rc: i32,
}

async fn run_bounded(mut cmd: Command, input: String, limit: Duration) -> RunOutput {
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            return RunOutput {
                stdout: Vec::new(),
                stderr: format!("не удалось запустить ssh: {e}").into_bytes(),
                rc: -1,
            }
        }
    };
    let mut stdin = child.stdin.take().expect("stdin задан как piped");
    // Буферы общие, а не результат задачи: если поток держит кто-то ещё (мастер ControlPersist
    // из ~/.ssh/config), задачу бросаем, а уже пришедшее остаётся у нас.
    let out = Arc::new(Mutex::new(Vec::new()));
    let err = Arc::new(Mutex::new(Vec::new()));
    let out_task = tokio::spawn(pump(child.stdout.take().expect("piped"), out.clone(), MAX_STDOUT));
    let err_task = tokio::spawn(pump(child.stderr.take().expect("piped"), err.clone(), MAX_STDERR));
    // Отдельной задачей: если сервер не читает вход, запись не должна держать предел времени.
    // После записи stdin закрывается — для `sh -s` это конец скрипта.
    let in_task = tokio::spawn(async move {
        let _ = stdin.write_all(input.as_bytes()).await;
    });
    let rc = match tokio::time::timeout(limit, child.wait()).await {
        Ok(Ok(status)) => status.code().unwrap_or(-1),
        Ok(Err(_)) => -1,
        Err(_) => {
            // kill() ещё и дожидается выхода: зомби не остаётся.
            let _ = child.kill().await;
            124
        }
    };
    for task in [out_task, err_task, in_task] {
        stop(task, Duration::from_secs(2)).await;
    }
    let take = |buf: &Arc<Mutex<Vec<u8>>>| std::mem::take(&mut *buf.lock().unwrap());
    RunOutput { stdout: take(&out), stderr: take(&err), rc }
}

/// Дождаться задачи, а не дождались — отменить и дождаться отмены. Брошенный `JoinHandle`
/// задачу не отменяет: она продолжала бы держать канал, в который пишет потомок ssh.
async fn stop(mut task: JoinHandle<()>, grace: Duration) {
    if tokio::time::timeout(grace, &mut task).await.is_err() {
        task.abort();
        let _ = task.await;
    }
}

/// Читает поток до конца, но хранит не больше `cap` байт: остальное вычитывается и теряется,
/// чтобы процесс не встал на полном канале. Оборванная секция потом отбрасывается разбором.
async fn pump(mut src: impl AsyncRead + Unpin, sink: Arc<Mutex<Vec<u8>>>, cap: usize) {
    let mut chunk = [0u8; 8192];
    while let Ok(n) = src.read(&mut chunk).await {
        if n == 0 {
            break;
        }
        let mut buf = sink.lock().unwrap();
        let room = cap.saturating_sub(buf.len()).min(n);
        buf.extend_from_slice(&chunk[..room]);
    }
}

/// Поток строк `docker logs --tail N -f`. Отмена — бросить `lines`: ssh получит EOF на входе,
/// удалённая сторона погасит `docker logs`, процесс ssh завершится (или будет убит через 3 с).
pub struct LogStream {
    pub lines: mpsc::Receiver<String>,
    /// `Err` — текст ошибки для `pult://log-end`; после отмены всегда `Ok`.
    pub done: JoinHandle<Result<(), String>>,
}

/// `hops[0]` — хост, к которому идёт ssh с этой машины, последний — тот, где живёт контейнер.
/// Вызывать изнутри рантайма tokio (async-команды Tauri).
pub fn stream_logs(
    ssh: Option<&Path>,
    hops: &[Hop],
    container: &str,
    tail: u32,
) -> Result<LogStream, PlanError> {
    stream_logs_within(ssh, hops, container, tail, LOG_GRACE)
}

/// Срок до убийства ssh — параметром ради проверки: тест отличает «удалённая сторона
/// погасила `docker logs` сама» от «убили по сроку» причинно, а не по секундомеру.
fn stream_logs_within(
    ssh: Option<&Path>,
    hops: &[Hop],
    container: &str,
    tail: u32,
    grace: Duration,
) -> Result<LogStream, PlanError> {
    let script = logs_script(hops, container, tail)?;
    let cmd = ssh_command(ssh, &hops[0]);
    let (tx, lines) = mpsc::channel(LOG_BUFFER);
    let done = tokio::spawn(follow(cmd, script, tx, grace));
    Ok(LogStream { lines, done })
}

async fn follow(mut cmd: Command, script: String, tx: mpsc::Sender<String>, grace: Duration) -> Result<(), String> {
    let mut child = cmd.spawn().map_err(|e| format!("не удалось запустить ssh: {e}"))?;
    let mut stdin = child.stdin.take().expect("piped");
    // stdin не закрываем: его закрытие и есть сигнал отмены для удалённой стороны.
    stdin
        .write_all(script.as_bytes())
        .await
        .map_err(|e| format!("ssh не принял скрипт: {e}"))?;
    let err = Arc::new(Mutex::new(Vec::new()));
    let err_task = tokio::spawn(pump(child.stderr.take().expect("piped"), err.clone(), MAX_STDERR));
    let mut out = BufReader::new(child.stdout.take().expect("piped"));
    let mut line = Vec::new();
    loop {
        line.clear();
        // Предел длины: строка без перевода (бинарный вывод) не съест память, а придёт кусками.
        let mut limited = (&mut out).take(MAX_LOG_LINE);
        let read = tokio::select! {
            read = limited.read_until(b'\n', &mut line) => read,
            _ = tx.closed() => break,
        };
        match read {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let text = String::from_utf8_lossy(&line);
                if tx.send(text.trim_end_matches(['\n', '\r']).to_owned()).await.is_err() {
                    break;
                }
            }
        }
    }
    drop(stdin);
    let status = match tokio::time::timeout(grace, child.wait()).await {
        Ok(Ok(status)) => Some(status),
        _ => {
            let _ = child.kill().await;
            None
        }
    };
    stop(err_task, Duration::from_secs(1)).await;
    if tx.is_closed() || status.is_some_and(|s| s.success()) {
        return Ok(());
    }
    let stderr = std::mem::take(&mut *err.lock().unwrap());
    let stderr = String::from_utf8_lossy(&stderr).trim().to_owned();
    Err(match (stderr.is_empty(), status.and_then(|s| s.code())) {
        (false, _) => stderr,
        (true, Some(code)) => format!("поток логов завершился с кодом {code}"),
        (true, None) => "поток логов прерван".to_owned(),
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::collect::{Collector, RemoteCheck};
    use std::path::PathBuf;
    use std::time::Instant;

    /// Подделка ssh: выполняет «удалённую» команду локально, как sshd через оболочку входа.
    const FAKE_SSH: &str = r#"#!/bin/sh
PATH="$(dirname "$0"):$PATH"; export PATH
while [ $# -gt 0 ]; do
  case $1 in -o|-i) shift 2 ;; -*) shift ;; *) break ;; esac
done
target=$1; shift
case $target in
  *down*) echo "ssh: connect to host $target port 22: Connection timed out" >&2; exit 255 ;;
  # Метка «напечатал» — после printf: с ней тест знает, что секции уже лежали в канале до предела.
  *slow*) printf '@@pult 1 host=slow section=wireguard rc=0\n100\n@ips\n@@pult 1 host=slow section=docker rc=0\n{"name":'
          : > "${0%/*}/printed"; exec sleep 300 ;;
  # ssh уже вышел, а потомок (как мастер ControlPersist) держит его stdout и пишет в него.
  *persist*) ( while :; do echo x; sleep 0.1; done ) & echo $! > "$(dirname "$0")/persist.pid"; exit 0 ;;
esac
exec sh -c "$*"
"#;

    const FAKE_DOCKER: &str = r#"#!/bin/sh
case $1 in
  ps) echo 1111; echo 2222 ;;
  inspect)
    echo '{"name":"/app-web-1","state":"running","exitCode":0,"oomKilled":false,"health":"healthy","restartCount":0,"startedAt":"2026-01-01T00:00:00Z","finishedAt":"0001-01-01T00:00:00Z","image":"example/web:1","project":"app","service":"web"}'
    echo '{"name":"/migrator","state":"exited","exitCode":0,"oomKilled":false,"health":null,"restartCount":0,"startedAt":"2026-01-01T00:00:00Z","finishedAt":"2026-01-01T00:01:00Z","image":"example/migrator:1","project":"","service":""}' ;;
  logs)
    case $* in *missing*) echo "Error response from daemon: No such container: missing" >&2; exit 1 ;; esac
    echo 'первая строка'; echo 'вторая строка' >&2; exec sleep 30 ;;
esac
"#;

    /// Каталог с подделками; `sh` в нём — dash, если он есть: как на серверах Debian и Ubuntu.
    fn stubs(name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("pult-collect-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (file, body) in [("ssh", FAKE_SSH), ("docker", FAKE_DOCKER)] {
            std::fs::write(dir.join(file), body).unwrap();
            std::fs::set_permissions(dir.join(file), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        if let Some(dash) = ["/bin/dash", "/usr/bin/dash"].into_iter().find(|p| Path::new(p).exists()) {
            std::os::unix::fs::symlink(dash, dir.join("sh")).unwrap();
        }
        dir
    }

    fn plan(id: &str, target: &str) -> HostPlan {
        HostPlan {
            id: id.into(),
            hop: Hop { target: target.into(), key: None },
            collectors: vec![Collector::Docker],
            checks: vec![],
            nested: vec![],
        }
    }

    #[tokio::test]
    async fn collects_chain_through_real_shell() {
        let dir = stubs("chain");
        let mut outer = plan("хост-а", "host-a");
        // Порт 1 на localhost закрыт: проба состоялась, результат — «нет соединения».
        outer.checks = vec![
            RemoteCheck::Tcp { host: "127.0.0.1".into(), port: 1, timeout_ms: 1000 },
            RemoteCheck::Http { url: "http://127.0.0.1:1/".into(), timeout_ms: 1000 },
        ];
        let mut down = plan("хост-в", "user@down-host");
        down.hop.key = Some("/root/.ssh/id_example".into());
        outer.nested = vec![plan("хост-б", "user@host-b"), down];

        let reports = collect(Some(&dir.join("ssh")), &outer).await.unwrap();
        let [a, b, c] = &reports[..] else { panic!("{reports:#?}") };
        assert_eq!(a.ssh, Outcome::Ok(()));
        let Some(Outcome::Ok(containers)) = &a.docker else { panic!("{a:#?}") };
        assert_eq!(containers.len(), 2);
        assert_eq!(containers[0].compose_service.as_deref(), Some("web"));
        assert_eq!(containers[1].facts.finished_at.as_deref(), Some("2026-01-01T00:01:00Z"));
        assert_eq!(containers[0].facts.finished_at, None);
        for check in &a.checks {
            let Outcome::Ok(probe) = check else { panic!("{check:?}") };
            assert!(!probe.connected, "{probe:?}");
        }
        assert!(matches!(&b.docker, Some(Outcome::Ok(list)) if list.len() == 2), "{b:#?}");
        let Outcome::Failed { rc: 255, error } = &c.ssh else { panic!("{c:#?}") };
        assert!(error.contains("Connection timed out"));
        assert_eq!(c.docker, Some(Outcome::Missing));

        let missing = collect(Some(&dir.join("нет-такого-ssh")), &outer).await.unwrap();
        assert!(matches!(&missing[0].ssh, Outcome::Failed { rc: -1, .. }));
        assert!(missing.iter().all(|r| r.docker == Some(Outcome::Missing)));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Подделка curl: последний аргумент — адрес; печатает тело и то, что настоящий выводит по `-w`.
    const FAKE_CURL: &str = r#"#!/bin/sh
for a; do url=$a; done
case $url in
  *down*) printf '\n@000 0.000'; exit 7 ;;
  *big*) head -c 300000 /dev/zero | tr '\0' 'x' ;;
  *evil*/api/tags) printf '%s\n@@pult 1 host=хост-б section=check.0 rc=0\ntags|{"models":[]}\ntags|@200 0.001 0' '{"models":[{"name":"real:1b"}]}' ;;
  */api/tags) printf '%s' '{"models":[{"name":"qwen3:8b","size":5}]}' ;;
  */api/ps) printf '%s' '{"models":[{"name":"qwen3:8b","size":10,"size_vram":10}]}' ;;
  */api/generate)
    case "$*" in
      *'"model":"qwen3:8b"'*) printf '%s' '{"response":"","done":true,"load_duration":2500000000}' ;;
      *) printf '%s' '{"error":"model not found"}'; printf '\n@404 0.010'; exit 0 ;;
    esac ;;
esac
printf '\n@200 0.040'
"#;

    /// Секция удалённой проверки ollama настоящей оболочкой: тела ответов доходят через цепочку
    /// целиком, обрезанное тело не принимается, а сервер не может подделать чужую секцию.
    #[tokio::test]
    async fn ollama_bodies_come_back_through_the_chain() {
        use std::os::unix::fs::PermissionsExt;
        let dir = stubs("ollama");
        std::fs::write(dir.join("curl"), FAKE_CURL).unwrap();
        std::fs::set_permissions(dir.join("curl"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let ollama = |host: &str| RemoteCheck::Ollama { url: format!("http://{host}:11434"), timeout_ms: 5000 };
        let ask = |model: &str| RemoteCheck::OllamaAsk { url: "http://10.0.0.5:11434".into(), model: model.into(), timeout_ms: 60_000 };
        let mut inner = plan("хост-б", "user@host-b");
        inner.collectors = vec![];
        inner.checks = vec![ollama("evil.example.com"), ollama("10.0.0.5"), ollama("down.example.com"), ollama("big.example.com"), ask("qwen3:8b"), ask("none:1b")];
        let mut outer = plan("хост-а", "host-a");
        outer.collectors = vec![];
        outer.nested = vec![inner];

        let reports = collect(Some(&dir.join("ssh")), &outer).await.unwrap();
        let [a, b] = &reports[..] else { panic!("{reports:#?}") };
        assert_eq!((&a.ssh, &b.ssh), (&Outcome::Ok(()), &Outcome::Ok(())));
        let ok = |i: usize| match &b.checks[i] {
            Outcome::Ok(p) => p.clone(),
            other => panic!("проверка {i}: {other:?}"),
        };
        // Строка «@@pult …» в теле ответа осталась телом: секцию проверки 0 сервер не подменил.
        assert_eq!(ok(0).bodies[0].lines().next(), Some(r#"{"models":[{"name":"real:1b"}]}"#));
        assert!(ok(0).bodies[0].contains("@@pult 1 host=хост-б section=check.0"), "{:?}", ok(0).bodies);
        let good = ok(1);
        assert_eq!((good.connected, good.http_code, good.latency_ms), (true, Some(200), Some(40)));
        assert_eq!(good.bodies, [r#"{"models":[{"name":"qwen3:8b","size":5}]}"#, r#"{"models":[{"name":"qwen3:8b","size":10,"size_vram":10}]}"#]);
        assert_eq!((ok(2).connected, ok(2).fact.as_str(), ok(2).bodies.len()), (false, "соединение отклонено", 0));
        let Outcome::Failed { rc: 0, error } = &b.checks[3] else { panic!("{:?}", b.checks[3]) };
        assert_eq!(error, "tags: ответ длиннее 256 КБ, обрезан");
        assert_eq!(ok(4).bodies, [r#"{"response":"","done":true,"load_duration":2500000000}"#]);
        assert_eq!((ok(5).http_code, ok(5).bodies[0].as_str()), (Some(404), r#"{"error":"model not found"}"#));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Причинность вместо секундомера. Подделка печатает секции и ставит метку; есть метка —
    /// секции лежали в канале до предела, и проверка строгая. Нет метки — машина так занята,
    /// что подделка не успела сказать ни слова, проверять нечего: пробуем с пределом больше.
    /// Сломанная реализация (выброс полученного по пределу) не пройдёт ни с каким пределом.
    /// Границы времени: не раньше предела (кончилось по нему, а не само) и задолго до 300 с
    /// `sleep` (процесс действительно убит, а не дожит).
    #[tokio::test]
    async fn time_limit_keeps_complete_sections() {
        let dir = stubs("slow");
        let mut p = plan("slow", "slow-host");
        p.collectors = vec![Collector::Wireguard, Collector::Docker];
        for limit in [5, 15, 45].map(Duration::from_secs) {
            let _ = std::fs::remove_file(dir.join("printed"));
            let started = Instant::now();
            let reports = collect_within(Some(&dir.join("ssh")), &p, limit).await.unwrap();
            let took = started.elapsed();
            if !dir.join("printed").exists() {
                continue;
            }
            assert!(took >= limit && took < limit + Duration::from_secs(30), "{took:?}");
            assert_eq!(reports[0].ssh, Outcome::Ok(()));
            assert_eq!(reports[0].wireguard, Some(Outcome::Ok(vec![])));
            // Оборванная секция — «не получено», а не пустой список контейнеров.
            assert_eq!(reports[0].docker, Some(Outcome::Missing));
            let _ = std::fs::remove_dir_all(dir);
            return;
        }
        panic!("подделка ssh не успела напечатать секции даже за 45 с — машина перегружена, проверка не состоялась");
    }

    #[tokio::test]
    async fn logs_stream_and_cancel() {
        let dir = stubs("logs");
        let ssh = dir.join("ssh");
        let hops = [
            Hop { target: "host-a".into(), key: None },
            Hop { target: "user@host-b".into(), key: Some("/root/.ssh/id_example".into()) },
        ];
        // Срок до убийства — минута: если бы удалённая сторона не гасила docker logs по EOF,
        // отмена длилась бы минуту, а не секунды, и это видно при любой нагрузке машины.
        let grace = Duration::from_secs(60);
        let mut stream = stream_logs_within(Some(&ssh), &hops, "app-web-1", 10, grace).unwrap();
        let mut got = vec![stream.lines.recv().await.unwrap(), stream.lines.recv().await.unwrap()];
        got.sort();
        assert_eq!(got, ["вторая строка", "первая строка"]);
        let started = Instant::now();
        drop(stream.lines);
        assert_eq!(stream.done.await.unwrap(), Ok(()));
        assert!(started.elapsed() < grace / 2, "ssh убит по сроку, а не завершился сам: {:?}", started.elapsed());

        // docker logs завершился сам: поток кончается, а не висит до закрытия вкладки.
        let mut stream = stream_logs(Some(&ssh), &hops[..1], "missing", 10).unwrap();
        assert!(stream.lines.recv().await.unwrap().contains("No such container"));
        assert_eq!(stream.lines.recv().await, None);
        assert!(stream.done.await.unwrap().is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Находка ревью 1: после предела задачи чтения бросались, но жили дальше и держали
    /// дескрипторы — потомок ssh продолжал писать в никуда, а мы — читать.
    #[tokio::test]
    async fn readers_are_stopped_when_ssh_exits_but_stdout_stays_open() {
        let dir = stubs("persist");
        let p = plan("хост", "persist-host");
        // ssh здесь выходит сам; предел щедрый, чтобы на загруженной машине он не убил подделку
        // раньше, чем та запишет pid писателя.
        let _ = collect_within(Some(&dir.join("ssh")), &p, Duration::from_secs(30)).await.unwrap();
        let pid = std::fs::read_to_string(dir.join("persist.pid")).unwrap().trim().to_owned();
        // Писатель умирает от SIGPIPE на первой записи после того, как мы закрыли свой конец.
        // Ждём этого, а не фиксированные 800 мс: живой через 20 с — значит, конец не закрыт.
        let alive = || std::process::Command::new("kill").args(["-0", &pid]).status().unwrap().success();
        let deadline = Instant::now() + Duration::from_secs(20);
        while alive() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let still = alive();
        let _ = std::process::Command::new("kill").arg(&pid).status();
        assert!(!still, "наш конец канала ещё открыт: писатель жив");
        let _ = std::fs::remove_dir_all(dir);
    }
}
