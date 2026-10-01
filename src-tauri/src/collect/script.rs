//! Скрипт сбора: POSIX sh, только чтение. Значения плана проходят белый список и всё равно
//! экранируются: одна защита не должна держаться на безошибочности другой.

use super::{Collector, Hop, HostPlan, PlanError, RemoteCheck};

/// Предел одного вложенного перехода: зависший дом не должен съесть общие 45 секунд.
const NESTED_LIMIT_SECS: u32 = 15;

/// Общие функции, повторяются на каждом уровне цепочки (вложенный хост получает свой полный скрипт).
/// - `pult_t` — предел времени: `timeout`, если на хосте есть совместимый (проверяем запуском, а не
///   `command -v`: у старого busybox другой синтаксис), иначе свой сторож. Без предела вложенный ssh
///   к зависшему хосту съел бы общие 45 секунд. Фоновой команде sh подменяет stdin на /dev/null,
///   поэтому вход (скрипт вложенного хоста) передаётся явно через fd 4; сторож отвязан от всех
///   каналов, иначе подстановка `$(…)` ждала бы и его. Сработавший сторож — код 124, как у `timeout`.
/// - `pult_sec` — секция печатается одним `printf` уже после команды: код выхода известен только
///   в конце, а одна запись не перемешивается с параллельными проверками.
/// - `docker ps` только перечисляет id: его `{{json .}}` отдаёт команду запуска, монтирования и все
///   метки (в метках Traefik бывают хэши паролей). Из `inspect` шаблон берёт лишь поля
///   `ContainerFacts`, имя и две метки compose; окружение и команда запуска не выводятся вовсе.
/// - `wg show all dump` и `showconf` печатают приватный ключ, поэтому только два безопасных вывода.
/// - Проверки отвечают кодом 0, если проба состоялась (итог — в строке `@tcp`/`@http`), и не нулём,
///   только если её нечем выполнить: «узнать не удалось» и «порт закрыт» — разные факты.
/// - Вложенный ssh с `UpdateHostKeys=no`: иначе клиент на промежуточном хосте мог бы дописать его
///   `known_hosts`, а сбор на серверах ничего не пишет. Keepalive — как у внешнего: оборванный
///   канал замечается за 10 секунд, а не по пределу.
const PRELUDE: &str = r#"pult_tm=
timeout 1 true >/dev/null 2>&1 && pult_tm=1
pult_t() {
  _l=$1; shift
  if [ -n "$pult_tm" ]; then timeout "$_l" "$@"; return; fi
  { "$@" <&4 4<&- & _p=$!
    ( trap 'kill $! 2>/dev/null; exit 0' TERM; sleep "$_l" & wait $!; kill "$_p" 2>/dev/null ) </dev/null >/dev/null 2>&1 4<&- &
    _w=$!
    wait "$_p"; _r=$?
    if kill "$_w" 2>/dev/null; then return $_r; fi
    return 124; } 4<&0
}
pult_sec() {
  _h=$1; _s=$2; shift 2
  _o=$("$@" 2>&1); _r=$?
  printf '@@pult 1 host=%s section=%s rc=%s\n%s\n' "$_h" "$_s" "$_r" "$_o"
}
pult_docker() {
  _ids=$(pult_t 10 docker ps -aq --no-trunc) || return
  [ -z "$_ids" ] && return 0
  pult_t 10 docker inspect --format '{"name":{{json .Name}},"state":{{json .State.Status}},"exitCode":{{json .State.ExitCode}},"oomKilled":{{json .State.OOMKilled}},"health":{{if .State.Health}}{{json .State.Health.Status}}{{else}}null{{end}},"restartCount":{{json .RestartCount}},"startedAt":{{json .State.StartedAt}},"finishedAt":{{json .State.FinishedAt}},"image":{{json .Config.Image}},"project":{{json (index .Config.Labels "com.docker.compose.project")}},"service":{{json (index .Config.Labels "com.docker.compose.service")}}}' $_ids
}
pult_wireguard() {
  date +%s && pult_t 5 wg show all latest-handshakes && echo @ips && pult_t 5 wg show all allowed-ips
}
pult_proxmox() {
  echo @qm && pult_t 10 qm list && echo @pct && pult_t 10 pct list
}
pult_tcp() {
  if command -v nc >/dev/null 2>&1; then
    _n=$(pult_t $(($3 + 1)) nc -v -z -w "$3" "$1" "$2" 2>&1); _c=$?
    # nc без -z отвечает справкой и кодом 1 — как закрытый порт. Это «нечем проверить», не отказ.
    case $_n in
      *[Uu]sage:*|*[Ii]nvalid\ option*|*[Ii]llegal\ option*|*[Uu]nrecognized\ option*) ;;
      *) printf '%s\n' "$_n"; echo "@tcp $_c"; return 0 ;;
    esac
  fi
  if command -v bash >/dev/null 2>&1; then
    pult_t "$3" bash -c 'exec 3<>"/dev/tcp/$0/$1"' "$1" "$2"
    echo "@tcp $?"; return 0
  fi
  echo 'нечем проверить порт: нет nc с -z и нет bash'; return 127
}
pult_http() {
  command -v curl >/dev/null 2>&1 || { echo 'нет curl'; return 127; }
  _c=$(curl -s -g -o /dev/null -w '%{http_code} %{time_total}' --max-time "$2" "$1")
  echo "@http $_c $?"
}
pult_ssh() {
  _h=$1; _l=$2; _g=$3; shift 3
  { _e=$(pult_t "$_l" ssh -T -o BatchMode=yes -o ConnectTimeout=5 -o ServerAliveInterval=5 -o ServerAliveCountMax=2 -o UpdateHostKeys=no "$@" "$_g" sh -s 2>&1 1>&3 3>&-); _r=$?; } 3>&1
  printf '@@pult 1 host=%s section=ssh rc=%s\n%s\n' "$_h" "$_r" "$_e"
}
"#;

/// Скрипт для внешнего хоста плана; вложенные хосты встроены в него here-doc'ами.
pub fn build_script(plan: &HostPlan) -> Result<String, PlanError> {
    validate(plan)?;
    let mut out = String::new();
    host_script(plan, 0, &mut out);
    Ok(out)
}

fn host_script(plan: &HostPlan, depth: usize, out: &mut String) {
    let id = quote(&plan.id);
    out.push_str(PRELUDE);
    // Всё тело одной составной командой: shell разбирает его целиком до запуска, и ни одна
    // команда не дочитает из stdin остаток скрипта.
    out.push_str("{\n");
    for collector in &plan.collectors {
        let name = match collector {
            Collector::Docker => "docker",
            Collector::Wireguard => "wireguard",
            Collector::Proxmox => "proxmox",
        };
        out.push_str(&format!("pult_sec {id} {name} pult_{name}\n"));
    }
    // Проверки параллельно: при упавшем доме десяток таймаутов подряд не уложился бы в предел цикла.
    for (i, check) in plan.checks.iter().enumerate() {
        let call = match check {
            RemoteCheck::Tcp { host, port, timeout_ms } => {
                format!("pult_tcp {} {port} {}", quote(host), secs(*timeout_ms))
            }
            RemoteCheck::Http { url, timeout_ms } => {
                format!("pult_http {} {}", quote(url), secs(*timeout_ms))
            }
        };
        out.push_str(&format!("pult_sec {id} check.{i} {call} &\n"));
    }
    out.push_str("wait\n");
    // Вложенные хосты — по очереди: их крупные секции не должны перемежаться в общем выводе.
    for nested in &plan.nested {
        let eof = format!("PULT_EOF_{}", depth + 1);
        let key = nested.hop.key.as_deref().map(|k| format!(" -i {}", quote(k)));
        out.push_str(&format!(
            "pult_ssh {} {NESTED_LIMIT_SECS} {}{} <<'{eof}'\n",
            quote(&nested.id),
            quote(&nested.hop.target),
            key.unwrap_or_default()
        ));
        host_script(nested, depth + 1, out);
        out.push_str(&format!("{eof}\n"));
    }
    // Метка конца: без неё последняя секция хоста могла оборваться на середине.
    out.push_str(&format!("printf '@@pult 1 host=%s section=end rc=0\\n' {id}\n"));
    out.push_str("} </dev/null\n");
}

/// Скрипт логов для `hops[0]`: на последнем хосте `docker logs -f`, на промежуточных — `exec ssh`.
pub(super) fn logs_script(hops: &[Hop], container: &str, tail: u32) -> Result<String, PlanError> {
    if hops.is_empty() {
        return Err(PlanError("логи: пустая цепочка ssh".into()));
    }
    for hop in hops {
        validate_hop("логи", hop)?;
    }
    check("логи", "имя контейнера", container, is_plain)?;
    // docker logs — в фоне; на переднем плане ждём его самого, а сторож читает stdin канала
    // (копия в fd 3: у фоновой команды stdin заменён на /dev/null). Закрыли вкладку — ssh закрыл
    // stdin — сторож гасит docker logs, и на сервере не остаётся висящего процесса. `exit` обязателен:
    // под `sh -s` оболочка иначе пошла бы читать следующую команду из того же открытого stdin.
    let mut script = format!(
        "{{\nexec 3<&0\ndocker logs --tail {tail} -f {} 2>&1 3<&- & p=$!\n\
         {{ cat <&3 >/dev/null 2>&1; kill $p 2>/dev/null; }} >/dev/null 2>&1 &\n\
         exec 3<&-\nwait $p; exit\n}}\n",
        quote(container)
    );
    // На промежуточном хосте stdin и есть канал отмены, поэтому скрипт идёт аргументом `sh -c`.
    for hop in hops[1..].iter().rev() {
        let key = hop.key.as_deref().map(|k| format!(" -i {}", quote(k)));
        script = format!(
            "exec ssh -T -o BatchMode=yes -o ConnectTimeout=5 -o ServerAliveInterval=5 -o ServerAliveCountMax=2 -o UpdateHostKeys=no{} {} {}\n",
            key.unwrap_or_default(),
            quote(&hop.target),
            quote(&format!("sh -c {}", quote(&script)))
        );
    }
    Ok(script)
}

fn validate(plan: &HostPlan) -> Result<(), PlanError> {
    let id = &plan.id;
    check(id, "id узла", id, is_plain)?;
    validate_hop(id, &plan.hop)?;
    for c in &plan.checks {
        match c {
            RemoteCheck::Tcp { host, .. } => check(id, "адрес tcp-проверки", host, is_plain)?,
            RemoteCheck::Http { url, .. } => check(id, "url http-проверки", url, is_url)?,
        }
    }
    plan.nested.iter().try_for_each(validate)
}

fn validate_hop(owner: &str, hop: &Hop) -> Result<(), PlanError> {
    check(owner, "цель ssh", &hop.target, is_plain)?;
    match &hop.key {
        Some(key) => check(owner, "ключ ssh", key, is_plain),
        None => Ok(()),
    }
}

fn check(owner: &str, what: &str, value: &str, ok: fn(&str) -> bool) -> Result<(), PlanError> {
    if ok(value) {
        Ok(())
    } else {
        Err(PlanError(format!("узел «{owner}»: недопустимое значение ({what}): {value:?}")))
    }
}

/// Белый список контракта: буквы, цифры, `@ . _ - / ~ :`. Ведущий `-` запрещён отдельно:
/// иначе значение стало бы опцией ssh или nc (`-oProxyCommand=…`, `-E файл`).
pub(crate) fn is_plain(v: &str) -> bool {
    !v.is_empty()
        && !v.starts_with('-')
        && v.chars().all(|c| c.is_alphanumeric() || "@._-/~:".contains(c))
}

/// Для url шире: запрос и `%`-кодирование. Только http(s): иначе curl прочитал бы `file://`.
pub(crate) fn is_url(v: &str) -> bool {
    (v.starts_with("http://") || v.starts_with("https://"))
        && v.chars().all(|c| c.is_alphanumeric() || "@._-/~:?&=%+,".contains(c))
}

fn quote(v: &str) -> String {
    format!("'{}'", v.replace('\'', r"'\''"))
}

fn secs(ms: u32) -> u32 {
    ms.div_ceil(1000).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(id: &str, target: &str) -> HostPlan {
        HostPlan {
            id: id.into(),
            hop: Hop { target: target.into(), key: None },
            collectors: vec![Collector::Docker],
            checks: vec![],
            nested: vec![],
        }
    }

    #[test]
    fn rejects_shell_metacharacters_everywhere() {
        let bad = ["host a", "host'a", "host\"a", "a;reboot", "$(reboot)", "`reboot`", "-oProxyCommand=x", ""];
        for v in bad {
            let mut p = host("хост-а", v);
            assert!(build_script(&p).is_err(), "цель {v:?}");
            p = host(v, "user@10.0.0.2");
            assert!(build_script(&p).is_err(), "id {v:?}");
            p = host("хост-а", "user@10.0.0.2");
            p.hop.key = Some(v.into());
            assert!(build_script(&p).is_err(), "ключ {v:?}");
            p = host("хост-а", "user@10.0.0.2");
            p.checks = vec![RemoteCheck::Tcp { host: v.into(), port: 22, timeout_ms: 3000 }];
            assert!(build_script(&p).is_err(), "tcp {v:?}");
            p = host("хост-а", "user@10.0.0.2");
            p.checks = vec![RemoteCheck::Http { url: format!("https://example.com/{v}"), timeout_ms: 3000 }];
            // В пути url пустота и ведущий дефис законны, остальное — нет.
            assert!(v.is_empty() || v.starts_with('-') || build_script(&p).is_err(), "url {v:?}");
            // Ошибка во вложенном хосте отклоняет весь скрипт.
            p = host("хост-а", "user@10.0.0.2");
            p.nested = vec![host("хост-б", v)];
            assert!(build_script(&p).is_err(), "вложенный {v:?}");
            assert!(logs_script(&[Hop { target: "host-a".into(), key: None }], v, 10).is_err());
        }
        let mut p = host("хост-а", "user@10.0.0.2");
        p.checks = vec![RemoteCheck::Http { url: "file:///etc/passwd".into(), timeout_ms: 3000 }];
        assert!(build_script(&p).is_err());
    }

    #[test]
    fn accepts_contract_values() {
        let mut p = host("хост-а", "user@10.0.0.2");
        p.hop.key = Some("~/.ssh/id_ed25519".into());
        p.checks = vec![
            RemoteCheck::Tcp { host: "10.0.0.3".into(), port: 587, timeout_ms: 3000 },
            RemoteCheck::Http { url: "https://tasks.example.com/health?full=1&x=%20".into(), timeout_ms: 2500 },
        ];
        p.nested = vec![host("хост-б", "host-b")];
        let s = build_script(&p).unwrap();
        assert!(s.contains("pult_sec 'хост-а' check.0 pult_tcp '10.0.0.3' 587 3 &"));
        assert!(s.contains("pult_http 'https://tasks.example.com/health?full=1&x=%20' 3 &"));
        assert!(s.contains("pult_ssh 'хост-б' 15 'host-b' <<'PULT_EOF_1'"));
        assert!(!s.contains("wg show all dump") && !s.contains("showconf"));
    }

    /// Синтаксис под dash — это /bin/sh на Debian и Ubuntu, где скрипт и выполняется.
    #[cfg(unix)]
    #[test]
    fn script_parses_in_posix_shells() {
        use std::io::Write;
        let mut p = host("хост-а", "user@10.0.0.2");
        p.collectors = vec![Collector::Docker, Collector::Wireguard, Collector::Proxmox];
        p.checks = vec![RemoteCheck::Tcp { host: "10.0.0.3".into(), port: 22, timeout_ms: 3000 }];
        let mut inner = host("хост-б", "user@10.0.0.4");
        inner.nested = vec![host("хост-в", "user@10.0.0.5")];
        p.nested = vec![inner];
        let hops = [
            Hop { target: "host-a".into(), key: None },
            Hop { target: "user@10.0.0.4".into(), key: Some("/root/.ssh/id_x".into()) },
        ];
        let scripts = [build_script(&p).unwrap(), logs_script(&hops, "app-web-1", 50).unwrap()];
        for shell in ["/bin/sh", "/bin/dash", "/usr/bin/dash"] {
            if !std::path::Path::new(shell).exists() {
                continue;
            }
            for script in &scripts {
                let mut child = std::process::Command::new(shell)
                    .arg("-n")
                    .stdin(std::process::Stdio::piped())
                    .spawn()
                    .unwrap();
                child.stdin.take().unwrap().write_all(script.as_bytes()).unwrap();
                assert!(child.wait().unwrap().success(), "{shell}:\n{script}");
            }
        }
    }

    /// Запуск куска скрипта под dash (или sh) с подделками в начале PATH.
    #[cfg(unix)]
    fn run_sh(body: &str, stubs: &[(&str, &str)]) -> (String, std::time::Duration) {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("pult-sh-{}-{}", std::process::id(), stubs.first().map_or("none", |s| s.0)));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, text) in stubs {
            std::fs::write(dir.join(name), text).unwrap();
            std::fs::set_permissions(dir.join(name), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let shell = ["/bin/dash", "/usr/bin/dash"].into_iter().find(|p| std::path::Path::new(p).exists()).unwrap_or("/bin/sh");
        let path = format!("{}:{}", dir.display(), std::env::var("PATH").unwrap_or_default());
        let started = std::time::Instant::now();
        let out = std::process::Command::new(shell).arg("-c").arg(format!("{PRELUDE}\n{body}")).env("PATH", path).output().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        (String::from_utf8_lossy(&out.stdout).into_owned(), started.elapsed())
    }

    /// Находка ревью 2: без совместимого `timeout` предел не действовал вовсе.
    #[cfg(unix)]
    #[test]
    fn limit_holds_without_timeout_and_nested_ssh_has_keepalive() {
        let (out, took) = run_sh("pult_t 1 sleep 4; echo \"rc=$?\"", &[("timeout", "#!/bin/sh\nexit 1\n")]);
        assert!(took < std::time::Duration::from_secs(3), "предел не сработал: {took:?}");
        assert!(out.contains("rc=124"), "{out}");
        let mut p = host("хост-а", "user@10.0.0.2");
        p.nested = vec![host("хост-б", "user@10.0.0.4")];
        let s = build_script(&p).unwrap();
        let line = s.lines().find(|l| l.contains("ssh -T") && l.contains("UpdateHostKeys")).unwrap();
        assert!(line.contains("ServerAliveInterval=5") && line.contains("ServerAliveCountMax=2"), "{line}");
    }

    /// Находка ревью 4: nc без `-z` давал «@tcp 1» — ложный «порт закрыт».
    #[cfg(unix)]
    #[test]
    fn nc_without_z_is_not_a_closed_port() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let fake_nc = "#!/bin/sh\necho 'usage: nc [-46DdhklnrStUuv] [-i interval] [-p source_port]' >&2\nexit 1\n";
        let (out, _) = run_sh(&format!("pult_tcp 127.0.0.1 {port} 2; echo \"rc=$?\""), &[("nc", fake_nc)]);
        // Либо запасной путь установил соединение, либо проба честно не состоялась (код ≠ 0).
        assert!(out.contains("@tcp 0") || !out.contains("rc=0"), "{out}");
        drop(listener);
    }
}
