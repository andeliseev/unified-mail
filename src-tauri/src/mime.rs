//! Writes the emails the app sends in the Internet Message Format (RFC 5322) with MIME parts
//! (RFC 2045-2047), as Gmail's `messages.send` takes them.
//!
//! The text goes as quoted-printable UTF-8, files as base64, and header text outside ASCII as
//! encoded words (`=?UTF-8?B?...?=`). Lines are folded under 78 characters where a header allows.
//! Nothing the page sends can start a header of its own: line breaks and other control
//! characters in names, subjects and file names become spaces.

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde::Deserialize;

/// Gmail sends up to 25 MB of attachments with an email.
pub const MAX_FILES: usize = 25 * 1024 * 1024;

/// A recipient: an email address, with the name to show with it, if any.
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Address {
    #[serde(default)]
    pub name: String,
    pub email: String,
}

/// A file going with an email: attached, or with a `content_id`, a picture its HTML shows in
/// place (a quoted or forwarded email's `cid:` links).
pub struct File {
    pub name: String,
    pub mime_type: String,
    pub content_id: Option<String>,
    pub data: Vec<u8>,
}

/// An email to send. Gmail fills in the Message-ID, and From when it's left out (the address alone).
pub struct Email {
    /// The account it goes from, with the name Gmail shows for it.
    pub from: Option<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub subject: String,
    pub text: String,
    /// The same text as HTML, where a quoted or forwarded email keeps its own formatting.
    pub html: Option<String>,
    /// Replying or forwarding: the Message-ID of the email answered, and its References.
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub files: Vec<File>,
}

impl Email {
    /// Whether it can go as it is: someone to send it to, every address looks like one, and the
    /// files fit. Errors are for people.
    pub fn check(&self) -> Result<(), String> {
        let everyone: Vec<&Address> = self.to.iter().chain(&self.cc).chain(&self.bcc).collect();
        if everyone.is_empty() {
            return Err("Add someone to send it to.".into());
        }
        for address in everyone {
            check_address(&address.email)?;
        }
        if self.files.iter().map(|f| f.data.len()).sum::<usize>() > MAX_FILES {
            return Err("The attachments are too big: Gmail sends up to 25 MB with an email.".into());
        }
        Ok(())
    }

    /// The email, ready for Gmail to send. `now`: seconds since the Unix epoch, for its Date.
    pub fn build(&self, now: i64) -> Result<Vec<u8>, String> {
        self.check()?;
        Ok(self.write(now))
    }

    /// The email as it is, for a draft, which may still lack recipients or have one half
    /// typed: a recipient that would break its header (a space, `<`, `,`, a letter outside
    /// ASCII…) is left out instead.
    pub fn write(&self, now: i64) -> Vec<u8> {
        let mut out = String::new();
        out += &format!("Date: {}\r\n", date(now));
        if let Some(from) = self.from.as_ref().filter(|a| check_address(&a.email).is_ok()) {
            out += &format!("From: {}\r\n", address(from));
        }
        for (name, list) in [("To", &self.to), ("Cc", &self.cc), ("Bcc", &self.bcc)] {
            let addresses: Vec<String> = list.iter().filter(|a| writable(&a.email)).map(address).collect();
            if !addresses.is_empty() {
                out += &format!("{name}: {}\r\n", addresses.join(",\r\n "));
            }
        }
        out += &format!("Subject: {}\r\n", unstructured(&self.subject, "Subject: ".len()));
        if let Some(id) = self.in_reply_to.as_deref().and_then(message_id) {
            out += &format!("In-Reply-To: {id}\r\n");
        }
        let references: Vec<&str> = self.references.iter().filter_map(|r| message_id(r)).collect();
        if !references.is_empty() {
            out += &format!("References: {}\r\n", references.join("\r\n "));
        }
        out += "MIME-Version: 1.0\r\n";
        out += &self.body();
        out.into_bytes()
    }

    /// The text, then the HTML with its pictures, then the attachments: each a part only where
    /// there's something to put in it.
    fn body(&self) -> String {
        let text = text_part("plain", &self.text);
        let mut body = match &self.html {
            Some(html) => multipart("alternative", &[text, text_part("html", html)]),
            None => text,
        };
        let (pictures, attached): (Vec<&File>, Vec<&File>) =
            self.files.iter().partition(|f| f.content_id.as_deref().and_then(content_id).is_some());
        // Pictures only show in HTML; without it they're left out.
        if self.html.is_some() && !pictures.is_empty() {
            let parts: Vec<String> = std::iter::once(body).chain(pictures.into_iter().map(file_part)).collect();
            body = multipart("related", &parts);
        }
        if !attached.is_empty() {
            let parts: Vec<String> = std::iter::once(body).chain(attached.into_iter().map(file_part)).collect();
            body = multipart("mixed", &parts);
        }
        body
    }
}

/// Whether `email` looks like an address Gmail can send to. Errors are for people.
pub fn check_address(email: &str) -> Result<(), String> {
    let looks_right = email.len() <= 254
        && email.is_ascii()
        && !email.bytes().any(|b| b <= b' ' || b == 0x7f || b"<>()[]\\,;:\"".contains(&b))
        && matches!(email.split_once('@'), Some((local, domain))
            if !local.is_empty() && !domain.contains('@') && domain.contains('.')
                && !domain.starts_with('.') && !domain.ends_with('.') && !domain.contains(".."));
    if looks_right {
        Ok(())
    } else {
        Err(format!("“{email}” isn't an email address. Check it and send again."))
    }
}

/// Whether an address, right or not yet, can go in a header as it is: printable ASCII without
/// the characters that separate addresses.
fn writable(email: &str) -> bool {
    !email.is_empty() && email.len() <= 254 && email.bytes().all(|b| b.is_ascii_graphic() && !b"<>,;\"()[]\\:".contains(&b))
}

/// `Anna Petrova <anna@example.com>`: the name as it is when it's plain ASCII, quoted when it has
/// characters that mean something in an address (`,` `"` `<`...), encoded otherwise.
fn address(a: &Address) -> String {
    let name = clean(&a.name);
    let name = name.trim();
    if name.is_empty() {
        return a.email.clone();
    }
    let plain = |c: char| c.is_ascii_alphanumeric() || " !#$%&'*+-/=?^_`{|}~".contains(c);
    let shown = if name.chars().all(plain) {
        name.to_string()
    } else if name.is_ascii() {
        format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        encoded_words(name).join(" ")
    };
    format!("{shown} <{}>", a.email)
}

/// Header text such as the subject, after a header name `used` characters long: ASCII as it is,
/// folded at spaces; anything else as encoded words, one per line.
fn unstructured(value: &str, used: usize) -> String {
    let value = clean(value);
    let longest_word = value.split(' ').map(str::len).max().unwrap_or(0);
    if !value.is_ascii() || longest_word > 900 {
        return encoded_words(&value).join("\r\n ");
    }
    let mut out = String::new();
    let mut width = used;
    for (i, word) in value.split(' ').enumerate() {
        if i > 0 {
            if width + 1 + word.len() > 76 && width > 1 {
                out += "\r\n";
                width = 0;
            }
            out.push(' ');
            width += 1;
        }
        out += word;
        width += word.len();
    }
    out
}

/// `text` as RFC 2047 encoded words, each under 76 characters, split between characters.
fn encoded_words(text: &str) -> Vec<String> {
    // 45 bytes are 60 characters of base64, 72 with `=?UTF-8?B?` and `?=`.
    const BYTES: usize = 45;
    let mut words = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + BYTES).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        words.push(format!("=?UTF-8?B?{}?=", STANDARD.encode(&text[start..end])));
        start = end;
    }
    if words.is_empty() {
        words.push(String::new());
    }
    words
}

/// Header text with line breaks, tabs and other control characters as spaces.
fn clean(text: &str) -> String {
    text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

/// A Message-ID as it goes in In-Reply-To and References: `<...>`, or nothing if it isn't one.
fn message_id(id: &str) -> Option<&str> {
    let id = id.trim();
    let inner = id.strip_prefix('<')?.strip_suffix('>')?;
    let fine = !inner.is_empty() && inner.bytes().all(|b| b.is_ascii_graphic() && b != b'<' && b != b'>');
    fine.then_some(id)
}

/// A Content-ID as the HTML's `cid:` links name it, or nothing if it can't be one.
fn content_id(id: &str) -> Option<&str> {
    let id = id.trim();
    let fine = !id.is_empty() && id.len() < 250 && id.bytes().all(|b| b.is_ascii_graphic() && b != b'<' && b != b'>');
    fine.then_some(id)
}

fn text_part(subtype: &str, text: &str) -> String {
    format!(
        "Content-Type: text/{subtype}; charset=UTF-8\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n{}",
        quoted_printable(text)
    )
}

/// A multipart of `kind` (mixed, alternative, related). `=_` can't occur in quoted-printable or
/// base64, so the boundary can't turn up inside a part.
fn multipart(kind: &str, parts: &[String]) -> String {
    let boundary = format!("=_um_{}", random_hex());
    let mut out = format!("Content-Type: multipart/{kind}; boundary=\"{boundary}\"\r\n\r\n");
    for part in parts {
        out += &format!("--{boundary}\r\n{part}\r\n");
    }
    out += &format!("--{boundary}--");
    out
}

fn file_part(file: &File) -> String {
    let name = parameter(&file.name);
    let kind = mime_type(&file.mime_type);
    let disposition = match file.content_id.as_deref().and_then(content_id) {
        Some(id) => format!("inline; filename={name}\r\nContent-ID: <{id}>"),
        None => format!("attachment; filename={name}"),
    };
    format!(
        "Content-Type: {kind}; name={name}\r\nContent-Disposition: {disposition}\r\nContent-Transfer-Encoding: base64\r\n\r\n{}",
        base64_lines(&file.data)
    )
}

/// A file name as a header parameter: quoted ASCII, or an encoded word in quotes, which is how
/// Gmail and most mail apps send names outside ASCII, and how they all read them.
fn parameter(name: &str) -> String {
    let name = clean(name);
    let name = if name.trim().is_empty() { "file".to_string() } else { name.trim().to_string() };
    if name.is_ascii() && !name.contains(['"', '\\']) {
        format!("\"{name}\"")
    } else {
        format!("\"{}\"", encoded_words(&name).concat())
    }
}

/// A file's type as the page knows it, if it's a plain `type/subtype`.
fn mime_type(kind: &str) -> String {
    let token = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$&^_.+-".contains(&b));
    match kind.trim().split_once('/') {
        Some((a, b)) if token(a) && token(b) => kind.trim().to_ascii_lowercase(),
        _ => "application/octet-stream".into(),
    }
}

fn base64_lines(data: &[u8]) -> String {
    let encoded = STANDARD.encode(data);
    let lines: Vec<&str> = encoded.as_bytes().chunks(76).map(|line| std::str::from_utf8(line).unwrap_or_default()).collect();
    lines.join("\r\n")
}

/// Quoted-printable (RFC 2045): ASCII stays readable, other bytes become `=XX`, lines end in CRLF
/// and longer ones break softly (`=` at the end) to stay under 77 characters.
fn quoted_printable(text: &str) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out += "\r\n";
        }
        let bytes = line.as_bytes();
        let mut width = 0;
        for (j, &b) in bytes.iter().enumerate() {
            // Spaces at a line's end would be dropped on the way, so they're encoded there.
            let at_end = j + 1 == bytes.len();
            let literal = matches!(b, b'!'..=b'<' | b'>'..=b'~') || (matches!(b, b' ' | b'\t') && !at_end);
            let size = if literal { 1 } else { 3 };
            if width + size > 75 {
                out += "=\r\n";
                width = 0;
            }
            if literal {
                out.push(b as char);
            } else {
                out += &format!("={b:02X}");
            }
            width += size;
        }
    }
    out
}

/// `Thu, 12 Nov 2026 14:15:11 +0000` for `secs` since the Unix epoch.
fn date(secs: i64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let days = secs.div_euclid(86_400);
    let time = secs.rem_euclid(86_400);
    // Days to year, month and day: Howard Hinnant's `civil_from_days`.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{}, {day} {} {year} {:02}:{:02}:{:02} +0000",
        DAYS[days.rem_euclid(7) as usize],
        MONTHS[(month - 1) as usize],
        time / 3600,
        time % 3600 / 60,
        time % 60
    )
}

/// 24 random hex digits, for MIME boundaries.
pub fn random_hex() -> String {
    let mut bytes = [0u8; 12];
    getrandom::getrandom(&mut bytes).expect("OS random number generator unavailable");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to(email: &str) -> Address {
        Address { name: String::new(), email: email.into() }
    }

    fn email(text: &str) -> Email {
        Email {
            from: None,
            to: vec![to("anna@example.com")],
            cc: vec![],
            bcc: vec![],
            subject: "Lunch".into(),
            text: text.into(),
            html: None,
            in_reply_to: None,
            references: vec![],
            files: vec![],
        }
    }

    fn built(email: &Email) -> String {
        String::from_utf8(email.build(1_730_000_000).unwrap()).unwrap()
    }

    #[test]
    fn writes_dates_as_mail_does() {
        assert_eq!(date(0), "Thu, 1 Jan 1970 00:00:00 +0000");
        assert_eq!(date(1_730_000_000), "Sun, 27 Oct 2024 03:33:20 +0000");
        assert_eq!(date(951_782_400), "Tue, 29 Feb 2000 00:00:00 +0000");
        assert_eq!(date(1_798_675_200), "Thu, 31 Dec 2026 00:00:00 +0000");
    }

    #[test]
    fn checks_addresses() {
        for good in ["anna@example.com", "a.b+tag@mail.example.co.uk", "x_y-z@sub.example.org"] {
            assert!(check_address(good).is_ok(), "{good}");
        }
        for bad in ["", "anna", "anna@", "@example.com", "anna@example", "a b@example.com", "anna@@example.com",
            "anna@example..com", "anna@.example.com", "anna@example.com\r\nBcc: x@y.z", "анна@example.com", "<a@b.c>"] {
            assert!(check_address(bad).is_err(), "{bad}");
        }
        let mut message = email("Hi");
        message.to.clear();
        assert_eq!(message.check().unwrap_err(), "Add someone to send it to.");
        message.bcc.push(to("me@example.com"));
        assert!(message.check().is_ok());
    }

    #[test]
    fn writes_names_safely() {
        let named = |name: &str| address(&Address { name: name.into(), email: "a@b.co".into() });
        assert_eq!(named(""), "a@b.co");
        assert_eq!(named("Anna Petrova"), "Anna Petrova <a@b.co>");
        assert_eq!(named("Petrova, Anna"), "\"Petrova, Anna\" <a@b.co>");
        assert_eq!(named("Anna \"Ann\" P."), "\"Anna \\\"Ann\\\" P.\" <a@b.co>");
        assert_eq!(named("Анна"), "=?UTF-8?B?0JDQvdC90LA=?= <a@b.co>");
        // A line break can't start a header of its own.
        assert_eq!(named("Anna\r\nBcc: spy@x.co"), "\"Anna  Bcc: spy@x.co\" <a@b.co>");
    }

    #[test]
    fn encodes_subjects() {
        assert_eq!(unstructured("Lunch on Friday?", 9), "Lunch on Friday?");
        assert_eq!(unstructured("Re: Привет", 9), "=?UTF-8?B?UmU6INCf0YDQuNCy0LXRgg==?=");
        assert_eq!(unstructured("one\r\nBcc: x@y.z", 9), "one  Bcc: x@y.z");
        let long = "word ".repeat(30);
        let folded = unstructured(long.trim(), 9);
        assert!(folded.split("\r\n").all(|line| line.len() <= 76), "{folded}");
        assert_eq!(folded.replace("\r\n", ""), long.trim());
        // Long text outside ASCII: encoded words that decode back to it, split between characters.
        let russian = "Отчёт за третий квартал и фотографии с объекта, как договаривались";
        let words = encoded_words(russian);
        assert!(words.len() > 1 && words.iter().all(|w| w.len() <= 75));
        let decoded: Vec<u8> = words
            .iter()
            .flat_map(|w| STANDARD.decode(w.trim_start_matches("=?UTF-8?B?").trim_end_matches("?=")).unwrap())
            .collect();
        assert_eq!(String::from_utf8(decoded).unwrap(), russian);
    }

    #[test]
    fn quoted_printable_round_trips() {
        assert_eq!(quoted_printable("Hi Anna,\n\nSee you = then."), "Hi Anna,\r\n\r\nSee you =3D then.");
        assert_eq!(quoted_printable("trailing space \nand tab\t"), "trailing space=20\r\nand tab=09");
        assert_eq!(quoted_printable("Привет"), "=D0=9F=D1=80=D0=B8=D0=B2=D0=B5=D1=82");
        let long = "x".repeat(200);
        let encoded = quoted_printable(&long);
        assert!(encoded.split("\r\n").all(|line| line.len() <= 76));
        assert_eq!(encoded.replace("=\r\n", ""), long);
        // A soft break never splits an `=XX`.
        let mixed = "ё".repeat(60);
        let encoded = quoted_printable(&mixed);
        for line in encoded.split("\r\n") {
            assert!(line.len() <= 76, "{line}");
            let line = line.strip_suffix('=').unwrap_or(line);
            let whole = |i: usize| line.get(i + 1..i + 3).is_some_and(|hex| hex.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(line.match_indices('=').all(|(i, _)| whole(i)), "{line}");
        }
        let unfolded = encoded.replace("=\r\n", "");
        let bytes: Vec<u8> = unfolded
            .split('=')
            .skip(1)
            .map(|hex| u8::from_str_radix(&hex[..2], 16).unwrap())
            .collect();
        assert_eq!(String::from_utf8(bytes).unwrap(), mixed);
    }

    #[test]
    fn writes_a_plain_email() {
        let mut message = email("Hi Anna,\nFriday at one?");
        message.cc = vec![Address { name: "Mark Chen".into(), email: "mark@example.com".into() }, to("x@example.com")];
        message.in_reply_to = Some("<CAB123@mail.gmail.com>".into());
        message.references = vec!["<first@example.com>".into(), "not an id".into(), "<CAB123@mail.gmail.com>".into()];
        assert_eq!(
            built(&message),
            "Date: Sun, 27 Oct 2024 03:33:20 +0000\r\n\
             To: anna@example.com\r\n\
             Cc: Mark Chen <mark@example.com>,\r\n x@example.com\r\n\
             Subject: Lunch\r\n\
             In-Reply-To: <CAB123@mail.gmail.com>\r\n\
             References: <first@example.com>\r\n <CAB123@mail.gmail.com>\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: text/plain; charset=UTF-8\r\n\
             Content-Transfer-Encoding: quoted-printable\r\n\
             \r\n\
             Hi Anna,\r\nFriday at one?"
        );
        // With the account's name from Gmail's settings, From comes after Date.
        message.from = Some(Address { name: "Алекс".into(), email: "alex@example.com".into() });
        assert!(built(&message).starts_with(
            "Date: Sun, 27 Oct 2024 03:33:20 +0000\r\nFrom: =?UTF-8?B?0JDQu9C10LrRgQ==?= <alex@example.com>\r\nTo: "
        ));
    }

    #[test]
    fn nests_html_pictures_and_files() {
        let mut message = email("See the photo.");
        message.html = Some("<p>See the photo.</p><img src=\"cid:logo@x\">".into());
        message.files = vec![
            File { name: "logo.png".into(), mime_type: "image/png".into(), content_id: Some("logo@x".into()), data: vec![1, 2, 3] },
            File { name: "Отчёт \"Q3\".pdf".into(), mime_type: "application/pdf".into(), content_id: None, data: vec![0; 100] },
            File { name: "notes".into(), mime_type: "text/plain\r\nX: y".into(), content_id: None, data: b"hi".to_vec() },
        ];
        let raw = built(&message);
        let mixed = raw.find("multipart/mixed").unwrap();
        let related = raw.find("multipart/related").unwrap();
        let alternative = raw.find("multipart/alternative").unwrap();
        assert!(mixed < related && related < alternative);
        assert!(raw.find("text/plain; charset").unwrap() < raw.find("text/html; charset").unwrap());
        assert!(raw.contains("Content-Type: image/png; name=\"logo.png\"\r\nContent-Disposition: inline; filename=\"logo.png\"\r\nContent-ID: <logo@x>\r\n"));
        assert!(raw.contains("Content-Type: application/pdf; name=\"=?UTF-8?B?"));
        assert!(raw.contains("Content-Disposition: attachment; filename=\"=?UTF-8?B?"));
        assert!(raw.contains("Content-Type: application/octet-stream; name=\"notes\""));
        assert!(!raw.contains("\r\nX: y"));
        // Every line stays under the 998-character limit, and every boundary opened is closed.
        assert!(raw.split("\r\n").all(|line| line.len() <= 998));
        for boundary in raw.split("boundary=\"").skip(1).map(|rest| &rest[..rest.find('"').unwrap()]) {
            assert_eq!(raw.matches(&format!("--{boundary}--")).count(), 1);
        }
        // Without HTML, pictures have nowhere to show and are left out.
        message.html = None;
        let raw = built(&message);
        assert!(!raw.contains("multipart/related") && !raw.contains("logo.png") && raw.contains("multipart/mixed"));
    }

    #[test]
    fn drafts_keep_what_can_go_in_a_header() {
        let mut draft = email("Half written");
        draft.to = vec![to("anna@exam"), to("bad address"), to("x@y.z\r\nBcc: spy@x.co"), to("ok@example.com")];
        assert!(draft.check().is_err());
        let raw = String::from_utf8(draft.write(1_730_000_000)).unwrap();
        assert!(raw.contains("\r\nTo: anna@exam,\r\n ok@example.com\r\n"), "{raw}");
        assert!(!raw.contains("spy") && !raw.contains("bad address"));
        // Nobody to send it to yet: no To at all.
        draft.to.clear();
        let raw = String::from_utf8(draft.write(1_730_000_000)).unwrap();
        assert!(!raw.contains("\r\nTo:") && raw.contains("\r\nSubject: Lunch\r\n"));
    }

    #[test]
    fn refuses_too_much_to_attach() {
        let mut message = email("Big");
        message.files.push(File { name: "big.bin".into(), mime_type: String::new(), content_id: None, data: vec![0; MAX_FILES + 1] });
        assert!(message.check().unwrap_err().contains("25 MB"));
    }
}
