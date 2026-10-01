//! Ядро приложения: состояние, расписание циклов, обновление инвентаря, события.

use crate::commands::{
    EnvCheck, HistoryEntry, InventoryInfo, LogEnd, LogLines, NodeView, Settings, Snapshot, StatesEvent, EVENT_LOG,
    EVENT_LOG_END, EVENT_SNAPSHOT, EVENT_STATES,
};
use crate::engine::facts::{CheckKey, CheckResult, CycleGate, Facts};
use crate::engine::{self, NodeState};
use crate::inventory::{self, Inventory, InventoryState};
use crate::notify::Notifier;
use crate::{adapter, collect, notify, probes, store, system, tray};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};
use tauri::{AppHandle, Emitter};
use time::OffsetDateTime;
use tokio::sync::{oneshot, Notify};

/// Шаг, с которым планировщик смотрит на часы. Короткий, чтобы сон машины
/// замечался быстро, а внеочередная проверка не ждала.
const STEP: Duration = Duration::from_secs(5);
/// Разрыв по настенным часам больше шага на столько — машина спала.
const WAKE_GAP: Duration = Duration::from_secs(10);
const PULL_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// Сбор по ssh — раз в минуту: дороже локальных проверок и нагружает серверы.
const COLLECT_INTERVAL: Duration = Duration::from_secs(60);
/// Строк логов в одном событии: хвост в сотни строк не должен уходить сотней событий.
const LOG_BATCH: usize = 200;

pub struct Monitor {
    app: AppHandle,
    data_dir: PathBuf,
    state: Mutex<State>,
    recheck: Notify,
    collect_now: Notify,
    /// Открытые потоки логов: закрытие отправителя — отмена.
    logs: Mutex<HashMap<String, oneshot::Sender<()>>>,
    next_log: AtomicU64,
}

struct State {
    settings: Settings,
    inventory: InventoryState,
    facts: Facts,
    /// Номер последнего начатого цикла.
    cycle: u64,
    gate: CycleGate,
    states: HashMap<String, NodeState>,
    taken_at: OffsetDateTime,
    notifier: Notifier,
    /// Что последний раз показано в трее; None — ещё ничего.
    tray_roots: Option<usize>,
    /// Номер последнего начатого сбора и сколько завершено.
    collect_cycle: u64,
    collects_done: u64,
    collect_gate: CycleGate,
    /// Узлы последнего отправленного снимка: неописанные контейнеры приходят и уходят.
    node_ids: Vec<String>,
}

impl Monitor {
    pub fn start(app: AppHandle, data_dir: PathBuf) -> Arc<Self> {
        if let Err(e) = std::fs::create_dir_all(&data_dir) {
            log::error!("папка данных {} не создаётся: {e}", data_dir.display());
        }
        let state = State::new(&data_dir, store::load_settings(&data_dir));
        // Настройка — правда: если автозапуск сняли или включили мимо Пульта, вернём как в ней.
        if let Err(e) = system::set_autostart(&app, state.settings.autostart) {
            log::warn!("{e}");
        }
        let monitor = Arc::new(Self {
            app,
            data_dir,
            state: Mutex::new(state),
            recheck: Notify::new(),
            collect_now: Notify::new(),
            logs: Mutex::new(HashMap::new()),
            next_log: AtomicU64::new(1),
        });
        monitor.emit(EVENT_SNAPSHOT, monitor.snapshot());
        tauri::async_runtime::spawn(monitor.clone().schedule());
        tauri::async_runtime::spawn(monitor.clone().collect_loop());
        tauri::async_runtime::spawn(monitor.clone().pull_loop());
        monitor
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // Паника внутри цикла не должна навсегда выключить карту.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn snapshot(&self) -> Snapshot {
        self.lock().snapshot()
    }

    /// Без id — внеочередной полный цикл. С id — сразу локальные проверки одного узла,
    /// не дожидаясь цикла: человек нажал «проверить» и ждёт ответа про этот узел.
    /// Сбор запускается в обоих случаях: контейнеры и проверки «откуда» знает только он.
    pub fn recheck(self: &Arc<Self>, id: Option<String>) {
        self.collect_now.notify_one();
        match id {
            None => self.recheck.notify_one(),
            Some(id) => {
                tauri::async_runtime::spawn(self.clone().recheck_node(id));
            }
        }
    }

    async fn recheck_node(self: Arc<Self>, id: String) {
        let Some(node) = self.lock().inventory.inventory().nodes.into_iter().find(|n| n.id == id) else { return };
        if node.checks.iter().all(|c| c.from.is_some()) {
            return; // локальных проверок нет — ответит сбор
        }
        let results = probes::run_local(std::slice::from_ref(&node)).await;
        let mut s = self.lock();
        // Пока шли проверки, карта могла смениться — тогда номера проверок уже о другом.
        if !s.inventory.inventory().nodes.contains(&node) {
            return;
        }
        s.facts.record_local(results);
        let states = s.evaluate(&self.data_dir);
        let cycle = s.cycle;
        self.after_evaluate(&mut s, cycle, states);
    }

    /// Поток `docker logs` контейнера. Строки — событиями `pult://log`, конец — `pult://log-end`.
    pub fn open_logs(self: &Arc<Self>, id: &str, tail: u32) -> Result<String, String> {
        let (hops, container) = {
            let s = self.lock();
            adapter::log_target(&s.effective_inventory(), &s.facts, id)?
        };
        let ssh = system::find("ssh");
        let stream = collect::stream_logs(ssh.as_deref(), &hops, &container, tail).map_err(|e| e.to_string())?;
        let stream_id = format!("log-{}", self.next_log.fetch_add(1, Ordering::Relaxed));
        let (cancel, cancelled) = oneshot::channel();
        self.logs.lock().unwrap_or_else(|e| e.into_inner()).insert(stream_id.clone(), cancel);
        tauri::async_runtime::spawn(self.clone().forward_logs(stream_id.clone(), stream, cancelled));
        Ok(stream_id)
    }

    /// Неизвестный поток — не ошибка: он мог уже кончиться сам.
    pub fn close_logs(&self, stream_id: &str) {
        self.logs.lock().unwrap_or_else(|e| e.into_inner()).remove(stream_id);
    }

    /// Окно закрыли — вкладок логов больше нет, держать `docker logs -f` на серверах незачем.
    pub fn close_all_logs(&self) {
        self.logs.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    async fn forward_logs(self: Arc<Self>, stream_id: String, stream: collect::LogStream, mut cancelled: oneshot::Receiver<()>) {
        let collect::LogStream { mut lines, done } = stream;
        loop {
            let first = tokio::select! {
                line = lines.recv() => line,
                // Отправитель закрыт (close_logs) — отмена.
                _ = &mut cancelled => None,
            };
            let Some(first) = first else { break };
            let mut batch = vec![first];
            while batch.len() < LOG_BATCH {
                match lines.try_recv() {
                    Ok(line) => batch.push(line),
                    Err(_) => break,
                }
            }
            self.emit(EVENT_LOG, LogLines { stream_id: stream_id.clone(), lines: batch });
        }
        // Бросить приёмник — сигнал сборщику: ssh получит EOF, `docker logs` на сервере погаснет.
        drop(lines);
        let error = match done.await {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(e),
            Err(e) => Some(e.to_string()),
        };
        self.close_logs(&stream_id);
        self.emit(EVENT_LOG_END, LogEnd { stream_id, error });
    }

    pub fn history(&self, id: &str, limit: usize) -> Result<Vec<HistoryEntry>, String> {
        store::history(&self.data_dir, id, limit).map_err(|e| format!("история не читается: {e}"))
    }

    pub fn settings(&self) -> Settings {
        self.lock().settings.clone()
    }

    pub fn set_settings(&self, mut settings: Settings) -> Result<Settings, String> {
        settings.inventory_path = settings.inventory_path.map(|p| p.trim().to_string()).filter(|p| !p.is_empty());
        if settings.autostart != self.lock().settings.autostart {
            system::set_autostart(&self.app, settings.autostart)?;
        }
        store::save_settings(&self.data_dir, &settings)?;
        let mut s = self.lock();
        let path_changed = s.settings.inventory_path != settings.inventory_path;
        s.settings = settings.clone();
        if path_changed {
            self.reload(&mut s);
        }
        Ok(settings)
    }

    pub fn check_environment(&self) -> Vec<EnvCheck> {
        let s = self.lock();
        system::check_environment(s.settings.inventory_path.as_deref(), &s.inventory.inventory())
    }

    fn emit(&self, event: &str, payload: impl serde::Serialize + Clone) {
        if let Err(e) = self.app.emit(event, payload) {
            log::warn!("{event} не отправлен: {e}");
        }
    }

    /// Перечитать каталог. true — снимок изменился и уже отправлен интерфейсу.
    fn reload(&self, s: &mut MutexGuard<'_, State>) -> bool {
        let Some(map_changed) = s.reload(&self.data_dir) else { return false };
        if map_changed {
            self.after_change(s);
            self.recheck.notify_one();
            self.collect_now.notify_one();
        }
        self.emit(EVENT_SNAPSHOT, s.snapshot());
        true
    }

    /// Циклы идут строго друг за другом: следующий начинается только после предыдущего.
    async fn schedule(self: Arc<Self>) {
        let mut last_cycle: Option<SystemTime> = None;
        let mut rechecked = false;
        let mut woke = false;
        loop {
            let now = SystemTime::now();
            let due = last_cycle.is_none_or(|t| now.duration_since(t).map_or(true, |d| d >= probes::INTERVAL));
            if due || rechecked || woke {
                last_cycle = Some(now);
                self.cycle(woke.then(engine::now)).await;
            }
            let before = SystemTime::now();
            rechecked = tokio::time::timeout(STEP, self.recheck.notified()).await.is_ok();
            // На маке монотонные часы во сне стоят, на винде идут; разрыв настенных часов
            // за один шаг ловит сон в обоих случаях.
            woke = SystemTime::now().duration_since(before).is_ok_and(|gap| gap > STEP + WAKE_GAP);
            if woke {
                log::info!("машина просыпалась: все измерения устарели, внеочередной цикл");
                self.collect_now.notify_one();
            }
        }
    }

    async fn cycle(&self, woke_at: Option<OffsetDateTime>) {
        let (inv, cycle) = self.lock().begin_cycle(woke_at);
        let results = probes::run_local(&inv.nodes).await;
        let mut s = self.lock();
        let Some(states) = s.finish_cycle(cycle, results, &self.data_dir) else { return };
        self.after_evaluate(&mut s, cycle, states);
    }

    /// Сбор по ssh: цепочки параллельно, сборы друг за другом. Ответ каждой цепочки ложится
    /// в факты сразу, как пришёл: медленный дом не задерживает данные с прода.
    async fn collect_loop(self: Arc<Self>) {
        loop {
            self.collect_cycle().await;
            let _ = tokio::time::timeout(COLLECT_INTERVAL, self.collect_now.notified()).await;
        }
    }

    async fn collect_cycle(&self) {
        let (inv, cycle) = {
            let mut s = self.lock();
            s.collect_cycle += 1;
            (s.inventory.inventory(), s.collect_cycle)
        };
        let plans = adapter::plans(&inv);
        if plans.is_empty() {
            return;
        }
        let ssh = system::find("ssh");
        let started = Instant::now();
        let mut set = tokio::task::JoinSet::new();
        for plan in plans {
            let ssh = ssh.clone();
            set.spawn(async move {
                let result = collect::collect(ssh.as_deref(), &plan).await;
                (plan, result)
            });
        }
        while let Some(done) = set.join_next().await {
            let Ok((plan, result)) = done else { continue };
            let mut s = self.lock();
            if !s.collect_gate.accepts(cycle) {
                return; // сменился инвентарь — ответы о старой карте
            }
            let facts = adapter::host_facts(&inv, &plan, result, engine::now(), COLLECT_INTERVAL);
            s.facts.hosts.extend(facts);
        }
        let mut s = self.lock();
        if !s.collect_gate.accepts(cycle) {
            return;
        }
        s.collect_gate.finish(cycle);
        s.collects_done += 1;
        let failed: Vec<&String> = s.facts.hosts.iter().filter(|(_, f)| f.result.is_err()).map(|(id, _)| id).collect();
        log::info!("сбор {cycle}: {:.1} с, не удалось с {} из {} узлов", started.elapsed().as_secs_f64(), failed.len(), s.facts.hosts.len());
        let states = s.evaluate(&self.data_dir);
        let local_cycle = s.cycle;
        self.after_evaluate(&mut s, local_cycle, states);
    }

    /// После каждой оценки по новым измерениям: уведомления, трей, событие.
    fn after_evaluate(&self, s: &mut State, cycle: u64, states: Vec<NodeState>) {
        let inv = s.effective_inventory();
        // Сводка при старте — когда подтвердились и проверки, и сбор (если он есть).
        let collects = inv.nodes.iter().any(|n| n.collect.is_some());
        let ready = s.cycle >= 2 && (!collects || s.collects_done >= 2);
        let alerts = s.notifier.on_cycle(ready, &inv, &s.states);
        // Выключенные уведомления не копятся: уведомитель всё равно идёт за состоянием,
        // и включение не вывалит старое разом.
        if s.settings.notifications {
            notify::send(&self.app, alerts);
        }
        self.after_change(s);
        // Сбор нашёл или потерял неописанный контейнер — интерфейсу нужен новый список узлов.
        let ids: Vec<String> = inv.nodes.iter().map(|n| n.id.clone()).collect();
        if s.node_ids != ids {
            s.node_ids = ids;
            self.emit(EVENT_SNAPSHOT, s.snapshot());
        }
        self.emit(EVENT_STATES, StatesEvent { cycle, states });
    }

    /// Трей — по текущим корням, без ожидания подтверждения: это сводка, а не тревога.
    fn after_change(&self, s: &mut State) {
        let roots = s.states.values().filter(|st| st.is_root).count();
        if s.tray_roots != Some(roots) {
            s.tray_roots = Some(roots);
            tray::update(&self.app, roots);
        }
    }

    async fn pull_loop(self: Arc<Self>) {
        loop {
            let dir = self.lock().settings.inventory_path.clone();
            if let Some(dir) = dir {
                let warning = inventory::pull(Path::new(&dir)).await.err();
                if let Some(w) = &warning {
                    log::warn!("{w}");
                }
                let mut s = self.lock();
                let warning_changed = s.inventory.pull_warning != warning;
                s.inventory.pull_warning = warning;
                if !self.reload(&mut s) && warning_changed {
                    self.emit(EVENT_SNAPSHOT, s.snapshot());
                }
            }
            tokio::time::sleep(PULL_INTERVAL).await;
        }
    }
}

impl State {
    fn new(data_dir: &Path, settings: Settings) -> Self {
        let inventory = InventoryState::startup(data_dir, settings.inventory_path.as_deref().map(Path::new));
        if let Some(e) = &inventory.error {
            log::warn!("инвентарь не принят: {e}");
        }
        let mut state = State {
            settings,
            inventory,
            facts: Facts { local_interval: probes::INTERVAL, ..Facts::default() },
            cycle: 0,
            gate: CycleGate::default(),
            states: HashMap::new(),
            taken_at: engine::now(),
            notifier: Notifier::default(),
            tray_roots: None,
            collect_cycle: 0,
            collects_done: 0,
            collect_gate: CycleGate::default(),
            node_ids: Vec::new(),
        };
        state.evaluate(data_dir);
        state
    }

    /// None — ничего не изменилось; Some(true) — сменилась карта, Some(false) — только ошибка.
    fn reload(&mut self, data_dir: &Path) -> Option<bool> {
        let before = self.inventory.inventory();
        let dir = self.settings.inventory_path.clone();
        if !self.inventory.reload(data_dir, dir.as_deref().map(Path::new)) {
            return None;
        }
        if let Some(e) = &self.inventory.error {
            log::warn!("инвентарь не принят: {e}");
        }
        let after = self.inventory.inventory();
        if after == before {
            return Some(false);
        }
        // Результат привязан к номеру проверки: оставляем только у проверок, которые
        // не поменялись. Сбросить всё — и история получила бы «неизвестно» по всем узлам.
        let check = |inv: &Inventory, (id, i): &CheckKey| {
            inv.nodes.iter().find(|n| &n.id == id).and_then(|n| n.checks.get(*i)).cloned()
        };
        self.facts.local.retain(|k, _| check(&before, k).is_some() && check(&before, k) == check(&after, k));
        // То же со сбором: факты узла остаются, только если его план не изменился.
        self.facts.hosts.retain(|id, _| adapter::same_plan(&before, &after, id));
        // Идущие цикл и сбор проверяют старую карту — их ответы отбросят шлюзы.
        self.gate.finish(self.cycle);
        self.collect_gate.finish(self.collect_cycle);
        self.evaluate(data_dir);
        Some(true)
    }

    fn begin_cycle(&mut self, woke_at: Option<OffsetDateTime>) -> (Inventory, u64) {
        self.cycle += 1;
        if woke_at.is_some() {
            self.facts.woke_at = woke_at;
        }
        (self.inventory.inventory(), self.cycle)
    }

    /// Ответы цикла — в факты, затем оценка. None — цикл закрыт раньше (сменился
    /// инвентарь), ответы относятся к старой карте и отброшены.
    fn finish_cycle(&mut self, cycle: u64, results: Vec<(CheckKey, CheckResult)>, data_dir: &Path) -> Option<Vec<NodeState>> {
        if !self.gate.accepts(cycle) {
            return None;
        }
        self.facts.record_local(results);
        self.gate.finish(cycle);
        Some(self.evaluate(data_dir))
    }

    /// Принятый инвентарь плюс неописанные контейнеры, которые увидел сбор.
    fn effective_inventory(&self) -> Inventory {
        let mut inv = self.inventory.inventory();
        let extra = adapter::undeclared(&inv, &self.facts);
        inv.nodes.extend(extra);
        inv
    }

    /// Оценить, запомнить, дописать переходы в историю. Возвращает изменившиеся состояния.
    fn evaluate(&mut self, data_dir: &Path) -> Vec<NodeState> {
        let now = engine::now();
        let inv = self.effective_inventory();
        let new = engine::evaluate(&inv, &self.facts, &self.states, self.cycle, now);
        let changed: Vec<NodeState> = new.iter().filter(|n| self.states.get(&n.id) != Some(*n)).cloned().collect();
        // Первое состояние узла — не переход: иначе каждый запуск писал бы «неизвестно» всем.
        let transitions: Vec<store::Transition> = new
            .iter()
            .filter(|n| self.states.get(&n.id).is_some_and(|p| p.own != n.own))
            .map(|n| store::Transition { id: n.id.clone(), at: now, own: n.own, fact: n.fact.clone() })
            .collect();
        if let Err(e) = store::append_history(data_dir, &transitions) {
            log::warn!("история не записалась: {e}");
        }
        if !transitions.is_empty() {
            let list: Vec<String> = transitions.iter().map(|t| format!("{} → {:?}", t.id, t.own)).collect();
            log::info!("цикл {}: {}", self.cycle, list.join(", "));
        }
        self.states = new.into_iter().map(|n| (n.id.clone(), n)).collect();
        self.taken_at = now;
        changed
    }

    fn snapshot(&self) -> Snapshot {
        let inv = self.effective_inventory();
        let accepted = self.inventory.accepted.as_ref();
        let declared: HashSet<&str> = accepted.iter().flat_map(|a| a.inventory.nodes.iter().map(|n| n.id.as_str())).collect();
        Snapshot {
            cycle: self.cycle,
            taken_at: self.taken_at,
            inventory: InventoryInfo {
                path: self.settings.inventory_path.clone(),
                commit: accepted.and_then(|a| a.commit.clone()),
                loaded_at: accepted.map(|a| a.loaded_at),
                error: self.inventory.error.clone(),
                warnings: self.inventory.warnings(),
            },
            nodes: inv
                .nodes
                .iter()
                .map(|n| NodeView { undeclared: !declared.contains(n.id.as_str()), ..NodeView::new(n, &inv) })
                .collect(),
            states: self.states.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_data(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pult-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Имена полей снимка — как в TS-типах контракта.
    #[test]
    fn snapshot_json_matches_contract() {
        let data = temp_data("snapshot");
        let state = State::new(&data, Settings::default());
        let json = serde_json::to_value(state.snapshot()).unwrap();
        for key in ["cycle", "takenAt", "inventory", "nodes", "states"] {
            assert!(json.get(key).is_some(), "{key}");
        }
        for key in ["path", "commit", "loadedAt", "error", "warnings"] {
            assert!(json["inventory"].get(key).is_some(), "inventory.{key}");
        }
        let node = &json["nodes"][0];
        for key in ["id", "title", "kind", "group", "on", "dependsOn", "access", "links", "undeclared", "hasLogs"] {
            assert!(node.get(key).is_some(), "node.{key}");
        }
        assert_eq!(node["kind"], "внешнее");
        let st = &json["states"]["интернет"];
        for key in ["id", "own", "confirmed", "fact", "hints", "isRoot", "blockedBy", "checks", "container", "measuredAt", "since"] {
            assert!(st.get(key).is_some(), "state.{key}");
        }
        assert!(st.get("sinceCycle").is_none());
        assert_eq!(st["own"], "unknown");
        let _ = std::fs::remove_dir_all(data);
    }

    #[test]
    fn inventory_change_keeps_results_of_unchanged_checks() {
        let data = temp_data("reload");
        let catalog = temp_data("reload-catalog");
        let yaml = |port: u16| {
            format!("версия_схемы: 1\nузлы:\n  - {{id: а, название: А, вид: хост, проверки: [{{вид: tcp, адрес: \"a:1\"}}]}}\n  - {{id: б, название: Б, вид: хост, проверки: [{{вид: tcp, адрес: \"b:{port}\"}}]}}\n")
        };
        std::fs::write(catalog.join(inventory::FILE_NAME), yaml(1)).unwrap();
        let settings = Settings { inventory_path: Some(catalog.display().to_string()), ..Settings::default() };
        let mut state = State::new(&data, settings);
        let (inv, cycle) = state.begin_cycle(None);
        let results = inv
            .nodes
            .iter()
            .map(|n| ((n.id.clone(), 0), probes::tests::ok_result(&n.checks[0])))
            .collect();
        state.finish_cycle(cycle, results, &data).unwrap();

        // Следующий цикл начался, и тут поменялась проверка узла «б».
        let (_, in_flight) = state.begin_cycle(None);
        std::fs::write(catalog.join(inventory::FILE_NAME), yaml(2)).unwrap();
        assert_eq!(state.reload(&data), Some(true));
        assert_eq!(state.states["а"].own, engine::OwnStatus::Ok, "проверка «а» не менялась");
        assert_eq!(state.states["б"].own, engine::OwnStatus::Unknown);
        assert!(state.finish_cycle(in_flight, Vec::new(), &data).is_none(), "ответы старой карты отброшены");
        let history = store::history(&data, "а", 10).unwrap();
        assert_eq!(history.len(), 1, "у «а» только переход в ok: {:?}", history.iter().map(|h| &h.fact).collect::<Vec<_>>());

        std::fs::write(catalog.join(inventory::FILE_NAME), "битый: [").unwrap();
        assert_eq!(state.reload(&data), Some(false), "ошибка видна, карта прежняя");
        assert!(state.inventory.error.is_some());
        let _ = std::fs::remove_dir_all(data);
        let _ = std::fs::remove_dir_all(catalog);
    }

    /// Живой снимок по examples/: настоящие tcp и http с этой машины.
    /// `cargo test live_example_snapshot -- --ignored --nocapture`
    #[test]
    #[ignore = "ходит в сеть"]
    fn live_example_snapshot() {
        let data = temp_data("live");
        let mut state = State::new(&data, Settings::default());
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        for _ in 0..2 {
            let (inv, cycle) = state.begin_cycle(None);
            let results = rt.block_on(probes::run_local(&inv.nodes));
            state.finish_cycle(cycle, results, &data).unwrap();
        }
        let snapshot = state.snapshot();
        let mut ids: Vec<_> = snapshot.nodes.iter().map(|n| n.id.clone()).collect();
        ids.sort();
        for id in ids {
            let s = &snapshot.states[&id];
            println!("{id:12} {:9} root={:5} confirmed={:5} blockedBy={:?} — {}", format!("{:?}", s.own), s.is_root, s.confirmed, s.blocked_by, s.fact);
        }
        println!("{}", serde_json::to_string_pretty(&snapshot.states["интернет"]).unwrap());
        assert_eq!(snapshot.cycle, 2);
        assert_eq!(snapshot.states.len(), snapshot.nodes.len());
        assert_eq!(snapshot.states["копии"].own, engine::OwnStatus::Unchecked);
        let _ = std::fs::remove_dir_all(data);
    }
}
