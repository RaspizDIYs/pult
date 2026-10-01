//! Файлы в папке данных приложения: настройки и история переходов состояний.

use crate::commands::{HistoryEntry, Settings};
use crate::engine::OwnStatus;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;
use time::OffsetDateTime;

const SETTINGS_FILE: &str = "settings.json";
// ponytail: JSONL, SQLite когда понадобятся запросы
const HISTORY_FILE: &str = "history.jsonl";

/// Запись через временный файл: оборванная запись не должна оставить битый файл.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

pub fn load_settings(dir: &Path) -> Settings {
    std::fs::read(dir.join(SETTINGS_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save_settings(dir: &Path, settings: &Settings) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?;
    write_atomic(&dir.join(SETTINGS_FILE), &bytes).map_err(|e| format!("настройки не сохранились: {e}"))
}

#[derive(Serialize, Deserialize)]
pub struct Transition {
    pub id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub own: OwnStatus,
    pub fact: String,
}

pub fn append_history(dir: &Path, transitions: &[Transition]) -> std::io::Result<()> {
    if transitions.is_empty() {
        return Ok(());
    }
    let mut lines = String::new();
    for t in transitions {
        lines.push_str(&serde_json::to_string(t).map_err(std::io::Error::other)?);
        lines.push('\n');
    }
    // Одной записью: строки разных циклов не перемешаются.
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(HISTORY_FILE))?
        .write_all(lines.as_bytes())
}

/// Последние `limit` переходов узла, новые первыми.
// ponytail: читает файл целиком; ротация или SQLite, когда история вырастет до мегабайт
pub fn history(dir: &Path, id: &str, limit: usize) -> std::io::Result<Vec<HistoryEntry>> {
    let text = match std::fs::read_to_string(dir.join(HISTORY_FILE)) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    Ok(text
        .lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<Transition>(l).ok())
        .filter(|t| t.id == id)
        .take(limit)
        .map(|t| HistoryEntry { at: t.at, own: t.own, fact: t.fact })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_tail_newest_first() {
        let dir = std::env::temp_dir().join(format!("pult-test-history-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _ = std::fs::remove_file(dir.join(HISTORY_FILE));
        let at = crate::engine::now();
        let t = |id: &str, own, fact: &str| Transition { id: id.into(), at, own, fact: fact.into() };
        append_history(&dir, &[t("а", OwnStatus::Ok, "1"), t("б", OwnStatus::Fail, "2")]).unwrap();
        append_history(&dir, &[t("а", OwnStatus::Fail, "3"), t("а", OwnStatus::Ok, "4")]).unwrap();
        let facts: Vec<String> = history(&dir, "а", 2).unwrap().into_iter().map(|e| e.fact).collect();
        assert_eq!(facts, ["4", "3"]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
