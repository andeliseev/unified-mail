//! Settings → Startup: the system opens the app when the user signs in to the computer. It starts
//! it with `--autostart` (`ARG`), which is how the app tells that launch from one the user made:
//! Start minimized applies only to it.
//!
//! The entry with the system is the only record of the choice, so turning it off outside the app
//! (Task Manager's Startup apps on Windows) shows in Settings too.
//! - Windows: a value in `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, named after the app.
//!   The uninstaller Tauri makes removes the value named after the product (not on updates), so
//!   the name is the main window's title: "Unified Mail", the product name, in the app, and
//!   "Unified Mail (test)" in the test app, which so never replaces the installed app's entry.
//! - macOS: a launch agent, `~/Library/LaunchAgents/<identifier>.plist`.

use std::path::Path;

/// What the system starts the app with at sign-in.
pub const ARG: &str = "--autostart";

/// True when the system started the app at sign-in.
pub fn at_sign_in() -> bool {
    std::env::args_os().skip(1).any(|arg| arg == ARG)
}

/// This app's entry with the system.
pub struct Autostart {
    /// The Run value's name on Windows, the launch agent's label on macOS.
    name: String,
}

impl Autostart {
    /// `title` is the main window's title, `identifier` the app's.
    pub fn new(title: &str, identifier: &str) -> Self {
        let name = if cfg!(target_os = "macos") { identifier } else { title };
        Self { name: name.to_string() }
    }

    pub fn is_on(&self) -> bool {
        system::is_on(&self.name)
    }

    pub fn set(&self, on: bool) -> Result<(), String> {
        let result = if on {
            let exe = std::env::current_exe().map_err(|e| format!("Couldn't find Unified Mail's own file: {e}"))?;
            system::write(&self.name, &entry(&self.name, &exe))
        } else {
            system::remove(&self.name)
        };
        result.map_err(|e| format!("Couldn't change whether Unified Mail opens when you sign in: {e}"))
    }
}

/// What the system runs: on Windows a command line, on macOS the launch agent.
#[cfg(not(target_os = "macos"))]
fn entry(_name: &str, exe: &Path) -> String {
    format!("\"{}\" {ARG}", exe.display())
}

#[cfg(target_os = "macos")]
fn entry(label: &str, exe: &Path) -> String {
    launch_agent(label, exe)
}

/// A launch agent that runs `exe` once when the user logs in. `Interactive`: launchd doesn't
/// hold an app back as it does background jobs.
#[cfg(any(target_os = "macos", test))]
fn launch_agent(label: &str, exe: &Path) -> String {
    let xml = |text: &str| text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{}</string>
		<string>{ARG}</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>ProcessType</key>
	<string>Interactive</string>
</dict>
</plist>
"#,
        xml(label),
        xml(&exe.to_string_lossy()),
    )
}

#[cfg(windows)]
mod system {
    use windows_registry::{Result, CURRENT_USER};

    const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    /// Where Task Manager's Startup apps turns an entry off without removing it: a value of the
    /// same name whose first byte is odd (2 on, 3 off, then when it changed).
    const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

    pub fn is_on(name: &str) -> bool {
        is_on_in(RUN, APPROVED, name)
    }

    pub fn write(name: &str, command: &str) -> Result<()> {
        write_in(RUN, APPROVED, name, command)
    }

    pub fn remove(name: &str) -> Result<()> {
        remove_in(RUN, APPROVED, name)
    }

    pub(super) fn is_on_in(run: &str, approved: &str, name: &str) -> bool {
        let there = CURRENT_USER.open(run).and_then(|key| key.get_string(name)).is_ok();
        let off = CURRENT_USER
            .open(approved)
            .and_then(|key| key.get_value(name))
            .is_ok_and(|value| value.first().is_some_and(|byte| byte & 1 == 1));
        there && !off
    }

    pub(super) fn write_in(run: &str, approved: &str, name: &str, command: &str) -> Result<()> {
        CURRENT_USER.create(run)?.set_string(name, command)?;
        // Turned off in Task Manager before: on there too.
        forget(approved, name)
    }

    pub(super) fn remove_in(run: &str, approved: &str, name: &str) -> Result<()> {
        forget(run, name)?;
        forget(approved, name)
    }

    /// Removes the value, if it's there.
    fn forget(path: &str, name: &str) -> Result<()> {
        // ERROR_FILE_NOT_FOUND, as an HRESULT: no such key or value.
        let missing = |code: i32| code as u32 == 0x8007_0002;
        match CURRENT_USER.options().read().write().open(path) {
            Ok(key) => match key.remove_value(name) {
                Err(e) if missing(e.code().0) => Ok(()),
                result => result,
            },
            Err(e) if missing(e.code().0) => Ok(()),
            Err(e) => Err(e),
        }
    }
}

#[cfg(target_os = "macos")]
mod system {
    use std::{fs, io, path::PathBuf};

    fn path(label: &str) -> io::Result<PathBuf> {
        let home = std::env::var_os("HOME").ok_or_else(|| io::Error::other("there's no home folder"))?;
        Ok(PathBuf::from(home).join("Library/LaunchAgents").join(format!("{label}.plist")))
    }

    pub fn is_on(label: &str) -> bool {
        path(label).is_ok_and(|path| path.is_file())
    }

    pub fn write(label: &str, plist: &str) -> io::Result<()> {
        let path = path(label)?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(path, plist)
    }

    pub fn remove(label: &str) -> io::Result<()> {
        match fs::remove_file(path(label)?) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod system {
    pub fn is_on(_: &str) -> bool {
        false
    }

    pub fn write(_: &str, _: &str) -> Result<(), &'static str> {
        Err("this system isn't supported")
    }

    pub fn remove(_: &str) -> Result<(), &'static str> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_agent_escapes_the_path() {
        let plist = launch_agent("com.example.app", Path::new("/Applications/A & B <1>.app/Contents/MacOS/app"));
        assert!(plist.contains("<string>com.example.app</string>"));
        assert!(plist.contains("<string>/Applications/A &amp; B &lt;1&gt;.app/Contents/MacOS/app</string>"));
        assert!(plist.contains("<string>--autostart</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n\t<true/>"));
    }

    /// The uninstaller removes the Run value named after the product, and the app names it after
    /// the main window's title.
    #[test]
    fn window_title_is_the_product_name() {
        let config: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let main = config["app"]["windows"].as_array().unwrap().iter().find(|w| w["label"] == "main").unwrap();
        assert_eq!(main["title"], config["productName"]);
    }

    #[cfg(windows)]
    #[test]
    fn command_quotes_the_path() {
        let exe = Path::new(r"C:\Users\A B\AppData\Local\Unified Mail\unified-mail.exe");
        assert_eq!(entry("Unified Mail", exe), r#""C:\Users\A B\AppData\Local\Unified Mail\unified-mail.exe" --autostart"#);
    }

    /// Against keys of its own under HKCU\Software, not the real Run key.
    #[cfg(windows)]
    #[test]
    fn run_entry_on_off_and_task_manager() {
        use system::{is_on_in, remove_in, write_in};
        use windows_registry::{Type, CURRENT_USER};
        let root = format!(r"Software\unified-mail-autostart-test-{}", std::process::id());
        let (run, approved) = (format!(r"{root}\Run"), format!(r"{root}\Approved"));
        let name = "Unified Mail (test)";

        assert!(!is_on_in(&run, &approved, name));
        remove_in(&run, &approved, name).unwrap(); // nothing there: fine

        write_in(&run, &approved, name, "\"app.exe\" --autostart").unwrap();
        assert!(is_on_in(&run, &approved, name));
        assert_eq!(CURRENT_USER.open(&run).unwrap().get_string(name).unwrap(), "\"app.exe\" --autostart");

        // Task Manager turns it off, then on.
        let task_manager = CURRENT_USER.create(&approved).unwrap();
        task_manager.set_bytes(name, Type::Bytes, &[3, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        assert!(!is_on_in(&run, &approved, name));
        task_manager.set_bytes(name, Type::Bytes, &[2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        assert!(is_on_in(&run, &approved, name));

        // Turned on in the app after Task Manager turned it off: on everywhere.
        task_manager.set_bytes(name, Type::Bytes, &[3, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        write_in(&run, &approved, name, "\"app.exe\" --autostart").unwrap();
        assert!(is_on_in(&run, &approved, name));
        assert!(task_manager.get_value(name).is_err());

        remove_in(&run, &approved, name).unwrap();
        assert!(!is_on_in(&run, &approved, name));
        assert!(CURRENT_USER.open(&run).unwrap().get_string(name).is_err());

        CURRENT_USER.remove_tree(&root).unwrap();
        assert!(CURRENT_USER.open(&root).is_err());
    }
}
