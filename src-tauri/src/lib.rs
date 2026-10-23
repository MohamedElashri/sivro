//! Sivro desktop shell and versioned session commands.
mod adapters;
pub mod api;
mod columnar;
mod commands;
pub mod dataset;
pub mod errors;
mod export;
pub mod policy;
mod query;
mod registry;
mod sample;
mod scratch;
mod sessions;

use commands::*;
use sessions::AppState;
use std::{path::PathBuf, sync::Arc};
use tauri::{
    Manager, Monitor, PhysicalPosition, PhysicalRect, PhysicalSize, Runtime, WebviewWindow,
};

// ---------------------------------------------------------------------------
// Window controls + geometry (frameless window)
// ---------------------------------------------------------------------------

#[tauri::command]
async fn window_minimize(window: WebviewWindow) -> Result<(), String> {
    window.minimize().map_err(|e| e.to_string())
}

#[tauri::command]
async fn window_maximize(window: WebviewWindow) -> Result<(), String> {
    if window.is_maximized().unwrap_or(false) {
        window.unmaximize().map_err(|e| e.to_string())
    } else {
        window.maximize().map_err(|e| e.to_string())
    }
}

#[tauri::command]
async fn window_close(window: WebviewWindow) -> Result<(), String> {
    window.close().map_err(|e| e.to_string())
}

struct WindowGeometry {
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
}

fn monitor_for_window<R: Runtime>(window: &WebviewWindow<R>) -> Option<Monitor> {
    window
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten())
        .or_else(|| {
            window
                .available_monitors()
                .ok()
                .and_then(|monitors| monitors.into_iter().next())
        })
}

fn constrained_window_geometry(
    geometry: WindowGeometry,
    work_area: PhysicalRect<i32, u32>,
    center_fallback: bool,
) -> WindowGeometry {
    let max_width = work_area.size.width.max(1);
    let max_height = work_area.size.height.max(1);
    let target_width = geometry.size.width.clamp(1, max_width);
    let target_height = geometry.size.height.clamp(1, max_height);

    let max_x = work_area.position.x + work_area.size.width.saturating_sub(target_width) as i32;
    let max_y = work_area.position.y + work_area.size.height.saturating_sub(target_height) as i32;

    let target_x = if center_fallback {
        work_area.position.x + (work_area.size.width.saturating_sub(target_width) / 2) as i32
    } else {
        geometry.position.x.clamp(work_area.position.x, max_x)
    };

    let target_y = if center_fallback {
        work_area.position.y + (work_area.size.height.saturating_sub(target_height) / 2) as i32
    } else {
        geometry.position.y.clamp(work_area.position.y, max_y)
    };

    WindowGeometry {
        position: PhysicalPosition::new(target_x, target_y),
        size: PhysicalSize::new(target_width, target_height),
    }
}

fn fit_initial_window_to_work_area<R: Runtime>(window: &WebviewWindow<R>) {
    let Some(monitor) = monitor_for_window(window) else {
        return;
    };
    let work_area = *monitor.work_area();
    let Ok(inner_size) = window.inner_size() else {
        return;
    };
    let Ok(outer_size) = window.outer_size() else {
        return;
    };
    let horizontal_chrome = outer_size.width.saturating_sub(inner_size.width);
    let vertical_chrome = outer_size.height.saturating_sub(inner_size.height);
    let position = window
        .outer_position()
        .unwrap_or_else(|_| PhysicalPosition::new(work_area.position.x, work_area.position.y));
    let constrained = constrained_window_geometry(
        WindowGeometry {
            position,
            size: outer_size,
        },
        work_area,
        true,
    );
    let target_inner_size = PhysicalSize::new(
        constrained
            .size
            .width
            .saturating_sub(horizontal_chrome)
            .max(1),
        constrained
            .size
            .height
            .saturating_sub(vertical_chrome)
            .max(1),
    );
    if target_inner_size != inner_size {
        let _ = window.set_size(target_inner_size);
    }

    let Ok(final_outer_size) = window.outer_size() else {
        return;
    };
    let Ok(final_position) = window.outer_position() else {
        return;
    };
    let final_geometry = constrained_window_geometry(
        WindowGeometry {
            position: final_position,
            size: final_outer_size,
        },
        work_area,
        true,
    );
    let _ = window.set_position(final_geometry.position);
}

// ---------------------------------------------------------------------------
// Reveal in file manager / open with default app.
//
// These use plain OS commands (`open -R` / `open` on macOS, Explorer on
// Windows, xdg-open on Linux) instead of the opener plugin's direct AppKit
// calls, which aborted the process on some macOS versions when invoked from
// a background thread.
// ---------------------------------------------------------------------------

#[tauri::command]
async fn reveal_path(path: String) -> Result<(), String> {
    let trimmed = path.trim().to_string();
    if trimmed.is_empty() {
        return Err("Nothing to reveal".to_string());
    }
    tauri::async_runtime::spawn_blocking(move || reveal_path_blocking(&trimmed))
        .await
        .map_err(|e| format!("Reveal was interrupted: {e}"))?
}

fn run_command(cmd: &mut std::process::Command) -> Result<(), String> {
    let output = cmd.output().map_err(|e| e.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stderr.is_empty() {
        Err(stderr)
    } else if !stdout.is_empty() {
        Err(stdout)
    } else {
        Err(format!("command exited with {}", output.status))
    }
}

fn reveal_path_blocking(path: &str) -> Result<(), String> {
    let p = PathBuf::from(path);
    if !p.exists() {
        return Err("File no longer exists".to_string());
    }
    #[cfg(target_os = "macos")]
    {
        run_command(std::process::Command::new("open").arg("-R").arg(path))
            .map_err(|e| format!("Reveal failed: {e}"))
    }
    #[cfg(target_os = "windows")]
    {
        run_command(std::process::Command::new("explorer").args(["/select,", path]))
            .map_err(|e| format!("Reveal failed: {e}"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        // Try the file-manager portal first, fall back to opening the folder.
        let parent = p
            .parent()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/".to_string());
        run_command(std::process::Command::new("xdg-open").arg(&parent))
            .map_err(|e| format!("Reveal failed: {e}"))
    }
}

#[tauri::command]
async fn open_external(path: String) -> Result<(), String> {
    let trimmed = path.trim().to_string();
    if trimmed.is_empty() {
        return Err("Nothing to open".to_string());
    }
    tauri::async_runtime::spawn_blocking(move || open_external_blocking(&trimmed))
        .await
        .map_err(|e| format!("Open was interrupted: {e}"))?
}

fn open_external_blocking(path: &str) -> Result<(), String> {
    let p = PathBuf::from(path);
    if !p.exists() {
        return Err("File no longer exists".to_string());
    }
    #[cfg(target_os = "macos")]
    {
        run_command(std::process::Command::new("open").arg(path))
            .map_err(|e| format!("Open failed: {e}"))
    }
    #[cfg(target_os = "windows")]
    {
        run_command(std::process::Command::new("cmd").args(["/C", "start", "", path]))
            .map_err(|e| format!("Open failed: {e}"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        run_command(std::process::Command::new("xdg-open").arg(path))
            .map_err(|e| format!("Open failed: {e}"))
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AppMetadata {
    version: String,
    author: &'static str,
    platform: &'static str,
    arch: &'static str,
    update_supported: bool,
    update_hint: &'static str,
}

#[tauri::command]
fn app_metadata(app: tauri::AppHandle) -> AppMetadata {
    // Linux's updater replaces the running AppImage. deb/rpm and source
    // builds must use their package manager or the release downloads.
    // `Env::appimage` only exists on Linux targets.
    #[cfg(target_os = "linux")]
    let installable = app.env().appimage.is_some();
    #[cfg(not(target_os = "linux"))]
    let installable = true;
    let supported = !cfg!(debug_assertions) && installable;
    AppMetadata {
        version: app.package_info().version.to_string(),
        author: "Mohamed Elashri",
        platform: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        update_supported: supported,
        update_hint: if cfg!(debug_assertions) {
            "Built-in installation is unavailable in development builds."
        } else if cfg!(target_os = "linux") {
            "Built-in installation on Linux requires running the AppImage."
        } else {
            ""
        },
    }
}

pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            let root = app.path().app_cache_dir()?.join("scratch-v1");
            let state = AppState::new(root, policy::ResourcePolicy::default())?;
            app.manage(state);
            if let Some(window) = app.get_webview_window("main") {
                fit_initial_window_to_work_area(&window);
                window.show()?;
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_metadata,
            probe_file,
            preview_delimited,
            discard_preview,
            inspect_workbook,
            inspect_database,
            open_dataset,
            list_objects,
            select_object,
            get_metadata,
            begin_operation,
            cancel_operation,
            fetch_page,
            count_rows,
            column_profile,
            read_row,
            read_document,
            export_data,
            close_dataset,
            refresh_dataset,
            get_resource_policy,
            generate_sample,
            reveal_path,
            open_external,
            window_minimize,
            window_maximize,
            window_close,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Sivro");
    app.run(|app, event| {
        if matches!(
            event,
            tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
        ) {
            if let Some(state) = app.try_state::<Arc<AppState>>() {
                state.drain_shutdown();
            }
        }
    });
}

#[cfg(test)]
mod tests;
