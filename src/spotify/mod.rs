pub mod auth;
pub mod client;
pub mod local;
pub mod tokens;

use crate::provider::{Device, LocalStatus, MusicProvider, Playback, ProviderError};
use client::Endpoints;
use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::Method;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokens::{StoredTokens, TokenCache};

/// How long the local engine may take to register as a Connect device
/// before `local_status` gives up and reports a failure. The OAuth wait
/// is longer — a human has to click through a browser flow.
const READY_TIMEOUT: Duration = Duration::from_secs(20);
const OAUTH_TIMEOUT: Duration = Duration::from_secs(180);

/// Engine lifecycle inside the adapter. `Starting`/`Ready` hold the
/// engine so dropping the provider (worker exit) kills the child.
enum Phase {
    NotStarted,
    Starting {
        engine: crate::engine::Engine,
        name: String,
        at: Instant,
        /// True while waiting for the user to sign the engine in by
        /// picking this device in their Spotify app (extends the
        /// readiness deadline — it's human-paced).
        oauth_pending: bool,
    },
    Ready {
        engine: crate::engine::Engine,
    },
    Failed(String),
    Disabled {
        reason: String,
        silent: bool,
    },
}

type EngineInvocation = Box<dyn Fn(&str) -> Result<local::Invocation, String> + Send>;

/// Spotify adapter for the `MusicProvider` seam. Blocking HTTP, token
/// refresh handled internally behind `&self` via a Mutex'd token cache.
pub struct SpotifyProvider {
    http: Client,
    endpoints: Endpoints,
    client_id: String,
    tokens: Mutex<TokenCache>,
    token_path: PathBuf,
    local: Mutex<Phase>,
    engine_invocation: EngineInvocation,
}

impl SpotifyProvider {
    pub fn new(client_id: impl Into<String>, state_dir: PathBuf) -> Self {
        Self::with_endpoints(client_id, Endpoints::default(), state_dir)
    }

    /// Internal seam for tests: arbitrary API bases + state dir.
    pub fn with_endpoints(
        client_id: impl Into<String>,
        endpoints: Endpoints,
        state_dir: PathBuf,
    ) -> Self {
        let dir = state_dir.clone();
        Self {
            http: Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("reqwest client"),
            endpoints,
            client_id: client_id.into(),
            tokens: Mutex::new(TokenCache::Unloaded),
            token_path: StoredTokens::token_path(&state_dir),
            local: Mutex::new(Phase::NotStarted),
            engine_invocation: Box::new(move |name| local::invocation(name, &dir)),
        }
    }

    /// Test seam: decide how the engine is invoked (binary, argv, sink,
    /// pidfile) without touching the real librespot.
    pub fn with_engine_invocation(
        mut self,
        f: impl Fn(&str) -> Result<local::Invocation, String> + Send + 'static,
    ) -> Self {
        self.engine_invocation = Box::new(f);
        self
    }

    /// Access token, lazily loaded from disk and refreshed when inside the
    /// expiry margin.
    fn access_token(&self) -> Result<String, ProviderError> {
        let mut cache = self.tokens.lock().unwrap();
        if matches!(*cache, TokenCache::Unloaded) {
            *cache = TokenCache::Loaded(StoredTokens::load(&self.token_path)?);
        }
        match &*cache {
            TokenCache::Loaded(Some(t)) if !t.needs_refresh() => Ok(t.access_token.clone()),
            TokenCache::Loaded(Some(_)) => self.refresh_locked(&mut cache),
            _ => Err(ProviderError::NotAuthenticated),
        }
    }

    fn refresh_locked(&self, cache: &mut TokenCache) -> Result<String, ProviderError> {
        let old = match cache {
            TokenCache::Loaded(Some(t)) => t.clone(),
            _ => return Err(ProviderError::NotAuthenticated),
        };
        let fresh = auth::refresh(&self.http, &self.endpoints.accounts, &self.client_id, &old)?;
        fresh.save(&self.token_path)?;
        let token = fresh.access_token.clone();
        *cache = TokenCache::Loaded(Some(fresh));
        Ok(token)
    }

    /// Force a refresh after a 401 even when the token looks unexpired.
    fn force_refresh(&self) -> Result<String, ProviderError> {
        self.refresh_locked(&mut self.tokens.lock().unwrap())
    }

    /// Send the request `build` produces; on 401 refresh once and retry.
    fn send(
        &self,
        build: impl Fn(&Client, &str) -> RequestBuilder,
    ) -> Result<Response, ProviderError> {
        let resp = build(&self.http, &self.access_token()?)
            .send()
            .map_err(api_err)?;
        if resp.status().as_u16() != 401 {
            return Ok(resp);
        }
        build(&self.http, &self.force_refresh()?)
            .send()
            .map_err(api_err)
    }

    /// Send an authorized request; on 401 refresh once and retry.
    fn authed(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<Response, ProviderError> {
        let url = format!("{}{}", self.endpoints.api, path);
        self.send(|http, token| {
            let req = http
                .request(method.clone(), &url)
                .bearer_auth(token)
                .query(query);
            // Spotify's edge returns 411 Length Required on PUT/POST
            // without an explicit empty body.
            if method == Method::GET {
                req
            } else {
                req.header(reqwest::header::CONTENT_LENGTH, "0")
            }
        })
    }

    /// Authorized request carrying a JSON body (used by transfer, which
    /// would otherwise hit the same 411 path as the transport verbs).
    fn authed_json(
        &self,
        method: Method,
        path: &str,
        body: serde_json::Value,
    ) -> Result<Response, ProviderError> {
        let url = format!("{}{}", self.endpoints.api, path);
        self.send(|http, token| {
            http.request(method.clone(), &url)
                .bearer_auth(token)
                .json(&body)
        })
    }

    fn command(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<(), ProviderError> {
        let resp = self.authed(method, path, query)?;
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(());
        }
        let body = resp.text().unwrap_or_default();
        Err(client::error_for_status(status, &body))
    }
}

fn api_err(e: reqwest::Error) -> ProviderError {
    ProviderError::Api(format!("spotify request failed: {e}"))
}

impl MusicProvider for SpotifyProvider {
    fn name(&self) -> &'static str {
        "spotify"
    }

    fn playback(&self) -> Result<Option<Playback>, ProviderError> {
        let resp = self.authed(Method::GET, "/me/player", &[])?;
        let status = resp.status().as_u16();
        if status == 204 {
            return Ok(None);
        }
        if !(200..300).contains(&status) {
            let body = resp.text().unwrap_or_default();
            return Err(client::error_for_status(status, &body));
        }
        let player: client::PlayerResponse = resp
            .json()
            .map_err(|e| ProviderError::Api(format!("bad playback response: {e}")))?;
        Ok(client::playback_from(player))
    }

    fn play(&self) -> Result<(), ProviderError> {
        self.command(Method::PUT, "/me/player/play", &[])
    }
    fn pause(&self) -> Result<(), ProviderError> {
        self.command(Method::PUT, "/me/player/pause", &[])
    }
    fn next(&self) -> Result<(), ProviderError> {
        self.command(Method::POST, "/me/player/next", &[])
    }
    fn previous(&self) -> Result<(), ProviderError> {
        self.command(Method::POST, "/me/player/previous", &[])
    }
    fn seek(&self, position: Duration) -> Result<(), ProviderError> {
        self.command(
            Method::PUT,
            "/me/player/seek",
            &[("position_ms", position.as_millis().to_string())],
        )
    }

    fn devices(&self) -> Result<Vec<Device>, ProviderError> {
        let resp = self.authed(Method::GET, "/me/player/devices", &[])?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let body = resp.text().unwrap_or_default();
            return Err(client::error_for_status(status, &body));
        }
        let list: client::DevicesResponse = resp
            .json()
            .map_err(|e| ProviderError::Api(format!("bad devices response: {e}")))?;
        Ok(client::devices_from(list))
    }

    fn transfer(&self, device_id: &str) -> Result<(), ProviderError> {
        let resp = self.authed_json(Method::PUT, "/me/player", client::transfer_body(device_id))?;
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(());
        }
        let body = resp.text().unwrap_or_default();
        Err(client::error_for_status(status, &body))
    }

    fn start_local(&self, name: &str) -> Result<(), ProviderError> {
        let mut phase = self.local.lock().unwrap();
        if matches!(*phase, Phase::Starting { .. } | Phase::Ready { .. }) {
            return Ok(()); // already running or registering
        }
        let inv = match (self.engine_invocation)(name) {
            Ok(inv) => inv,
            Err(reason) => {
                *phase = Phase::Disabled {
                    reason,
                    silent: false,
                };
                return Ok(());
            }
        };
        let spawn = crate::engine::Spawn {
            engine: crate::engine::Cmd {
                program: &inv.program,
                args: &inv.args,
                envs: &[],
            },
            sink: inv.sink.as_ref().map(|(p, a)| crate::engine::Cmd {
                program: p.as_path(),
                args: a.as_slice(),
                envs: &[],
            }),
            pidfile: &inv.pidfile,
        };
        match crate::engine::Engine::spawn(spawn) {
            Ok(engine) => {
                *phase = Phase::Starting {
                    engine,
                    name: name.to_string(),
                    at: Instant::now(),
                    oauth_pending: inv.oauth_pending,
                };
                Ok(())
            }
            Err(e) => Err(ProviderError::LocalPlayback(format!(
                "local engine failed to start: {e}"
            ))),
        }
    }

    fn stop_local(&self) -> Result<(), ProviderError> {
        // Dropping the phase kills the engine (kill-on-drop).
        *self.local.lock().unwrap() = Phase::NotStarted;
        Ok(())
    }

    fn local_status(&self) -> LocalStatus {
        /// What this tick decided about the current phase.
        enum Step {
            Keep,
            Fail(String),
            ToReady,
        }
        let mut phase = self.local.lock().unwrap();
        let step = match &mut *phase {
            Phase::Starting {
                engine,
                name,
                at,
                oauth_pending,
            } => {
                let timeout = if *oauth_pending {
                    OAUTH_TIMEOUT
                } else {
                    READY_TIMEOUT
                };
                if engine.exited() {
                    let line = engine
                        .last_error_line()
                        .unwrap_or_else(|| "engine exited".into());
                    Step::Fail(format!("local engine exited: {line}"))
                } else if at.elapsed() > timeout {
                    Step::Fail(if *oauth_pending {
                        "pick this device in your Spotify app once to sign it in".into()
                    } else {
                        format!("local engine did not register within {READY_TIMEOUT:?}")
                    })
                } else {
                    match self.devices() {
                        // Absent from the list or a transient API error:
                        // keep waiting until the deadline.
                        Ok(devices) if devices.iter().any(|d| &d.name == name) => Step::ToReady,
                        _ => Step::Keep,
                    }
                }
            }
            Phase::Ready { engine } => {
                if engine.exited() {
                    let line = engine
                        .last_error_line()
                        .unwrap_or_else(|| "engine exited".into());
                    Step::Fail(format!("local engine exited: {line}"))
                } else {
                    Step::Keep
                }
            }
            _ => Step::Keep,
        };
        match step {
            Step::Keep => match &*phase {
                Phase::NotStarted => LocalStatus::NotStarted,
                Phase::Starting { .. } => LocalStatus::Starting,
                Phase::Ready { .. } => LocalStatus::Ready,
                Phase::Failed(r) => LocalStatus::Failed(r.clone()),
                Phase::Disabled { reason, silent } => LocalStatus::Disabled {
                    reason: reason.clone(),
                    silent: *silent,
                },
            },
            Step::Fail(reason) => {
                *phase = Phase::Failed(reason.clone());
                LocalStatus::Failed(reason)
            }
            Step::ToReady => {
                let Phase::Starting { engine, .. } =
                    std::mem::replace(&mut *phase, Phase::NotStarted)
                else {
                    unreachable!("Step::ToReady only from Starting")
                };
                *phase = Phase::Ready { engine };
                LocalStatus::Ready
            }
        }
    }
}

/// Build the provider from loaded config; explains what's missing otherwise.
pub fn provider_from_config(
    cfg: &crate::config::AppConfig,
) -> Result<SpotifyProvider, ProviderError> {
    let client_id = cfg.spotify_client_id.clone().ok_or_else(|| {
        ProviderError::Api(
            "spotify client id not configured — set HERDR_MUSIC_SPOTIFY_CLIENT_ID \
             or add spotify_client_id to config.toml"
                .into(),
        )
    })?;
    Ok(SpotifyProvider::new(client_id, crate::config::state_dir()))
}

// --- auth subcommand plumbing ------------------------------------------------

pub fn login(client_id: &str, state_dir: &Path, port: u16) -> Result<StoredTokens, ProviderError> {
    let http = Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .expect("reqwest client");
    auth::login(
        &http,
        &Endpoints::default().accounts,
        client_id,
        &StoredTokens::token_path(state_dir),
        port,
    )
}

pub fn logout(state_dir: &Path) -> Result<(), ProviderError> {
    StoredTokens::delete(&StoredTokens::token_path(state_dir))
}

pub fn connection_status(state_dir: &Path) -> Result<bool, ProviderError> {
    Ok(StoredTokens::load(&StoredTokens::token_path(state_dir))?.is_some())
}
