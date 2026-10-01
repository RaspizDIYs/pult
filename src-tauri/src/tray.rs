//! Значок в трее: сводка в подсказке, меню, другой вид при корневых отказах.
//! Окно при закрытии прячется, а проверки идут дальше — выход только из меню.

use crate::monitor::Monitor;
use std::sync::Arc;
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

pub fn show_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

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
