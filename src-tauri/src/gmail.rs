//! Thin client for the Gmail REST API: https://developers.google.com/gmail/api/reference/rest

use reqwest::{header::CONTENT_LENGTH, Client, RequestBuilder, Response};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};

const API: &str = "https://gmail.googleapis.com/gmail/v1/users/me";

/// One row of the inbox list. Cached on disk so the app can paint before the first sync.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageSummary {
    pub account: String,
    pub id: String,
    pub thread_id: String,
    pub from: String,
    pub subject: String,
    pub snippet: String,
    /// Milliseconds since the Unix epoch.
    pub date: i64,
    pub unread: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FullMessage {
    pub from: String,
    pub to: String,
    pub cc: String,
    pub subject: String,
    pub date: i64,
    pub html: Option<Body>,
    pub text: Option<Body>,
}

/// A body part exactly as Gmail returns it: base64url bytes in the part's own charset.
/// The webview decodes it with `TextDecoder`, which knows every legacy charset.
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

/// Ids of the newest messages carrying all of `labels`, newest first.
/// Spam and Trash are left out unless `include_spam_trash` (needed to list Trash).
pub async fn list_ids(
    http: &Client,
    token: &str,
    labels: &[&str],
    include_spam_trash: bool,
    max: u32,
) -> Result<Vec<String>, String> {
    #[derive(Deserialize)]
    struct List {
        #[serde(default)]
        messages: Vec<Id>,
    }
    #[derive(Deserialize)]
    struct Id {
        id: String,
    }
    let mut query: Vec<(&str, String)> = labels.iter().map(|l| ("labelIds", l.to_string())).collect();
    query.push(("maxResults", max.to_string()));
    query.push(("fields", "messages/id".into()));
    if include_spam_trash {
        query.push(("includeSpamTrash", "true".into()));
    }
    let resp = send(http.get(format!("{API}/messages")).bearer_auth(token).query(&query)).await?;
    let list: List = parse_json(resp).await?;
    Ok(list.messages.into_iter().map(|m| m.id).collect())
}

pub async fn get_summary(http: &Client, token: &str, account: &str, id: &str) -> Result<MessageSummary, String> {
    check_id(id)?;
    let url = format!(
        "{API}/messages/{id}?format=metadata&metadataHeaders=From&metadataHeaders=Subject\
         &fields=id,threadId,labelIds,snippet,internalDate,payload/headers"
    );
    let raw: RawMessage = get_json(http, token, &url).await?;
    let headers = raw.payload.map(|p| p.headers).unwrap_or_default();
    Ok(MessageSummary {
        account: account.to_string(),
        id: raw.id,
        thread_id: raw.thread_id,
        from: header(&headers, "From"),
        subject: header(&headers, "Subject"),
        snippet: unescape_html(&raw.snippet),
        date: parse_date(raw.internal_date.as_ref()),
        unread: raw.label_ids.iter().any(|l| l == "UNREAD"),
    })
}

/// Fetches a full message. Also returns whether it is currently unread.
pub async fn get_full(http: &Client, token: &str, id: &str) -> Result<(FullMessage, bool), String> {
    check_id(id)?;
    let raw: RawMessage = get_json(http, token, &format!("{API}/messages/{id}?format=full")).await?;
    let unread = raw.label_ids.iter().any(|l| l == "UNREAD");
    let payload = raw.payload.unwrap_or_default();

    let (mut html_part, mut text_part) = (None, None);
    find_bodies(&payload, &mut html_part, &mut text_part);
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
        from: header(h, "From"),
        to: header(h, "To"),
        cc: header(h, "Cc"),
        subject: header(h, "Subject"),
        date: parse_date(raw.internal_date.as_ref()),
        html,
        text,
    };
    Ok((message, unread))
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

pub async fn mark_read(http: &Client, token: &str, id: &str) -> Result<(), String> {
    modify(http, token, id, &[], &["UNREAD"]).await
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

async fn load_body(http: &Client, token: &str, message_id: &str, part: &Part) -> Result<Body, String> {
    let body = part.body.as_ref();
    let data = match (body.and_then(|b| b.data.clone()), body.and_then(|b| b.attachment_id.as_deref())) {
        (Some(data), _) => data,
        // Gmail moves very large bodies out of line, like attachments.
        (None, Some(attachment_id)) => {
            #[derive(Deserialize)]
            struct Attachment {
                #[serde(default)]
                data: String,
            }
            let url = format!("{API}/messages/{message_id}/attachments/{attachment_id}");
            get_json::<Attachment>(http, token, &url).await?.data
        }
        _ => String::new(),
    };
    Ok(Body { data, charset: charset(part) })
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

async fn send(request: RequestBuilder) -> Result<Response, String> {
    let resp = request.send().await.map_err(|e| format!("Network error: {}", describe(&e)))?;
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
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
    fn rejects_odd_ids() {
        assert!(check_id("18c2f0a9b3e4d5f6").is_ok());
        assert!(check_id("../profile").is_err());
        assert!(check_id("").is_err());
    }
}
