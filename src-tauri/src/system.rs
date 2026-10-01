//! Внешние программы (ssh, git) и проверка окружения.

use crate::commands::EnvCheck;
use crate::inventory::{key_paths, Inventory, FILE_NAME};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Путь к программе по PATH. Ищем сами, а не полагаемся на оболочку: путь показывается
/// в проверке окружения, и на винде дочерние процессы запускаются без оболочки.
pub fn find(name: &str) -> Option<PathBuf> {
    let file = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(&file))
        .find(|p| p.is_file())
}

/// Дочерний процесс без окна консоли на винде и без унаследованного ввода.
pub fn command(program: &Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}

pub fn check_environment(inventory_path: Option<&str>, inv: &Inventory) -> Vec<EnvCheck> {
    let program = |name: &str, missing: &str| match find(name) {
        Some(p) => EnvCheck { name: name.into(), ok: true, detail: p.display().to_string() },
        None => EnvCheck { name: name.into(), ok: false, detail: missing.into() },
    };
    let mut checks = vec![
        program("ssh", "ssh не найден в PATH: без него не будет сбора с узлов"),
        program("git", "git не найден в PATH: инвентарь не будет обновляться сам"),
    ];

    let inventory = match inventory_path.map(Path::new) {
        None => (false, "путь к каталогу инвентаря не задан".to_string()),
        Some(dir) if !dir.is_dir() => (false, format!("{}: каталога нет", dir.display())),
        Some(dir) => match std::fs::metadata(dir.join(FILE_NAME)) {
            Err(e) => (false, format!("{}: нет {FILE_NAME} ({e})", dir.display())),
            Ok(_) if dir.join(".git").exists() => (true, format!("{}: git-клон, обновляется сам", dir.display())),
            Ok(_) => (true, format!("{}: обычный каталог, без git pull", dir.display())),
        },
    };
    checks.push(EnvCheck { name: "инвентарь".into(), ok: inventory.0, detail: inventory.1 });

    for key in key_paths(inv) {
        let ok = expand_home(&key).is_file();
        let detail = format!("{key}: {}", if ok { "найден" } else { "файла нет" });
        checks.push(EnvCheck { name: "ключ".into(), ok, detail });
    }
    checks
}
