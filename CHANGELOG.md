# Changelog

What changed in each version of Unified Mail. The newest is at the top; each release's notes on GitHub show the same list.

## Unreleased

## 0.2.1 (2026-11-11)

- macOS: after an update, Unified Mail comes back under its own Dock icon. Before, it opened with a second icon next to it, and the icon you keep in the Dock looked closed. This starts with your next update after this one: updating to this version still shows the second icon one last time.

## 0.2.0 (2026-11-11)

- Drag the line between the message list and the message to make the list narrower or wider. The app remembers the width; double-click the line to reset it.
- A narrow window no longer squeezes the message: it stays at least 400 px wide, so a typical newsletter still fits, and the list narrows instead. The smallest window is a little wider than before (880 px).

## 0.1.14 (2026-11-10)

- macOS: after an update, the keychain password prompt comes up once per account instead of again and again, and no longer comes back every hour. Click **Always Allow** to stop it until the next update. Refusing it no longer makes the account look signed out.

## 0.1.13 (2026-11-10)

- macOS: new-mail notifications play a sound, as on Windows. You can turn it off in System Settings → Notifications → Unified Mail.
- macOS: links in emails open in your browser, and right-clicking in an email shows Open link, Copy link and Copy. Before, clicking a link did nothing, and the right-click menu's items didn't work. Keyboard shortcuts such as J, K and E also work after you click into an email.

## 0.1.12 (2026-11-10)

- macOS: the grey title bar is gone, so the app's name no longer shows twice. The window's buttons sit at the top of the sidebar, above the name. Drag the window by the sidebar's top, the list's header or the strip above the message. A click in the window while another app is in front now works right away, for example to drag the window or open a message.
- macOS: closing the window (the red button or Cmd+W) no longer quits Unified Mail. It keeps running in the Dock, so new mail still arrives and notifies you. Click the Dock icon to bring the window back; Cmd+Q quits.
- Windows: the grey Windows title bar is gone. The app's own icon and name at the top of the sidebar now head the window, and Minimize, Maximize and Close sit in its top-right corner. Drag the window by the sidebar's top, the list's header or the strip above the message.
- Pointing at a message in the list shows small Archive, Delete and Mark as read/unread buttons in place of its time, so you can act on it without opening it. In All Mail and Trash they offer Move to Inbox where the reader does.
- Unified Mail's data folder is now called `com.andeliseev.unified-mail`. Your accounts, settings and saved message lists move there by themselves the first time this version starts.

## 0.1.11 (2026-11-04)

- Keyboard shortcuts (J, K, E, #, Z, Shift+U, Shift+I) work with any keyboard layout, such as Russian, without switching to English first.
- The Delete button's tooltip names the Delete key instead of #. Both still delete.
- A new app icon: a white envelope on the blue tile. The website uses it too.
- When an account's sign-in expires, a **Reconnect** button appears above the message list (and in Settings). It opens Google's sign-in with that account already chosen, instead of an error after every sync that said to use Add account.

## 0.1.10 (2026-11-04)

- Dates are in English like the rest of the app ("Nov 4"), instead of in your system's language. Times keep your system's clock style: 24-hour or AM/PM.

## 0.1.9 (2026-11-04)

- In dark mode, emails are dark too, instead of a bright white block. Emails with their own dark design use it; others are darkened, keeping images and colored buttons. **Original** next to Archive / Delete shows an email in its original light colors.

## 0.1.8 (2026-11-04)

- A new app icon: the blue tile is the envelope, with its flap folded down. The website uses it too.

## 0.1.7 (2026-11-03)

- Wide emails fit the reading pane as soon as they open. Before, an email with a slow image could stay cut off, with a sideways scroll bar and links that didn't work, for 20 seconds or more.

## 0.1.6 (2026-11-03)

- Settings → Appearance: follow the system, or always light or dark.
- Opening Settings always checks for a new version, so one released while the app is open shows up there. The Update button is orange.

## 0.1.5 (2026-11-03)

- No changes in the app itself; only how releases are built.

## 0.1.4 (2026-11-03)

- The app updates itself: when a new version is out, Settings shows an **Update** button, and one click installs it, with no installer pages.

## 0.1.3 (2026-11-03)

- Wide emails, such as newsletters and receipts, are scaled down to fit instead of being cut off.
- Settings shows the app's version, with links to GitHub, the releases, the website and the privacy policy.

## 0.1.2 (2026-11-03)

- Notifications for new mail while the app is in the background; click one to open the message.
- The unread count shows on the taskbar / Dock icon.
- Mark as unread / read: a button in the reader, `Shift+U` / `Shift+I`, and the right-click menu.
- Right-click a message for its actions. The browser's own menu, in the system's language, is gone.
- Opening the app again brings its window forward instead of starting a second copy.

## 0.1.1 (2026-11-03)

- Fixed garbled text in some messages, when Gmail had already converted them to UTF-8.

## 0.1.0 (2026-11-03)

- First release: one inbox for all your Gmail accounts, plus All Mail and Trash. New mail appears within seconds. Archive, delete and move to Inbox, each with Undo. Older mail loads as you scroll, and there are keyboard shortcuts.
