mod engine;
mod inventory;
mod probes;
mod sources;
mod store;

use tauri::{AppHandle, WebviewWindowBuilder};
use tauri_plugin_log::{log, Target, TargetKind};
use tauri_plugin_updater::UpdaterExt;

pub fn run() {
    // Диагностические флаги нужны, чтобы проверить автообновление скриптом, без окна.
    let cli_apply = std::env::args().find_map(|arg| match arg.as_str() {
        "--check-update" => Some(false),
        "--apply-update" => Some(true),
        _ => None,
    });

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
        .setup(move |app| {
            let Some(apply) = cli_apply else {
                // Окно создаём сами (в конфиге create: false), чтобы в режиме флагов его не было вовсе.
                WebviewWindowBuilder::from_config(app.handle(), &app.config().app.windows[0])?
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
                        eprintln!("error={err}");
                        1
                    }
                };
                handle.exit(code);
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("не удалось запустить приложение");
}

/// Вывод в формате ключ=значение, чтобы скрипт мог разобрать его grep'ом.
async fn update_cli(app: &AppHandle, apply: bool) -> tauri_plugin_updater::Result<()> {
    println!("current={}", app.package_info().version);
    let update = app.updater()?.check().await?;
    println!("available={}", update.as_ref().map_or("", |u| u.version.as_str()));
    let Some(update) = update.filter(|_| apply) else {
        return Ok(());
    };
    let version = update.version.clone();
    println!("installing={version}");
    // Без перезапуска: иначе установщик NSIS снова запустит нас с --apply-update.
    // На Windows install() сам завершает процесс, пока установщик работает.
    update
        .restart_after_install(false)
        .download_and_install(|_, _| {}, || {})
        .await?;
    println!("installed={version}");
    Ok(())
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
