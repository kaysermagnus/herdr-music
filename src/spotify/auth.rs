use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use url::Url;

use crate::provider::ProviderError;

use super::client::TokenResponse;
use super::tokens::StoredTokens;

pub const DEFAULT_REDIRECT_PORT: u16 = 8888;

const SCOPES: &str =
    "user-read-playback-state user-modify-playback-state user-read-currently-playing";

/// Loopback redirect registered in the Spotify app settings.
pub fn redirect_uri(port: u16) -> String {
    format!("http://127.0.0.1:{port}/callback")
}

/// PKCE code verifier: 48 random bytes → 64 base64url chars.
pub fn generate_verifier() -> String {
    let mut bytes = [0u8; 48];
    rand::Rng::fill(&mut rand::thread_rng(), &mut bytes[..]);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// S256 code challenge for a verifier.
pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// CSRF token for the authorize round-trip.
pub fn generate_state() -> String {
    let mut bytes = [0u8; 16];
    rand::Rng::fill(&mut rand::thread_rng(), &mut bytes[..]);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The accounts.spotify.com/authorize URL the user's browser must visit.
pub fn authorize_url(
    accounts_base: &str,
    client_id: &str,
    redirect_uri: &str,
    challenge: &str,
    state: &str,
) -> String {
    let mut pairs = url::form_urlencoded::Serializer::new(String::new());
    pairs.extend_pairs([
        ("client_id", client_id),
        ("response_type", "code"),
        ("redirect_uri", redirect_uri),
        ("code_challenge_method", "S256"),
        ("code_challenge", challenge),
        ("state", state),
        ("scope", SCOPES),
    ]);
    format!("{accounts_base}/authorize?{}", pairs.finish())
}

/// Query params Spotify appends to the redirect URI.
#[derive(Debug, Default)]
pub struct Callback {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
}

pub fn parse_callback_query(query: &str) -> Callback {
    let mut cb = Callback::default();
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "code" => cb.code = Some(value.into_owned()),
            "state" => cb.state = Some(value.into_owned()),
            "error" => cb.error = Some(value.into_owned()),
            _ => {}
        }
    }
    cb
}

fn token_request(
    http: &reqwest::blocking::Client,
    accounts_base: &str,
    form: &[(&str, &str)],
) -> Result<TokenResponse, ProviderError> {
    let resp = http
        .post(format!("{accounts_base}/api/token"))
        .form(form)
        .send()
        .map_err(|e| ProviderError::Api(format!("token request failed: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().unwrap_or_default();
        return Err(ProviderError::Api(format!(
            "token request failed ({status}): {}",
            body.trim()
        )));
    }
    resp.json::<TokenResponse>()
        .map_err(|e| ProviderError::Api(format!("bad token response: {e}")))
}

/// Swap an authorization code for tokens, proving possession of `verifier`.
pub fn exchange_code(
    http: &reqwest::blocking::Client,
    accounts_base: &str,
    client_id: &str,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<StoredTokens, ProviderError> {
    let tr = token_request(
        http,
        accounts_base,
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", client_id),
            ("code_verifier", verifier),
        ],
    )?;
    let refresh_token = tr
        .refresh_token
        .ok_or_else(|| ProviderError::Api("no refresh token in response".into()))?;
    Ok(StoredTokens::new(
        tr.access_token,
        refresh_token,
        tr.expires_in,
    ))
}

/// Mint a fresh access token. Spotify sometimes rotates the refresh token;
/// when the response omits it, the old one stays valid.
pub fn refresh(
    http: &reqwest::blocking::Client,
    accounts_base: &str,
    client_id: &str,
    tokens: &StoredTokens,
) -> Result<StoredTokens, ProviderError> {
    let tr = token_request(
        http,
        accounts_base,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", &tokens.refresh_token),
            ("client_id", client_id),
        ],
    )?;
    let refresh_token = tr
        .refresh_token
        .unwrap_or_else(|| tokens.refresh_token.clone());
    Ok(StoredTokens::new(
        tr.access_token,
        refresh_token,
        tr.expires_in,
    ))
}

fn callback_code(cb: &Callback, state: &str) -> Result<String, ProviderError> {
    if let Some(e) = &cb.error {
        return Err(ProviderError::Api(e.clone()));
    }
    if cb.state.as_deref() != Some(state) {
        return Err(ProviderError::Api("oauth state mismatch".into()));
    }
    cb.code
        .clone()
        .ok_or_else(|| ProviderError::Api("no authorization code in callback".into()))
}

/// Answer the browser before `login` returns — success or failure alike.
fn respond(stream: &mut TcpStream, status: &str, message: &str) {
    let body = format!("<!doctype html><title>herdr-music</title><p>{message}</p>");
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

/// Interactive PKCE login: open the browser, catch the loopback redirect on
/// `port`, exchange the code, persist the tokens. Blocks until the browser
/// calls back (or the socket fails).
pub fn login(
    http: &reqwest::blocking::Client,
    accounts_base: &str,
    client_id: &str,
    token_path: &Path,
    port: u16,
) -> Result<StoredTokens, ProviderError> {
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| {
        ProviderError::Api(format!(
            "cannot listen on 127.0.0.1:{port} — is the port busy? {e}"
        ))
    })?;
    let redirect = redirect_uri(port);
    let verifier = generate_verifier();
    let state = generate_state();
    let url = authorize_url(
        accounts_base,
        client_id,
        &redirect,
        &challenge(&verifier),
        &state,
    );

    println!("Opening browser to connect Spotify — if it doesn't open, visit:\n{url}");
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(&url).spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(&url).spawn();

    let (mut stream, _) = listener
        .accept()
        .map_err(|e| ProviderError::Api(format!("accept failed: {e}")))?;
    let mut buf = [0u8; 8192];
    let n = stream
        .read(&mut buf)
        .map_err(|e| ProviderError::Api(format!("read failed: {e}")))?;
    let request = String::from_utf8_lossy(&buf[..n]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");
    let cb = Url::parse(&format!("http://127.0.0.1{path}"))
        .map(|u| parse_callback_query(u.query().unwrap_or("")))
        .unwrap_or_default();

    let result = callback_code(&cb, &state).and_then(|code| {
        exchange_code(http, accounts_base, client_id, &code, &redirect, &verifier)
    });
    match &result {
        Ok(_) => respond(&mut stream, "200 OK", "Connected — you can close this tab."),
        Err(e) => respond(
            &mut stream,
            "400 Bad Request",
            &format!("Spotify connection failed: {e}"),
        ),
    }
    let tokens = result?;
    tokens.save(token_path)?;
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifier_is_64_url_safe_chars() {
        let v = generate_verifier();
        assert_eq!(v.len(), 64);
        assert!(v
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn challenge_matches_rfc7636_vector() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn state_is_url_safe_and_random() {
        let s = generate_state();
        assert_eq!(s.len(), 22);
        assert!(s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_ne!(s, generate_state());
    }

    #[test]
    fn parses_code_and_state() {
        let cb = parse_callback_query("code=abc123&state=xyz");
        assert_eq!(cb.code.as_deref(), Some("abc123"));
        assert_eq!(cb.state.as_deref(), Some("xyz"));
        assert!(cb.error.is_none());
    }

    #[test]
    fn parses_error_callback() {
        let cb = parse_callback_query("error=access_denied&state=xyz");
        assert_eq!(cb.error.as_deref(), Some("access_denied"));
        assert!(cb.code.is_none());
    }

    #[test]
    fn missing_code_is_none() {
        let cb = parse_callback_query("state=xyz");
        assert!(cb.code.is_none());
    }

    #[test]
    fn authorize_url_carries_pkce_and_scopes() {
        let url = authorize_url(
            "https://accounts.spotify.com",
            "client123",
            &redirect_uri(DEFAULT_REDIRECT_PORT),
            "challenge",
            "state",
        );
        assert!(url.starts_with("https://accounts.spotify.com/authorize?"));
        assert!(url.contains("client_id=client123"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("code_challenge=challenge"));
        assert!(url.contains("state=state"));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A8888%2Fcallback"));
        for scope in [
            "user-read-playback-state",
            "user-modify-playback-state",
            "user-read-currently-playing",
        ] {
            assert!(url.contains(scope));
        }
    }
}
