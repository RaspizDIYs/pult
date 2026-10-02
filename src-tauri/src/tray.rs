//! Значок в трее: сводка в подсказке, меню, другой вид при корневых отказах.
//! Окно при закрытии прячется, а проверки идут дальше — выход только из меню (на маке ещё Cmd+Q).

use crate::monitor::Monitor;
use std::sync::Arc;
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager};

const ID: &str = "main";

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Открыть", true, None::<&str>)?;
    let recheck = MenuItem::with_id(app, "recheck", "Проверить сейчас", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;
    let mut tray = TrayIconBuilder::with_id(ID)
        .tooltip("Пульт: проверяю…")
        .menu(&Menu::with_items(app, &[&open, &recheck, &quit])?)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_window(app),
            "recheck" => app.state::<Arc<Monitor>>().recheck(None),
            "quit" => app.exit(0),
            _ => {}
        });
    if let Some(icon) = icon(app, false) {
        tray = tray.icon(icon);
    }
    tray.build(app)?;
    Ok(())
}

/// roots — сколько сейчас корневых отказов.
pub fn update(app: &AppHandle, roots: usize) {
    let Some(tray) = app.tray_by_id(ID) else { return };
    let tip = if roots == 0 { "Пульт: всё работает".to_string() } else { format!("Сломано: {roots}") };
    let _ = tray.set_tooltip(Some(tip));
    if let Some(icon) = icon(app, roots > 0) {
        let _ = tray.set_icon(Some(icon));
    }
}

/// Окно показано (трей, Dock, повторный запуск). Приложение без окна — Accessory, его надо
/// вернуть в Regular раньше, чем окно: иначе оно покажется без значка в Dock и без меню.
pub fn show_window(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    set_policy(app, tauri::ActivationPolicy::Regular);
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

#[cfg(target_os = "macos")]
fn set_policy(app: &AppHandle, policy: tauri::ActivationPolicy) {
    if let Err(e) = app.set_activation_policy(policy) {
        log::warn!("режим приложения не переключился: {e}");
    }
}

/// Окно спрятано крестиком. На маке приложение без окна уходит из Dock и Cmd-Tab: иначе оно
/// остаётся там «открытым», а Cmd-Tab делает его активным без окна (`Reopen` при этом не
/// приходит). Живёт значком в строке меню; первый раз за запуск говорим об этом, иначе
/// непонятно, почему процесс остался. На винде окно просто прячется в трей, как раньше.
#[cfg(target_os = "macos")]
pub fn window_hidden(app: &AppHandle) {
    set_policy(app, tauri::ActivationPolicy::Accessory);
    static HINTED: AtomicBool = AtomicBool::new(false);
    if app.state::<Arc<Monitor>>().settings().notifications && !HINTED.swap(true, Ordering::Relaxed) {
        crate::notify::send(
            app,
            vec![crate::notify::Alert {
                title: "Пульт продолжает проверять в фоне".into(),
                body: "Значок в строке меню, выход там же.".into(),
            }],
        );
    }
}

#[cfg(not(target_os = "macos"))]
pub fn window_hidden(_app: &AppHandle) {}

/// Значок приложения; при отказе — с красной точкой в углу. Рисуем сами, чтобы не
/// держать второй файл значка, который разойдётся с основным.
fn icon(app: &AppHandle, alert: bool) -> Option<Image<'static>> {
    let base = app.default_window_icon()?;
    let (w, h) = (base.width(), base.height());
    let mut rgba = base.rgba().to_vec();
    if alert {
        let r = w.min(h) as f32 * 0.22;
        let (cx, cy) = (w as f32 - r - 1.0, h as f32 - r - 1.0);
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                if dx * dx + dy * dy <= r * r {
                    let i = ((y * w + x) * 4) as usize;
                    rgba[i..i + 4].copy_from_slice(&[0xE5, 0x2E, 0x2E, 0xFF]);
                }
            }
        }
    }
    Some(Image::new_owned(rgba, w, h))
}
