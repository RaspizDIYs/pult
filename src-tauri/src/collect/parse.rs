//! Разбор вывода скрипта сбора: секции `@@pult 1 host=<id> section=<имя> rc=<код>`.

use serde::Deserialize;

use super::{
    Collector, Container, ContainerFacts, Guest, GuestKind, HostPlan, HostReport, Outcome,
    ProbeResult, RemoteCheck, WgPeer,
};

struct Section<'a> {
    host: &'a str,
    name: &'a str,
    rc: i32,
    lines: Vec<&'a str>,
}

impl Section<'_> {
    fn text(&self) -> String {
        self.lines.join("\n").trim().to_owned()
    }
}

/// Отчёты по всем хостам плана: внешний первым, дальше вложенные в порядке обхода.
/// У внешнего хоста `ssh` = `Missing`, если не пришло ни одной секции: причину знает только запуск.
pub fn parse_output(plan: &HostPlan, stdout: &str) -> Vec<HostReport> {
    let mut sections: Vec<Section> = Vec::new();
    for line in stdout.lines() {
        if let Some(section) = header(line) {
            sections.push(section);
        } else if let Some(last) = sections.last_mut() {
            last.lines.push(line);
        }
        // Строки до первой секции — баннеры и предупреждения входа, их отбрасываем.
    }
    let mut reports = Vec::new();
    report(plan, &sections, &mut reports);
    reports
}

fn header(line: &str) -> Option<Section<'_>> {
    let mut fields = line.strip_prefix("@@pult 1 ")?.split(' ');
    let host = fields.next()?.strip_prefix("host=")?;
    let name = fields.next()?.strip_prefix("section=")?;
    let rc = fields.next()?.strip_prefix("rc=")?.parse().ok()?;
    Some(Section { host, name, rc, lines: Vec::new() })
}

fn report(plan: &HostPlan, all: &[Section], out: &mut Vec<HostReport>) {
    // Оборваться может только последняя секция потока убитого хоста. Пока хост жив, за его секцией
    // идёт заголовок его же следующей секции (последнюю закрывает метка `end`) или кого-то из его
    // вложенных хостов. Любой другой заголовок — `ssh` от родителя после обрыва, а пустота — конец
    // потока по пределу времени. Неполный список контейнеров выглядел бы как «контейнер исчез»,
    // поэтому такой секции не верим.
    let mut below = Vec::new();
    descendants(plan, &mut below);
    let complete = |i: usize| {
        all.get(i + 1).is_some_and(|n| {
            (n.host == plan.id && n.name != "ssh") || below.contains(&n.host)
        })
    };
    // Секцию `ssh` печатает родитель после выхода вложенного ssh, она не из потока самого хоста.
    let own: Vec<(usize, &Section)> = all
        .iter()
        .enumerate()
        .filter(|(_, s)| s.host == plan.id && s.name != "ssh")
        .collect();
    let reached = !own.is_empty();
    let find = |name: &str| {
        own.iter()
            .find(|(i, s)| s.name == name && complete(*i))
            .map(|(_, s)| *s)
    };
    let ssh = match all.iter().find(|s| s.host == plan.id && s.name == "ssh") {
        Some(s) if s.rc != 0 => Outcome::Failed { rc: s.rc, error: s.text() },
        Some(_) => Outcome::Ok(()),
        None if reached => Outcome::Ok(()),
        None => Outcome::Missing,
    };
    let wanted = |c| plan.collectors.contains(&c);
    let checks = plan
        .checks
        .iter()
        .enumerate()
        .map(|(i, check)| {
            let section = find(&format!("check.{i}"));
            match check {
                RemoteCheck::Tcp { .. } => outcome(section, parse_tcp),
                RemoteCheck::Http { .. } => outcome(section, parse_http),
                RemoteCheck::Ollama { .. } => outcome(section, parse_ollama),
                RemoteCheck::OllamaAsk { .. } => outcome(section, parse_ask),
            }
        })
        .collect();
    out.push(HostReport {
        id: plan.id.clone(),
        ssh,
        docker: wanted(Collector::Docker).then(|| outcome(find("docker"), parse_docker)),
        wireguard: wanted(Collector::Wireguard).then(|| outcome(find("wireguard"), parse_wireguard)),
        proxmox: wanted(Collector::Proxmox).then(|| outcome(find("proxmox"), parse_proxmox)),
        checks,
    });
    for nested in &plan.nested {
        report(nested, all, out);
    }
}

fn descendants<'a>(plan: &'a HostPlan, out: &mut Vec<&'a str>) {
    for nested in &plan.nested {
        out.push(&nested.id);
        descendants(nested, out);
    }
}

fn outcome<T>(section: Option<&Section>, parse: fn(&[&str]) -> Result<T, String>) -> Outcome<T> {
    match section {
        None => Outcome::Missing,
        Some(s) if s.rc != 0 => Outcome::Failed { rc: s.rc, error: s.text() },
        // Ответ пришёл, но не разобран — тоже «узнать не удалось», а не «пусто».
        Some(s) => parse(&s.lines).map_or_else(|error| Outcome::Failed { rc: 0, error }, Outcome::Ok),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Inspect {
    name: String,
    state: String,
    exit_code: Option<i64>,
    oom_killed: Option<bool>,
    health: Option<String>,
    restart_count: Option<u64>,
    started_at: Option<String>,
    finished_at: Option<String>,
    image: Option<String>,
    project: Option<String>,
    service: Option<String>,
}

fn parse_docker(lines: &[&str]) -> Result<Vec<Container>, String> {
    lines
        .iter()
        .map(|l| l.trim())
        // stderr слит с выводом: предупреждения docker («WARNING: …») — известные строки.
        // Любая другая непонятная строка — «узнать не удалось»: молча выброшенная, она дала бы
        // неполный или пустой список, а это для движка «контейнера нет».
        .filter(|l| !l.is_empty() && !l.starts_with("WARNING:"))
        .map(|line| {
            if !line.starts_with('{') {
                return Err(format!("непонятная строка в ответе docker: {line:?}"));
            }
            let i: Inspect = serde_json::from_str(line)
                .map_err(|e| format!("не разобран ответ docker inspect: {e}"))?;
            Ok(Container {
                name: i.name.trim_start_matches('/').to_owned(),
                compose_project: non_empty(i.project),
                compose_service: non_empty(i.service),
                facts: ContainerFacts {
                    state: i.state,
                    exit_code: i.exit_code,
                    oom_killed: i.oom_killed,
                    health: i.health,
                    restart_count: i.restart_count,
                    started_at: timestamp(i.started_at),
                    finished_at: timestamp(i.finished_at),
                    image: non_empty(i.image),
                },
            })
        })
        .collect()
}

fn non_empty(v: Option<String>) -> Option<String> {
    v.filter(|s| !s.is_empty())
}

/// docker пишет нулевое время Go, если контейнер ни разу не запускался или ещё не остановлен.
/// Наносекунды срезаем до секунд, как во всех временах снимка: дробь из девяти знаков
/// не всякий движок JS разберёт.
fn timestamp(v: Option<String>) -> Option<String> {
    use time::format_description::well_known::Rfc3339;
    let t = v.filter(|t| !t.is_empty() && !t.starts_with("0001-"))?;
    let parsed = time::OffsetDateTime::parse(&t, &Rfc3339).ok().and_then(|d| d.replace_nanosecond(0).ok());
    Some(parsed.and_then(|d| d.format(&Rfc3339).ok()).unwrap_or(t))
}

/// Первая строка — время хоста, потом `latest-handshakes`, после `@ips` — `allowed-ips`.
fn parse_wireguard(lines: &[&str]) -> Result<Vec<WgPeer>, String> {
    let mut lines = lines.iter().map(|l| l.trim()).filter(|l| !l.is_empty());
    let now: u64 = lines
        .next()
        .and_then(|l| l.parse().ok())
        .ok_or("wireguard: нет времени хоста")?;
    let mut peers: Vec<WgPeer> = Vec::new();
    let mut ips = false;
    for line in lines {
        if line == "@ips" {
            ips = true;
            continue;
        }
        let f: Vec<&str> = line.splitn(3, '\t').collect();
        let [interface, key, value] = f[..] else {
            return Err(format!("wireguard: непонятная строка {line:?}"));
        };
        if !ips {
            let at: u64 = value
                .parse()
                .map_err(|_| format!("wireguard: непонятное время {line:?}"))?;
            peers.push(WgPeer {
                interface: interface.into(),
                public_key: key.into(),
                allowed_ips: Vec::new(),
                handshake_age_secs: (at > 0).then(|| now.saturating_sub(at)),
            });
        } else if let Some(p) = peers.iter_mut().find(|p| p.interface == interface && p.public_key == key) {
            p.allowed_ips = value
                .split_whitespace()
                .filter(|ip| *ip != "(none)")
                .map(String::from)
                .collect();
        }
    }
    Ok(peers)
}

/// `qm list`: VMID NAME STATUS MEM BOOTDISK PID — имя может быть с пробелом, поэтому
/// статус берём четвёртым с конца. `pct list`: VMID Status [Lock] Name — Lock бывает пустым.
fn parse_proxmox(lines: &[&str]) -> Result<Vec<Guest>, String> {
    // Без обеих частей список неполный: отсутствие ВМ в нём ничего не доказывает.
    if !(lines.iter().any(|l| l.trim() == "@qm") && lines.iter().any(|l| l.trim() == "@pct")) {
        return Err("proxmox: в ответе нет частей @qm и @pct".into());
    }
    let mut kind = None;
    let mut guests = Vec::new();
    for line in lines {
        let f: Vec<&str> = line.split_whitespace().collect();
        match f[..] {
            [] | ["VMID", ..] => continue,
            ["@qm"] => kind = Some(GuestKind::Vm),
            ["@pct"] => kind = Some(GuestKind::Lxc),
            _ => {
                let bad = || format!("proxmox: непонятная строка {line:?}");
                let vmid = f[0].parse().map_err(|_| bad())?;
                let (name, status) = match kind {
                    Some(GuestKind::Vm) if f.len() >= 6 => (f[1..f.len() - 4].join(" "), f[f.len() - 4]),
                    Some(GuestKind::Lxc) if f.len() >= 3 => (f[f.len() - 1].to_owned(), f[1]),
                    Some(GuestKind::Lxc) if f.len() == 2 => (String::new(), f[1]),
                    _ => return Err(bad()),
                };
                guests.push(Guest { vmid, name, status: status.into(), kind: kind.unwrap() });
            }
        }
    }
    Ok(guests)
}

/// Последняя строка `@tcp <код>`, выше — что сказали nc или bash.
fn parse_tcp(lines: &[&str]) -> Result<ProbeResult, String> {
    let lines: Vec<&str> = lines.iter().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
    let (last, detail) = lines.split_last().ok_or("tcp: пустой ответ")?;
    let rc: i32 = last
        .strip_prefix("@tcp ")
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| format!("tcp: непонятный ответ {last:?}"))?;
    let fact = match rc {
        0 => "соединение установлено".to_owned(),
        124 => "таймаут".to_owned(),
        _ if !detail.is_empty() => detail.join("; "),
        _ => format!("нет соединения (код {rc})"),
    };
    Ok(ProbeResult { connected: rc == 0, fact, ..ProbeResult::default() })
}

/// Строка `@http <код> <секунды> <код curl>`.
fn parse_http(lines: &[&str]) -> Result<ProbeResult, String> {
    let line = lines
        .iter()
        .rev()
        .find_map(|l| l.trim().strip_prefix("@http "))
        .ok_or("http: нет строки результата")?;
    let f: Vec<&str> = line.split_whitespace().collect();
    let [code, time, rc] = f[..] else {
        return Err(format!("http: непонятный ответ {line:?}"));
    };
    let code: u16 = code.parse().unwrap_or(0);
    let rc: i32 = rc.parse().map_err(|_| format!("http: непонятный ответ {line:?}"))?;
    Ok(curl_result(code, time, rc))
}

/// Итог одного запроса curl; тела ответа здесь нет.
fn curl_result(code: u16, time: &str, rc: i32) -> ProbeResult {
    if rc != 0 || code == 0 {
        let fact = match rc {
            6 => "имя не разрешилось".to_owned(),
            7 => "соединение отклонено".to_owned(),
            28 => "таймаут".to_owned(),
            35 => "ошибка TLS".to_owned(),
            51 | 60 => "сертификат не принят".to_owned(),
            52 => "пустой ответ".to_owned(),
            56 => "соединение оборвалось".to_owned(),
            _ => format!("ошибка curl {rc}"),
        };
        return ProbeResult { fact, ..ProbeResult::default() };
    }
    // Десятичный разделитель у curl может зависеть от локали.
    let secs: f64 = time.replace(',', ".").parse().unwrap_or(0.0);
    ProbeResult {
        connected: true,
        http_code: Some(code),
        latency_ms: Some((secs * 1000.0).round() as u64),
        fact: format!("HTTP {code}"),
        bodies: Vec::new(),
    }
}

/// Ответ одного запроса `pult_body`: строки помечены `<имя>|`, последняя из них —
/// `@<код http> <секунды> <код curl>`, остальные — тело. Нет итоговой строки — тело обрезано
/// пределом размера: такой ответ не принимаем, а не разбираем половину JSON.
fn body(lines: &[&str], name: &str) -> Result<ProbeResult, String> {
    let prefix = format!("{name}|");
    let own: Vec<&str> = lines.iter().filter_map(|l| l.strip_prefix(prefix.as_str())).collect();
    let (last, text) = own.split_last().ok_or_else(|| format!("{name}: ответа нет"))?;
    let f: Vec<&str> = last.strip_prefix('@').map(|v| v.split_whitespace().collect()).unwrap_or_default();
    let [code, time, rc] = f[..] else {
        return Err(format!("{name}: ответ длиннее {} КБ, обрезан", crate::probes::ollama::MAX_BODY >> 10));
    };
    let rc: i32 = rc.parse().map_err(|_| format!("{name}: непонятный ответ {last:?}"))?;
    let mut result = curl_result(code.parse().unwrap_or(0), time, rc);
    if result.connected {
        result.bodies = vec![text.join("\n")];
    }
    Ok(result)
}

/// Проверка ollama: решает `/api/tags`; `/api/ps` — дополнение, его отсутствие не ошибка.
fn parse_ollama(lines: &[&str]) -> Result<ProbeResult, String> {
    let mut tags = body(lines, "tags")?;
    if let Ok(ps) = body(lines, "ps") {
        if tags.connected && ps.http_code == Some(200) {
            tags.bodies.extend(ps.bodies);
        }
    }
    Ok(tags)
}

fn parse_ask(lines: &[&str]) -> Result<ProbeResult, String> {
    body(lines, "ask")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::Hop;

    /// Формат снят с настоящего вывода; имена, адреса и ключи выдуманы.
    const CHAIN: &str = include_str!("fixtures/chain.txt");

    fn host(id: &str, collectors: Vec<Collector>) -> HostPlan {
        HostPlan {
            id: id.into(),
            hop: Hop { target: "user@10.0.0.2".into(), key: None },
            collectors,
            checks: vec![],
            nested: vec![],
        }
    }

    fn plan() -> HostPlan {
        let mut outer = host("хост-а", vec![Collector::Docker, Collector::Wireguard]);
        outer.checks = vec![
            RemoteCheck::Tcp { host: "10.0.0.2".into(), port: 22, timeout_ms: 3000 },
            RemoteCheck::Tcp { host: "10.0.0.9".into(), port: 9000, timeout_ms: 3000 },
            RemoteCheck::Http { url: "http://10.0.0.2:8080/health".into(), timeout_ms: 5000 },
            RemoteCheck::Http { url: "http://10.0.0.9:8080/".into(), timeout_ms: 5000 },
        ];
        outer.nested = vec![
            host("хост-б", vec![Collector::Proxmox, Collector::Docker]),
            host("хост-в", vec![Collector::Docker]),
        ];
        outer
    }

    fn containers(r: &HostReport) -> &[Container] {
        match &r.docker {
            Some(Outcome::Ok(list)) => list,
            other => panic!("docker: {other:?}"),
        }
    }

    fn by_name<'a>(r: &'a HostReport, name: &str) -> &'a ContainerFacts {
        &containers(r).iter().find(|c| c.name == name).expect(name).facts
    }

    #[test]
    fn banner_before_first_section_is_ignored() {
        let reports = parse_output(&plan(), CHAIN);
        assert_eq!(reports.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["хост-а", "хост-б", "хост-в"]);
        assert_eq!(reports[0].ssh, Outcome::Ok(()));
        assert_eq!(containers(&reports[0]).len(), 6);
    }

    #[test]
    fn stopped_by_sigkill_is_not_oom() {
        let reports = parse_output(&plan(), CHAIN);
        let worker = by_name(&reports[0], "worker");
        assert_eq!((worker.state.as_str(), worker.exit_code, worker.oom_killed), ("exited", Some(137), Some(false)));
        assert_eq!(worker.health, None);
        let cache = by_name(&reports[0], "cache");
        assert_eq!((cache.exit_code, cache.oom_killed, cache.restart_count), (Some(137), Some(true), Some(5)));
        // Нулевое время Go — «не было», а не 1 января первого года.
        assert_eq!(by_name(&reports[0], "app-backend-1").finished_at, None);
        assert_eq!(by_name(&reports[0], "app-db").finished_at.as_deref(), Some("2026-01-01T08:59:58Z"));
    }

    #[test]
    fn compose_service_with_two_instances() {
        let reports = parse_output(&plan(), CHAIN);
        let backends: Vec<_> = containers(&reports[0])
            .iter()
            .filter(|c| c.compose_project.as_deref() == Some("app") && c.compose_service.as_deref() == Some("backend"))
            .map(|c| (c.name.as_str(), c.facts.health.as_deref()))
            .collect();
        assert_eq!(backends, [("app-backend-1", Some("healthy")), ("app-backend-2", Some("starting"))]);
        let worker = containers(&reports[0]).iter().find(|c| c.name == "worker").unwrap();
        assert_eq!((worker.compose_project.as_ref(), worker.compose_service.as_ref()), (None, None));
    }

    #[test]
    fn wireguard_and_proxmox() {
        let reports = parse_output(&plan(), CHAIN);
        let Some(Outcome::Ok(peers)) = &reports[0].wireguard else { panic!("{:?}", reports[0].wireguard) };
        assert_eq!(peers.len(), 2);
        assert_eq!((peers[0].allowed_ips.as_slice(), peers[0].handshake_age_secs), (&["10.0.0.2/32".to_owned()][..], Some(60)));
        assert_eq!((peers[1].allowed_ips.len(), peers[1].handshake_age_secs), (2, None));
        let Some(Outcome::Ok(guests)) = &reports[1].proxmox else { panic!("{:?}", reports[1].proxmox) };
        let got: Vec<_> = guests.iter().map(|g| (g.vmid, g.name.as_str(), g.status.as_str(), g.kind)).collect();
        assert_eq!(
            got,
            [
                (100, "vm-a", "running", GuestKind::Vm),
                (101, "ct-mail", "running", GuestKind::Lxc),
                (102, "ct-old", "stopped", GuestKind::Lxc),
            ]
        );
    }

    #[test]
    fn remote_checks_keep_plan_order() {
        let reports = parse_output(&plan(), CHAIN);
        let facts: Vec<_> = reports[0]
            .checks
            .iter()
            .map(|c| match c {
                Outcome::Ok(p) => (p.connected, p.http_code, p.fact.as_str()),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            facts,
            [
                (true, None, "соединение установлено"),
                (false, None, "nc: connect to 10.0.0.9 port 9000 (tcp) failed: Connection refused"),
                (true, Some(200), "HTTP 200"),
                (false, None, "таймаут"),
            ]
        );
        let Outcome::Ok(http) = &reports[0].checks[2] else { unreachable!() };
        assert_eq!(http.latency_ms, Some(126));
    }

    #[test]
    fn failed_section_is_unknown_not_empty() {
        let reports = parse_output(&plan(), CHAIN);
        let Some(Outcome::Failed { rc: 1, error }) = &reports[1].docker else { panic!("{:?}", reports[1].docker) };
        assert!(error.contains("permission denied"));
        // Соседняя секция того же хоста при этом собрана.
        assert!(matches!(reports[1].proxmox, Some(Outcome::Ok(_))));
    }

    #[test]
    fn nested_host_down_outer_collected() {
        let reports = parse_output(&plan(), CHAIN);
        let Outcome::Failed { rc: 255, error } = &reports[2].ssh else { panic!("{:?}", reports[2].ssh) };
        assert!(error.contains("Connection timed out"));
        assert_eq!(reports[2].docker, Some(Outcome::Missing));
        assert_eq!(reports[1].ssh, Outcome::Ok(()));
        assert_eq!(containers(&reports[0]).len(), 6);
    }

    #[test]
    fn truncated_output_keeps_what_arrived() {
        // Обрыв посреди списка контейнеров внешнего хоста.
        let cut = &CHAIN[..CHAIN.find(r#""name":"/app-db""#).unwrap() + 20];
        let reports = parse_output(&plan(), cut);
        assert_eq!(reports[0].ssh, Outcome::Ok(()));
        assert_eq!(reports[0].docker, Some(Outcome::Missing), "неполный список — не «контейнеры исчезли»");
        assert_eq!(reports[0].wireguard, Some(Outcome::Missing));
        assert!(reports[0].checks.iter().all(|c| *c == Outcome::Missing));
        assert!(reports[1..].iter().all(|r| r.ssh == Outcome::Missing));

        // Обрыв на вложенном хосте: всё внешнее до него цело, у вложенного — последняя секция под сомнением.
        let cut = &CHAIN[..CHAIN.find("ct-old").unwrap()];
        let reports = parse_output(&plan(), cut);
        assert_eq!(containers(&reports[0]).len(), 6);
        assert!(matches!(reports[0].wireguard, Some(Outcome::Ok(_))));
        assert!(reports[0].checks.iter().all(|c| matches!(c, Outcome::Ok(_))));
        assert_eq!(reports[1].ssh, Outcome::Ok(()));
        assert_eq!(reports[1].proxmox, Some(Outcome::Missing));
        assert_eq!(reports[1].docker, Some(Outcome::Missing));
        assert_eq!(reports[2].ssh, Outcome::Missing);
    }

    #[test]
    fn grandchild_cut_when_its_parent_timed_out() {
        let mut outer = host("хост-а", vec![Collector::Docker]);
        let mut middle = host("хост-б", vec![Collector::Docker]);
        middle.nested = vec![host("хост-г", vec![Collector::Docker])];
        outer.nested = vec![middle, host("хост-в", vec![Collector::Docker])];
        let line = r#"{"name":"/x","state":"running","exitCode":0,"oomKilled":false,"health":null,"restartCount":0,"startedAt":"2026-01-01T00:00:00Z","finishedAt":"0001-01-01T00:00:00Z","image":"x","project":"","service":""}"#;
        // `timeout` на хосте-а убил ssh до хоста-б, пока шёл поток хоста-г; хост-в недоступен.
        let out = format!(
            "@@pult 1 host=хост-а section=docker rc=0\n{line}\n\
             @@pult 1 host=хост-б section=docker rc=0\n{line}\n\
             @@pult 1 host=хост-г section=docker rc=0\n{line}\n\
             @@pult 1 host=хост-б section=ssh rc=124\n\
             @@pult 1 host=хост-в section=ssh rc=255\nConnection refused\n\
             @@pult 1 host=хост-а section=end rc=0\n"
        );
        let reports = parse_output(&outer, &out);
        let ids: Vec<_> = reports.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["хост-а", "хост-б", "хост-г", "хост-в"]);
        assert_eq!(containers(&reports[0]).len(), 1);
        // У хоста-б секция не последняя в его потоке: за ней поток его вложенного хоста.
        assert_eq!(containers(&reports[1]).len(), 1);
        assert!(matches!(reports[1].ssh, Outcome::Failed { rc: 124, .. }));
        assert_eq!(reports[2].docker, Some(Outcome::Missing));
        assert_eq!(reports[2].ssh, Outcome::Ok(()));
        assert!(matches!(reports[3].ssh, Outcome::Failed { rc: 255, .. }));
    }

    #[test]
    fn unparsable_line_fails_section() {
        let broken = CHAIN.replace(r#"{"name":"/cache""#, r#"{"name":"/cache"#);
        let reports = parse_output(&plan(), &broken);
        assert!(matches!(&reports[0].docker, Some(Outcome::Failed { rc: 0, .. })));
    }

    /// Находка ревью 3: непонятные строки давали пустой список — «контейнеров нет».
    #[test]
    fn unparsed_lines_are_section_errors_not_empty_lists() {
        assert!(parse_docker(&["что-то непонятное"]).is_err());
        let json = r#"{"name":"/a","state":"running","exitCode":0,"oomKilled":false,"health":null,"restartCount":0,"startedAt":"","finishedAt":"","image":"x","project":"","service":""}"#;
        // Предупреждения docker — известные строки, список при них верен.
        assert_eq!(parse_docker(&["WARNING: No swap limit support", json]).unwrap().len(), 1);
        assert!(parse_proxmox(&["@qm"]).is_err(), "нет части @pct");
        assert!(parse_proxmox(&[]).is_err());
        assert_eq!(parse_proxmox(&["@qm", "      VMID NAME STATUS MEM(MB) BOOTDISK(GB) PID", "@pct", "VMID Status Lock Name"]).unwrap(), vec![]);
    }
}
