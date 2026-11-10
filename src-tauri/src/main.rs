// Hide the console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod gmail;
mod idle;
#[cfg(target_os = "macos")]
mod macos;
mod oauth;
mod state;

use gmail::{FullMessage, MessageSummary};
use state::{AppState, ClientCreds, Folder, FolderResult, OlderPage, Setup, Theme, Trashed};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_updater::{Update, UpdaterExt};

/// The newer release `check_update` found, kept for `install_update`.
#[derive(Default)]
struct PendingUpdate(Mutex<Option<Update>>);

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
    signed_in(&window, &email);
    Ok(email)
}

/// Signs in again to an account whose sign-in expired, with that account preselected.
#[tauri::command]
async fn reconnect_account(window: WebviewWindow, state: State<'_, AppState>, email: String) -> Result<String, String> {
    let email = state.reconnect(&email).await?;
    signed_in(&window, &email);
    Ok(email)
}

/// After a sign-in: (re)starts push for the account with its new token, and brings the app
/// back, since sign-in happened in the browser.
fn signed_in(window: &WebviewWindow, email: &str) {
    idle::start(window.app_handle(), email);
    show_window(window.app_handle());
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
    // Notifications are silent unless they name a sound. On Windows this is the system's new-mail
    // sound; on macOS, the system's default notification sound.
    if cfg!(windows) {
        notification = notification.sound("Mail");
    } else if cfg!(target_os = "macos") {
        notification = notification.sound("NSUserNotificationDefaultSoundName");
    }
    if let (Some(account), Some(id)) = (account, id) {
        notification = notification.extra("account", account).extra("id", id);
    }
    notification.show().map_err(|e| format!("Couldn't show a notification: {e}"))
}

/// Asks the release site (`plugins.updater.endpoints`) for a newer version. Returns that
/// version, or null when this one is the newest.
#[tauri::command]
async fn check_update(app: AppHandle, pending: State<'_, PendingUpdate>) -> Result<Option<String>, String> {
    let fail = |e: tauri_plugin_updater::Error| format!("Couldn't check for updates: {e}");
    let update = app.updater().map_err(fail)?.check().await.map_err(fail)?;
    let version = update.as_ref().map(|u| u.version.clone());
    *pending.0.lock().unwrap() = update;
    Ok(version)
}

/// Downloads the update `check_update` found, checks its signature and installs it. On Windows
/// the app exits here and the installer starts it again; elsewhere the app restarts itself.
#[tauri::command]
async fn install_update(app: AppHandle, pending: State<'_, PendingUpdate>) -> Result<(), String> {
    let update = pending.0.lock().unwrap().take();
    let update = update.ok_or("There's no update to install. Open Settings again to check.")?;
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| format!("Couldn't install the update: {e}"))?;
    app.restart()
}

/// The message list's width in px, set by dragging its edge; null goes back to the default.
#[tauri::command]
fn set_list_width(state: State<'_, AppState>, width: Option<u32>) -> Result<(), String> {
    state.save_list_width(width)
}

/// Settings → Appearance. The window's theme also sets the page's `prefers-color-scheme`
/// (on Windows Tauri passes it on to WebView2), so the CSS's dark mode follows it as is.
#[tauri::command]
fn set_theme(window: WebviewWindow, state: State<'_, AppState>, theme: Theme) -> Result<(), String> {
    window
        .set_theme(window_theme(theme))
        .map_err(|e| format!("Couldn't change the appearance: {e}"))?;
    state.save_theme(theme)
}

/// None follows the system.
fn window_theme(theme: Theme) -> Option<tauri::Theme> {
    match theme {
        Theme::System => None,
        Theme::Light => Some(tauri::Theme::Light),
        Theme::Dark => Some(tauri::Theme::Dark),
    }
}

/// Brings the window forward, also when it's minimized or hidden. On macOS the window hides
/// when it's closed, and the whole app when it's closed in full screen or with Cmd+H.
fn show_window(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    let _ = app.show();
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

/// Where the webview keeps its cache and website data for this identifier.
fn webview_dirs(app: &tauri::App) -> Vec<std::path::PathBuf> {
    let path = app.path();
    let dirs = if cfg!(target_os = "macos") {
        // WKWebView: ~/Library/Caches/<identifier> and ~/Library/WebKit/<identifier>.
        vec![path.app_cache_dir(), path.home_dir().map(|home| home.join("Library").join("WebKit").join(&app.config().identifier))]
    } else if cfg!(windows) {
        // WebView2's user data, in %LOCALAPPDATA%\<identifier>.
        vec![path.app_local_data_dir()]
    } else {
        Vec::new()
    };
    dirs.into_iter().flatten().collect()
}

fn main() {
    tauri::Builder::default()
        // Launching the app again (also what clicking an old notification in the Action Center
        // does on Windows) brings this window forward instead of starting a second copy.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_window(app)))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(PendingUpdate::default())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            // Up to 0.1.11 the app was io.unifiedmail.desktop. Before anything reads the data folder
            // or the webview starts, move what that identifier left.
            state::move_earlier_data(&app.config().identifier, &dir, &webview_dirs(app));
            let state = AppState::load(dir);
            let Setup { accounts, theme, .. } = state.setup();
            // The window is created here, not from tauri.conf.json ("create": false), so it starts
            // in the chosen theme. Setting a theme on a window that already exists misses its
            // title bar on Windows.
            let config = app.config().app.windows.iter().find(|w| w.label == "main").ok_or("No main window in tauri.conf.json")?;
            let window = tauri::WebviewWindowBuilder::from_config(app.handle(), config)?.theme(window_theme(theme));
            // The UI draws its own title bar, so the app's name appears once, in the sidebar
            // (docs/ui.md#title-bar). On Windows the window has no system title bar, and the UI
            // also draws the window buttons. Tauri keeps resizing at the edges.
            #[cfg(windows)]
            let window = window.decorations(false);
            // On macOS the system's buttons stay, over the sidebar above its brand, centered on
            // the list header's row; the title bar itself is see-through, with its title hidden.
            // The page's drag areas need the first click on an inactive window to reach the page,
            // as a system title bar would.
            #[cfg(target_os = "macos")]
            let window = window
                .title_bar_style(tauri::TitleBarStyle::Overlay)
                .hidden_title(true)
                .traffic_light_position(tauri::LogicalPosition::new(18.0, 18.0))
                .accept_first_mouse(true);
            // macOS: a link clicked in the email asks for a new window; it opens in the browser
            // instead (macos.rs).
            #[cfg(target_os = "macos")]
            let window = window.on_navigation(macos::on_navigation);
            let window = window.build()?;
            // macOS: right-clicks and keys in the email reach the page (macos.rs).
            #[cfg(target_os = "macos")]
            macos::install(&window);
            #[cfg(not(target_os = "macos"))]
            let _ = window;
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
            reconnect_account,
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
            check_update,
            install_update,
            set_theme,
            set_list_width,
            open_external,
        ])
        .build(tauri::generate_context!())
        .expect("error while running Unified Mail")
        .run(on_event);
}

/// macOS: closing the window (its close button, Cmd+W) only hides it, and the app stays in the
/// Dock, as Mail does, so push and notifications go on. Clicking the Dock icon brings it back.
/// Cmd+Q still quits: it ends the app without asking the window to close. Elsewhere closing the
/// window quits the app.
fn on_event(app: &AppHandle, event: tauri::RunEvent) {
    #[cfg(target_os = "macos")]
    match event {
        tauri::RunEvent::WindowEvent { label, event: tauri::WindowEvent::CloseRequested { api, .. }, .. } => {
            api.prevent_close();
            let Some(window) = app.get_webview_window(&label) else { return };
            // A full-screen window that hides leaves its empty, black Space on screen, so in
            // full screen the whole app hides instead, as Cmd+H does.
            if window.is_fullscreen().unwrap_or(false) {
                let _ = app.hide();
            } else {
                let _ = window.hide();
            }
        }
        tauri::RunEvent::Reopen { has_visible_windows: false, .. } => show_window(app),
        _ => {}
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, event);
}
