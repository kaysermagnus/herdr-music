use herdr_music::provider::{LocalStatus, MusicProvider, ProviderError};
use herdr_music::spotify::client::Endpoints;
use herdr_music::spotify::local::Invocation;
use herdr_music::spotify::tokens::StoredTokens;
use herdr_music::spotify::SpotifyProvider;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const PLAYER_JSON: &str = r#"{
    "is_playing": true,
    "progress_ms": 61000,
    "device": {"name": "Test Box"},
    "item": {
        "name": "The Song",
        "duration_ms": 200000,
        "album": {"name": "The Album"},
        "artists": [{"name": "A1"}]
    }
}"#;

/// A stub HTTP server: serves `responses` in order, one per connection,
/// recording each raw request for assertions.
struct Stub {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<String>>>,
    _handle: thread::JoinHandle<()>,
}

impl Stub {
    fn serve(responses: Vec<(u16, &'static str)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let reqs = requests.clone();
        let handle = thread::spawn(move || {
            for (status, body) in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                reqs.lock().unwrap().push(read_request(&mut stream));
                let reason = match status {
                    200 => "OK",
                    204 => "No Content",
                    401 => "Unauthorized",
                    403 => "Forbidden",
                    404 => "Not Found",
                    _ => "Error",
                };
                let resp = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        Self {
            addr,
            requests,
            _handle: handle,
        }
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

fn read_request(stream: &mut std::net::TcpStream) -> String {
    let mut buf = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    loop {
        if Instant::now() > deadline {
            break;
        }
        let mut chunk = [0u8; 4096];
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if request_complete(&buf) {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&buf).to_string()
}

fn request_complete(buf: &[u8]) -> bool {
    let text = String::from_utf8_lossy(buf);
    let Some((head, body)) = text.split_once("\r\n\r\n") else {
        return false;
    };
    let len: usize = head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse().ok())
                .flatten()
        })
        .unwrap_or(0);
    body.len() >= len
}

fn state_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "hm-test-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn provider(stub: &Stub, tag: &str, tokens: Option<StoredTokens>) -> SpotifyProvider {
    let dir = state_dir(tag);
    if let Some(t) = tokens {
        t.save(&StoredTokens::token_path(&dir)).unwrap();
    }
    SpotifyProvider::with_endpoints(
        "test-client",
        Endpoints {
            api: format!("http://{}", stub.addr),
            accounts: format!("http://{}", stub.addr),
        },
        dir,
    )
}

fn fresh_tokens() -> StoredTokens {
    StoredTokens::new("access-1".into(), "refresh-1".into(), 3600)
}

/// Tokens carrying the `streaming` scope, as post-feature logins save.
fn scoped_tokens() -> StoredTokens {
    let mut t = fresh_tokens();
    t.scopes = vec![
        "user-read-playback-state".into(),
        "user-modify-playback-state".into(),
        "user-read-currently-playing".into(),
        "streaming".into(),
    ];
    t
}

/// Same self-reinvoke trick as tests/engine.rs: the spawned copy of this
/// test binary parks as the fake engine when the env var is set.
#[test]
fn fake_engine() {
    if std::env::var("HERDR_MUSIC_FAKE_ENGINE").is_err() {
        return;
    }
    loop {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// An invocation that spawns this test binary as a parked fake engine.
fn fake_invocation(dir: PathBuf) -> Invocation {
    Invocation {
        program: std::env::current_exe().unwrap(),
        args: vec!["fake_engine".into(), "--exact".into(), "--nocapture".into()],
        sink: None,
        pidfile: dir.join("engine.pid"),
        oauth_pending: false,
    }
}

fn provider_with_engine(stub: &Stub, tag: &str) -> SpotifyProvider {
    let dir = state_dir(tag);
    scoped_tokens()
        .save(&StoredTokens::token_path(&dir))
        .unwrap();
    let inv_dir = dir.clone();
    SpotifyProvider::with_endpoints(
        "test-client",
        Endpoints {
            api: format!("http://{}", stub.addr),
            accounts: format!("http://{}", stub.addr),
        },
        dir,
    )
    .with_engine_invocation(move |_| Ok(fake_invocation(inv_dir.clone())))
}

const DEVICES_EMPTY: &str = r#"{"devices": []}"#;
const DEVICES_LOCAL: &str = r#"{"devices": [
    {"id": "local-1", "name": "Herdr Music", "is_active": false}
]}"#;

#[test]
fn playback_maps_player_json() {
    let stub = Stub::serve(vec![(200, PLAYER_JSON)]);
    let p = provider(&stub, "pb", Some(fresh_tokens()));
    let pb = p.playback().unwrap().unwrap();
    assert_eq!(pb.track.title, "The Song");
    assert_eq!(pb.track.album, "The Album");
    assert_eq!(pb.progress, Duration::from_secs(61));
    assert_eq!(pb.device.as_deref(), Some("Test Box"));
    assert!(stub.requests()[0].starts_with("GET /me/player"));
}

#[test]
fn playback_204_is_nothing_playing() {
    let stub = Stub::serve(vec![(204, "")]);
    let p = provider(&stub, "204", Some(fresh_tokens()));
    assert!(p.playback().unwrap().is_none());
}

#[test]
fn transport_commands_hit_expected_endpoints() {
    let stub = Stub::serve(vec![
        (204, ""), // play
        (204, ""), // pause
        (204, ""), // next
        (204, ""), // previous
        (204, ""), // seek
    ]);
    let p = provider(&stub, "cmd", Some(fresh_tokens()));
    p.play().unwrap();
    p.pause().unwrap();
    p.next().unwrap();
    p.previous().unwrap();
    p.seek(Duration::from_secs(70)).unwrap();
    let reqs = stub.requests();
    assert!(reqs[0].starts_with("PUT /me/player/play"), "{}", reqs[0]);
    assert!(reqs[1].starts_with("PUT /me/player/pause"), "{}", reqs[1]);
    assert!(reqs[2].starts_with("POST /me/player/next"), "{}", reqs[2]);
    assert!(
        reqs[3].starts_with("POST /me/player/previous"),
        "{}",
        reqs[3]
    );
    assert!(
        reqs[4].contains("PUT /me/player/seek?position_ms=70000"),
        "{}",
        reqs[4]
    );
    assert!(reqs
        .iter()
        .all(|r| r.contains("authorization: Bearer access-1")));
    // Spotify answers 411 Length Required without it.
    assert!(reqs.iter().all(|r| r.contains("content-length: 0")));
}

#[test]
fn expired_status_refreshes_and_retries() {
    let stub = Stub::serve(vec![
        (401, ""),                                                 // first GET
        (200, r#"{"access_token":"access-2","expires_in":3600}"#), // refresh
        (200, PLAYER_JSON),                                        // retried GET
    ]);
    let p = provider(&stub, "refresh", Some(fresh_tokens()));
    let pb = p.playback().unwrap().unwrap();
    assert_eq!(pb.track.title, "The Song");
    let reqs = stub.requests();
    assert_eq!(reqs.len(), 3);
    assert!(reqs[1].starts_with("POST /api/token"), "{}", reqs[1]);
    assert!(reqs[1].contains("grant_type=refresh_token"));
    assert!(reqs[2].contains("authorization: Bearer access-2"));
}

#[test]
fn revoked_refresh_token_is_not_authenticated() {
    let stub = Stub::serve(vec![
        (401, ""),
        (
            400,
            r#"{"error":"invalid_grant","error_description":"Refresh token revoked"}"#,
        ),
    ]);
    let p = provider(&stub, "revoked", Some(fresh_tokens()));
    assert_eq!(p.playback().unwrap_err(), ProviderError::NotAuthenticated);
}

#[test]
fn errors_map_to_typed_variants() {
    for (status, want) in [
        (404u16, ProviderError::NoActiveDevice),
        (403, ProviderError::PremiumRequired),
    ] {
        let stub = Stub::serve(vec![(status, "err")]);
        let p = provider(&stub, "err", Some(fresh_tokens()));
        assert_eq!(p.play().unwrap_err(), want, "status {status}");
    }
}

#[test]
fn missing_tokens_is_not_authenticated_without_http() {
    let stub = Stub::serve(vec![]);
    let p = provider(&stub, "noauth", None);
    assert_eq!(p.play().unwrap_err(), ProviderError::NotAuthenticated);
    assert!(stub.requests().is_empty());
}

#[test]
fn devices_maps_the_device_list() {
    let stub = Stub::serve(vec![(
        200,
        r#"{"devices": [
        {"id": "d1", "name": "Herdr Music", "is_active": false},
        {"id": "d2", "name": "Kitchen", "is_active": true}
    ]}"#,
    )]);
    let p = provider(&stub, "devs", Some(fresh_tokens()));
    let devices = p.devices().unwrap();
    assert_eq!(devices.len(), 2);
    assert_eq!(devices[0].id, "d1");
    assert_eq!(devices[0].name, "Herdr Music");
    assert!(!devices[0].is_active);
    assert!(devices[1].is_active);
    assert!(stub.requests()[0].starts_with("GET /me/player/devices"));
}

#[test]
fn transfer_puts_me_player_with_a_json_body() {
    let stub = Stub::serve(vec![(204, "")]);
    let p = provider(&stub, "xfer", Some(fresh_tokens()));
    p.transfer("d1").unwrap();
    let req = &stub.requests()[0];
    assert!(req.starts_with("PUT /me/player "), "{req}");
    assert!(req.contains("content-length:"), "{req}");
    // The JSON body must name the device and keep playback going — and
    // must NOT be the Content-Length: 0 empty body the transport verbs use.
    assert!(!req.contains("content-length: 0\r"), "{req}");
    assert!(req.contains("\"device_ids\":[\"d1\"]"), "{req}");
    assert!(req.contains("\"play\":true"), "{req}");
}

#[test]
fn transfer_404_is_no_active_device() {
    let stub = Stub::serve(vec![(404, "not found")]);
    let p = provider(&stub, "xfer404", Some(fresh_tokens()));
    assert_eq!(
        p.transfer("gone").unwrap_err(),
        ProviderError::NoActiveDevice
    );
}

#[test]
fn start_local_is_disabled_when_invocation_fails() {
    let stub = Stub::serve(vec![]);
    let dir = state_dir("noinv");
    scoped_tokens()
        .save(&StoredTokens::token_path(&dir))
        .unwrap();
    let p = SpotifyProvider::with_endpoints(
        "test-client",
        Endpoints {
            api: format!("http://{}", stub.addr),
            accounts: format!("http://{}", stub.addr),
        },
        dir,
    )
    .with_engine_invocation(|_| Err("local engine not installed".to_string()));
    p.start_local("Herdr Music").unwrap();
    match p.local_status() {
        LocalStatus::Disabled { reason, silent } => {
            assert!(reason.contains("not installed"), "{reason}");
            assert!(!silent, "a missing engine belongs on the status line");
        }
        other => panic!("expected Disabled, got {other:?}"),
    }
}

#[test]
fn start_local_spawns_and_becomes_ready_via_the_device_list() {
    let stub = Stub::serve(vec![
        (200, DEVICES_EMPTY), // first tick: not registered yet
        (200, DEVICES_LOCAL), // second tick: our device appeared
    ]);
    let p = provider_with_engine(&stub, "local");
    p.start_local("Herdr Music").unwrap();
    assert_eq!(p.local_status(), LocalStatus::Starting);
    assert_eq!(p.local_status(), LocalStatus::Ready);
    // Readiness is detected through the device list, not engine output.
    let reqs = stub.requests();
    assert!(reqs.iter().all(|r| r.starts_with("GET /me/player/devices")));
}
