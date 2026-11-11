//! Saving attachments (`save_attachments` in main.rs): file names that are safe to create, a name
//! no other file in the folder has, marking the file as coming from email, and showing it in
//! Finder or File Explorer.

use std::fs::OpenOptions;
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

/// The longest name, in bytes, `safe_file_name` gives. File systems allow 255.
const MAX_NAME: usize = 200;

/// `name` as a file name that's safe to create: no folders in it, no characters Windows refuses,
/// no control characters, no direction marks that could disguise the extension
/// (`invoice\u{202E}fdp.exe` shows as `invoiceexe.pdf`), not hidden (no leading dot), not a device
/// name Windows reserves (CON, NUL, COM1…), and at most `MAX_NAME` bytes, keeping the extension.
pub fn safe_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|&c| !is_direction_mark(c))
        .map(|c| if c.is_control() || "/\\:*?\"<>|".contains(c) { '_' } else { c })
        .collect();
    let trimmed = cleaned
        .trim_start_matches(|c: char| c == '.' || c.is_whitespace())
        .trim_end_matches(|c: char| c == '.' || c.is_whitespace());
    let mut name = shorten(trimmed, MAX_NAME);
    if name.is_empty() {
        name = "attachment".to_string();
    }
    let device = name.split('.').next().unwrap_or_default().trim_end().to_ascii_uppercase();
    let numbered = device.len() == 4
        && (device.starts_with("COM") || device.starts_with("LPT"))
        && matches!(device.as_bytes()[3], b'1'..=b'9');
    if numbered || matches!(device.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        name.insert(0, '_');
    }
    name
}

/// Marks that change the direction text runs in, which can make a name read differently.
fn is_direction_mark(c: char) -> bool {
    matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// `name` cut to at most `max` bytes, on a character boundary, keeping a short extension.
fn shorten(name: &str, max: usize) -> String {
    if name.len() <= max {
        return name.to_string();
    }
    let extension = match name.rfind('.') {
        Some(i) if name.len() - i <= 16 => &name[i..],
        _ => "",
    };
    let mut end = max - extension.len();
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{extension}", &name[..end])
}

/// Writes `bytes` to a new file `name` in `dir`, or `name (1)`, `name (2)`… when that's taken, so
/// no file is ever replaced. Returns its path.
pub fn save_new(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    let (stem, extension) = match name.rfind('.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, ""),
    };
    for n in 0..1000 {
        let path = dir.join(if n == 0 { name.to_string() } else { format!("{stem} ({n}){extension}") });
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        if let Err(e) = file.write_all(bytes) {
            drop(file);
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
        return Ok(path);
    }
    Err(io::Error::new(ErrorKind::AlreadyExists, "a thousand files with this name are already there"))
}

/// Marks a saved attachment as coming from email, as mail apps and browsers do: macOS checks it
/// before it opens and asks first if it's an app (quarantine); Windows treats it as from the
/// internet (Mark of the Web), so SmartScreen and Office's Protected View apply. Best effort: a
/// file system that can't hold the mark still keeps the file.
pub fn mark_from_email(path: &Path) {
    #[cfg(target_os = "macos")]
    crate::macos::quarantine(path);
    #[cfg(windows)]
    {
        let mut stream = path.as_os_str().to_owned();
        stream.push(":Zone.Identifier");
        let _ = std::fs::write(stream, "[ZoneTransfer]\r\nZoneId=3\r\n");
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    let _ = path;
}

/// Shows `paths`, files in one folder, selected in Finder (all of them) or File Explorer (the
/// first); elsewhere opens their folder.
pub fn reveal(paths: &[PathBuf]) -> io::Result<()> {
    let Some(first) = paths.first() else { return Ok(()) };
    #[cfg(target_os = "macos")]
    {
        let _ = first;
        let status = std::process::Command::new("/usr/bin/open").arg("-R").args(paths).status()?;
        if !status.success() {
            return Err(io::Error::other("Finder couldn't show it"));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Explorer wants `/select,"path"` as one piece, which Rust's own quoting would split.
        // Windows paths can't contain quotes.
        std::process::Command::new("explorer").raw_arg(format!("/select,\"{}\"", first.display())).spawn()?;
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    open::that_detached(first.parent().unwrap_or(first))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_ordinary_names() {
        assert_eq!(safe_file_name("invoice-1042.pdf"), "invoice-1042.pdf");
        assert_eq!(safe_file_name("Отчёт за октябрь.xlsx"), "Отчёт за октябрь.xlsx");
        assert_eq!(safe_file_name("archive.tar.gz"), "archive.tar.gz");
    }

    #[test]
    fn makes_names_safe() {
        assert_eq!(safe_file_name("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(safe_file_name("C:\\Windows\\evil.exe"), "C__Windows_evil.exe");
        assert_eq!(safe_file_name("a\tb\n?.txt"), "a_b__.txt");
        assert_eq!(safe_file_name("invoice\u{202E}fdp.exe"), "invoicefdp.exe");
        assert_eq!(safe_file_name(" .hidden "), "hidden");
        assert_eq!(safe_file_name("report.pdf. "), "report.pdf");
        assert_eq!(safe_file_name(""), "attachment");
        assert_eq!(safe_file_name("..."), "attachment");
        assert_eq!(safe_file_name("con.txt"), "_con.txt");
        assert_eq!(safe_file_name("LPT9"), "_LPT9");
        assert_eq!(safe_file_name("COM0.txt"), "COM0.txt");
        assert_eq!(safe_file_name("console.log"), "console.log");
    }

    #[test]
    fn shortens_long_names_keeping_the_extension() {
        let long = safe_file_name(&format!("{}.pdf", "a".repeat(300)));
        assert_eq!(long.len(), MAX_NAME);
        assert!(long.ends_with("aaa.pdf"));
        let cyrillic = safe_file_name(&format!("{}.docx", "ж".repeat(150)));
        assert!(cyrillic.len() <= MAX_NAME && cyrillic.ends_with("жж.docx"), "{cyrillic}");
    }

    #[test]
    fn never_replaces_a_file() {
        let dir = std::env::temp_dir().join(format!("unified-mail-files-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = save_new(&dir, "scan.jpg", b"one").unwrap();
        let second = save_new(&dir, "scan.jpg", b"two").unwrap();
        let bare = save_new(&dir, "README", b"x").and_then(|_| save_new(&dir, "README", b"y")).unwrap();
        assert_eq!(first.file_name().unwrap(), "scan.jpg");
        assert_eq!(second.file_name().unwrap(), "scan (1).jpg");
        assert_eq!(bare.file_name().unwrap(), "README (1)");
        assert_eq!(std::fs::read(&first).unwrap(), b"one");
        assert_eq!(std::fs::read(&second).unwrap(), b"two");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// macOS: the saved file carries the quarantine mark. LaunchServices keeps the app's name and
    /// "email attachment" in its own records, under the event ID at the end of the mark.
    #[cfg(target_os = "macos")]
    #[test]
    fn marks_saved_files_as_from_email() {
        let dir = std::env::temp_dir().join(format!("unified-mail-quarantine-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = save_new(&dir, "tool.command", b"echo hi").unwrap();
        mark_from_email(&path);
        let output = std::process::Command::new("/usr/bin/xattr").args(["-p", "com.apple.quarantine"]).arg(&path).output().unwrap();
        let mark = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success() && mark.split(';').count() == 4, "{mark:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
