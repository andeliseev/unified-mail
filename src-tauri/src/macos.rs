//! macOS: WebKit doesn't run the page's event handlers inside the email frame. The frame is
//! sandboxed without scripts, and WebKit counts the page's handlers on it as that frame's
//! scripts; Chromium, on Windows, runs them. So the webview passes on what happens there
//! (docs/ui.md#sandbox):
//! - a click on a link asks for a new window (the frame has `allow-popups` on macOS), and
//!   `on_navigation` opens the link in the default browser instead;
//! - a right-click gets the app's menu (`email-context-menu`) instead of WebKit's, whose items
//!   do nothing here;
//! - keys reach the app's shortcuts (`email-key`).

use objc2::runtime::{AnyObject, Imp, Sel};
use objc2::{ffi, sel};
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSMenu, NSUserInterfaceItemIdentification, NSView};
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

#[cfg(test)]
mod tests {
    use super::*;

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
