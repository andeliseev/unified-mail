//! Google OAuth 2.0 for installed apps: system browser + loopback redirect + PKCE.
//! https://developers.google.com/identity/protocols/oauth2/native-app

use std::collections::HashMap;
use std::time::Duration;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;
use url::Url;

use crate::state::ClientCreds;

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const REVOKE_URL: &str = "https://oauth2.googleapis.com/revoke";
/// Full Gmail access: the only scope Gmail accepts for IMAP, which push (IDLE) needs.
/// The app itself only reads, archives and moves to Trash.
const SCOPE: &str = "https://mail.google.com/";
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Returned by [`refresh`] when Google no longer accepts the refresh token.
pub const EXPIRED: &str = "invalid_grant";

#[derive(Deserialize)]
pub struct Tokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default = "default_expiry")]
    pub expires_in: u64,
}

fn default_expiry() -> u64 {
    3600
}

/// Opens Google sign-in in the system browser and waits for the redirect back to us.
/// `login_hint` (an email address) preselects that account, for reconnecting it.
pub async fn authorize(http: &reqwest::Client, creds: &ClientCreds, login_hint: Option<&str>) -> Result<Tokens, String> {
    let listener = TcpListener::bind("127.0.0.1:0").await.map_err(err)?;
    let redirect_uri = format!("http://127.0.0.1:{}", listener.local_addr().map_err(err)?.port());
    let verifier = random_token(48);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_token(16);

    let mut params = vec![
        ("client_id", creds.client_id.as_str()),
        ("redirect_uri", redirect_uri.as_str()),
        ("response_type", "code"),
        ("scope", SCOPE),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("state", state.as_str()),
        ("access_type", "offline"),
        // Always show the account chooser and always return a refresh token.
        ("prompt", "select_account consent"),
    ];
    if let Some(email) = login_hint {
        params.push(("login_hint", email));
    }
    let auth_url = Url::parse_with_params(AUTH_URL, &params).map_err(err)?;
    open::that_detached(auth_url.as_str()).map_err(|e| format!("Could not open the browser: {e}"))?;

    let code = timeout(SIGN_IN_TIMEOUT, wait_for_code(&listener, &state))
        .await
        .map_err(|_| "Sign-in timed out. Try again.".to_string())??;

    token_request(
        http,
        &[
            ("client_id", creds.client_id.as_str()),
            ("client_secret", creds.client_secret.as_str()),
            ("code", code.as_str()),
            ("code_verifier", verifier.as_str()),
            ("grant_type", "authorization_code"),
            ("redirect_uri", redirect_uri.as_str()),
        ],
    )
    .await
}

pub async fn refresh(http: &reqwest::Client, creds: &ClientCreds, refresh_token: &str) -> Result<Tokens, String> {
    token_request(
        http,
        &[
            ("client_id", creds.client_id.as_str()),
            ("client_secret", creds.client_secret.as_str()),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ],
    )
    .await
}

/// Best effort: tells Google to drop this app's access to the account.
pub async fn revoke(http: &reqwest::Client, token: &str) {
    let _ = http.post(REVOKE_URL).form(&[("token", token)]).send().await;
}

async fn token_request(http: &reqwest::Client, form: &[(&str, &str)]) -> Result<Tokens, String> {
    let resp = http
        .post(TOKEN_URL)
        .form(form)
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;
    if resp.status().is_success() {
        return resp.json().await.map_err(err);
    }
    let body: Value = resp.json().await.unwrap_or_default();
    match body["error"].as_str().unwrap_or("unknown_error") {
        EXPIRED => Err(EXPIRED.into()),
        code => Err(format!(
            "Google sign-in failed ({code}): {}",
            body["error_description"].as_str().unwrap_or("no details")
        )),
    }
}

/// Accepts connections on the loopback listener until the browser delivers the auth code.
async fn wait_for_code(listener: &TcpListener, expected_state: &str) -> Result<String, String> {
    loop {
        let (mut stream, _) = listener.accept().await.map_err(err)?;
        // Browsers sometimes open a speculative connection that never sends a request.
        let Ok(Some(target)) = timeout(Duration::from_secs(3), read_request_target(&mut stream)).await else {
            continue;
        };
        let Ok(url) = Url::parse(&format!("http://127.0.0.1{target}")) else {
            continue;
        };
        let params: HashMap<String, String> = url.query_pairs().into_owned().collect();

        if params.contains_key("error") {
            respond(&mut stream, "Sign-in cancelled", "You can close this tab.").await;
            return Err("Sign-in was cancelled.".into());
        }
        let Some(code) = params.get("code") else {
            respond(&mut stream, "Unified Mail", "Waiting for Google sign-in…").await;
            continue;
        };
        if params.get("state").map(String::as_str) != Some(expected_state) {
            respond(&mut stream, "Sign-in failed", "The response did not match this request. Try again.").await;
            return Err("Sign-in response did not match the request.".into());
        }
        respond(&mut stream, "Signed in", "You can close this tab and return to Unified Mail.").await;
        return Ok(code.clone());
    }
}

/// Reads the request line and returns its target, e.g. `/?code=...&state=...`.
async fn read_request_target(stream: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 1024];
    while !buf.windows(2).any(|w| w == b"\r\n") && buf.len() < 16 * 1024 {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let head = String::from_utf8_lossy(&buf);
    head.lines().next()?.split_whitespace().nth(1).map(str::to_string)
}

async fn respond(stream: &mut TcpStream, title: &str, message: &str) {
    let html = format!(
        "<!doctype html><meta charset=utf-8><title>{title}</title>\
         <body style=\"font:16px system-ui,sans-serif;display:grid;place-items:center;height:90vh;margin:0\">\
         <div style=\"text-align:center\"><h2>{title}</h2><p>{message}</p></div>"
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
        html.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::getrandom(&mut buf).expect("OS random number generator unavailable");
    URL_SAFE_NO_PAD.encode(buf)
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
