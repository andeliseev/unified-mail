//! Yandex Mail accounts, over IMAP at imap.yandex.com: Yandex has no mail REST API like Gmail's.
//!
//! - Accounts sign in with an app password the user creates in Yandex ID (IMAP LOGIN). Yandex
//!   doesn't give third-party apps OAuth tokens with mail rights (`unauthorized_client`), see
//!   docs/auth.md.
//! - Folders are IMAP folders: the Inbox is `INBOX` and Trash is the folder marked `\Trash`.
//!   Yandex has no All Mail. Until it shows Inbox + Archive (docs/todo.md), All Mail shows the Inbox.
//! - Message ids are `<folder letter><UIDVALIDITY>.<UID>`, e.g. `i1700000000.42`: an IMAP UID only
//!   identifies a message within one folder, and only while the folder keeps its UIDVALIDITY.
//! - The server splits messages into parts (`BODYSTRUCTURE`), so no MIME parser is needed: the app
//!   fetches only the part it shows and undoes its transfer encoding (base64, quoted-printable).
//!   Charsets, and encoded words in headers (`=?koi8-r?B?...?=`), are decoded by the webview, as
//!   for Gmail bodies.
//! - Reading uses `EXAMINE` (read-only) and `BODY.PEEK`, so it never marks mail read by itself.
//!   `set_unread` is the only change made so far; moving mail (Archive, Delete, Undo) comes next.
//! - Each operation opens its own IMAP connection and signs out after.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use async_imap::imap_proto::types::{Address, BodyStructure, ContentEncoding, SectionPath};
use async_imap::types::{Fetch, Flag, NameAttribute};
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig, STANDARD, URL_SAFE};
use base64::engine::DecodePaddingMode;
use base64::{alphabet, Engine as _};
use futures_util::TryStreamExt;

use crate::imap::{self, step, Auth, Session};
use crate::provider::{Body, Folder, FullMessage, MessageSummary, Page, PAGE_SIZE};

/// TLS 1.2 only: Windows' TLS dropped Yandex's long-lived TLS 1.3 connections (push), see `imap::Server`.
pub const SERVER: imap::Server = imap::Server { host: "imap.yandex.com", tls12_only: true };
/// Bytes of a message's text part fetched for its preview in the list.
const PREVIEW_BYTES: u32 = 600;
const PREVIEW_CHARS: usize = 200;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;
pub const NOT_YET: &str = "Archive, Delete and Undo don't work for Yandex accounts yet.";

/// Base64 as found in mail: padding optional, stray bits at the end tolerated.
const LENIENT_BASE64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

/// Signs in once to check the address and app password, for Add account and Reconnect.
pub async fn check_sign_in(account: &str, password: &str) -> Result<(), String> {
    let mut session = imap::login(&SERVER, account, Auth::Password(password)).await.map_err(|e| {
        if imap::is_rejection(&e) {
            "Yandex didn't accept this address and password. Use an app password (not your Yandex \
             password), and check that IMAP is on in Yandex Mail settings."
                .to_string()
        } else {
            format!("Couldn't reach Yandex Mail: {e}")
        }
    })?;
    let _ = session.logout().await;
    Ok(())
}

/// One page of `folder`: the newest messages, or with `before` (ms) the newest ones at or before it.
pub async fn page(account: &str, password: &str, folder: Folder, before: Option<i64>) -> Result<Page, String> {
    let mut session = imap::login(&SERVER, account, Auth::Password(password)).await?;
    let result = page_in(&mut session, folder, before).await;
    let _ = session.logout().await;
    result
}

async fn page_in(session: &mut Session, folder: Folder, before: Option<i64>) -> Result<Page, String> {
    let folder = shown_folder(folder);
    let validity = open_folder(session, folder, false).await?;
    let page_size = PAGE_SIZE as usize;
    let (uids, more) = match before {
        None => {
            // UIDs grow as mail arrives, so the highest ones are the newest.
            let mut all = newest_first(step(session.uid_search("ALL")).await?);
            let more = all.len() > page_size;
            all.truncate(page_size);
            (all, more)
        }
        Some(before) => {
            // SEARCH works in whole days (in each message's own time zone), so ask for everything
            // before two days later, then check the exact dates of the newest candidates.
            let query = format!("BEFORE {}", imap_date(before + 2 * DAY_MS));
            let candidates = newest_first(step(session.uid_search(query)).await?);
            let mut older: Vec<(i64, u32)> = Vec::new();
            let mut checked = 0;
            while older.len() <= page_size && checked < candidates.len() {
                let chunk = &candidates[checked..candidates.len().min(checked + 2 * page_size)];
                checked += chunk.len();
                let dates = fetch(session, &uid_set(chunk), "(UID INTERNALDATE)").await?;
                older.extend(dates.iter().filter_map(|f| {
                    let date = f.internal_date()?.timestamp_millis();
                    (date <= before).then_some((date, f.uid?))
                }));
            }
            older.sort_unstable_by(|a, b| b.cmp(a));
            let more = older.len() > page_size || checked < candidates.len();
            older.truncate(page_size);
            (older.into_iter().map(|(_, uid)| uid).collect(), more)
        }
    };
    let unseen: HashSet<u32> = if uids.is_empty() {
        HashSet::new()
    } else {
        step(session.uid_search(format!("UID {} UNSEEN", uid_set(&uids)))).await?
    };
    let id = |uid: u32| format_id(folder, validity, uid);
    Ok(Page {
        ids: uids.iter().map(|&uid| id(uid)).collect(),
        unread: unseen.into_iter().map(id).collect(),
        more,
    })
}

/// Summaries for the list. One that fails is returned as `(id, error)`.
pub async fn summaries(account: &str, password: &str, ids: Vec<String>) -> Vec<Result<MessageSummary, (String, String)>> {
    if ids.is_empty() {
        return Vec::new();
    }
    let mut session = match imap::login(&SERVER, account, Auth::Password(password)).await {
        Ok(session) => session,
        Err(e) => return ids.into_iter().map(|id| Err((id, e.clone()))).collect(),
    };
    let mut results = Vec::new();
    let mut groups: HashMap<(Folder, u32), Vec<(u32, String)>> = HashMap::new();
    for id in ids {
        match parse_id(&id) {
            Ok(m) => groups.entry((m.folder, m.validity)).or_default().push((m.uid, id)),
            Err(e) => results.push(Err((id, e))),
        }
    }
    for ((folder, validity), members) in groups {
        match summaries_in(&mut session, account, folder, validity, &members).await {
            Ok(mut found) => results.extend(members.into_iter().map(|(uid, id)| {
                found.remove(&uid).ok_or((id, "The message is no longer on the server.".to_string()))
            })),
            Err(e) => results.extend(members.into_iter().map(|(_, id)| Err((id, e.clone())))),
        }
    }
    let _ = session.logout().await;
    results
}

async fn summaries_in(
    session: &mut Session,
    account: &str,
    folder: Folder,
    validity: u32,
    members: &[(u32, String)],
) -> Result<HashMap<u32, MessageSummary>, String> {
    check_validity(open_folder(session, folder, false).await?, validity)?;
    let uids: Vec<u32> = members.iter().map(|(uid, _)| *uid).collect();
    let fetches = fetch(session, &uid_set(&uids), "(UID FLAGS INTERNALDATE ENVELOPE BODYSTRUCTURE)").await?;
    let mut found = HashMap::new();
    let mut previews = Vec::new();
    for f in &fetches {
        let Some(uid) = f.uid else { continue };
        let envelope = f.envelope();
        let id = format_id(folder, validity, uid);
        found.insert(
            uid,
            MessageSummary {
                account: account.to_string(),
                thread_id: id.clone(),
                id,
                from: envelope.map(|e| addresses(&e.from)).unwrap_or_default(),
                subject: envelope.map(|e| text(e.subject.as_deref())).unwrap_or_default(),
                snippet: String::new(),
                date: f.internal_date().map(|d| d.timestamp_millis()).unwrap_or(0),
                unread: !f.flags().any(|flag| matches!(flag, Flag::Seen)),
            },
        );
        if let Some(part) = f.bodystructure().and_then(|b| find_bodies(b).1) {
            previews.push((uid, part));
        }
    }
    // A preview needs the start of each message's text part: one small fetch per message.
    for (uid, part) in previews {
        let query = format!("(UID BODY.PEEK[{}]<0.{PREVIEW_BYTES}>)", part.section_name());
        let Ok(fetches) = fetch(session, &uid.to_string(), &query).await else { continue };
        let data = fetches.iter().find_map(|f| f.section(&part.section_path()));
        if let (Some(data), Some(summary)) = (data, found.get_mut(&uid)) {
            summary.snippet = snippet(&part.decode(data, true), &part.charset);
        }
    }
    Ok(found)
}

/// A message to show: its headers, and its HTML part, or its text part if it has no HTML.
pub async fn message(account: &str, password: &str, id: &str) -> Result<FullMessage, String> {
    let m = parse_id(id)?;
    let mut session = imap::login(&SERVER, account, Auth::Password(password)).await?;
    let result = message_in(&mut session, &m).await;
    let _ = session.logout().await;
    result
}

async fn message_in(session: &mut Session, m: &MessageId) -> Result<FullMessage, String> {
    check_validity(open_folder(session, m.folder, false).await?, m.validity)?;
    let fetches = fetch(session, &m.uid.to_string(), "(UID INTERNALDATE ENVELOPE BODYSTRUCTURE)").await?;
    let f = fetches
        .iter()
        .find(|f| f.uid == Some(m.uid))
        .ok_or("This message is no longer on the server.")?;
    let envelope = f.envelope();
    let field = |pick: fn(&async_imap::imap_proto::types::Envelope<'_>) -> String| envelope.map(pick).unwrap_or_default();
    let (from, to, cc) = (field(|e| addresses(&e.from)), field(|e| addresses(&e.to)), field(|e| addresses(&e.cc)));
    let subject = field(|e| text(e.subject.as_deref()));
    let date = f.internal_date().map(|d| d.timestamp_millis()).unwrap_or(0);
    let (html, text_part) = f.bodystructure().map(find_bodies).unwrap_or_default();

    let shown = html.as_ref().or(text_part.as_ref());
    let body = match shown {
        Some(part) => {
            let query = format!("(UID BODY.PEEK[{}])", part.section_name());
            let fetches = fetch(session, &m.uid.to_string(), &query).await?;
            let data = fetches.iter().find_map(|f| f.section(&part.section_path())).unwrap_or_default();
            Some(Body { data: URL_SAFE.encode(part.decode(data, false)), charset: part.charset.clone() })
        }
        None => None,
    };
    let (html, text) = if html.is_some() { (body, None) } else { (None, body) };
    Ok(FullMessage { from, to, cc, subject, date, html, text })
}

/// Sets or clears the message's `\Seen` flag.
pub async fn set_unread(account: &str, password: &str, id: &str, unread: bool) -> Result<(), String> {
    let m = parse_id(id)?;
    let mut session = imap::login(&SERVER, account, Auth::Password(password)).await?;
    let result = async {
        check_validity(open_folder(&mut session, m.folder, true).await?, m.validity)?;
        let change = if unread { "-FLAGS.SILENT (\\Seen)" } else { "+FLAGS.SILENT (\\Seen)" };
        step(async { session.uid_store(m.uid.to_string(), change).await?.try_collect::<Vec<_>>().await }).await?;
        Ok(())
    }
    .await;
    let _ = session.logout().await;
    result
}

/// The folder whose messages `folder` shows. Yandex has no All Mail yet: it shows the Inbox.
fn shown_folder(folder: Folder) -> Folder {
    match folder {
        Folder::Inbox | Folder::All => Folder::Inbox,
        Folder::Trash => Folder::Trash,
    }
}

/// Opens the folder (read-only unless `write`) and returns its UIDVALIDITY.
async fn open_folder(session: &mut Session, folder: Folder, write: bool) -> Result<u32, String> {
    let name = match folder {
        Folder::Inbox | Folder::All => "INBOX".to_string(),
        Folder::Trash => {
            let names = step(async { session.list(Some(""), Some("*")).await?.try_collect::<Vec<_>>().await }).await?;
            names
                .iter()
                .find(|n| n.attributes().iter().any(|a| matches!(a, NameAttribute::Trash)))
                .map(|n| n.name().to_string())
                .ok_or("This Yandex account has no Trash folder.")?
        }
    };
    let mailbox = if write { step(session.select(&name)).await? } else { step(session.examine(&name)).await? };
    mailbox.uid_validity.ok_or_else(|| "The mail server didn't identify the folder (no UIDVALIDITY).".to_string())
}

fn check_validity(current: u32, expected: u32) -> Result<(), String> {
    if current == expected {
        Ok(())
    } else {
        Err("The folder changed on the mail server. It will reload with the next sync.".into())
    }
}

async fn fetch(session: &mut Session, uid_set: &str, query: &str) -> Result<Vec<Fetch>, String> {
    step(async { session.uid_fetch(uid_set, query).await?.try_collect::<Vec<_>>().await }).await
}

fn newest_first(uids: HashSet<u32>) -> Vec<u32> {
    let mut uids: Vec<u32> = uids.into_iter().collect();
    uids.sort_unstable_by(|a, b| b.cmp(a));
    uids
}

fn uid_set(uids: &[u32]) -> String {
    uids.iter().map(u32::to_string).collect::<Vec<_>>().join(",")
}

/// Where a message is: its folder, that folder's UIDVALIDITY, and its UID there.
#[derive(Debug, PartialEq)]
struct MessageId {
    folder: Folder,
    validity: u32,
    uid: u32,
}

fn format_id(folder: Folder, validity: u32, uid: u32) -> String {
    let letter = match shown_folder(folder) {
        Folder::Trash => 't',
        _ => 'i',
    };
    format!("{letter}{validity}.{uid}")
}

fn parse_id(id: &str) -> Result<MessageId, String> {
    let invalid = || format!("Invalid message id: {id}");
    let folder = match id.chars().next() {
        Some('i') => Folder::Inbox,
        Some('t') => Folder::Trash,
        _ => return Err(invalid()),
    };
    let (validity, uid) = id[1..].split_once('.').ok_or_else(invalid)?;
    Ok(MessageId {
        folder,
        validity: validity.parse().map_err(|_| invalid())?,
        uid: uid.parse().map_err(|_| invalid())?,
    })
}

/// An IMAP SEARCH date (`4-Nov-2026`) for the UTC day of `ms`.
fn imap_date(ms: i64) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    // Days since 1970-01-01 to a civil date (Howard Hinnant's `civil_from_days`).
    let z = ms.div_euclid(DAY_MS) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{day}-{}-{year}", MONTHS[(month - 1) as usize])
}

/// Header bytes as text. Encoded words (`=?utf-8?B?...?=`) are left for the webview to decode.
fn text(bytes: Option<&[u8]>) -> String {
    bytes.map(String::from_utf8_lossy).map(Cow::into_owned).unwrap_or_default()
}

/// `Name <user@host>, ...`, like a From or To header.
fn addresses(list: &Option<Vec<Address<'_>>>) -> String {
    list.iter()
        .flatten()
        .filter(|a| a.mailbox.is_some())
        .map(|a| {
            let mailbox = text(a.mailbox.as_deref());
            let email = match a.host.as_deref() {
                Some(host) if !host.is_empty() => format!("{mailbox}@{}", String::from_utf8_lossy(host)),
                _ => mailbox,
            };
            match text(a.name.as_deref()) {
                name if name.is_empty() => email,
                name => format!("{name} <{email}>"),
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum TransferEncoding {
    Plain,
    Base64,
    QuotedPrintable,
}

/// A body part to fetch: where it is, and how to decode it.
#[derive(Clone, Debug, PartialEq)]
struct Part {
    section: Vec<u32>,
    encoding: TransferEncoding,
    charset: String,
}

impl Part {
    fn section_name(&self) -> String {
        self.section.iter().map(u32::to_string).collect::<Vec<_>>().join(".")
    }

    fn section_path(&self) -> SectionPath {
        SectionPath::Part(self.section.clone(), None)
    }

    /// Undoes the transfer encoding. `partial` data may end mid-way through it.
    fn decode(&self, data: &[u8], partial: bool) -> Vec<u8> {
        match self.encoding {
            TransferEncoding::Plain => data.to_vec(),
            TransferEncoding::QuotedPrintable => decode_quoted_printable(data),
            TransferEncoding::Base64 => {
                let mut clean: Vec<u8> = data.iter().copied().filter(|b| !b.is_ascii_whitespace()).collect();
                if partial {
                    clean.truncate(clean.len() / 4 * 4);
                }
                // Undecodable base64 is shown as it came rather than not at all.
                LENIENT_BASE64.decode(&clean).unwrap_or_else(|_| data.to_vec())
            }
        }
    }
}

/// The first HTML and plain-text parts that aren't attachments, depth first.
/// Forwarded messages (message/rfc822) count as attachments.
fn find_bodies(root: &BodyStructure<'_>) -> (Option<Part>, Option<Part>) {
    fn walk(part: &BodyStructure<'_>, section: Vec<u32>, html: &mut Option<Part>, text: &mut Option<Part>) {
        match part {
            BodyStructure::Multipart { bodies, .. } => {
                for (i, child) in bodies.iter().enumerate() {
                    let mut child_section = section.clone();
                    child_section.push(i as u32 + 1);
                    walk(child, child_section, html, text);
                }
            }
            BodyStructure::Text { common, other, .. } => {
                let attachment = common.disposition.as_ref().is_some_and(|d| d.ty.eq_ignore_ascii_case("attachment"))
                    || param(&common.ty.params, "name").is_some();
                if attachment {
                    return;
                }
                let found = Part {
                    section,
                    encoding: match other.transfer_encoding {
                        ContentEncoding::Base64 => TransferEncoding::Base64,
                        ContentEncoding::QuotedPrintable => TransferEncoding::QuotedPrintable,
                        _ => TransferEncoding::Plain,
                    },
                    charset: param(&common.ty.params, "charset")
                        .map(|c| c.trim_matches('"').to_ascii_lowercase())
                        .filter(|c| !c.is_empty())
                        .unwrap_or_else(|| "utf-8".into()),
                };
                match common.ty.subtype.to_ascii_lowercase().as_str() {
                    "html" if html.is_none() => *html = Some(found),
                    "plain" if text.is_none() => *text = Some(found),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    let (mut html, mut text) = (None, None);
    // A message that isn't multipart is its own part 1; a multipart one's parts are 1, 2, ...
    let start = if matches!(root, BodyStructure::Multipart { .. }) { vec![] } else { vec![1] };
    walk(root, start, &mut html, &mut text);
    (html, text)
}

fn param<'a>(params: &'a Option<Vec<(Cow<'_, str>, Cow<'_, str>)>>, name: &str) -> Option<&'a str> {
    params.iter().flatten().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_ref())
}

fn decode_quoted_printable(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        match data[i] {
            b'=' => {
                let rest = &data[i + 1..];
                if rest.starts_with(b"\r\n") {
                    i += 3; // soft line break
                } else if rest.starts_with(b"\n") {
                    i += 2;
                } else if let Some(byte) = rest.get(..2).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    out.push(byte);
                    i += 3;
                } else if rest.len() < 2 {
                    break; // cut off mid-escape (a partial fetch)
                } else {
                    out.push(b'=');
                    i += 1;
                }
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    out
}

/// A list preview from the start of a text part. UTF-8 is decoded here; other charsets are
/// passed on as an encoded word, which the webview decodes like any header.
fn snippet(bytes: &[u8], charset: &str) -> String {
    let utf8 = matches!(charset, "utf-8" | "utf8" | "us-ascii" | "ascii");
    let text = if utf8 {
        String::from_utf8_lossy(bytes).trim_end_matches('\u{FFFD}').to_string()
    } else {
        return format!("=?{charset}?B?{}?=", STANDARD.encode(bytes));
    };
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(PREVIEW_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip() {
        let id = format_id(Folder::Inbox, 1_700_000_000, 42);
        assert_eq!(id, "i1700000000.42");
        assert_eq!(parse_id(&id).unwrap(), MessageId { folder: Folder::Inbox, validity: 1_700_000_000, uid: 42 });
        assert_eq!(parse_id(&format_id(Folder::Trash, 7, 9)).unwrap().folder, Folder::Trash);
        // All Mail shows the Inbox, so its messages keep their Inbox ids.
        assert_eq!(format_id(Folder::All, 1, 2), "i1.2");
        for bad in ["", "x1.2", "i1", "i1.", "i.2", "i1.2.3", "i-1.2", "18c2f0a9b3e4d5f6"] {
            assert!(parse_id(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn imap_dates() {
        assert_eq!(imap_date(0), "1-Jan-1970");
        assert_eq!(imap_date(1_730_000_000_000), "27-Oct-2024");
        assert_eq!(imap_date(951_782_400_000), "29-Feb-2000");
        assert_eq!(imap_date(1_798_675_200_000), "31-Dec-2026");
    }

    #[test]
    fn decodes_quoted_printable() {
        assert_eq!(decode_quoted_printable(b"caf=C3=A9 =\r\nau lait"), "café au lait".as_bytes());
        assert_eq!(decode_quoted_printable(b"a=3Db=\nc"), b"a=bc");
        // Cut off mid-escape by a partial fetch: the escape is dropped.
        assert_eq!(decode_quoted_printable(b"ok=C"), b"ok");
        assert_eq!(decode_quoted_printable(b"50=% off"), b"50=% off");
    }

    #[test]
    fn decodes_partial_base64() {
        let part = Part { section: vec![1], encoding: TransferEncoding::Base64, charset: "utf-8".into() };
        assert_eq!(part.decode(b"aGVs\r\nbG8=", false), b"hello");
        // A partial fetch can stop mid-group: the incomplete group is left out.
        assert_eq!(part.decode(b"aGVsbG8gd2", true), b"hello ");
        assert_eq!(part.section_name(), "1");
    }

    /// The BODYSTRUCTURE of the response line, as the server sends it.
    fn bodies(line: &[u8]) -> (Option<Part>, Option<Part>) {
        use async_imap::imap_proto::{parser::parse_response, AttributeValue, Response};
        let (_, response) = parse_response(line).unwrap();
        let Response::Fetch(_, attrs) = response else { panic!("not a FETCH") };
        let structure = attrs
            .iter()
            .find_map(|a| match a {
                AttributeValue::BodyStructure(b) => Some(b),
                _ => None,
            })
            .unwrap();
        find_bodies(structure)
    }

    #[test]
    fn finds_the_parts_to_show() {
        // multipart/mixed: (alternative: KOI8-R text + HTML), then a PDF and a text attachment.
        let (html, text) = bodies(
            b"* 1 FETCH (UID 5 BODYSTRUCTURE (((\"text\" \"plain\" (\"charset\" \"KOI8-R\") NIL NIL \"base64\" 120 2)\
              (\"text\" \"html\" (\"charset\" \"utf-8\") NIL NIL \"quoted-printable\" 900 12) \"alternative\")\
              (\"application\" \"pdf\" (\"name\" \"a.pdf\") NIL NIL \"base64\" 5000)\
              (\"text\" \"plain\" (\"name\" \"notes.txt\") NIL NIL \"7bit\" 40 1) \"mixed\"))\r\n",
        );
        let html = html.unwrap();
        assert_eq!((html.section_name(), html.encoding, html.charset.as_str()), ("1.2".into(), TransferEncoding::QuotedPrintable, "utf-8"));
        let text = text.unwrap();
        assert_eq!((text.section_name(), text.encoding, text.charset.as_str()), ("1.1".into(), TransferEncoding::Base64, "koi8-r"));

        // A message that isn't multipart is its own part 1.
        let (html, text) = bodies(b"* 2 FETCH (UID 6 BODYSTRUCTURE (\"text\" \"plain\" NIL NIL NIL \"7bit\" 10 1))\r\n");
        assert!(html.is_none());
        let text = text.unwrap();
        assert_eq!((text.section_name(), text.charset.as_str()), ("1".into(), "utf-8"));
    }

    #[test]
    fn snippets() {
        assert_eq!(snippet(b"Hi  Anna,\r\n\r\nsee you", "utf-8"), "Hi Anna, see you");
        // A UTF-8 character cut in half at the end is dropped.
        assert_eq!(snippet("ok é".as_bytes().split_last().unwrap().1, "utf-8"), "ok");
        // Other charsets go to the webview as an encoded word.
        assert_eq!(snippet(&[0xf0, 0xd2], "koi8-r"), "=?koi8-r?B?8NI=?=");
    }
}
