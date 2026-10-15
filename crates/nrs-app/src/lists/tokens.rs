//! The OAuth tokens `auth` saves and `sync` reads, in one JSON file only
//! its owner can read: `$NRS_LIST_SYNC_TOKENS`, else
//! `$XDG_CONFIG_HOME/nrs/list-sync.json` (`~/.config/...`).

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::lists::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Unix seconds.
    pub expires_at: i64,
    /// The client the token was issued to; refreshing needs it.
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub client_secret: Option<String>,
    /// Who logged in, as the site names them.
    #[serde(default)]
    pub user: Option<String>,
}

impl Token {
    pub fn days_left(&self, now: i64) -> i64 {
        (self.expires_at - now).div_euclid(86_400)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Tokens {
    #[serde(default)]
    pub mal: Option<Token>,
    #[serde(default)]
    pub anilist: Option<Token>,
}

pub fn default_path() -> PathBuf {
    if let Some(path) = std::env::var_os("NRS_LIST_SYNC_TOKENS") {
        return path.into();
    }
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    config.join("nrs").join("list-sync.json")
}

/// MAL tokens last 31 days; refresh with this much left.
const REFRESH_DAYS: i64 = 7;

/// The saved MAL token, refreshed (and saved) when it has under a week
/// left, plus what happened on the way. A failed refresh keeps the old
/// token while it lasts.
pub fn fresh_mal(path: &Path, now: i64) -> Result<(Token, Option<String>)> {
    let mut tokens = Tokens::load(path)?;
    let token = tokens
        .mal
        .clone()
        .ok_or_else(|| Error::Auth("not logged in to MAL".into()))?;
    if token.days_left(now) >= REFRESH_DAYS {
        return Ok((token, None));
    }
    match crate::lists::mal::refresh(&token, now) {
        Ok(mut fresh) => {
            fresh.user = token.user.clone();
            tokens.mal = Some(fresh.clone());
            tokens.save(path)?;
            Ok((fresh, Some("refreshed the MAL token".into())))
        }
        Err(e) if token.days_left(now) >= 0 => {
            Ok((token, Some(format!("MAL token refresh failed ({e}); using the old one"))))
        }
        Err(e) => Err(Error::Auth(format!(
            "the MAL token expired and refreshing it failed ({e}); log in again"
        ))),
    }
}

impl Tokens {
    /// A missing file is no tokens.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|error| Error::Auth(format!("{}: {error}", path.display()))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(Error::Auth(format!("{}: {error}", path.display()))),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let fail = |error: std::io::Error| Error::Auth(format!("{}: {error}", path.display()));
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(fail)?;
        }
        let text = serde_json::to_string_pretty(self).expect("tokens serialize");
        let temp = path.with_extension("json.tmp");
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)
            .map_err(fail)?;
        file.write_all(text.as_bytes()).map_err(fail)?;
        file.sync_all().map_err(fail)?;
        std::fs::rename(&temp, path).map_err(fail)
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn saved_tokens_load_back_owner_only() {
        let dir = std::env::temp_dir().join(format!("nrs-list-sync-{}", std::process::id()));
        let path = dir.join("nested").join("tokens.json");
        assert!(Tokens::load(&path).unwrap().mal.is_none());
        let tokens = Tokens {
            mal: Some(Token {
                access_token: "a".into(),
                refresh_token: Some("r".into()),
                expires_at: 100,
                client_id: Some("c".into()),
                client_secret: None,
                user: None,
            }),
            anilist: None,
        };
        tokens.save(&path).unwrap();
        let loaded = Tokens::load(&path).unwrap();
        assert_eq!(loaded.mal.unwrap().refresh_token.as_deref(), Some("r"));
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
