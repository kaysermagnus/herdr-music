pub mod auth;
pub mod client;
pub mod tokens;

use crate::provider::{MusicProvider, Playback, ProviderError};
use client::Endpoints;
use reqwest::blocking::{Client, Response};
use reqwest::Method;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use tokens::{StoredTokens, TokenCache};

/// Spotify adapter for the `MusicProvider` seam. Blocking HTTP, token
/// refresh handled internally behind `&self` via a Mutex'd token cache.
pub struct SpotifyProvider {
    http: Client,
    endpoints: Endpoints,
    client_id: String,
    tokens: Mutex<TokenCache>,
    token_path: PathBuf,
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
        Self {
            http: Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("reqwest client"),
            endpoints,
            client_id: client_id.into(),
            tokens: Mutex::new(TokenCache::Unloaded),
            token_path: StoredTokens::token_path(&state_dir),
        }
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

    /// Send an authorized request; on 401 refresh once and retry.
    fn authed(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<Response, ProviderError> {
        let url = format!("{}{}", self.endpoints.api, path);
        let build = |token: &str| {
            self.http
                .request(method.clone(), &url)
                .bearer_auth(token)
                .query(query)
        };
        let resp = build(&self.access_token()?).send().map_err(api_err)?;
        if resp.status().as_u16() != 401 {
            return Ok(resp);
        }
        build(&self.force_refresh()?).send().map_err(api_err)
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
