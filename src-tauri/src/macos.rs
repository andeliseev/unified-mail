//! macOS: WebKit doesn't run the page's event handlers inside the email frame. The frame is
//! sandboxed without scripts, and WebKit counts the page's handlers on it as that frame's
//! scripts; Chromium, on Windows, runs them. So the webview passes on what happens there
//! (docs/ui.md#sandbox):
//! - a click on a link asks for a new window (the frame has `allow-popups` on macOS), and
//!   `on_navigation` opens the link in the default browser instead;
//! - a right-click gets the app's menu (`email-context-menu`) instead of WebKit's, whose items
//!   do nothing here;
//! - keys reach the app's shortcuts (`email-key`).
//!
//! It also tells the page when the window enters or leaves full screen (`on_resized`), marks saved
//! attachments as coming from email (`quarantine`), and starts the app again after an update
//! (`relaunch_after_exit`).

use objc2::runtime::{AnyObject, Imp, Sel};
use objc2::{ffi, sel};
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSMenu, NSUserInterfaceItemIdentification, NSView, NSWindow, NSWindowStyleMask};
use objc2_foundation::{ns_string, NSDictionary, NSString, NSURLQuarantinePropertiesKey, NSURL};
use std::ffi::OsStr;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use tauri::{AppHandle, Emitter, Manager, Url, WebviewWindow};

type WillOpenMenu = unsafe extern "C-unwind" fn(&NSView, Sel, &NSMenu, &NSEvent);
type KeyDown = unsafe extern "C-unwind" fn(&NSView, Sel, &NSEvent);

static APP: OnceLock<AppHandle> = OnceLock::new();
/// WebKit's own implementations, which ours call after doing their part.
static WEBKIT_WILL_OPEN_MENU: OnceLock<WillOpenMenu> = OnceLock::new();
static WEBKIT_KEY_DOWN: OnceLock<KeyDown> = OnceLock::new();

#[derive(Debug, PartialEq)]
enum Navigation {
    Allow,
    OpenOutside,
    Block,
}

/// The app's own pages (`tauri://localhost`, the dev server, the email's `about:srcdoc`) load.
/// http(s) and mailto links open in the default browser or mail app. Anything else is refused.
fn navigation(url: &Url) -> Navigation {
    match url.scheme() {
        "tauri" | "about" => Navigation::Allow,
        "http" | "https" if matches!(url.host_str(), Some("localhost" | "127.0.0.1")) => Navigation::Allow,
        "http" | "https" | "mailto" => Navigation::OpenOutside,
        _ => Navigation::Block,
    }
}

/// For `on_navigation`: true lets the webview load `url`.
pub fn on_navigation(url: &Url) -> bool {
    match navigation(url) {
        Navigation::Allow => true,
        Navigation::OpenOutside => {
            let _ = crate::open_external(url.to_string());
            false
        }
        Navigation::Block => false,
    }
}

/// Puts our `willOpenMenu:withEvent:` and `keyDown:` on the webview's class (wry's WKWebView
/// subclass, which has neither), in front of WebKit's.
pub fn install(window: &WebviewWindow) {
    let _ = APP.set(window.app_handle().clone());
    let _ = window.with_webview(|webview| unsafe {
        // `inner()` is the WKWebView, alive as long as the window.
        let view = &*webview.inner().cast::<AnyObject>();
        let class = view.class();
        let Some(webkit) = class.superclass() else { return };
        let class = class as *const _ as *mut _;
        if let Some(method) = webkit.instance_method(sel!(willOpenMenu:withEvent:)) {
            if WEBKIT_WILL_OPEN_MENU.set(std::mem::transmute::<Imp, WillOpenMenu>(method.implementation())).is_ok() {
                let ours = std::mem::transmute::<WillOpenMenu, Imp>(will_open_menu);
                ffi::class_addMethod(class, sel!(willOpenMenu:withEvent:), ours, c"v@:@@".as_ptr());
            }
        }
        if let Some(method) = webkit.instance_method(sel!(keyDown:)) {
            if WEBKIT_KEY_DOWN.set(std::mem::transmute::<Imp, KeyDown>(method.implementation())).is_ok() {
                let ours = std::mem::transmute::<KeyDown, Imp>(key_down);
                ffi::class_addMethod(class, sel!(keyDown:), ours, c"v@:@".as_ptr());
            }
        }
    });
}

/// WebKit is about to show its menu, so the page didn't show its own: the click was in the email
/// or in a text field. Text fields keep WebKit's Cut / Copy / Paste menu, as in every app. In the
/// email, the menu is emptied, so AppKit shows nothing, and the page gets the click's position.
unsafe extern "C-unwind" fn will_open_menu(view: &NSView, cmd: Sel, menu: &NSMenu, event: &NSEvent) {
    let editable = (0..menu.numberOfItems()).any(|i| {
        let id = menu.itemAtIndex(i).and_then(|item| item.identifier());
        id.is_some_and(|id| id.to_string().contains("Paste"))
    });
    if !editable {
        menu.removeAllItems();
        let point = view.convertPoint_fromView(event.locationInWindow(), None);
        let y = if view.isFlipped() { point.y } else { view.bounds().size.height - point.y };
        if let Some(app) = APP.get() {
            let _ = app.emit("email-context-menu", (point.x, y));
        }
    }
    if let Some(webkit) = WEBKIT_WILL_OPEN_MENU.get() {
        webkit(view, cmd, menu, event);
    }
}

/// Every key still goes to WebKit. The page also gets it (`email-key`) and uses it when the email
/// has focus, where its own handler doesn't run. Keys with Cmd, Ctrl or Option are never the
/// app's shortcuts.
unsafe extern "C-unwind" fn key_down(view: &NSView, cmd: Sel, event: &NSEvent) {
    let flags = event.modifierFlags();
    let not_ours = NSEventModifierFlags::Command | NSEventModifierFlags::Control | NSEventModifierFlags::Option;
    if !flags.intersects(not_ours) {
        if let Some(app) = APP.get() {
            let key = match event.keyCode() {
                0x75 => "Delete".to_string(), // the forward Delete key
                0x35 => "Escape".to_string(),
                _ => event.characters().map(|c| c.to_string()).unwrap_or_default(),
            };
            // Where the key sits, for layouts without Latin letters (`shortcutKey` in app.js).
            let code = match event.keyCode() {
                0x26 => "KeyJ",
                0x28 => "KeyK",
                0x0e => "KeyE",
                0x06 => "KeyZ",
                0x20 => "KeyU",
                0x22 => "KeyI",
                0x14 => "Digit3",
                _ => "",
            };
            let _ = app.emit("email-key", (key, code, flags.contains(NSEventModifierFlags::Shift)));
        }
    }
    if let Some(webkit) = WEBKIT_KEY_DOWN.get() {
        webkit(view, cmd, event);
    }
}

/// Whether the window was in full screen at its last resize.
static FULL_SCREEN: AtomicBool = AtomicBool::new(false);

/// On every resize: tells the page when the window enters or leaves full screen (`full-screen`),
/// where the system hides the window buttons and the page drops the room it keeps for them.
/// AppKit's style mask changes as soon as the switch starts, with the first resize. Tauri's
/// `is_fullscreen` keeps saying full screen until the animation leaving it ends, about 0.6 s later.
pub fn on_resized(window: &WebviewWindow) {
    let Ok(ns_window) = window.ns_window() else { return };
    // The window's NSWindow, alive as long as the window. Window events run on the main thread.
    let style = unsafe { &*ns_window.cast::<NSWindow>() }.styleMask();
    let full_screen = style.contains(NSWindowStyleMask::FullScreen);
    if FULL_SCREEN.swap(full_screen, Ordering::Relaxed) != full_screen {
        let _ = window.emit("full-screen", full_screen);
    }
}

/// Marks a saved attachment as an email attachment from Unified Mail, as Mail does: macOS then
/// checks it before it opens, and asks first if it's an app. Best effort.
pub fn quarantine(path: &Path) {
    let Some(path) = path.to_str() else { return };
    let url = NSURL::fileURLWithPath(&NSString::from_str(path));
    // The values of LaunchServices' kLSQuarantineAgentNameKey, kLSQuarantineTypeKey and
    // kLSQuarantineTypeEmailAttachment.
    let properties = NSDictionary::from_slices(
        &[ns_string!("LSQuarantineAgentName"), ns_string!("LSQuarantineType")],
        &[ns_string!("Unified Mail"), ns_string!("LSQuarantineTypeEmailAttachment")],
    );
    // A dictionary is what this key takes.
    let _ = unsafe { url.setResourceValue_forKey_error(Some(&properties), NSURLQuarantinePropertiesKey) };
}

/// After an update: opens the app again once this copy has quit, as a click on its Dock icon
/// would. Tauri's restart runs `open -n` while this copy still runs, and the Dock gives that
/// second copy a second icon; a kept-in-Dock icon then looks closed. False when not running from
/// an app bundle (`npm run dev`).
pub fn relaunch_after_exit() -> bool {
    let Ok(exe) = tauri::utils::platform::current_exe() else { return false };
    let Some(bundle) = app_bundle(&exe) else { return false };
    run_after_exit(std::process::id(), &["/usr/bin/open".as_ref(), bundle.as_os_str()]).is_ok()
}

/// `X.app` for `X.app/Contents/MacOS/binary`.
fn app_bundle(exe: &Path) -> Option<&Path> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    let named = |dir: &Path, name: &str| dir.file_name().is_some_and(|n| n == name);
    let is_app = bundle.extension().is_some_and(|e| e == "app");
    (named(macos, "MacOS") && named(contents, "Contents") && is_app).then_some(bundle)
}

/// Waits for process `$1` to end, checking every 0.1 s, then runs the other arguments. LaunchServices
/// lists an app a few ms longer than it runs, and an `open` in between only tries to bring the
/// gone copy forward, so it also waits until `lsappinfo` no longer finds it. Gives up after a
/// minute: a copy that hasn't quit by then shouldn't get a second one next to it.
const AFTER_EXIT: &str = r#"pid=$1; shift; i=0
while kill -0 "$pid" 2>/dev/null || /usr/bin/lsappinfo find pid="$pid" | grep -q ASN; do
  [ "$i" -lt 600 ] || exit 1
  sleep 0.1; i=$((i + 1))
done
exec "$@""#;

/// Runs `command` once process `pid` has ended. The shell gets its own process group and no
/// stdio, so it outlives the app.
fn run_after_exit(pid: u32, command: &[&OsStr]) -> std::io::Result<Child> {
    Command::new("/bin/sh")
        .args(["-c", AFTER_EXIT, "sh"])
        .arg(pid.to_string())
        .args(command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_app_bundle() {
        let exe = Path::new("/Applications/Unified Mail.app/Contents/MacOS/unified-mail");
        assert_eq!(app_bundle(exe), Some(Path::new("/Applications/Unified Mail.app")));
        assert_eq!(app_bundle(Path::new("/Users/me/unified-mail/src-tauri/target/debug/unified-mail")), None);
        assert_eq!(app_bundle(Path::new("/unified-mail")), None);
    }

    #[test]
    fn runs_the_command_once_the_process_has_ended() {
        let dir = std::env::temp_dir().join(format!("unified-mail-relaunch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("ran");
        let mut process = Command::new("/bin/sleep").arg("1").spawn().unwrap();
        let mut waiting = run_after_exit(process.id(), &["/usr/bin/touch".as_ref(), marker.as_os_str()]).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(!marker.exists(), "ran while the process was still running");
        process.wait().unwrap(); // reaped: until then it's a zombie, which still counts as running
        assert!(waiting.wait().unwrap().success());
        assert!(marker.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn nav(url: &str) -> Navigation {
        navigation(&Url::parse(url).unwrap())
    }

    #[test]
    fn the_apps_own_pages_load() {
        assert_eq!(nav("tauri://localhost/index.html"), Navigation::Allow);
        assert_eq!(nav("http://127.0.0.1:1430/"), Navigation::Allow);
        assert_eq!(nav("http://localhost:1430/"), Navigation::Allow);
        assert_eq!(nav("about:srcdoc"), Navigation::Allow);
        assert_eq!(nav("about:blank"), Navigation::Allow);
    }

    #[test]
    fn links_open_outside() {
        assert_eq!(nav("https://example.com/a?b=c"), Navigation::OpenOutside);
        assert_eq!(nav("http://example.com/"), Navigation::OpenOutside);
        assert_eq!(nav("mailto:someone@example.com"), Navigation::OpenOutside);
    }

    #[test]
    fn other_schemes_are_refused() {
        assert_eq!(nav("file:///etc/hosts"), Navigation::Block);
        assert_eq!(nav("data:text/html,hi"), Navigation::Block);
        assert_eq!(nav("javascript:alert(1)"), Navigation::Block);
        assert_eq!(nav("tel:+123"), Navigation::Block);
    }
}
