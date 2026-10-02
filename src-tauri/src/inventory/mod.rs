//! Инвентарь: узлы, зависимости, проверки (docs/контракт.md, раздел 1).
//! Ключи YAML — по-русски, как их пишет человек; поля в коде — по-английски.

mod source;

pub use source::{key_paths, pull, InventoryState};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Новее этой версии разбирать не беремся: поля могли поменять смысл.
pub const SCHEMA_VERSION: u32 = 1;
pub const FILE_NAME: &str = "инвентарь.yaml";

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Inventory {
    #[serde(rename = "версия_схемы")]
    pub schema: u32,
    #[serde(rename = "узлы", default)]
    pub nodes: Vec<Node>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    #[serde(rename = "название")]
    pub title: String,
    #[serde(rename = "вид")]
    pub kind: NodeKind,
    #[serde(rename = "группа", default)]
    pub group: Option<String>,
    /// Проект, к которому относится сервис: на карте сервисы хоста разложены по папкам проектов.
    #[serde(rename = "проект", default)]
    pub project: Option<String>,
    /// Описан и проверяется, но на карту не выводится: служебное, чьё «работает» никому не нужно
    /// (заглушка «сайт обновляется»). Описан — значит и не «не описан».
    #[serde(rename = "скрыть", default)]
    pub hidden: bool,
    #[serde(rename = "на", default)]
    pub on: Option<String>,
    #[serde(rename = "зависит_от", default)]
    pub depends_on: Vec<String>,
    #[serde(rename = "проверки", default)]
    pub checks: Vec<Check>,
    #[serde(rename = "сбор", default)]
    pub collect: Option<Collect>,
    #[serde(rename = "контейнер", default)]
    pub container: Option<String>,
    #[serde(default)]
    pub compose: Option<Compose>,
    #[serde(rename = "вм", default)]
    pub vm: Option<u32>,
    #[serde(rename = "ожидается", default)]
    pub expected: Expected,
    #[serde(rename = "доступ", default)]
    pub access: Option<Access>,
    #[serde(rename = "ссылки", default)]
    pub links: Vec<Link>,
}

impl Node {
    /// Узел описывает объект на узле `на`, факты о котором приносит сбор.
    pub fn is_bound(&self) -> bool {
        self.container.is_some() || self.compose.is_some() || self.vm.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum NodeKind {
    #[serde(rename = "внешнее")]
    External,
    #[serde(rename = "хост")]
    Host,
    #[serde(rename = "вм")]
    Vm,
    #[serde(rename = "контейнер")]
    Container,
    #[serde(rename = "сервис")]
    Service,
    #[serde(rename = "туннель")]
    Tunnel,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub enum Expected {
    #[default]
    #[serde(rename = "работает")]
    Running,
    #[serde(rename = "остановлен")]
    Stopped,
    #[serde(rename = "любое")]
    Any,
}

/// Одна плоская структура на оба вида: так незнакомые поля внутри проверки тоже
/// попадают в предупреждения (у enum с тегом serde прячет их от serde_ignored).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Check {
    #[serde(rename = "вид")]
    pub kind: CheckKind,
    #[serde(rename = "адрес", default)]
    pub address: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(rename = "ожидать", default)]
    pub expect: Option<Codes>,
    #[serde(rename = "таймаут_мс", default)]
    pub timeout_ms: Option<u64>,
    #[serde(rename = "откуда", default)]
    pub from: Option<String>,
}

impl Check {
    /// Что проверяем, по-человечески: адрес или url.
    pub fn target(&self) -> String {
        self.address.clone().or_else(|| self.url.clone()).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum CheckKind {
    #[serde(rename = "tcp")]
    Tcp,
    #[serde(rename = "http")]
    Http,
}

/// `ожидать: 200` или `ожидать: [200, 204]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Codes {
    One(u16),
    Many(Vec<u16>),
}

impl Codes {
    pub fn list(&self) -> Vec<u16> {
        match self {
            Codes::One(c) => vec![*c],
            Codes::Many(c) => c.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Collect {
    pub ssh: String,
    #[serde(rename = "через", default)]
    pub via: Option<String>,
    #[serde(rename = "ключ", default)]
    pub key: Option<String>,
    #[serde(rename = "что", default)]
    pub what: Vec<Collector>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Collector {
    Docker,
    Wireguard,
    Proxmox,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Compose {
    #[serde(rename = "проект")]
    pub project: String,
    #[serde(rename = "сервис")]
    pub service: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Access {
    #[serde(rename = "как_зайти", default)]
    pub how: Option<String>,
    #[serde(rename = "секрет", default)]
    pub secret: Option<String>,
    #[serde(rename = "у_кого", default)]
    pub who: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Link {
    #[serde(rename = "название")]
    pub title: String,
    pub url: String,
}

/// Разбор одного файла: инвентарь и предупреждения о незнакомых полях.
/// Ссылки между узлами здесь не проверяются — это делает `validate` после слияния.
pub fn parse(text: &str) -> Result<(Inventory, Vec<String>), String> {
    // Версию читаем отдельно и первой: файл новой схемы может не разобраться
    // полностью, а человеку нужна понятная причина, а не ошибка разбора.
    #[derive(Deserialize)]
    struct Head {
        #[serde(rename = "версия_схемы")]
        schema: Option<u32>,
    }
    let head: Head = serde_yaml::from_str(text).map_err(|e| format!("не разбирается YAML: {e}"))?;
    match head.schema {
        None => return Err("нет поля версия_схемы".into()),
        Some(v) if v > SCHEMA_VERSION => {
            return Err(format!(
                "версия_схемы {v} новее поддерживаемой ({SCHEMA_VERSION}): обнови Пульт"
            ))
        }
        Some(_) => {}
    }
    let mut unknown = Vec::new();
    let inv: Inventory = serde_ignored::deserialize(serde_yaml::Deserializer::from_str(text), |path| {
        unknown.push(path.to_string())
    })
    .map_err(|e| e.to_string())?;
    let warnings = unknown_fields(&unknown, &inv);
    Ok((inv, warnings))
}

/// Одна строка на незнакомое поле, а не на каждое его вхождение: новое поле схемы у
/// не обновившихся выглядело бы стеной одинаковых предупреждений, похожей на аварию.
fn unknown_fields(paths: &[String], inv: &Inventory) -> Vec<String> {
    // Поле без номеров («проверки.порт») и номера узлов, где оно встретилось, в порядке файла.
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for path in paths {
        let parts: Vec<&str> = path.split('.').collect();
        let node = match parts.as_slice() {
            ["узлы", i, ..] => i.parse::<usize>().ok(),
            _ => None,
        };
        let skip = usize::from(node.is_some()); // «узлы» в начале пути ничего не добавляет
        let field = parts.iter().filter(|p| p.parse::<usize>().is_err()).skip(skip).copied().collect::<Vec<_>>().join(".");
        let at = match groups.iter_mut().position(|(f, _)| *f == field) {
            Some(at) => at,
            None => {
                groups.push((field, Vec::new()));
                groups.len() - 1
            }
        };
        if let Some(i) = node.filter(|i| !groups[at].1.contains(i)) {
            groups[at].1.push(i);
        }
    }
    let id = |i: usize| format!("«{}»", inv.nodes.get(i).map_or("?", |n| n.id.as_str()));
    groups
        .into_iter()
        .map(|(field, nodes)| {
            let whose = match nodes.len() {
                0 => String::new(),
                1 => format!(" у узла {}", id(nodes[0])),
                2 | 3 => format!(" у узлов {}", nodes.iter().map(|&i| id(i)).collect::<Vec<_>>().join(", ")),
                n if n % 10 == 1 && n % 100 != 11 => format!(" у {n} узла"),
                n => format!(" у {n} узлов"),
            };
            format!("незнакомое поле «{field}»{whose} пропущено: эта версия Пульта его не знает — обнови приложение, если это не опечатка")
        })
        .collect()
}

/// Личные узлы поверх каталога: узел с тем же id заменяет узел каталога.
pub fn merge(mut base: Inventory, personal: Inventory, warnings: &mut Vec<String>) -> Inventory {
    for node in personal.nodes {
        match base.nodes.iter_mut().find(|n| n.id == node.id) {
            Some(slot) => {
                warnings.push(format!("личный узел «{}» заменяет узел каталога", node.id));
                *slot = node;
            }
            None => base.nodes.push(node),
        }
    }
    base
}

/// Уникальность id внутри одного файла — до слияния, пока дубли ещё видны.
pub fn validate_unique(inv: &Inventory) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    let dups: Vec<String> = inv
        .nodes
        .iter()
        .filter(|n| !seen.insert(n.id.as_str()))
        .map(|n| format!("id «{}» повторяется", n.id))
        .collect();
    if dups.is_empty() { Ok(()) } else { Err(dups.join("\n")) }
}

/// Полная проверка по правилам контракта. Любая ошибка отклоняет снимок целиком,
/// поэтому собираем все сразу: чинить по одной за перезапуск утомительно.
pub fn validate(inv: &Inventory) -> Result<(), String> {
    let mut errors = Vec::new();
    let mut by_id: HashMap<&str, &Node> = HashMap::new();
    for n in &inv.nodes {
        if n.id.trim().is_empty() {
            errors.push(format!("узел «{}» без id", n.title));
        } else if by_id.insert(&n.id, n).is_some() {
            errors.push(format!("id «{}» повторяется", n.id));
        }
    }

    let need_node = |errors: &mut Vec<String>, from: &str, field: &str, to: &str, collects: bool| {
        match by_id.get(to) {
            None => errors.push(format!("{from}: {field} ссылается на несуществующий узел «{to}»")),
            Some(t) if collects && t.collect.is_none() => {
                errors.push(format!("{from}: {field} «{to}» — у этого узла нет «сбор»"))
            }
            Some(_) => {}
        }
    };

    for n in &inv.nodes {
        let id = n.id.as_str();
        if let Some(on) = &n.on {
            need_node(&mut errors, id, "на", on, false);
        }
        for dep in &n.depends_on {
            need_node(&mut errors, id, "зависит_от", dep, false);
        }
        if n.is_bound() && n.on.is_none() {
            errors.push(format!("{id}: контейнер, compose или вм без «на» — неизвестно, где их искать"));
        }
        if let Some(c) = &n.collect {
            safe_arg(&mut errors, id, "ssh", &c.ssh);
            if let Some(key) = &c.key {
                safe_arg(&mut errors, id, "ключ", key);
            }
            if let Some(via) = &c.via {
                need_node(&mut errors, id, "через", via, true);
            }
        }
        for (i, ch) in n.checks.iter().enumerate() {
            let at = format!("{id}: проверка {}", i + 1);
            match ch.kind {
                CheckKind::Tcp => match ch.address.as_deref().and_then(|a| a.rsplit_once(':')) {
                    Some((host, port)) if !host.is_empty() && port.parse::<u16>().is_ok() => {}
                    _ => errors.push(format!("{at}: tcp нужен «адрес» вида хост:порт")),
                },
                CheckKind::Http => match ch.url.as_deref() {
                    Some(u) if u.starts_with("http://") || u.starts_with("https://") => {}
                    _ => errors.push(format!("{at}: http нужен «url» с http:// или https://")),
                },
            }
            if let Some(from) = &ch.from {
                need_node(&mut errors, &at, "откуда", from, true);
                // Проверка уйдёт в скрипт сбора: недопустимое значение сорвало бы сбор всей
                // цепочки, поэтому ловим его здесь, по тому же белому списку.
                let ok = match ch.kind {
                    CheckKind::Tcp => ch.address.as_deref().and_then(|a| a.rsplit_once(':')).is_some_and(|(h, _)| crate::collect::is_plain(h)),
                    CheckKind::Http => ch.url.as_deref().is_some_and(crate::collect::is_url),
                };
                if !ok {
                    errors.push(format!("{at}: адрес для проверки на узле содержит недопустимые символы"));
                }
            }
        }
    }

    // Циклы ищем только по целому графу: с битыми ссылками поиск путей бессмыслен.
    if errors.is_empty() {
        errors.extend(find_cycle(inv, &by_id));
        errors.extend(find_via_loop(inv, &by_id));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

/// Значения `ssh` и `ключ` уходят аргументами в ssh: белый список символов не даёт
/// подставить из инвентаря команду, а запрет ведущего «-» — опцию вроде `-F` или `-o`.
/// Список тот же, что у сборщика: расхождение отклонило бы план уже при сборе.
fn safe_arg(errors: &mut Vec<String>, id: &str, field: &str, value: &str) {
    if !crate::collect::is_plain(value) {
        errors.push(format!(
            "{id}: «{field}» содержит недопустимые символы (можно буквы, цифры и @ . _ - / ~ :, не с «-»)"
        ));
    }
}

/// Цикл по `на` + `зависит_от`: с ним «корень проблемы» не определён.
fn find_cycle(inv: &Inventory, by_id: &HashMap<&str, &Node>) -> Option<String> {
    fn visit<'a>(
        id: &'a str,
        by_id: &HashMap<&str, &'a Node>,
        mark: &mut HashMap<&'a str, bool>, // false — на пути обхода, true — пройден
        path: &mut Vec<&'a str>,
    ) -> Option<String> {
        match mark.get(id) {
            Some(true) => return None,
            Some(false) => {
                let start = path.iter().position(|p| *p == id).unwrap_or(0);
                let mut cycle = path[start..].to_vec();
                cycle.push(id);
                return Some(format!("цикл зависимостей: {}", cycle.join(" → ")));
            }
            None => {}
        }
        mark.insert(id, false);
        path.push(id);
        let node = by_id[id];
        for next in node.on.iter().chain(&node.depends_on) {
            if let Some(found) = visit(next, by_id, mark, path) {
                return Some(found);
            }
        }
        path.pop();
        mark.insert(id, true);
        None
    }
    let mut mark = HashMap::new();
    inv.nodes
        .iter()
        .find_map(|n| visit(&n.id, by_id, &mut mark, &mut Vec::new()))
}

/// Петля по `через` заставила бы сборщик ходить по кругу вложенными ssh.
fn find_via_loop(inv: &Inventory, by_id: &HashMap<&str, &Node>) -> Option<String> {
    let via = |id: &str| by_id[id].collect.as_ref().and_then(|c| c.via.as_deref());
    inv.nodes.iter().find_map(|n| {
        let mut chain = vec![n.id.as_str()];
        let mut cur = via(&n.id)?;
        while chain.len() <= inv.nodes.len() {
            if chain.contains(&cur) {
                chain.push(cur);
                return Some(format!("петля сбора по «через»: {}", chain.join(" → ")));
            }
            chain.push(cur);
            cur = via(cur)?;
        }
        None
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(yaml: &str) -> Result<Inventory, String> {
        let (inv, _) = parse(yaml)?;
        validate(&inv).map(|_| inv)
    }

    fn err(yaml: &str) -> String {
        check(yaml).expect_err("инвентарь должен быть отклонён")
    }

    const GOOD: &str = r#"
версия_схемы: 1
узлы:
  - {id: интернет, название: Интернет, вид: внешнее, проверки: [{вид: tcp, адрес: "1.1.1.1:443"}]}
  - id: хост-а
    название: Хост А
    вид: хост
    зависит_от: [интернет]
    сбор: {ssh: "user@a.example.com", ключ: "~/.ssh/id_ed25519", что: [docker]}
  - {id: панель, название: Панель, вид: контейнер, на: хост-а, контейнер: panel,
     проверки: [{вид: http, url: "https://panel.example.com/health", ожидать: [200, 204]}]}
"#;

    #[test]
    fn good_inventory_is_accepted() {
        let inv = check(GOOD).unwrap();
        assert_eq!(inv.nodes.len(), 3);
        assert_eq!(inv.nodes[2].checks[0].expect.as_ref().unwrap().list(), vec![200, 204]);
    }

    #[test]
    fn project_and_hidden_are_known_fields() {
        let (inv, warnings) = parse(
            "версия_схемы: 1\nузлы:\n  - {id: а, название: А, вид: хост}\n  - {id: б, название: Б, вид: контейнер, на: а, контейнер: b, проект: Сайт, скрыть: true}\n",
        )
        .unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(inv.nodes[1].project.as_deref(), Some("Сайт"));
        assert!(inv.nodes[1].hidden && !inv.nodes[0].hidden);
    }

    #[test]
    fn duplicate_id() {
        let e = err("версия_схемы: 1\nузлы:\n  - {id: а, название: А, вид: хост}\n  - {id: а, название: Б, вид: хост}\n");
        assert!(e.contains("«а» повторяется"), "{e}");
    }

    #[test]
    fn broken_reference() {
        let e = err("версия_схемы: 1\nузлы:\n  - {id: а, название: А, вид: хост, зависит_от: [нет-такого]}\n");
        assert!(e.contains("несуществующий узел «нет-такого»"), "{e}");
    }

    #[test]
    fn dependency_cycle() {
        let e = err(r#"
версия_схемы: 1
узлы:
  - {id: а, название: А, вид: хост, зависит_от: [в]}
  - {id: б, название: Б, вид: вм, на: а}
  - {id: в, название: В, вид: сервис, зависит_от: [б]}
"#);
        assert!(e.contains("цикл зависимостей: а → в → б → а"), "{e}");
    }

    #[test]
    fn from_must_point_to_collecting_node() {
        let e = err(r#"
версия_схемы: 1
узлы:
  - {id: а, название: А, вид: хост}
  - {id: б, название: Б, вид: сервис, проверки: [{вид: tcp, адрес: "10.0.0.2:22", откуда: а}]}
"#);
        assert!(e.contains("откуда «а» — у этого узла нет «сбор»"), "{e}");
    }

    #[test]
    fn unsafe_ssh_target() {
        for bad in ["host; rm -rf ~", "$(reboot)", "a b", "-oProxyCommand", "-F/tmp/x"] {
            let yaml = format!(
                "версия_схемы: 1\nузлы:\n  - {{id: а, название: А, вид: хост, сбор: {{ssh: {bad:?}}}}}\n"
            );
            let e = err(&yaml);
            assert!(e.contains("«ssh» содержит недопустимые символы"), "{bad}: {e}");
        }
        let e = err("версия_схемы: 1\nузлы:\n  - {id: а, название: А, вид: хост, сбор: {ssh: a, ключ: \"~/key`id`\"}}\n");
        assert!(e.contains("«ключ» содержит недопустимые символы"), "{e}");
    }

    #[test]
    fn newer_schema_asks_to_update() {
        let e = err("версия_схемы: 2\nузлы: {совсем: другое}\n");
        assert!(e.contains("обнови Пульт"), "{e}");
    }

    #[test]
    fn unknown_kind_is_error_unknown_field_is_warning() {
        assert!(err("версия_схемы: 1\nузлы:\n  - {id: а, название: А, вид: сервер}\n").contains("unknown variant"));
        let (_, warnings) = parse(
            "версия_схемы: 1\nузлы:\n  - {id: а, название: А, вид: хост, цвет: синий, проверки: [{вид: tcp, адрес: \"a:1\", порт: 2}]}\n",
        )
        .unwrap();
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0].contains("цвет"), "{warnings:?}");
    }

    #[test]
    fn unknown_field_is_one_warning_however_many_nodes_have_it() {
        let nodes: String = (0..21).map(|i| format!("  - {{id: у{i}, название: У, вид: хост, приоритет: {i}}}\n")).collect();
        let yaml = format!(
            "версия_схемы: 1\nузлы:\n{nodes}  - {{id: а, название: А, вид: хост, цвет: синий, проверки: [{{вид: tcp, адрес: \"a:1\", порт: 2}}, {{вид: tcp, адрес: \"a:2\", порт: 3}}]}}\nавтор: я\n"
        );
        let (_, warnings) = parse(&yaml).unwrap();
        assert_eq!(warnings.len(), 4, "{warnings:#?}");
        assert!(warnings[0].starts_with("незнакомое поле «приоритет» у 21 узла пропущено"), "{warnings:#?}");
        assert!(warnings[0].contains("обнови приложение"), "{warnings:#?}");
        assert!(warnings[1].starts_with("незнакомое поле «цвет» у узла «а»"), "{warnings:#?}");
        assert!(warnings[2].starts_with("незнакомое поле «проверки.порт» у узла «а»"), "{warnings:#?}");
        assert!(warnings[3].starts_with("незнакомое поле «автор» пропущено"), "{warnings:#?}");
    }

    #[test]
    fn personal_node_replaces_catalog_node() {
        let (base, _) = parse(GOOD).unwrap();
        let (personal, _) =
            parse("версия_схемы: 1\nузлы:\n  - {id: панель, название: Моя панель, вид: сервис}\n  - {id: своё, название: Своё, вид: сервис}\n").unwrap();
        let mut warnings = Vec::new();
        let merged = merge(base, personal, &mut warnings);
        assert_eq!(merged.nodes.len(), 4);
        assert_eq!(merged.nodes[2].title, "Моя панель");
        assert_eq!(warnings.len(), 1);
    }
}
