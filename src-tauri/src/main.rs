// Hide the console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod gmail;
mod idle;
mod oauth;
mod state;

use gmail::{FullMessage, MessageSummary};
use state::{AppState, ClientCreds, Folder, FolderResult, OlderPage, Setup, Trashed};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};
use tauri_plugin_notification::NotificationExt;

#[tauri::command]
fn get_setup(state: State<'_, AppState>) -> Setup {
    state.setup()
}

#[tauri::command]
fn save_client(state: State<'_, AppState>, client_id: String, client_secret: String) -> Result<(), String> {
    let (client_id, client_secret) = (client_id.trim().to_string(), client_secret.trim().to_string());
    if client_id.is_empty() || client_secret.is_empty() {
        return Err("Both Client ID and Client secret are required.".into());
    }
    state.save_client(ClientCreds { client_id, client_secret })
}

#[tauri::command]
async fn add_account(window: WebviewWindow, state: State<'_, AppState>) -> Result<String, String> {
    let email = state.add_account().await?;
    idle::start(window.app_handle(), &email);
    // Sign-in happened in the browser; bring the app back.
    let _ = window.unminimize();
    let _ = window.set_focus();
    Ok(email)
}

#[tauri::command]
fn remove_account(app: AppHandle, state: State<'_, AppState>, email: String) -> Result<(), String> {
    idle::stop(&app, &email);
    state.remove_account(&email)
}

#[tauri::command]
fn cached_folder(state: State<'_, AppState>, folder: Folder) -> Vec<MessageSummary> {
    state.cached(folder)
}

#[tauri::command]
async fn refresh_folder(state: State<'_, AppState>, folder: Folder) -> Result<FolderResult, String> {
    Ok(state.refresh(folder).await)
}

/// The account's messages older than `before` (ms since epoch), as the list scrolls down.
#[tauri::command]
async fn load_older(
    state: State<'_, AppState>,
    folder: Folder,
    account: String,
    before: i64,
) -> Result<OlderPage, String> {
    state.older(folder, &account, before).await
}

#[tauri::command]
async fn get_message(state: State<'_, AppState>, account: String, id: String) -> Result<FullMessage, String> {
    state.get_message(&account, &id).await
}

/// Opening a message marks it read; "Mark as unread" and "Mark as read" set it either way.
#[tauri::command]
async fn set_unread(state: State<'_, AppState>, account: String, id: String, unread: bool) -> Result<(), String> {
    state.set_unread(&account, &id, unread).await
}

#[tauri::command]
async fn archive_message(state: State<'_, AppState>, account: String, id: String) -> Result<(), String> {
    state.archive(&account, &id).await
}

#[tauri::command]
async fn trash_message(state: State<'_, AppState>, account: String, id: String) -> Result<Trashed, String> {
    state.trash(&account, &id).await
}

/// Undo for Archive/Delete, and "Move to Inbox" in Trash.
#[tauri::command]
async fn restore_message(
    state: State<'_, AppState>,
    account: String,
    id: String,
    from_trash: bool,
    to_inbox: bool,
) -> Result<(), String> {
    state.restore(&account, &id, from_trash, to_inbox).await
}

/// Shows the Inbox's unread count on the app icon. macOS draws a Dock badge from the number.
/// Windows has no badge numbers for desktop apps, so the UI draws one (`icon`: RGBA,
/// `size`×`size`) and it becomes the taskbar button's overlay icon.
#[tauri::command]
fn set_badge(window: WebviewWindow, count: u32, icon: Vec<u8>, size: u32) -> Result<(), String> {
    #[cfg(windows)]
    let result = {
        if count > 0 && icon.len() != (size * size * 4) as usize {
            return Err("The unread badge image has the wrong size.".into());
        }
        window.set_overlay_icon((count > 0).then(|| tauri::image::Image::new_owned(icon, size, size)))
    };
    #[cfg(not(windows))]
    let result = {
        let _ = (icon, size);
        window.set_badge_count((count > 0).then_some(i64::from(count)))
    };
    result.map_err(|e| format!("Couldn't show the unread count on the app icon: {e}"))
}

/// Shows a desktop notification. Clicking it while it's on screen brings the window forward
/// and sends `open-message` with the message to open (`account`, `id`), or null for the Inbox.
#[tauri::command]
fn notify(
    app: AppHandle,
    title: String,
    body: String,
    account: Option<String>,
    id: Option<String>,
) -> Result<(), String> {
    let mut notification = app.notification().builder().title(title).body(body);
    // Windows toasts are silent unless they name a sound; this is the system's new-mail sound.
    if cfg!(windows) {
        notification = notification.sound("Mail");
    }
    if let (Some(account), Some(id)) = (account, id) {
        notification = notification.extra("account", account).extra("id", id);
    }
    notification.show().map_err(|e| format!("Couldn't show a notification: {e}"))
}

fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Opens links from emails and the setup screen in the default browser / mail app.
#[tauri::command]
fn open_external(url: String) -> Result<(), String> {
    let lower = url.to_ascii_lowercase();
    if !["https://", "http://", "mailto:"].iter().any(|scheme| lower.starts_with(scheme)) {
        return Err(format!("Not opening unsupported link: {url}"));
    }
    open::that_detached(&url).map_err(|e| e.to_string())
}

fn main() {
    tauri::Builder::default()
        // Launching the app again (also what clicking an old notification in the Action Center
        // does on Windows) brings this window forward instead of starting a second copy.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_window(app)))
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let state = AppState::load(app.path().app_data_dir()?);
            let accounts = state.setup().accounts;
            app.manage(state);
            app.manage(idle::Watchers::default());
            for account in &accounts {
                idle::start(app.handle(), account);
            }
            let handle = app.handle().clone();
            app.notification().on_action(move |action| {
                if action.action_id() != "tap" {
                    return;
                }
                let extra = action.notification().map(|n| n.extra());
                let message = extra.and_then(|e| Some(serde_json::json!({ "account": e.get("account")?, "id": e.get("id")? })));
                show_window(&handle);
                let _ = handle.emit("open-message", message);
            })?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_setup,
            save_client,
            add_account,
            remove_account,
            cached_folder,
            refresh_folder,
            load_older,
            get_message,
            set_unread,
            archive_message,
            trash_message,
            restore_message,
            set_badge,
            notify,
            open_external,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Unified Mail");
}
