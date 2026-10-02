//! Откуда берётся инвентарь: каталог (часто git-клон), личный файл, принятый снимок.

use super::{merge, parse, validate, validate_unique, Inventory, FILE_NAME};
use crate::{engine::now, store, system};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use time::OffsetDateTime;

/// Личные узлы — второй файл той же схемы в папке данных приложения.
pub const PERSONAL_FILE: &str = "личный-инвентарь.yaml";
const ACCEPTED_FILE: &str = "accepted-inventory.json";
const PULL_TIMEOUT: Duration = Duration::from_secs(30);

/// Снимок, прошедший проверку. Только он и попадает на карту.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Accepted {
    pub inventory: Inventory,
    pub commit: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub loaded_at: OffsetDateTime,
    pub warnings: Vec<String>,
}

#[derive(Debug, Default)]
pub struct InventoryState {
    /// None — карта пуста: путь не задан или ничего ещё не принималось.
    pub accepted: Option<Accepted>,
    /// Почему последняя загрузка отклонена; карта при этом остаётся на `accepted`.
    pub error: Option<String>,
    /// Ошибка git pull — не повод отклонять снимок, только предупреждение.
    pub pull_warning: Option<String>,
}

impl InventoryState {
    /// При старте сначала поднимаем последний принятый снимок: если каталог сейчас
    /// сломан, карта всё равно будет.
    pub fn startup(data_dir: &Path, dir: Option<&Path>) -> Self {
        let accepted = std::fs::read(data_dir.join(ACCEPTED_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice::<Accepted>(&b).ok())
            .filter(|a| validate(&a.inventory).is_ok());
        let mut state = Self { accepted, ..Self::default() };
        state.reload(data_dir, dir);
        state
    }

    pub fn inventory(&self) -> Inventory {
        self.accepted.as_ref().map(|a| a.inventory.clone()).unwrap_or_default()
    }

    /// Перечитать каталог. true — изменилось то, что видит интерфейс (карта или ошибка).
    pub fn reload(&mut self, data_dir: &Path, dir: Option<&Path>) -> bool {
        let before = (self.accepted.clone(), self.error.clone());
        match dir {
            None => {
                self.accepted = None;
                self.error = Some("путь к инвентарю не задан: укажи каталог с инвентарь.yaml в настройках".into());
            }
            Some(dir) => match load(dir, &data_dir.join(PERSONAL_FILE)) {
                Ok(new) => {
                    self.error = None;
                    let same = self.accepted.as_ref().is_some_and(|a| {
                        a.inventory == new.inventory && a.commit == new.commit && a.warnings == new.warnings
                    });
                    if !same {
                        if let Err(e) = serde_json::to_vec_pretty(&new)
                            .map_err(|e| e.to_string())
                            .and_then(|b| store::write_atomic(&data_dir.join(ACCEPTED_FILE), &b).map_err(|e| e.to_string()))
                        {
                            log::warn!("не удалось сохранить принятый инвентарь: {e}");
                        }
                        self.accepted = Some(new);
                    }
                }
                Err(e) => self.error = Some(e),
            },
        }
        before != (self.accepted.clone(), self.error.clone())
    }

    pub fn warnings(&self) -> Vec<String> {
        let mut w = self.accepted.as_ref().map(|a| a.warnings.clone()).unwrap_or_default();
        w.extend(self.pull_warning.clone());
        w
    }
}

fn load(dir: &Path, personal: &Path) -> Result<Accepted, String> {
    let file = dir.join(FILE_NAME);
    let text = std::fs::read_to_string(&file).map_err(|e| format!("не читается {}: {e}", file.display()))?;
    let (mut inventory, mut warnings) = parse(&text).map_err(|e| format!("{FILE_NAME}: {e}"))?;
    // Нет личного файла — норма; не читается — ошибка: иначе его узлы молча пропали бы с карты.
    match std::fs::read_to_string(personal) {
        Ok(text) => {
            let (mine, w) = parse(&text).map_err(|e| format!("{PERSONAL_FILE}: {e}"))?;
            // Внутри файла id уникальны до слияния: при слиянии второй узел молча заменил бы первый.
            validate_unique(&mine).map_err(|e| format!("{PERSONAL_FILE}: {e}"))?;
            warnings.extend(w);
            inventory = merge(inventory, mine, &mut warnings);
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("не читается {}: {e}", personal.display())),
    }
    validate(&inventory)?;
    Ok(Accepted { inventory, commit: commit(dir), loaded_at: now(), warnings })
}

fn is_git_clone(dir: &Path) -> bool {
    dir.join(".git").exists()
}

fn git(dir: &Path) -> std::process::Command {
    let mut cmd = system::command(&system::find("git").unwrap_or_else(|| PathBuf::from("git")));
    // Без терминала git не должен ничего спрашивать: висящий запрос пароля съест предел времени.
    cmd.arg("-C").arg(dir).env("GIT_TERMINAL_PROMPT", "0");
    cmd
}

fn commit(dir: &Path) -> Option<String> {
    if !is_git_clone(dir) {
        return None;
    }
    let out = git(dir).args(["rev-parse", "--short", "HEAD"]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `git pull --ff-only`, если каталог — git-клон. Ошибка — текст предупреждения.
pub async fn pull(dir: &Path) -> Result<(), String> {
    if !is_git_clone(dir) {
        return Ok(());
    }
    let mut cmd = tokio::process::Command::from(git(dir));
    cmd.args(["pull", "--ff-only", "--quiet"]).kill_on_drop(true);
    let out = tokio::time::timeout(PULL_TIMEOUT, cmd.output())
        .await
        .map_err(|_| format!("git pull: нет ответа за {} с, работаю на прежней копии", PULL_TIMEOUT.as_secs()))?
        .map_err(|e| format!("git pull не запустился: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let last = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    Err(format!("git pull не удался, работаю на прежней копии: {last}"))
}

/// Локальные ключи сбора (без `через`) — для проверки окружения.
pub fn key_paths(inv: &Inventory) -> Vec<String> {
    let mut keys: Vec<String> = inv
        .nodes
        .iter()
        .filter_map(|n| n.collect.as_ref())
        .filter(|c| c.via.is_none())
        .filter_map(|c| c.key.clone())
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pult-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn rejected_file_does_not_replace_accepted() {
        let catalog = temp_dir("catalog");
        let data = temp_dir("data");
        let good = "версия_схемы: 1\nузлы:\n  - {id: а, название: А, вид: хост}\n";
        std::fs::write(catalog.join(FILE_NAME), good).unwrap();
        let mut state = InventoryState::startup(&data, Some(&catalog));
        assert!(state.error.is_none());
        assert_eq!(state.inventory().nodes.len(), 1);

        std::fs::write(catalog.join(FILE_NAME), "версия_схемы: 1\nузлы:\n  - {id: а, название: А, вид: хост, на: нет}\n").unwrap();
        assert!(state.reload(&data, Some(&catalog)));
        assert!(state.error.as_deref().unwrap().contains("несуществующий"), "{:?}", state.error);
        assert_eq!(state.inventory().nodes[0].on, None, "действует прежний снимок");

        // После перезапуска с тем же сломанным каталогом поднимается последний принятый.
        let restarted = InventoryState::startup(&data, Some(&catalog));
        assert!(restarted.error.is_some());
        assert_eq!(restarted.inventory().nodes.len(), 1);
        assert_eq!(restarted.inventory().nodes[0].id, "а");

        let _ = std::fs::remove_dir_all(catalog);
        let _ = std::fs::remove_dir_all(data);
    }

    #[test]
    fn no_path_means_empty_map_with_error() {
        let data = temp_dir("nopath");
        let state = InventoryState::startup(&data, None);
        assert!(state.inventory().nodes.is_empty());
        assert!(state.error.as_deref().unwrap().contains("не задан"));
        let _ = std::fs::remove_dir_all(data);
    }

    /// Пример в репозитории обязан проходить проверку: по нему работает режим разработки.
    #[test]
    fn example_inventory_is_valid() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples");
        let data = temp_dir("example");
        let state = InventoryState::startup(&data, Some(&dir));
        assert_eq!(state.error, None);
        assert!(state.accepted.as_ref().unwrap().warnings.is_empty(), "{:?}", state.warnings());
        assert!(state.inventory().nodes.len() >= 15);
        let _ = std::fs::remove_dir_all(data);
    }

    /// Свой каталог — тем же кодом, что у приложения:
    /// `PULT_INVENTORY=путь/к/каталогу cargo test own_inventory_is_valid -- --ignored --nocapture`
    #[test]
    #[ignore = "нужен каталог в PULT_INVENTORY"]
    fn own_inventory_is_valid() {
        let dir = PathBuf::from(std::env::var("PULT_INVENTORY").expect("задай PULT_INVENTORY"));
        let data = temp_dir("own");
        let state = InventoryState::startup(&data, Some(&dir));
        assert_eq!(state.error, None);
        let inv = state.inventory();
        let hidden: Vec<&str> = inv.nodes.iter().filter(|n| n.hidden).map(|n| n.id.as_str()).collect();
        println!("узлов: {}, скрыто: {hidden:?}, предупреждений: {:?}", inv.nodes.len(), state.warnings());
        let _ = std::fs::remove_dir_all(data);
    }

    /// Находка ревью 6: нечитаемый личный файл считался отсутствующим — его узлы молча пропадали.
    #[test]
    fn unreadable_personal_file_is_an_error_not_absence() {
        let catalog = temp_dir("personal-catalog");
        let data = temp_dir("personal-data");
        std::fs::write(catalog.join(FILE_NAME), "версия_схемы: 1\nузлы:\n  - {id: а, название: А, вид: хост}\n").unwrap();
        std::fs::create_dir_all(data.join(PERSONAL_FILE)).unwrap(); // каталог вместо файла — не читается
        let state = InventoryState::startup(&data, Some(&catalog));
        assert!(state.error.as_deref().is_some_and(|e| e.contains(PERSONAL_FILE)), "{:?}", state.error);
        let _ = std::fs::remove_dir_all(catalog);
        let _ = std::fs::remove_dir_all(data);
    }

    /// Находка ревью 7: дубль id внутри личного файла сливался до проверки и не ловился.
    #[test]
    fn duplicate_id_inside_personal_file_is_rejected() {
        let catalog = temp_dir("dup-catalog");
        let data = temp_dir("dup-data");
        std::fs::write(catalog.join(FILE_NAME), "версия_схемы: 1\nузлы:\n  - {id: а, название: А, вид: хост}\n").unwrap();
        std::fs::write(data.join(PERSONAL_FILE), "версия_схемы: 1\nузлы:\n  - {id: б, название: Б1, вид: сервис}\n  - {id: б, название: Б2, вид: сервис}\n").unwrap();
        let state = InventoryState::startup(&data, Some(&catalog));
        assert!(state.error.as_deref().is_some_and(|e| e.contains("«б» повторяется")), "{:?}", state.error);
        let _ = std::fs::remove_dir_all(catalog);
        let _ = std::fs::remove_dir_all(data);
    }
}
