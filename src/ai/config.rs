//! Which provider and model to talk to, and where the key comes from.
//!
//! The provider and model are not secret, so they persist to `~/.log-scouter/ai.json`.
//! The key is read from the environment on every call and never written anywhere.

use crate::core::filters::{home_dir, USER_DIR};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    OpenAi,
    Anthropic,
    Deepseek,
}

impl Provider {
    pub const ALL: [Provider; 3] = [Provider::OpenAi, Provider::Anthropic, Provider::Deepseek];

    pub fn label(self) -> &'static str {
        match self {
            Provider::OpenAi => "openai",
            Provider::Anthropic => "anthropic",
            Provider::Deepseek => "deepseek",
        }
    }

    /// The environment variable holding this provider's key.
    pub fn key_var(self) -> &'static str {
        match self {
            Provider::OpenAi => "OPENAI_API_KEY",
            Provider::Anthropic => "ANTHROPIC_API_KEY",
            Provider::Deepseek => "DEEPSEEK_API_KEY",
        }
    }

    /// A capable default model, used until the user picks another.
    pub fn default_model(self) -> &'static str {
        match self {
            Provider::OpenAi => "gpt-4o",
            Provider::Anthropic => "claude-opus-4-8",
            Provider::Deepseek => "deepseek-chat",
        }
    }

    /// The default base URL for the chat endpoint. DeepSeek speaks the OpenAI wire format,
    /// so the two share an adapter and differ only here.
    pub fn default_base_url(self) -> &'static str {
        match self {
            Provider::OpenAi => "https://api.openai.com/v1",
            Provider::Anthropic => "https://api.anthropic.com/v1",
            Provider::Deepseek => "https://api.deepseek.com/v1",
        }
    }

    /// True when this provider uses the Anthropic Messages API rather than the
    /// OpenAI-compatible `/chat/completions` shape.
    pub fn is_anthropic(self) -> bool {
        matches!(self, Provider::Anthropic)
    }

    pub fn from_label(label: &str) -> Option<Provider> {
        Provider::ALL
            .into_iter()
            .find(|provider| provider.label() == label.trim().to_lowercase())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiConfig {
    pub provider: Provider,
    /// Empty means "use the provider's default model".
    #[serde(default)]
    pub model: String,
    /// A key stored in the config file for the configured provider. Optional -- the
    /// environment variable takes precedence, and `/key` in the chat overrides both for the
    /// session. Always written (even blank) so it is easy to fill in with an editor.
    #[serde(default)]
    pub api_key: String,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            provider: Provider::OpenAi,
            model: String::new(),
            api_key: String::new(),
        }
    }
}

impl AiConfig {
    /// The chosen model, or the provider's default when none was set.
    pub fn model(&self) -> String {
        if self.model.trim().is_empty() {
            self.provider.default_model().to_string()
        } else {
            self.model.clone()
        }
    }

    /// The key for the configured provider: the environment variable if set, otherwise the
    /// one stored in `ai.json`. `None` when neither is present.
    pub fn api_key(&self) -> Option<String> {
        let from_env = std::env::var(self.provider.key_var())
            .ok()
            .map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty());
        from_env.or_else(|| {
            let stored = self.api_key.trim();
            (!stored.is_empty()).then(|| stored.to_string())
        })
    }

    /// The base URL to POST to. `LOGSCOUT_AI_BASE_URL` overrides the provider default, for
    /// a corporate gateway, a compatible self-hosted endpoint, or a test double.
    pub fn base_url(&self) -> String {
        std::env::var("LOGSCOUT_AI_BASE_URL")
            .ok()
            .map(|url| url.trim().trim_end_matches('/').to_string())
            .filter(|url| !url.is_empty())
            .unwrap_or_else(|| self.provider.default_base_url().to_string())
    }

    pub fn load() -> Self {
        config_path()
            .and_then(|path| fs::read_to_string(path).ok())
            .and_then(|body| serde_json::from_str(&body).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        // Surfacing this (rather than a silent no-op) matters on a machine with no HOME or
        // USERPROFILE: otherwise `logscout config set` would report success without writing.
        let path = config_path().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "could not find your home directory (set HOME or USERPROFILE)",
            )
        })?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let body = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        fs::write(&path, body)?;
        // The file can hold an API key, so keep it readable only by the owner.
        restrict_permissions(&path);
        Ok(())
    }
}

/// Best-effort `chmod 600` on the config file. A failure here is not worth surfacing: the
/// file was still written, and not every filesystem models Unix permissions.
#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {}

/// The full path to `ai.json` under the user's home directory, or `None` when it cannot be
/// found. Public so the CLI can show where it wrote (or would write) the file.
pub fn config_path() -> Option<PathBuf> {
    home_dir().map(|home| home.join(USER_DIR).join("ai.json"))
}
