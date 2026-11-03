// Hide the console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod gmail;
mod idle;
mod oauth;
mod state;

use gmail::{FullMessage, MessageSummary};
use state::{AppState, ClientCreds, Folder, FolderResult, Setup, Trashed};
use tauri::{AppHandle, Manager, State, WebviewWindow};

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

#[tauri::command]
async fn get_message(state: State<'_, AppState>, account: String, id: String) -> Result<FullMessage, String> {
    state.get_message(&account, &id).await
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
        .setup(|app| {
            let state = AppState::load(app.path().app_data_dir()?);
            let accounts = state.setup().accounts;
            app.manage(state);
            app.manage(idle::Watchers::default());
            for account in &accounts {
                idle::start(app.handle(), account);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_setup,
            save_client,
            add_account,
            remove_account,
            cached_folder,
            refresh_folder,
            get_message,
            archive_message,
            trash_message,
            restore_message,
            open_external,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Unified Mail");
}
