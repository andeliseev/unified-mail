use std::{env, fs};

fn main() {
    embed_google_client();
    tauri_build::build()
}

/// Bakes the Google OAuth client into the binary, so users only ever see Google's
/// own sign-in. Read from the GOOGLE_CLIENT_ID / GOOGLE_CLIENT_SECRET env vars,
/// else from `google-client.json` (the file Google Cloud Console downloads).
/// Without either, the app asks for a client in Settings instead.
fn embed_google_client() {
    const FILE: &str = "google-client.json";
    println!("cargo:rerun-if-env-changed=GOOGLE_CLIENT_ID");
    println!("cargo:rerun-if-env-changed=GOOGLE_CLIENT_SECRET");
    println!("cargo:rerun-if-changed={FILE}");

    let (id, secret) = match (env::var("GOOGLE_CLIENT_ID"), env::var("GOOGLE_CLIENT_SECRET")) {
        (Ok(id), Ok(secret)) => (id, secret),
        _ => {
            let Ok(text) = fs::read_to_string(FILE) else {
                println!("cargo:warning=No Google OAuth client embedded; see docs/development.md. The app will ask for one in Settings.");
                return;
            };
            let json: serde_json::Value = serde_json::from_str(&text).expect("google-client.json is not valid JSON");
            let client = json.get("installed").unwrap_or(&json);
            let field = |name: &str| {
                client[name]
                    .as_str()
                    .unwrap_or_else(|| panic!("google-client.json has no {name}"))
                    .to_string()
            };
            (field("client_id"), field("client_secret"))
        }
    };
    println!("cargo:rustc-env=EMBEDDED_GOOGLE_CLIENT_ID={id}");
    println!("cargo:rustc-env=EMBEDDED_GOOGLE_CLIENT_SECRET={secret}");
}
