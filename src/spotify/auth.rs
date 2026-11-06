use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};
use url::Url;

use crate::provider::ProviderError;

use super::client::TokenResponse;
use super::tokens::StoredTokens;

pub const DEFAULT_REDIRECT_PORT: u16 = 8888;
const LOGIN_TIMEOUT: Duration = Duration::from_secs(180);
const CALLBACK_READ_TIMEOUT: Duration = Duration::from_secs(10);

const SCOPES: &str =
    "user-read-playback-state user-modify-playback-state user-read-currently-playing streaming";

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
    let mut tokens = StoredTokens::new(tr.access_token, refresh_token, tr.expires_in);
    tokens.scopes = SCOPES.split(' ').map(String::from).collect();
    Ok(tokens)
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
    )
    .map_err(|e| match e {
        // Revoked or expired refresh token: the only fix is a new login.
        ProviderError::Api(msg) if msg.contains("invalid_grant") => ProviderError::NotAuthenticated,
        e => e,
    })?;
    let refresh_token = tr
        .refresh_token
        .unwrap_or_else(|| tokens.refresh_token.clone());
    let mut fresh = StoredTokens::new(tr.access_token, refresh_token, tr.expires_in);
    // Refresh inherits the original grant's scopes.
    fresh.scopes = tokens.scopes.clone();
    Ok(fresh)
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
    let body = format!(
        "<!doctype html><title>herdr-music</title><p>{}</p>",
        escape_html(message)
    );
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

/// The message can carry the callback's `error` param, which is attacker-
/// controlled text from the URL.
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Wait for the browser's redirect, giving up after `timeout` so an
/// abandoned login doesn't hold the port forever.
fn accept_callback(listener: &TcpListener, timeout: Duration) -> Result<TcpStream, ProviderError> {
    let io = |what: &str, e: std::io::Error| ProviderError::Api(format!("{what} failed: {e}"));
    listener
        .set_nonblocking(true)
        .map_err(|e| io("listen", e))?;
    let deadline = Instant::now() + timeout;
    let stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(ProviderError::Api(format!(
                        "login timed out — no browser callback within {}s",
                        timeout.as_secs()
                    )));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(io("accept", e)),
        }
    };
    // Accepted sockets inherit non-blocking mode on macOS/BSD.
    stream.set_nonblocking(false).map_err(|e| io("accept", e))?;
    stream
        .set_read_timeout(Some(CALLBACK_READ_TIMEOUT))
        .map_err(|e| io("accept", e))?;
    Ok(stream)
}

/// Browser launcher for `url` on `os` (a `std::env::consts::OS` value), as
/// (program, args). The URL is always one plain argv element — on Windows
/// `rundll32` hands it to the default browser without cmd interpreting the
/// query string's `&` as a command separator. `None` where no launcher is
/// known.
pub fn browser_launch_command(os: &str, url: &str) -> Option<(String, Vec<String>)> {
    let mut args = Vec::new();
    let program = match os {
        "macos" => "open",
        "linux" => "xdg-open",
        "windows" => {
            args.push("url.dll,FileProtocolHandler".to_string());
            "rundll32"
        }
        _ => return None,
    };
    args.push(url.to_string());
    Some((program.to_string(), args))
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
            "cannot listen on 127.0.0.1:{port} ({e}). To use another port, register \
             http://127.0.0.1:<port>/callback in your Spotify app, then run \
             herdr-music auth login <port>"
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
    if let Some((program, args)) = browser_launch_command(std::env::consts::OS, &url) {
        let _ = std::process::Command::new(program).args(args).spawn();
    }

    let mut stream = accept_callback(&listener, LOGIN_TIMEOUT)?;
    // Read until the request line arrives — only the first line is parsed,
    // but a single read() is not guaranteed to return it whole.
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream
            .read(&mut chunk)
            .map_err(|e| ProviderError::Api(format!("read failed: {e}")))?;
        if n == 0 || buf.len() >= 8192 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(2).any(|w| w == b"\r\n") {
            break;
        }
    }
    let request = String::from_utf8_lossy(&buf);
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
    fn accept_callback_times_out_when_nobody_connects() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let started = Instant::now();
        let err = accept_callback(&listener, Duration::from_millis(300)).unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn accept_callback_returns_blocking_stream() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = std::thread::spawn(move || {
            let mut s = TcpStream::connect(addr).unwrap();
            s.write_all(b"GET /callback?code=c&state=s HTTP/1.1\r\n\r\n")
                .unwrap();
        });
        let mut stream = accept_callback(&listener, Duration::from_secs(5)).unwrap();
        let mut buf = [0u8; 64];
        let n = stream.read(&mut buf).unwrap();
        assert!(buf[..n].starts_with(b"GET /callback"));
        client.join().unwrap();
    }

    #[test]
    fn escape_html_neutralizes_markup() {
        assert_eq!(
            escape_html("<script>a&b</script>"),
            "&lt;script&gt;a&amp;b&lt;/script&gt;"
        );
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
    fn browser_launch_linux_uses_xdg_open() {
        assert_eq!(
            browser_launch_command("linux", "https://example.com/auth"),
            Some(("xdg-open".into(), vec!["https://example.com/auth".into()]))
        );
    }

    #[test]
    fn browser_launch_macos_uses_open() {
        assert_eq!(
            browser_launch_command("macos", "https://example.com/auth"),
            Some(("open".into(), vec!["https://example.com/auth".into()]))
        );
    }

    #[test]
    fn browser_launch_windows_uses_rundll32_and_keeps_url_whole() {
        // The OAuth URL's `&` must reach rundll32 as one argv element — cmd
        // would split it as a command separator, so rundll32 is used.
        let url = "https://accounts.spotify.com/authorize?client_id=x&state=y";
        assert_eq!(
            browser_launch_command("windows", url),
            Some((
                "rundll32".into(),
                vec!["url.dll,FileProtocolHandler".into(), url.into()]
            ))
        );
    }

    #[test]
    fn browser_launch_unknown_os_is_none() {
        assert_eq!(browser_launch_command("freebsd", "https://x"), None);
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
            "streaming",
        ] {
            assert!(url.contains(scope));
        }
    }
}
