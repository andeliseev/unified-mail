// Hide the console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod files;
mod gmail;
mod idle;
#[cfg(target_os = "macos")]
mod macos;
mod mime;
mod oauth;
mod state;

use gmail::{FullMessage, MessageSummary};
use state::{AppState, ClientCreds, Draft, Folder, FolderResult, OlderPage, Setup, Theme, Trashed};
use futures_util::future::{AbortHandle, Abortable};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::oneshot;

/// The longest a sent email waits for Undo, whatever the page asks.
const MAX_SEND_DELAY: Duration = Duration::from_secs(30);
/// How long quitting (or installing an update) waits for emails still on their way.
const SEND_ON_QUIT: Duration = Duration::from_secs(20);

/// The newer release `check_update` found, kept for `install_update`.
#[derive(Default)]
struct PendingUpdate(Mutex<Option<Update>>);

/// Attachment downloads still running, by the page's job number, so `cancel_save` can stop one.
#[derive(Default)]
struct Saving(Mutex<HashMap<u64, AbortHandle>>);

/// Emails sent in the app that are still waiting out the time Undo has (`send_message`), by the
/// page's job number. Each waits for its time to pass or to be told: `true` sends it at once,
/// `false` (Undo) never.
#[derive(Default)]
struct Outbox {
    waiting: Mutex<HashMap<u64, oneshot::Sender<bool>>>,
    /// Emails waiting or being sent. Quitting waits for them (`flush`).
    unsent: AtomicUsize,
}

impl Outbox {
    /// Sends every email that's waiting at once, then waits for all of them to be sent, up to
    /// `limit`. For when the app quits, so a sent email isn't lost with it.
    fn flush(&self, limit: Duration) {
        for (_, go) in self.waiting.lock().unwrap().drain() {
            let _ = go.send(true);
        }
        let start = Instant::now();
        while self.unsent.load(Ordering::SeqCst) > 0 && start.elapsed() < limit {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Counts an email as unsent for as long as it lives.
struct Unsent<'a>(&'a AtomicUsize);

impl<'a> Unsent<'a> {
    fn new(count: &'a AtomicUsize) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Self(count)
    }
}

impl Drop for Unsent<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

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

/// Gmail search in the account's All Mail: its newest messages matching `query`, typed in Gmail's
/// search syntax, or the next ones older than `before` (ms since epoch) as the results scroll.
#[tauri::command]
async fn search_messages(
    state: State<'_, AppState>,
    account: String,
    query: String,
    before: Option<i64>,
) -> Result<OlderPage, String> {
    state.search(&account, &query, before).await
}

#[tauri::command]
async fn get_message(state: State<'_, AppState>, account: String, id: String) -> Result<FullMessage, String> {
    state.get_message(&account, &id).await
}

/// Saves attachments `parts` of message `id` to the Downloads folder, each under a name no file
/// there has yet, marked as coming from email (`files.rs`). Returns where they went, or nothing
/// when `cancel_save(job)` stopped the download; files are only written once all have arrived.
/// Meanwhile `on_progress` gets how many bytes of the files have arrived, up to ten times a
/// second. Errors give the reason; the page puts "Couldn't save …:" in front.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn save_attachments(
    app: AppHandle,
    state: State<'_, AppState>,
    saving: State<'_, Saving>,
    account: String,
    id: String,
    parts: Vec<String>,
    job: u64,
    on_progress: tauri::ipc::Channel<u64>,
) -> Result<Vec<String>, String> {
    let downloads = downloads_dir(&app)?;
    let last_sent = Mutex::new(None::<Instant>);
    let arrived = |bytes: u64| {
        let mut last = last_sent.lock().unwrap();
        if last.is_some_and(|sent| sent.elapsed() < Duration::from_millis(100)) {
            return;
        }
        *last = Some(Instant::now());
        let _ = on_progress.send(bytes);
    };
    let (handle, registration) = AbortHandle::new_pair();
    saving.0.lock().unwrap().insert(job, handle);
    let download = Abortable::new(state.attachments(&account, &id, &parts, &arrived), registration).await;
    saving.0.lock().unwrap().remove(&job);
    // Cancelled: dropping the download also ended its request to Gmail.
    let Ok(files) = download else { return Ok(Vec::new()) };
    let mut saved = Vec::new();
    for (name, bytes) in files? {
        let path = files::save_new(&downloads, &name, &bytes).map_err(|e| save_error(&e))?;
        files::mark_from_email(&path);
        saved.push(path.to_string_lossy().into_owned());
    }
    Ok(saved)
}

/// The pictures `parts` that message `id` shows in place (`cid:` links), with their data
/// (base64url), for the reader to put in the email.
#[tauri::command]
async fn inline_images(state: State<'_, AppState>, account: String, id: String, parts: Vec<String>) -> Result<Vec<gmail::PartData>, String> {
    state.inline_images(&account, &id, &parts).await
}

/// Stops `save_attachments(job)` if it's still downloading.
#[tauri::command]
fn cancel_save(saving: State<'_, Saving>, job: u64) {
    if let Some(handle) = saving.0.lock().unwrap().remove(&job) {
        handle.abort();
    }
}

/// Shows saved attachments in Finder or File Explorer. Only files in the Downloads folder,
/// where `save_attachments` puts them. Errors give the reason, as for `save_attachments`.
#[tauri::command]
async fn show_in_folder(app: AppHandle, paths: Vec<String>) -> Result<(), String> {
    let downloads = downloads_dir(&app)?;
    let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    if paths.is_empty() || paths.iter().any(|p| p.parent() != Some(downloads.as_path()) || !p.is_file()) {
        return Err("It isn't in your Downloads folder any more.".to_string());
    }
    files::reveal(&paths).map_err(|e| e.to_string())
}

fn downloads_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().download_dir().map_err(|_| "Your Downloads folder wasn't found.".to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("Your Downloads folder can't be opened: {e}"))?;
    Ok(dir)
}

/// macOS asks once before an app writes to Downloads; after "Don't Allow" writing fails.
fn save_error(error: &std::io::Error) -> String {
    if cfg!(target_os = "macos") && error.kind() == std::io::ErrorKind::PermissionDenied {
        return "macOS didn't allow access to your Downloads folder. Allow Unified Mail in System Settings → \
                Privacy & Security → Files & Folders, then try again."
            .to_string();
    }
    error.to_string()
}

/// Sends an email written in the app after `delay_ms`, the time its Undo shows: until then
/// `cancel_send(job)` stops it. Returns the sent message's id, or null when Undo stopped it.
/// If the app quits first, it's sent right away (`Outbox::flush`). Errors give the reason; the
/// page puts "Couldn't send the message:" in front.
#[tauri::command]
async fn send_message(
    state: State<'_, AppState>,
    outbox: State<'_, Outbox>,
    job: u64,
    delay_ms: u64,
    draft: Draft,
) -> Result<Option<String>, String> {
    let _unsent = Unsent::new(&outbox.unsent);
    let (go, told) = oneshot::channel();
    outbox.waiting.lock().unwrap().insert(job, go);
    // `told` comes first: an Undo that arrives as the time runs out still stops it.
    let send = tokio::select! {
        biased;
        told = told => told.unwrap_or(true),
        _ = tokio::time::sleep(Duration::from_millis(delay_ms).min(MAX_SEND_DELAY)) => true,
    };
    outbox.waiting.lock().unwrap().remove(&job);
    if !send {
        return Ok(None);
    }
    state.send(draft).await.map(Some)
}

/// Undo for `send_message(job)`: returns whether it was stopped in time.
#[tauri::command]
fn cancel_send(outbox: State<'_, Outbox>, job: u64) -> bool {
    let waiting = outbox.waiting.lock().unwrap().remove(&job);
    waiting.is_some_and(|go| go.send(false).is_ok())
}

/// Saves what's written as a Gmail draft of `draft.account`: a new one, or `draft.draftId`
/// updated. Returns the draft's id and its new message.
#[tauri::command]
async fn save_draft(state: State<'_, AppState>, draft: Draft) -> Result<gmail::SavedDraft, String> {
    state.save_draft(draft).await
}

/// Deletes Gmail draft `id` for good: Discard.
#[tauri::command]
async fn delete_draft(state: State<'_, AppState>, account: String, id: String) -> Result<(), String> {
    state.delete_draft(&account, &id).await
}

/// Gmail draft `id`, to go on writing it: its recipients, subject, text, files with their data,
/// and the email it answers, when it's found.
#[tauri::command]
async fn open_draft(state: State<'_, AppState>, account: String, id: String) -> Result<gmail::OpenedDraft, String> {
    state.open_draft(&account, &id).await
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
/// the app exits here and the installer starts it again. On macOS the app quits and is opened
/// again once it's gone, so it comes back under its own Dock icon (`macos::relaunch_after_exit`);
/// elsewhere it restarts itself.
#[tauri::command]
async fn install_update(app: AppHandle, pending: State<'_, PendingUpdate>) -> Result<(), String> {
    let update = pending.0.lock().unwrap().take();
    let update = update.ok_or("There's no update to install. Open Settings again to check.")?;
    // Installing ends the app without the `Exit` event that sends what's waiting, so send it first.
    let handle = app.clone();
    let _ = tauri::async_runtime::spawn_blocking(move || handle.state::<Outbox>().flush(SEND_ON_QUIT)).await;
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| format!("Couldn't install the update: {e}"))?;
    #[cfg(target_os = "macos")]
    if macos::relaunch_after_exit() {
        app.exit(0);
        return Ok(());
    }
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
        .manage(Saving::default())
        .manage(Outbox::default())
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
            // On macOS the system's buttons stay, over the sidebar above its brand, centered 26 px
            // down on the row of the list header and the message's buttons. AppKit puts them 9 px
            // above the bottom of a title bar as tall as they are (14 px) plus the inset, so their
            // center is the inset minus 2. The title bar itself is see-through, with its title
            // hidden, and clicks under it reach the page.
            // The page's drag areas need the first click on an inactive window to reach the page,
            // as a system title bar would.
            #[cfg(target_os = "macos")]
            let window = window
                .title_bar_style(tauri::TitleBarStyle::Overlay)
                .hidden_title(true)
                .traffic_light_position(tauri::LogicalPosition::new(18.0, 28.0))
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
            search_messages,
            get_message,
            save_attachments,
            inline_images,
            cancel_save,
            show_in_folder,
            send_message,
            cancel_send,
            save_draft,
            delete_draft,
            open_draft,
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
/// window quits the app. Resizes on macOS go to `macos::on_resized`, which follows full screen.
/// Quitting, however it happens (Cmd+Q included), first sends the emails still waiting for Undo.
fn on_event(app: &AppHandle, event: tauri::RunEvent) {
    if let tauri::RunEvent::Exit = event {
        app.state::<Outbox>().flush(SEND_ON_QUIT);
        return;
    }
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
        tauri::RunEvent::WindowEvent { label, event: tauri::WindowEvent::Resized(_), .. } => {
            if let Some(window) = app.get_webview_window(&label) {
                macos::on_resized(&window);
            }
        }
        _ => {}
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, event);
}
