use crate::provider::ProviderError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Tokens persisted to disk so login survives restarts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredTokens {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds when the access token expires.
    pub expires_at: u64,
    /// Scopes granted at login. Empty for tokens saved before scopes were
    /// recorded, so pre-feature tokens can be told apart without a call.
    #[serde(default)]
    pub scopes: Vec<String>,
}

const REFRESH_MARGIN_SECS: u64 = 60;
const TOKEN_FILE: &str = "spotify-tokens.json";

impl StoredTokens {
    pub fn new(access_token: String, refresh_token: String, expires_in: u64) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            access_token,
            refresh_token,
            expires_at: now + expires_in,
            scopes: Vec::new(),
        }
    }

    /// True when the granted scope list contains `scope`.
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope)
    }

    /// True when the token is expired or within the refresh margin.
    pub fn needs_refresh_at(&self, now: u64) -> bool {
        now + REFRESH_MARGIN_SECS >= self.expires_at
    }

    pub fn needs_refresh(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.needs_refresh_at(now)
    }

    pub fn token_path(state_dir: &Path) -> PathBuf {
        state_dir.join(TOKEN_FILE)
    }

    /// `Ok(None)` when no token file exists; `Err` on unreadable/corrupt files.
    pub fn load(path: &Path) -> Result<Option<Self>, ProviderError> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .map(Some)
                .map_err(|e| ProviderError::Api(format!("corrupt token file: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(ProviderError::Api(format!("cannot read tokens: {e}"))),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), ProviderError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ProviderError::Api(format!("cannot create state dir: {e}")))?;
        }
        let json =
            serde_json::to_string_pretty(self).map_err(|e| ProviderError::Api(e.to_string()))?;
        std::fs::write(path, json)
            .map_err(|e| ProviderError::Api(format!("cannot write tokens: {e}")))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    pub fn delete(path: &Path) -> Result<(), ProviderError> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(ProviderError::Api(format!("cannot remove tokens: {e}"))),
        }
    }
}

/// Lazily-loaded token cache behind a Mutex — distinguishes "not read from
/// disk yet" from "read, and there are no tokens".
pub enum TokenCache {
    Unloaded,
    Loaded(Option<StoredTokens>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_refresh_respects_margin() {
        let t = StoredTokens {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: 1_000,
            scopes: Vec::new(),
        };
        assert!(!t.needs_refresh_at(900));
        assert!(t.needs_refresh_at(941));
        assert!(t.needs_refresh_at(2_000));
    }

    #[test]
    fn round_trip_through_disk() {
        let dir = std::env::temp_dir().join(format!("hm-tok-{}", std::process::id()));
        let path = dir.join("spotify-tokens.json");
        let t = StoredTokens::new("access".into(), "refresh".into(), 3600);
        t.save(&path).unwrap();

        let loaded = StoredTokens::load(&path).unwrap().unwrap();
        assert_eq!(loaded.access_token, "access");
        assert_eq!(loaded.refresh_token, "refresh");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_file_is_none_corrupt_is_err() {
        let dir = std::env::temp_dir().join(format!("hm-tok2-{}", std::process::id()));
        let path = dir.join("nope.json");
        assert!(StoredTokens::load(&path).unwrap().is_none());

        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "not json").unwrap();
        assert!(StoredTokens::load(&path).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn delete_missing_file_is_ok() {
        let path = std::env::temp_dir().join("hm-never-existed.json");
        assert!(StoredTokens::delete(&path).is_ok());
    }

    #[test]
    fn scopes_round_trip_and_default_empty_for_old_files() {
        let dir = std::env::temp_dir().join(format!("hm-tok3-{}", std::process::id()));
        let path = dir.join("spotify-tokens.json");
        let mut t = StoredTokens::new("access".into(), "refresh".into(), 3600);
        t.scopes = vec!["streaming".into(), "user-read-playback-state".into()];
        t.save(&path).unwrap();
        let loaded = StoredTokens::load(&path).unwrap().unwrap();
        assert!(loaded.has_scope("streaming"));
        assert!(!loaded.has_scope("user-modify-playback-state"));

        // A pre-feature file has no `scopes` field at all.
        std::fs::write(
            &path,
            r#"{"access_token":"a","refresh_token":"r","expires_at":9999999999}"#,
        )
        .unwrap();
        let old = StoredTokens::load(&path).unwrap().unwrap();
        assert!(!old.has_scope("streaming"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
