//! Thin client for the Gmail REST API: https://developers.google.com/gmail/api/reference/rest

use reqwest::{
    header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE},
    Client, Method, RequestBuilder, Response, StatusCode,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

const API: &str = "https://gmail.googleapis.com/gmail/v1/users/me";
/// Where emails are sent, and drafts saved, with their files (`upload`).
const UPLOAD: &str = "https://gmail.googleapis.com/upload/gmail/v1/users/me";
/// Pages of 500 drafts read for the Drafts folder's draft ids (`draft_ids`).
const DRAFT_LIST_PAGES: usize = 4;
/// For attachments and large bodies, in place of the client's 30 s: Gmail sends them as base64 in
/// JSON, so a 25 MB attachment is 33 MB, more than a slow connection loads in 30 s. Sending an
/// email with its files gets as long.
const DOWNLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// One row of the inbox list. Cached on disk so the app can paint before the first sync.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageSummary {
    pub account: String,
    pub id: String,
    pub thread_id: String,
    pub from: String,
    /// Whom it went to (its To header), which the Sent and Drafts rows show. `None` in summaries
    /// saved before it was noted; those folders fetch such rows again.
    #[serde(default)]
    pub to: Option<String>,
    pub subject: String,
    pub snippet: String,
    /// Milliseconds since the Unix epoch.
    pub date: i64,
    pub unread: bool,
    /// In the Drafts folder: which Gmail draft it is, to open, save or delete it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<String>,
    /// Whether the reader will list files for it (`carries_files`), for the list's paperclip.
    /// `None` until that's known: in lists saved by versions that didn't note it, or by 0.2.4,
    /// which told from the email's type alone (and wrote `hasAttachments`), and when its parts
    /// couldn't be checked. `sync_account` fetches those rows again.
    #[serde(default)]
    pub has_files: Option<bool>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FullMessage {
    pub thread_id: String,
    pub from: String,
    pub to: String,
    pub cc: String,
    /// Where replies go, when not to the sender.
    pub reply_to: String,
    pub subject: String,
    /// Its Message-ID and References headers, which a reply or forward carries on, so it joins
    /// the conversation.
    pub message_id: String,
    pub references: String,
    pub date: i64,
    pub html: Option<Body>,
    pub text: Option<Body>,
    /// Files attached to the message, in its order.
    pub attachments: Vec<Attachment>,
}

/// A file attached to a message, as the reader lists it and `get_attachments` fetches it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    /// Gmail's part ID ("2", "1.3"), the same each time the message is fetched.
    pub part: String,
    /// The name it's saved under (`files::safe_file_name`).
    pub name: String,
    pub mime_type: String,
    /// In bytes.
    pub size: u64,
    /// Its Content-ID, without the angle brackets: a `cid:` link in the email can show it in place.
    pub content_id: Option<String>,
}

/// A part's data, for the reader to show in place (`get_inline_images`).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PartData {
    pub part: String,
    pub mime_type: String,
    /// base64url, as Gmail sends it.
    pub data: String,
}

/// A body part exactly as Gmail returns it: base64url bytes, plus the charset its header
/// declares. Gmail may already have converted the bytes to UTF-8 despite that header, so
/// the webview tries UTF-8 first, then the declared charset (`decodeBody` in app.js).
#[derive(Serialize)]
pub struct Body {
    pub data: String,
    pub charset: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMessage {
    id: String,
    #[serde(default)]
    thread_id: String,
    #[serde(default)]
    label_ids: Vec<String>,
    #[serde(default)]
    snippet: String,
    /// Documented as a string of milliseconds; a plain number is accepted too.
    #[serde(default)]
    internal_date: Option<Value>,
    #[serde(default)]
    payload: Option<Part>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Part {
    #[serde(default)]
    part_id: String,
    #[serde(default)]
    mime_type: String,
    #[serde(default)]
    filename: String,
    #[serde(default)]
    headers: Vec<Header>,
    #[serde(default)]
    body: Option<PartBody>,
    #[serde(default)]
    parts: Vec<Part>,
}

/// Google leaves out empty fields, e.g. `value` of an empty `Subject:` header,
/// so nothing here is required.
#[derive(Deserialize)]
struct Header {
    #[serde(default)]
    name: String,
    #[serde(default)]
    value: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PartBody {
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    attachment_id: Option<String>,
    #[serde(default)]
    size: u64,
}

pub async fn profile_email(http: &Client, token: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Profile {
        email_address: String,
    }
    let profile: Profile = get_json(http, token, &format!("{API}/profile")).await?;
    Ok(profile.email_address)
}

/// One page of a message list, newest first.
pub struct Page {
    pub ids: Vec<String>,
    /// More messages exist beyond this page.
    pub more: bool,
}

/// The newest messages carrying all of `labels` (and matching the Gmail search `q`, if any).
/// Spam and Trash are left out unless `include_spam_trash` (needed to list Trash).
pub async fn list_ids(
    http: &Client,
    token: &str,
    labels: &[&str],
    include_spam_trash: bool,
    q: Option<&str>,
    max: u32,
) -> Result<Page, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct List {
        #[serde(default)]
        messages: Vec<Id>,
        #[serde(default)]
        next_page_token: Option<String>,
    }
    #[derive(Deserialize)]
    struct Id {
        id: String,
    }
    let mut query: Vec<(&str, String)> = labels.iter().map(|l| ("labelIds", l.to_string())).collect();
    query.push(("maxResults", max.to_string()));
    query.push(("fields", "messages/id,nextPageToken".into()));
    if include_spam_trash {
        query.push(("includeSpamTrash", "true".into()));
    }
    if let Some(q) = q {
        query.push(("q", q.to_string()));
    }
    let resp = send(http.get(format!("{API}/messages")).bearer_auth(token).query(&query)).await?;
    let list: List = parse_json(resp).await?;
    Ok(Page {
        ids: list.messages.into_iter().map(|m| m.id).collect(),
        more: list.next_page_token.is_some(),
    })
}

/// Gmail search for messages older than `before_ms`. Search works in whole seconds, so this
/// rounds up and also matches messages from the same second; callers drop the duplicates.
pub fn before_query(before_ms: i64) -> String {
    format!("before:{}", before_ms.div_euclid(1000) + 1)
}

/// The Gmail search for what someone typed, in Gmail's own search syntax (`from:`,
/// `has:attachment`, `OR`...). As the results scroll, `before_ms` limits it to older messages:
/// what was typed goes in parentheses, so `before:` applies to all of it, `OR` included, and a
/// quote left open is closed, so it can't take `before:` into the quoted words.
pub fn search_query(typed: &str, before_ms: Option<i64>) -> String {
    let mut query = typed.trim().to_string();
    if query.matches('"').count() % 2 == 1 {
        query.push('"');
    }
    match before_ms {
        Some(before) => format!("({query}) {}", before_query(before)),
        None => query,
    }
}

pub async fn get_summary(http: &Client, token: &str, account: &str, id: &str) -> Result<MessageSummary, String> {
    check_id(id)?;
    let url = format!(
        "{API}/messages/{id}?format=metadata&metadataHeaders=From&metadataHeaders=To&metadataHeaders=Subject\
         &metadataHeaders=Content-Type&fields=id,threadId,labelIds,snippet,internalDate,payload/headers"
    );
    let raw: RawMessage = get_json(http, token, &url).await?;
    let headers = raw.payload.map(|p| p.headers).unwrap_or_default();
    // If its parts can't be checked, the paperclip waits for the next sync; the message still shows.
    let has_files = if may_carry_files(&header(&headers, "Content-Type")) {
        get_outline(http, token, id).await.map(|outline| carries_files(&outline)).ok()
    } else {
        Some(false)
    };
    Ok(MessageSummary {
        account: account.to_string(),
        id: raw.id,
        thread_id: raw.thread_id,
        from: header(&headers, "From"),
        to: Some(header(&headers, "To")),
        subject: header(&headers, "Subject"),
        snippet: unescape_html(&raw.snippet),
        date: parse_date(raw.internal_date.as_ref()),
        unread: raw.label_ids.iter().any(|l| l == "UNREAD"),
        has_files,
        draft: None,
    })
}

/// Whether an email of this `Content-Type` may carry files: mail attaches files in
/// `multipart/mixed`, while pictures shown in the text come in `multipart/related`, which the
/// reader doesn't list either. Not every `multipart/mixed` email has files, though: some senders
/// wrap every email in it, and a mailing list adds its footer as a part of its own. Its parts
/// tell (`carries_files`).
fn may_carry_files(content_type: &str) -> bool {
    content_type.split(';').next().unwrap_or_default().trim().eq_ignore_ascii_case("multipart/mixed")
}

/// What `get_outline` asks for: each part's type, name and headers, five levels deep.
const OUTLINE_FIELDS: &str = "payload/parts(mimeType,filename,headers,parts(mimeType,filename,headers,\
    parts(mimeType,filename,headers,parts(mimeType,filename,headers,parts(mimeType,filename,headers)))))";

/// Message `id`'s parts without their content (`OUTLINE_FIELDS`): what `carries_files` needs,
/// without downloading the email.
async fn get_outline(http: &Client, token: &str, id: &str) -> Result<Part, String> {
    #[derive(Deserialize)]
    struct Outline {
        #[serde(default)]
        payload: Option<Part>,
    }
    let outline: Outline = get_json(http, token, &format!("{API}/messages/{id}?format=full&fields={OUTLINE_FIELDS}")).await?;
    Ok(outline.payload.unwrap_or_default())
}

/// Whether the reader will list files for an email of these parts (`get_full`), as far as they
/// tell without its text. The reader leaves out the pictures that its HTML shows in place
/// (`cid:` links); those have a Content-ID and aren't marked as attachments, while pictures sent
/// as files are, Gmail's own included, though Gmail gives them a Content-ID too. Without an HTML
/// body, nothing shows in place.
fn carries_files(payload: &Part) -> bool {
    let (mut html, mut text) = (None, None);
    find_bodies(payload, &mut html, &mut text);
    let mut found = Vec::new();
    find_attachments(payload, &mut found);
    found.into_iter().any(|part| html.is_none() || !is_picture(part) || disposition(part) == "attachment")
}

/// A part's `Content-Disposition` without its parameters, in lower case: `inline`, `attachment`,
/// or empty when it has none.
fn disposition(part: &Part) -> String {
    header(&part.headers, "Content-Disposition").split(';').next().unwrap_or_default().trim().to_ascii_lowercase()
}

pub async fn get_full(http: &Client, token: &str, id: &str) -> Result<FullMessage, String> {
    check_id(id)?;
    let raw: RawMessage = get_json(http, token, &format!("{API}/messages/{id}?format=full")).await?;
    let payload = raw.payload.unwrap_or_default();

    let (mut html_part, mut text_part) = (None, None);
    find_bodies(&payload, &mut html_part, &mut text_part);
    let mut files = Vec::new();
    find_attachments(&payload, &mut files);
    let attachments = files.into_iter().map(attachment).collect();
    let html = match html_part {
        Some(part) => Some(load_body(http, token, id, part).await?),
        None => None,
    };
    let text = match (&html, text_part) {
        (None, Some(part)) => Some(load_body(http, token, id, part).await?),
        _ => None,
    };

    let h = &payload.headers;
    let message = FullMessage {
        thread_id: raw.thread_id,
        from: header(h, "From"),
        to: header(h, "To"),
        cc: header(h, "Cc"),
        reply_to: header(h, "Reply-To"),
        subject: header(h, "Subject"),
        message_id: header(h, "Message-ID"),
        references: header(h, "References"),
        date: parse_date(raw.internal_date.as_ref()),
        html,
        text,
        attachments,
    };
    Ok(message)
}

/// Labels currently on a message.
pub async fn labels(http: &Client, token: &str, id: &str) -> Result<Vec<String>, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Labels {
        #[serde(default)]
        label_ids: Vec<String>,
    }
    check_id(id)?;
    let url = format!("{API}/messages/{id}?format=minimal&fields=labelIds");
    Ok(get_json::<Labels>(http, token, &url).await?.label_ids)
}

/// Removes the INBOX label, which is exactly what Gmail's Archive button does.
pub async fn archive(http: &Client, token: &str, id: &str) -> Result<(), String> {
    modify(http, token, id, &[], &["INBOX"]).await
}

/// Adds the INBOX label: undoes Archive, and puts a message restored from Trash back in the Inbox.
pub async fn move_to_inbox(http: &Client, token: &str, id: &str) -> Result<(), String> {
    modify(http, token, id, &["INBOX"], &[]).await
}

/// Adds or removes the UNREAD label.
pub async fn set_unread(http: &Client, token: &str, id: &str, unread: bool) -> Result<(), String> {
    if unread {
        modify(http, token, id, &["UNREAD"], &[]).await
    } else {
        modify(http, token, id, &[], &["UNREAD"]).await
    }
}

/// Moves to Trash, like Gmail's Delete button. Gmail purges Trash after 30 days.
pub async fn trash(http: &Client, token: &str, id: &str) -> Result<(), String> {
    post_empty(http, token, &format!("{API}/messages/{id}/trash"), id).await
}

/// Takes a message out of Trash. Gmail doesn't document whether this restores the INBOX
/// label, so callers that want the message back in the Inbox also call `move_to_inbox`.
pub async fn untrash(http: &Client, token: &str, id: &str) -> Result<(), String> {
    post_empty(http, token, &format!("{API}/messages/{id}/untrash"), id).await
}

async fn post_empty(http: &Client, token: &str, url: &str, id: &str) -> Result<(), String> {
    check_id(id)?;
    // Google rejects bodiless POSTs that lack a Content-Length.
    send(http.post(url).bearer_auth(token).header(CONTENT_LENGTH, "0")).await?;
    Ok(())
}

async fn modify(http: &Client, token: &str, id: &str, add: &[&str], remove: &[&str]) -> Result<(), String> {
    check_id(id)?;
    let body = json!({ "addLabelIds": add, "removeLabelIds": remove });
    send(http.post(format!("{API}/messages/{id}/modify")).bearer_auth(token).json(&body)).await?;
    Ok(())
}

/// First non-attachment text/html and text/plain parts, depth first.
fn find_bodies<'a>(part: &'a Part, html: &mut Option<&'a Part>, text: &mut Option<&'a Part>) {
    if part.filename.is_empty() {
        match part.mime_type.to_ascii_lowercase().as_str() {
            "text/html" if html.is_none() => *html = Some(part),
            "text/plain" if text.is_none() => *text = Some(part),
            _ => {}
        }
    }
    for child in &part.parts {
        find_bodies(child, html, text);
    }
}

/// The message's files, depth first: parts with a file name, and pictures with a Content-ID, which
/// the email may show in place (`cid:` links; the reader leaves those out of its list). The bodies
/// `find_bodies` picks are neither. A part that is itself a message (a forwarded email) is listed
/// as one file, without the files inside it.
fn find_attachments<'a>(part: &'a Part, found: &mut Vec<&'a Part>) {
    if !part.filename.is_empty() || is_picture(part) {
        found.push(part);
        return;
    }
    for child in &part.parts {
        find_attachments(child, found);
    }
}

/// A picture with a Content-ID, which the email may show in place.
fn is_picture(part: &Part) -> bool {
    part.mime_type.to_ascii_lowercase().starts_with("image/") && content_id(part).is_some()
}

/// How the reader lists a part `find_attachments` found.
fn attachment(part: &Part) -> Attachment {
    Attachment {
        part: part.part_id.clone(),
        name: file_name(part),
        mime_type: part.mime_type.to_ascii_lowercase(),
        size: part.body.as_ref().map_or(0, |b| b.size),
        content_id: content_id(part),
    }
}

/// A part's Content-ID without its angle brackets, the way `cid:` links name it.
fn content_id(part: &Part) -> Option<String> {
    let id = header(&part.headers, "Content-ID");
    let id = id.trim().trim_start_matches('<').trim_end_matches('>').trim();
    (!id.is_empty()).then(|| id.to_string())
}

/// The name a part is listed and saved under: its own, or `image.png` and the like for a picture
/// without one.
fn file_name(part: &Part) -> String {
    if !part.filename.is_empty() {
        return crate::files::safe_file_name(&part.filename);
    }
    let kind = part.mime_type.to_ascii_lowercase();
    let extension = match kind.trim_start_matches("image/") {
        "jpeg" => "jpg",
        "svg+xml" => "svg",
        other => other,
    };
    crate::files::safe_file_name(&format!("image.{extension}"))
}

fn find_part<'a>(part: &'a Part, part_id: &str) -> Option<&'a Part> {
    if part.part_id == part_id {
        return Some(part);
    }
    part.parts.iter().find_map(|child| find_part(child, part_id))
}

/// Attachments `parts` (`Attachment::part`) of message `id`: each one's name and bytes.
/// `arrived` hears, as the data comes, how many bytes of the files have arrived in all.
pub async fn get_attachments(
    http: &Client,
    token: &str,
    id: &str,
    parts: &[String],
    arrived: &(dyn Fn(u64) + Sync),
) -> Result<Vec<(String, Vec<u8>)>, String> {
    let found = get_parts(http, token, id, parts, arrived).await?;
    found.into_iter().map(|(name, part)| Ok((name, decode_base64url(&part.data)?))).collect()
}

/// Parts `parts` of message `id` as files to send on (forwarding it, or quoting the pictures it
/// shows in place): each one's name, type and bytes.
pub async fn get_files(http: &Client, token: &str, id: &str, parts: &[String]) -> Result<Vec<(String, String, Vec<u8>)>, String> {
    let found = get_parts(http, token, id, parts, &|_| {}).await?;
    found.into_iter().map(|(name, part)| Ok((name, part.mime_type, decode_base64url(&part.data)?))).collect()
}

/// The name Gmail shows with `account`'s address in what it sends (Settings → Accounts → Send mail
/// as), or None when none is set there. Without it in From, the email would show the address alone.
pub async fn display_name(http: &Client, token: &str, account: &str) -> Result<Option<String>, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct SendAsList {
        #[serde(default)]
        send_as: Vec<SendAs>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct SendAs {
        #[serde(default)]
        send_as_email: String,
        #[serde(default)]
        display_name: String,
    }
    let url = format!("{API}/settings/sendAs?fields=sendAs(sendAsEmail,displayName)");
    let list: SendAsList = get_json(http, token, &url).await?;
    let name = list.send_as.into_iter().find(|s| s.send_as_email.eq_ignore_ascii_case(account)).map(|s| s.display_name);
    Ok(name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()))
}

/// Sends an email (`raw`, from `mime`), in conversation `thread_id` when it answers one. Returns
/// the sent message's id.
pub async fn send_email(http: &Client, token: &str, raw: &[u8], thread_id: Option<&str>) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Sent {
        id: String,
    }
    let response = upload(http, token, Method::POST, &format!("{UPLOAD}/messages/send"), thread(thread_id), raw).await?;
    Ok(parse_json::<Sent>(response).await?.id)
}

/// A Gmail draft as a save left it: its id stays, its message (`message_id`) is new each time.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedDraft {
    pub id: String,
    pub message_id: String,
    pub thread_id: String,
}

/// Saves `raw` as a Gmail draft: draft `id` updated, or a new draft. A draft deleted meanwhile
/// (in Gmail, or sent from there) is saved as a new one.
pub async fn save_draft(http: &Client, token: &str, id: Option<&str>, raw: &[u8], thread_id: Option<&str>) -> Result<SavedDraft, String> {
    #[derive(Deserialize)]
    struct Draft {
        id: String,
        #[serde(default)]
        message: Option<Message>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Message {
        #[serde(default)]
        id: String,
        #[serde(default)]
        thread_id: String,
    }
    let message = thread(thread_id);
    let updated = match id {
        Some(id) => {
            check_draft_id(id)?;
            let url = format!("{UPLOAD}/drafts/{id}");
            match upload(http, token, Method::PUT, &url, json!({ "id": id, "message": message }), raw).await {
                Ok(response) => Some(response),
                Err(e) if e.starts_with("Gmail error 404") => None,
                Err(e) => return Err(e),
            }
        }
        None => None,
    };
    let response = match updated {
        Some(response) => response,
        None => upload(http, token, Method::POST, &format!("{UPLOAD}/drafts"), json!({ "message": message }), raw).await?,
    };
    let draft: Draft = parse_json(response).await?;
    let message = draft.message.unwrap_or(Message { id: String::new(), thread_id: String::new() });
    Ok(SavedDraft { id: draft.id, message_id: message.id, thread_id: message.thread_id })
}

/// Sends draft `id` as Gmail keeps it, which moves it to Sent. Returns the sent message's id.
pub async fn send_draft(http: &Client, token: &str, id: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Sent {
        id: String,
    }
    check_draft_id(id)?;
    let request = http.post(format!("{API}/drafts/send")).bearer_auth(token).json(&json!({ "id": id })).timeout(DOWNLOAD_TIMEOUT);
    Ok(parse_json::<Sent>(send(request).await?).await?.id)
}

/// Deletes draft `id` for good, as discarding a draft in Gmail does. One already gone is fine.
pub async fn delete_draft(http: &Client, token: &str, id: &str) -> Result<(), String> {
    check_draft_id(id)?;
    match send(http.delete(format!("{API}/drafts/{id}")).bearer_auth(token).header(CONTENT_LENGTH, "0")).await {
        Err(e) if !e.starts_with("Gmail error 404") => Err(e),
        _ => Ok(()),
    }
}

/// Which draft each draft message is (message id → draft id), for the Drafts folder's rows.
pub async fn draft_ids(http: &Client, token: &str) -> Result<HashMap<String, String>, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct List {
        #[serde(default)]
        drafts: Vec<Entry>,
        #[serde(default)]
        next_page_token: Option<String>,
    }
    #[derive(Deserialize)]
    struct Entry {
        id: String,
        #[serde(default)]
        message: Option<Id>,
    }
    #[derive(Deserialize)]
    struct Id {
        #[serde(default)]
        id: String,
    }
    let mut ids = HashMap::new();
    let mut page_token: Option<String> = None;
    for _ in 0..DRAFT_LIST_PAGES {
        let mut query = vec![("maxResults", "500".to_string()), ("fields", "drafts(id,message/id),nextPageToken".to_string())];
        if let Some(token) = &page_token {
            query.push(("pageToken", token.clone()));
        }
        let list: List = parse_json(send(http.get(format!("{API}/drafts")).bearer_auth(token).query(&query)).await?).await?;
        ids.extend(list.drafts.into_iter().filter_map(|d| Some((d.message?.id, d.id))));
        match list.next_page_token {
            Some(next) => page_token = Some(next),
            None => break,
        }
    }
    Ok(ids)
}

/// A Gmail draft to go on writing: whom it's to, its subject, its text and HTML, its files with
/// their data, and the email it answers, if any (`AppState::open_draft` finds that one).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenedDraft {
    pub id: String,
    pub message_id: String,
    pub thread_id: String,
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    pub in_reply_to: String,
    pub references: String,
    pub text: Option<Body>,
    pub html: Option<Body>,
    pub files: Vec<DraftFile>,
    pub answered: Option<MessageSummary>,
}

/// A file in a draft, with its data as base64 (standard, as the page reads picked files).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftFile {
    pub name: String,
    pub mime_type: String,
    pub size: u64,
    pub content_id: Option<String>,
    pub data: String,
}

/// Draft `id`, with its files' data: each save sends them all again, since Gmail keeps no
/// draft's files between saves.
pub async fn get_draft(http: &Client, token: &str, id: &str) -> Result<OpenedDraft, String> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    #[derive(Deserialize)]
    struct Draft {
        id: String,
        message: RawMessage,
    }
    check_draft_id(id)?;
    let draft: Draft = get_json(http, token, &format!("{API}/drafts/{id}?format=full")).await?;
    let message = draft.message;
    let payload = message.payload.unwrap_or_default();
    let (mut html_part, mut text_part) = (None, None);
    find_bodies(&payload, &mut html_part, &mut text_part);
    let html = match html_part {
        Some(part) => Some(load_body(http, token, &message.id, part).await?),
        None => None,
    };
    let text = match text_part {
        Some(part) => Some(load_body(http, token, &message.id, part).await?),
        None => None,
    };
    let mut parts = Vec::new();
    find_attachments(&payload, &mut parts);
    let mut files = Vec::new();
    for part in parts {
        let data = decode_base64url(&part_data(http, token, &message.id, part, &|_| {}).await?)?;
        files.push(DraftFile {
            name: file_name(part),
            mime_type: part.mime_type.to_ascii_lowercase(),
            size: data.len() as u64,
            content_id: content_id(part),
            data: STANDARD.encode(&data),
        });
    }
    let h = &payload.headers;
    Ok(OpenedDraft {
        id: draft.id,
        message_id: message.id,
        thread_id: message.thread_id,
        to: header(h, "To"),
        cc: header(h, "Cc"),
        bcc: header(h, "Bcc"),
        subject: header(h, "Subject"),
        in_reply_to: header(h, "In-Reply-To"),
        references: header(h, "References"),
        text,
        html,
        files,
        answered: None,
    })
}

/// The message in conversation `thread_id` whose Message-ID is `message_id`: the email a draft
/// answers. Drafts in it are skipped.
pub async fn find_in_thread(http: &Client, token: &str, thread_id: &str, message_id: &str) -> Result<Option<String>, String> {
    #[derive(Deserialize)]
    struct Thread {
        #[serde(default)]
        messages: Vec<RawMessage>,
    }
    check_id(thread_id)?;
    let url = format!(
        "{API}/threads/{thread_id}?format=metadata&metadataHeaders=Message-ID&fields=messages(id,labelIds,payload/headers)"
    );
    let thread: Thread = get_json(http, token, &url).await?;
    let wanted = message_id.trim();
    Ok(thread
        .messages
        .into_iter()
        .filter(|m| !m.label_ids.iter().any(|l| l == "DRAFT"))
        .find(|m| m.payload.as_ref().is_some_and(|p| header(&p.headers, "Message-ID").trim() == wanted))
        .map(|m| m.id))
}

/// The metadata part of an upload naming the conversation the email goes in.
fn thread(thread_id: Option<&str>) -> Value {
    match thread_id.filter(|id| check_id(id).is_ok()) {
        Some(id) => json!({ "threadId": id }),
        None => json!({}),
    }
}

/// Sends an email (`raw`, RFC 5322) to Gmail's upload endpoint `url`, with `metadata` (JSON) in
/// front of it. Uploads take emails up to 35 MB with their files; `raw` in a JSON body would be
/// base64 again, a third larger. They get the long timeout of downloads.
async fn upload(http: &Client, token: &str, method: Method, url: &str, metadata: Value, raw: &[u8]) -> Result<Response, String> {
    let boundary = loop {
        let boundary = format!("upload_{}", crate::mime::random_hex());
        if !raw.windows(boundary.len()).any(|w| w == boundary.as_bytes()) {
            break boundary;
        }
    };
    let mut body = format!(
        "--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{metadata}\r\n\
         --{boundary}\r\nContent-Type: message/rfc822\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(raw);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let request = http
        .request(method, url)
        .query(&[("uploadType", "multipart")])
        .bearer_auth(token)
        .header(CONTENT_TYPE, format!("multipart/related; boundary={boundary}"))
        .timeout(DOWNLOAD_TIMEOUT)
        .body(body);
    send(request).await
}

/// Pictures `parts` that message `id` shows in place (`cid:` links), with their data.
pub async fn get_inline_images(http: &Client, token: &str, id: &str, parts: &[String]) -> Result<Vec<PartData>, String> {
    Ok(get_parts(http, token, id, parts, &|_| {}).await?.into_iter().map(|(_, part)| part).collect())
}

/// Parts `parts` (`Attachment::part`) of message `id`, found in a fresh copy of it, so attachment
/// IDs that changed since it was opened don't matter: each one's name and data. `arrived` hears
/// the bytes of them that have arrived in all, as they come.
async fn get_parts(
    http: &Client,
    token: &str,
    id: &str,
    parts: &[String],
    arrived: &(dyn Fn(u64) + Sync),
) -> Result<Vec<(String, PartData)>, String> {
    check_id(id)?;
    let raw: RawMessage = get_json(http, token, &format!("{API}/messages/{id}?format=full")).await?;
    let payload = raw.payload.unwrap_or_default();
    let mut found = Vec::new();
    for part_id in parts {
        match find_part(&payload, part_id) {
            Some(part) => found.push(part),
            None => return Err("This attachment isn't in the message any more. Open the message again.".to_string()),
        }
    }
    // The parts load at the same time; each adds what arrives for it to the total.
    let received = AtomicU64::new(0);
    let more = &|bytes: u64| arrived(received.fetch_add(bytes, Ordering::Relaxed) + bytes);
    let loads = found.into_iter().map(|part| async move {
        let data = part_data(http, token, id, part, more).await?;
        let mime_type = part.mime_type.to_ascii_lowercase();
        Ok::<_, String>((file_name(part), PartData { part: part.part_id.clone(), mime_type, data }))
    });
    futures_util::future::try_join_all(loads).await
}

/// Gmail's base64url, with or without padding.
fn decode_base64url(data: &str) -> Result<Vec<u8>, String> {
    use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
    use base64::{alphabet, Engine};
    const URL_SAFE: GeneralPurpose =
        GeneralPurpose::new(&alphabet::URL_SAFE, GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent));
    URL_SAFE.decode(data).map_err(|e| format!("Gmail sent an attachment that couldn't be read: {e}"))
}

async fn load_body(http: &Client, token: &str, message_id: &str, part: &Part) -> Result<Body, String> {
    Ok(Body { data: part_data(http, token, message_id, part, &|_| {}).await?, charset: charset(part) })
}

/// A part's base64url data: in the message, or fetched separately when Gmail keeps it out of
/// line, as it does for attachments and very large bodies. `more` hears how many more bytes of the
/// part have arrived, up to its size.
async fn part_data(
    http: &Client,
    token: &str,
    message_id: &str,
    part: &Part,
    more: &(dyn Fn(u64) + Sync),
) -> Result<String, String> {
    let body = part.body.as_ref();
    let size = body.map_or(0, |b| b.size);
    let data = match (body.and_then(|b| b.data.clone()), body.and_then(|b| b.attachment_id.as_deref())) {
        (Some(data), _) => {
            more(size);
            data
        }
        // Gmail moves very large bodies out of line, like attachments.
        (None, Some(attachment_id)) => {
            #[derive(Deserialize)]
            struct OutOfLine {
                #[serde(default)]
                data: String,
            }
            let url = format!("{API}/messages/{message_id}/attachments/{attachment_id}");
            let request = http.get(url).bearer_auth(token).timeout(DOWNLOAD_TIMEOUT);
            let json = read_counting(send(request).await?, size, more).await?;
            let out_of_line: OutOfLine =
                decode(&json).map_err(|e| format!("Unexpected Gmail response ({} bytes): {e}", json.len()))?;
            out_of_line.data
        }
        _ => String::new(),
    };
    Ok(data)
}

/// The body of `resp`, which carries a part of `size` bytes, read piece by piece: `more` hears how
/// many more bytes of the part each piece brings. Gmail sends parts as base64 in JSON, 4
/// characters for every 3 bytes, so that's three quarters of what came, up to `size`.
async fn read_counting(mut resp: Response, size: u64, more: &(dyn Fn(u64) + Sync)) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    let mut counted = 0;
    while let Some(piece) = resp.chunk().await.map_err(|e| format!("Couldn't read Gmail's response: {}", describe(&e)))? {
        body.extend_from_slice(&piece);
        let now = (body.len() as u64 / 4 * 3).min(size);
        if now > counted {
            more(now - counted);
            counted = now;
        }
    }
    if size > counted {
        more(size - counted);
    }
    Ok(body)
}

async fn get_json<T: DeserializeOwned>(http: &Client, token: &str, url: &str) -> Result<T, String> {
    parse_json(send(http.get(url).bearer_auth(token)).await?).await
}

/// Parses with serde_json directly: its errors name the field and position,
/// where reqwest's `.json()` only says "error decoding response body".
async fn parse_json<T: DeserializeOwned>(resp: Response) -> Result<T, String> {
    let body = resp
        .bytes()
        .await
        .map_err(|e| format!("Couldn't read Gmail's response: {}", describe(&e)))?;
    decode(&body).map_err(|e| format!("Unexpected Gmail response ({} bytes): {e}", body.len()))
}

/// Gmail can answer a successful `messages.list` with an empty body where `{}` is meant
/// (seen with `fields=messages/id` when nothing matches, e.g. no unread mail).
fn decode<T: DeserializeOwned>(body: &[u8]) -> serde_json::Result<T> {
    if body.iter().all(u8::is_ascii_whitespace) {
        serde_json::from_slice(b"{}")
    } else {
        serde_json::from_slice(body)
    }
}

/// Access tokens Google turned down (401), such as one whose access was removed in the Google
/// account: `AppState::cached_token` stops using them, so the next call gets a new token or
/// finds the sign-in expired, instead of failing until the token would have run out.
static REJECTED: Mutex<Vec<String>> = Mutex::new(Vec::new());
const REJECTED_KEPT: usize = 16;

pub fn was_rejected(token: &str) -> bool {
    REJECTED.lock().unwrap().iter().any(|t| t == token)
}

async fn send(request: RequestBuilder) -> Result<Response, String> {
    let (client, request) = request.build_split();
    let request = request.map_err(|e| format!("Network error: {}", describe(&e)))?;
    let token = request.headers().get(AUTHORIZATION).and_then(|v| v.to_str().ok()?.strip_prefix("Bearer ")).map(str::to_string);
    let resp = client.execute(request).await.map_err(|e| format!("Network error: {}", describe(&e)))?;
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    if status == StatusCode::UNAUTHORIZED {
        if let Some(token) = token {
            let mut rejected = REJECTED.lock().unwrap();
            if rejected.len() >= REJECTED_KEPT {
                rejected.remove(0);
            }
            rejected.push(token);
        }
    }
    let body = resp.text().await.unwrap_or_default();
    let message = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or(body);
    Err(format!("Gmail error {}: {message}", status.as_u16()))
}

/// An error plus its causes; reqwest keeps the useful detail (timeout, TLS, ...) in the chain.
fn describe(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut cause = error.source();
    while let Some(inner) = cause {
        text.push_str(": ");
        text.push_str(&inner.to_string());
        cause = inner.source();
    }
    text
}

/// Message ids end up in URL paths, so only accept what Gmail issues (hex).
fn check_id(id: &str) -> Result<(), String> {
    if !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        Ok(())
    } else {
        Err(format!("Invalid message id: {id}"))
    }
}

/// Draft ids (`r-8211…`) go into URL paths too.
fn check_draft_id(id: &str) -> Result<(), String> {
    if !id.is_empty() && id.len() < 100 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        Ok(())
    } else {
        Err(format!("Invalid draft id: {id}"))
    }
}

fn header(headers: &[Header], name: &str) -> String {
    headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case(name))
        .map(|h| h.value.clone())
        .unwrap_or_default()
}

fn charset(part: &Part) -> String {
    header(&part.headers, "Content-Type")
        .split(';')
        .filter_map(|param| param.split_once('='))
        .find(|(key, _)| key.trim().eq_ignore_ascii_case("charset"))
        .map(|(_, value)| value.trim().trim_matches('"').to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "utf-8".into())
}

fn parse_date(internal_date: Option<&Value>) -> i64 {
    match internal_date {
        Some(Value::String(ms)) => ms.parse().unwrap_or(0),
        Some(Value::Number(ms)) => ms.as_i64().unwrap_or(0),
        _ => 0,
    }
}

/// Gmail snippets come HTML-escaped (`&#39;`, `&amp;`, ...).
fn unescape_html(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        // `&` and `;` are ASCII, so byte offsets between them are char boundaries.
        let decoded = rest.bytes().take(12).position(|b| b == b';').and_then(|end| {
            let entity = &rest[1..end];
            let ch = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some('\u{a0}'),
                _ => entity.strip_prefix('#').and_then(|num| {
                    match num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok(),
                        None => num.parse().ok(),
                    }
                    .and_then(char::from_u32)
                }),
            };
            ch.map(|c| (c, end))
        });
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescapes_snippets() {
        assert_eq!(unescape_html("Tom &amp; Jerry"), "Tom & Jerry");
        assert_eq!(unescape_html("it&#39;s &quot;fine&quot;"), "it's \"fine\"");
        assert_eq!(unescape_html("&#x41;&#66;"), "AB");
        assert_eq!(unescape_html("Привет &amp; пока"), "Привет & пока");
        assert_eq!(unescape_html("a & b &unknown; c&"), "a & b &unknown; c&");
        assert_eq!(unescape_html("&Привет;"), "&Привет;");
    }

    #[test]
    fn tells_which_types_may_carry_files() {
        assert!(may_carry_files("multipart/mixed; boundary=\"000000000000a1b2c3\""));
        assert!(may_carry_files(" Multipart/Mixed;boundary=x"));
        assert!(may_carry_files("multipart/mixed"));
        assert!(!may_carry_files("multipart/alternative; boundary=x"));
        assert!(!may_carry_files("multipart/related; type=\"multipart/alternative\""));
        assert!(!may_carry_files("text/plain; charset=UTF-8"));
        assert!(!may_carry_files(""));
    }

    #[test]
    fn tells_which_emails_carry_files() {
        // `multipart/mixed` emails, as `get_outline` returns them.
        let carries = |parts: Value| carries_files(&serde_json::from_value(json!({ "parts": parts })).unwrap());
        let text = json!({ "mimeType": "text/plain" });
        let alternative = json!({ "mimeType": "multipart/alternative", "parts": [text, { "mimeType": "text/html" }] });
        let picture = |disposition: &str| {
            let mut headers = vec![json!({ "name": "Content-ID", "value": "<logo@sender>" })];
            if !disposition.is_empty() {
                headers.push(json!({ "name": "Content-Disposition", "value": format!("{disposition}; filename=\"logo.png\"") }));
            }
            json!({ "mimeType": "image/png", "filename": "logo.png", "headers": headers })
        };
        let pdf = json!({
            "mimeType": "application/pdf",
            "filename": "invoice.pdf",
            "headers": [{ "name": "Content-Disposition", "value": "attachment; filename=\"invoice.pdf\"" }]
        });

        // A wrapper around the text alone, and a mailing list's footer: no files.
        assert!(!carries(json!([alternative])));
        assert!(!carries(json!([alternative, { "mimeType": "text/plain", "headers": [{ "name": "Content-Disposition", "value": "inline" }] }])));
        // Pictures the HTML shows in place, marked inline or not marked.
        assert!(!carries(json!([alternative, picture("inline"), picture("")])));
        assert!(!carries(json!([{ "mimeType": "multipart/related", "parts": [alternative, picture("Inline")] }])));
        // Files: any attached file, pictures marked as attachments (Gmail gives them a
        // Content-ID too), and pictures in an email without HTML, which can't show them.
        assert!(carries(json!([alternative, pdf])));
        assert!(carries(json!([{ "mimeType": "multipart/related", "parts": [alternative, picture("inline")] }, picture("ATTACHMENT")])));
        assert!(carries(json!([text, picture("inline")])));
        assert!(carries(json!([alternative, { "mimeType": "message/rfc822", "filename": "Forwarded.eml" }])));
        assert!(carries(json!([alternative, { "mimeType": "image/jpeg", "filename": "photo.jpg" }])));
    }

    #[test]
    fn asks_gmail_for_the_outline_only() {
        assert!(!OUTLINE_FIELDS.contains(char::is_whitespace));
        assert_eq!(OUTLINE_FIELDS.matches('(').count(), OUTLINE_FIELDS.matches(')').count());
        assert_eq!(OUTLINE_FIELDS.matches("parts(mimeType,filename,headers").count(), 5);
        assert!(!OUTLINE_FIELDS.contains("body"));
    }

    #[test]
    fn picks_bodies_and_charsets() {
        let payload: Part = serde_json::from_value(json!({
            "mimeType": "multipart/mixed",
            "parts": [
                {
                    "mimeType": "multipart/alternative",
                    "parts": [
                        {
                            "mimeType": "text/plain",
                            "headers": [{ "name": "Content-Type", "value": "text/plain; charset=\"KOI8-R\"" }],
                            "body": { "data": "cGxhaW4" }
                        },
                        {
                            "mimeType": "text/html",
                            "headers": [{ "name": "content-type", "value": "text/html; charset=windows-1251" }],
                            "body": { "data": "aHRtbA" }
                        }
                    ]
                },
                { "mimeType": "text/html", "filename": "attached.html", "body": { "attachmentId": "x" } }
            ]
        }))
        .unwrap();
        let (mut html, mut text) = (None, None);
        find_bodies(&payload, &mut html, &mut text);
        let (html, text) = (html.unwrap(), text.unwrap());
        assert_eq!(html.body.as_ref().unwrap().data.as_deref(), Some("aHRtbA"));
        assert_eq!(charset(html), "windows-1251");
        assert_eq!(charset(text), "koi8-r");
        assert_eq!(charset(&Part::default()), "utf-8");
    }

    #[test]
    fn lists_attachments() {
        let payload: Part = serde_json::from_value(json!({
            "partId": "",
            "mimeType": "multipart/mixed",
            "parts": [
                {
                    "partId": "0",
                    "mimeType": "multipart/alternative",
                    "parts": [
                        { "partId": "0.0", "mimeType": "text/plain", "body": { "data": "cGxhaW4", "size": 5 } },
                        { "partId": "0.1", "mimeType": "text/html", "body": { "data": "aHRtbA", "size": 4 } }
                    ]
                },
                { "partId": "1", "mimeType": "application/PDF", "filename": "invoice.pdf", "body": { "attachmentId": "a", "size": 253952 } },
                { "partId": "2", "mimeType": "image/png", "filename": "../logo\u{202E}gnp.exe", "body": { "data": "iVBO", "size": 3 } },
                {
                    "partId": "4",
                    "mimeType": "image/JPEG",
                    "headers": [{ "name": "Content-Id", "value": " <photo@corp.example> " }],
                    "body": { "attachmentId": "d", "size": 5000 }
                },
                { "partId": "5", "mimeType": "text/plain", "headers": [{ "name": "Content-ID", "value": "<note>" }], "body": { "data": "eA" } },
                {
                    "partId": "3",
                    "mimeType": "message/rfc822",
                    "filename": "Forwarded.eml",
                    "body": { "attachmentId": "b", "size": 9000 },
                    "parts": [{ "partId": "3.1", "mimeType": "image/jpeg", "filename": "inside.jpg", "body": { "attachmentId": "c" } }]
                }
            ]
        }))
        .unwrap();
        let mut found = Vec::new();
        find_attachments(&payload, &mut found);
        let found: Vec<Attachment> = found.into_iter().map(attachment).collect();
        let listed: Vec<_> = found.iter().map(|a| (a.part.as_str(), a.name.as_str(), a.mime_type.as_str(), a.size, a.content_id.as_deref())).collect();
        assert_eq!(listed, [
            ("1", "invoice.pdf", "application/pdf", 253952, None),
            ("2", "_logognp.exe", "image/png", 3, None),
            ("4", "image.jpg", "image/jpeg", 5000, Some("photo@corp.example")),
            ("3", "Forwarded.eml", "message/rfc822", 9000, None),
        ]);
        assert_eq!(find_part(&payload, "3.1").unwrap().filename, "inside.jpg");
        assert_eq!(find_part(&payload, "0.1").unwrap().mime_type, "text/html");
        assert!(find_part(&payload, "9").is_none());
    }

    #[test]
    fn decodes_gmail_base64() {
        assert_eq!(decode_base64url("aGk-Pz8_").unwrap(), b"hi>???");
        assert_eq!(decode_base64url("aGk=").unwrap(), b"hi");
        assert_eq!(decode_base64url("aGk").unwrap(), b"hi");
        assert!(decode_base64url("a b").is_err());
    }

    #[test]
    fn tolerates_missing_fields() {
        let raw: RawMessage = serde_json::from_value(json!({
            "id": "18c2f0a9b3e4d5f6",
            "internalDate": "1730000000000",
            "payload": { "headers": [{ "name": "Subject" }, { "name": "From", "value": "a@b.c" }] }
        }))
        .unwrap();
        let headers = raw.payload.unwrap().headers;
        assert_eq!(header(&headers, "Subject"), "");
        assert_eq!(header(&headers, "From"), "a@b.c");
        assert_eq!(parse_date(raw.internal_date.as_ref()), 1_730_000_000_000);

        let numeric: RawMessage = serde_json::from_value(json!({ "id": "1", "internalDate": 1730000000000_i64 })).unwrap();
        assert_eq!(parse_date(numeric.internal_date.as_ref()), 1_730_000_000_000);
        assert!(numeric.label_ids.is_empty() && numeric.snippet.is_empty());
    }

    #[test]
    fn empty_body_means_nothing_matched() {
        #[derive(Deserialize)]
        struct List {
            #[serde(default)]
            messages: Vec<Value>,
        }
        assert!(decode::<List>(b"").unwrap().messages.is_empty());
        assert!(decode::<List>(b" \n").unwrap().messages.is_empty());
        assert_eq!(decode::<List>(br#"{"messages":[{"id":"a"}]}"#).unwrap().messages.len(), 1);
        // A message still needs its id; an empty body must not pass as one.
        assert!(decode::<RawMessage>(b"").is_err());
    }

    #[test]
    fn before_query_includes_the_same_second() {
        assert_eq!(before_query(1_730_000_000_000), "before:1730000001");
        assert_eq!(before_query(1_730_000_000_999), "before:1730000001");
    }

    #[test]
    fn search_query_keeps_what_was_typed() {
        assert_eq!(search_query("  invoice from:anna ", None), "invoice from:anna");
        // Older results: before: applies to all of it, OR included.
        assert_eq!(search_query("from:a OR from:b", Some(1_730_000_000_000)), "(from:a OR from:b) before:1730000001");
        // A quote left open is closed, so it can't swallow before:.
        assert_eq!(search_query("\"q3 report", Some(1_730_000_000_000)), "(\"q3 report\") before:1730000001");
        assert_eq!(search_query("\"q3 report\" pdf", None), "\"q3 report\" pdf");
    }

    #[test]
    fn rejects_odd_ids() {
        assert!(check_id("18c2f0a9b3e4d5f6").is_ok());
        assert!(check_id("../profile").is_err());
        assert!(check_id("").is_err());
        assert!(check_draft_id("r-8211374409212364512").is_ok());
        assert!(check_draft_id("r-82/../send").is_err());
        assert!(check_draft_id("").is_err());
    }

    #[test]
    fn uploads_name_the_conversation() {
        assert_eq!(thread(Some("18c2f0a9b3e4d5f6")), json!({ "threadId": "18c2f0a9b3e4d5f6" }));
        assert_eq!(thread(Some("../x")), json!({}));
        assert_eq!(thread(None), json!({}));
    }

    /// Talks to the real gmail.googleapis.com: run with `cargo test -- --ignored`. Gmail's own
    /// 401 message coming back shows that TLS, HTTP and reading errors work with this reqwest.
    #[tokio::test]
    #[ignore]
    async fn gmail_api_rejects_bad_token() {
        assert!(!was_rejected("not-a-real-token"));
        let error = profile_email(&Client::new(), "not-a-real-token").await.err().unwrap();
        assert!(error.starts_with("Gmail error 401"), "{error}");
        assert!(was_rejected("not-a-real-token"), "a token Google turned down isn't used again");
    }
}
