//! Уведомления по правилу 7 контракта: только подтверждённые смены состояния и только
//! для корней — отказ и восстановление. Логика отдельно от отправки, чтобы её проверять.

use crate::engine::{NodeState, OwnStatus};
use crate::inventory::Inventory;
use std::collections::{HashMap, HashSet};
use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;

/// Первое состояние подтверждается не раньше второго цикла: в первом только измерили.
const FIRST_CONFIRMED_CYCLE: u64 = 2;
/// Больше стольких уведомлений за цикл — уже пачка: шлём одно сводное.
const MAX_SEPARATE: usize = 3;

#[derive(Debug, PartialEq)]
pub struct Alert {
    pub title: String,
    pub body: String,
}

#[derive(Debug, Default)]
pub struct Notifier {
    started: bool,
    /// Корни, об отказе которых уже сказали: по ним ждём восстановления.
    alerted: HashSet<String>,
}

impl Notifier {
    pub fn on_cycle(&mut self, cycle: u64, inv: &Inventory, states: &HashMap<String, NodeState>) -> Vec<Alert> {
        let line = |id: &str, s: &NodeState| {
            let title = inv.nodes.iter().find(|n| n.id == id).map_or(id, |n| n.title.as_str());
            format!("{title}: {}", s.fact)
        };
        let failed_roots: Vec<(&str, &NodeState)> = inv
            .nodes
            .iter()
            .filter_map(|n| states.get(&n.id).map(|s| (n.id.as_str(), s)))
            .filter(|(_, s)| s.confirmed && s.own == OwnStatus::Fail && s.is_root)
            .collect();

        // При старте — одно сводное вместо пачки: всё, что уже лежит, — не новость.
        if !self.started {
            if cycle < FIRST_CONFIRMED_CYCLE {
                return Vec::new();
            }
            self.started = true;
            self.alerted = failed_roots.iter().map(|(id, _)| id.to_string()).collect();
            let lines: Vec<String> = failed_roots.iter().map(|(id, s)| line(id, s)).collect();
            return summary(format!("Сломано: {}", lines.len()), lines).into_iter().collect();
        }

        self.alerted.retain(|id| states.contains_key(id));
        let mut alerts = Vec::new();
        for (id, s) in &failed_roots {
            if self.alerted.insert(id.to_string()) {
                alerts.push(Alert { title: "Сломалось".into(), body: line(id, s) });
            }
        }
        for n in &inv.nodes {
            let Some(s) = states.get(&n.id) else { continue };
            if s.confirmed && s.own == OwnStatus::Ok && self.alerted.remove(&n.id) {
                alerts.push(Alert { title: "Снова работает".into(), body: line(&n.id, s) });
            }
        }
        if alerts.len() > MAX_SEPARATE {
            let lines = alerts.iter().map(|a| format!("{} — {}", a.title, a.body)).collect();
            return summary(format!("Пульт: изменений {}", alerts.len()), lines).into_iter().collect();
        }
        alerts
    }
}

fn summary(title: String, lines: Vec<String>) -> Option<Alert> {
    if lines.is_empty() {
        return None;
    }
    let mut body: Vec<String> = lines.iter().take(MAX_SEPARATE).cloned().collect();
    if lines.len() > MAX_SEPARATE {
        body.push(format!("и ещё {}", lines.len() - MAX_SEPARATE));
    }
    Some(Alert { title, body: body.join("\n") })
}

pub fn send(app: &AppHandle, alerts: Vec<Alert>) {
    for a in alerts {
        // В логе — чтобы на «уведомление не пришло» было что ответить.
        log::info!("уведомление: {} — {}", a.title, a.body.replace('\n', "; "));
        if let Err(e) = app.notification().builder().title(&a.title).body(&a.body).show() {
            log::warn!("уведомление не показано: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory::parse;

    fn inv() -> Inventory {
        let yaml = "версия_схемы: 1\nузлы:\n  - {id: хост, название: Хост, вид: хост}\n  - {id: панель, название: Панель задач, вид: контейнер, на: хост, контейнер: p}\n";
        parse(yaml).unwrap().0
    }

    fn st(id: &str, own: OwnStatus, confirmed: bool, is_root: bool, fact: &str) -> (String, NodeState) {
        let s = NodeState {
            id: id.into(),
            own,
            confirmed,
            fact: fact.into(),
            hints: vec![],
            is_root,
            blocked_by: vec![],
            checks: vec![],
            container: None,
            measured_at: None,
            since: None,
            since_cycle: 0,
        };
        (id.into(), s)
    }

    #[test]
    fn one_summary_at_start_then_confirmed_root_changes_only() {
        let inv = inv();
        let mut n = Notifier::default();
        let fail = |confirmed| {
            HashMap::from([
                st("хост", OwnStatus::Ok, true, false, "порт 22: открыт"),
                st("панель", OwnStatus::Fail, confirmed, true, "контейнер остановлен, код 137"),
            ])
        };
        assert!(n.on_cycle(1, &inv, &fail(false)).is_empty(), "в первом цикле ничего не подтверждено");
        let start = n.on_cycle(2, &inv, &fail(true));
        assert_eq!(start, [Alert { title: "Сломано: 1".into(), body: "Панель задач: контейнер остановлен, код 137".into() }]);
        assert!(n.on_cycle(3, &inv, &fail(true)).is_empty(), "о том же не повторяем");

        let ok = |confirmed| {
            HashMap::from([
                st("хост", OwnStatus::Ok, true, false, "порт 22: открыт"),
                st("панель", OwnStatus::Ok, confirmed, false, "контейнер работает"),
            ])
        };
        assert!(n.on_cycle(4, &inv, &ok(false)).is_empty(), "не подтверждено — молчим");
        assert_eq!(n.on_cycle(5, &inv, &ok(true))[0].body, "Панель задач: контейнер работает");

        // Отказ не корня — не уведомляем; отказ корня — уведомляем один раз.
        let child = HashMap::from([
            st("хост", OwnStatus::Fail, true, true, "порт 22: таймаут 3 с"),
            st("панель", OwnStatus::Fail, true, false, "GET /health: таймаут 5 с"),
        ]);
        assert_eq!(
            n.on_cycle(6, &inv, &child),
            [Alert { title: "Сломалось".into(), body: "Хост: порт 22: таймаут 3 с".into() }]
        );
    }

    #[test]
    fn quiet_start_when_nothing_is_broken() {
        let mut n = Notifier::default();
        let ok = HashMap::from([st("хост", OwnStatus::Ok, true, false, "открыт")]);
        assert!(n.on_cycle(2, &inv(), &ok).is_empty());
    }
}
