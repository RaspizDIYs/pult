mod adapter;
mod collect;
mod commands;
mod engine;
mod inventory;
mod monitor;
mod notify;
mod probes;
mod sources;
mod store;
mod system;
mod tray;

use tauri::{AppHandle, Manager, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_log::{log, Target, TargetKind};
use tauri_plugin_updater::UpdaterExt;

/// С этим флагом Пульт запускает автозапуск: при входе в систему окно не нужно, хватит трея.
const HIDDEN_FLAG: &str = "--hidden";

pub fn run() {
    // Диагностические флаги нужны, чтобы проверить автообновление скриптом, без окна.
    let cli_apply = std::env::args().find_map(|arg| match arg.as_str() {
        "--check-update" => Some(false),
        "--apply-update" => Some(true),
        _ => None,
    });
    let hidden = std::env::args().any(|arg| arg == HIDDEN_FLAG);

    tauri::Builder::default()
        .plugin(
            // Только в файл: stdout занят выводом диагностических флагов.
            tauri_plugin_log::Builder::new()
                .clear_targets()
                .target(Target::new(TargetKind::LogDir { file_name: None }))
                .level(log::LevelFilter::Info)
                .build(),
        )
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![HIDDEN_FLAG]),
        ))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::get_snapshot,
            commands::recheck,
            commands::get_history,
            commands::get_settings,
            commands::set_settings,
            commands::check_environment,
            commands::get_update_blocker,
            commands::open_logs,
            commands::close_logs,
            commands::open_url,
            sources::fleet::get_fleet,
        ])
        .on_window_event(|window, event| {
            // Закрытие окна только прячет его: проверки и трей работают дальше, выход — из меню трея.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
                // Вкладки логов закрылись вместе с окном: `docker logs -f` на серверах не нужен.
                if let Some(core) = window.try_state::<std::sync::Arc<monitor::Monitor>>() {
                    core.close_all_logs();
                }
            }
        })
        .setup(move |app| {
            let Some(apply) = cli_apply else {
                // Ядро и трей — только в обычном режиме: флагам обновления они не нужны.
                tray::create(app.handle())?;
                let data_dir = app.path().app_data_dir()?;
                app.manage(monitor::Monitor::start(app.handle().clone(), data_dir));
                app.manage(sources::fleet::Fleet::start(app.handle().clone()));
                // Окно создаём сами (в конфиге create: false), чтобы в режиме флагов его не было вовсе.
                WebviewWindowBuilder::from_config(app.handle(), &app.config().app.windows[0])?
                    .visible(!hidden)
                    .build()?;
                return Ok(());
            };
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            attach_parent_console();
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let code = match update_cli(&handle, apply).await {
                    Ok(()) => 0,
                    Err(err) => {
                        let err = explain_update_error(&err.to_string());
                        log::error!("обновление: {err}");
                        eprintln!("error={err}");
                        1
                    }
                };
                handle.exit(code);
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("не удалось запустить приложение")
        .run(|_app, _event| {
            // Мак: щелчок по значку в доке, когда окно спрятано, должен его вернуть.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = _event {
                tray::show_window(_app);
            }
        });
}

/// Вывод в формате ключ=значение, чтобы скрипт мог разобрать его grep'ом; то же — в лог,
/// иначе по логу не понять, прошла ли проверка.
async fn update_cli(app: &AppHandle, apply: bool) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let current = app.package_info().version.to_string();
    println!("current={current}");
    // Во временной копии только для чтения заменять нечего: установка упала бы на замене бандла.
    // Отказ сразу, а не «когда найдётся обновление»: скрипту нужен один и тот же ответ на этой машине.
    if let Some(blocker) = update_blocker().filter(|_| apply) {
        return Err(blocker.into());
    }
    let update = app.updater()?.check().await?;
    let available = update.as_ref().map_or("", |u| u.version.as_str());
    println!("available={available}");
    log::info!("обновление: установлена {current}, доступна {}", if available.is_empty() { "та же" } else { available });
    let Some(update) = update.filter(|_| apply) else {
        return Ok(());
    };
    let version = update.version.clone();
    println!("installing={version}");
    log::info!("обновление: ставлю {version}");
    // Без перезапуска: иначе установщик NSIS снова запустит нас с --apply-update.
    // На Windows install() сам завершает процесс, пока установщик работает.
    update
        .restart_after_install(false)
        .download_and_install(|_, _| {}, || {})
        .await?;
    println!("installed={version}");
    log::info!("обновление: {version} установлена");
    Ok(())
}

/// Мак запускает приложение с карантином (скопированное `cp -R`, открытое из «Загрузок») из временной
/// копии только для чтения: `.../AppTranslocation/<UUID>/d/Pult.app`. Заменить её на месте нельзя.
/// Сравниваем компонент пути целиком, чтобы не принять за копию каталог с похожим именем.
fn is_translocated(exe: &std::path::Path) -> bool {
    exe.components().any(|c| c.as_os_str() == "AppTranslocation")
}

/// Почему обновление не поставить ещё до попытки (`None` — можно пробовать).
/// Текст один на окно и на `--apply-update`: интерфейс берёт его через команду `get_update_blocker`.
fn update_blocker() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    (cfg!(target_os = "macos") && is_translocated(&exe)).then(|| {
        "Пульт запущен из временной копии macOS: так система открывает приложение, скачанное или \
         скопированное с карантином, — заменить его на месте нельзя. Закрой Пульт, перетащи Pult.app \
         в «Программы» через Finder (или вынеси из «Программ» и верни обратно) и открой снова."
            .to_string()
    })
}

/// Сырой текст ошибки замены бандла ничего не говорит человеку — добавляем, что делать.
/// Та же фраза — в src/lib/updater.ts для установки из окна.
fn explain_update_error(e: &str) -> String {
    let hint = if e.contains("Cross-device link") || e.contains("os error 18") {
        "не удалось заменить приложение на месте: оно запущено не из «Программ» (из образа диска или копией с карантином). Перенеси Пульт в «Программы», открой оттуда и обнови снова"
    } else if e.contains("Read-only file system") || e.contains("os error 30") {
        "приложение лежит на диске только для чтения (например, в открытом образе .dmg): перенеси Пульт в «Программы»"
    } else if e.contains("Permission denied") || e.contains("os error 13") {
        "нет прав заменить файлы приложения: проверь, что Пульт лежит в «Программах» и принадлежит тебе"
    } else {
        return e.to_string();
    };
    format!("{hint} ({e})")
}

/// Приложение собрано как оконное (windows_subsystem), поэтому без этого вызова
/// вывод флагов в cmd/PowerShell пропадает.
#[cfg(windows)]
fn attach_parent_console() {
    #[link(name = "kernel32")]
    extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
    }
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

#[cfg(not(windows))]
fn attach_parent_console() {}

#[cfg(test)]
mod tests {
    #[test]
    fn update_error_gets_human_hint() {
        let e = super::explain_update_error("failed to rename: Cross-device link (os error 18)");
        assert!(e.starts_with("не удалось заменить приложение"), "{e}");
        assert!(e.ends_with("(failed to rename: Cross-device link (os error 18))"), "{e}");
        assert_eq!(super::explain_update_error("timeout"), "timeout");
    }

    #[test]
    fn translocated_path_is_recognized() {
        let is = |p: &str| super::is_translocated(std::path::Path::new(p));
        assert!(is("/private/var/folders/ab/cd/T/AppTranslocation/6F1E-77/d/Pult.app/Contents/MacOS/pult"));
        assert!(!is("/Applications/Pult.app/Contents/MacOS/pult"));
        assert!(!is("/Users/me/AppTranslocation-notes/Pult.app/Contents/MacOS/pult"));
    }
}
