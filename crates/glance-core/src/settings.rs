//! User settings (`config.toml`) and program state (`state.toml`).
//!
//! `config.toml` is edited by the user; `state.toml` is written by the program.
//! They live in separate files so the program never rewrites the user's comments.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

const QUALIFIER: &str = "com";
const ORGANIZATION: &str = "ryan";
const APPLICATION: &str = "Glance";

pub const CONFIG_FILE: &str = "config.toml";
pub const STATE_FILE: &str = "state.toml";

/// Directory holding the settings files. Debug builds use a separate `-dev`
/// directory so experiments never touch day-to-day settings.
pub fn settings_dir() -> Option<PathBuf> {
    settings_dir_for(cfg!(debug_assertions))
}

pub fn settings_dir_for(dev: bool) -> Option<PathBuf> {
    let application = if dev {
        format!("{APPLICATION}-dev")
    } else {
        APPLICATION.to_owned()
    };
    ProjectDirs::from(QUALIFIER, ORGANIZATION, &application).map(|dirs| dirs.config_dir().to_owned())
}

#[derive(Debug)]
pub enum SettingsError {
    Io(io::Error),
    Parse(toml::de::Error),
    Serialize(toml::ser::Error),
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "settings I/O error: {e}"),
            Self::Parse(e) => write!(f, "invalid settings file: {e}"),
            Self::Serialize(e) => write!(f, "cannot write settings: {e}"),
        }
    }
}

impl std::error::Error for SettingsError {}

/// Where a Highlight Rule applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum HighlightScope {
    #[default]
    Entry,
    Text,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HighlightRule {
    pub pattern: String,
    #[serde(default)]
    pub regex: bool,
    pub color: String,
    #[serde(default)]
    pub scope: HighlightScope,
}

/// User-edited preferences.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub wrap_long_lines: bool,
    /// IANA name (e.g. `"UTC"`, `"Asia/Taipei"`) for timestamps that carry no
    /// zone. `None` means the local zone. A per-file choice overrides it.
    pub default_time_zone: Option<String>,
    pub highlight_rules: Vec<HighlightRule>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowSize {
    pub width: u32,
    pub height: u32,
}

/// Program-written state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct State {
    pub window: Option<WindowSize>,
    pub recent_files: Vec<PathBuf>,
}

/// Loads a settings file. A missing file yields the defaults; a malformed one
/// is an error so the caller can warn and fall back without losing the file.
pub fn load<T: DeserializeOwned + Default>(path: &Path) -> Result<T, SettingsError> {
    match fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).map_err(SettingsError::Parse),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(SettingsError::Io(e)),
    }
}

pub fn save<T: Serialize>(path: &Path, value: &T) -> Result<(), SettingsError> {
    let text = toml::to_string_pretty(value).map_err(SettingsError::Serialize)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(SettingsError::Io)?;
    }
    fs::write(path, text).map_err(SettingsError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_directory_has_suffix() {
        let release = settings_dir_for(false).unwrap();
        let dev = settings_dir_for(true).unwrap();
        assert_ne!(release, dev);
        assert!(dev.to_string_lossy().ends_with("-dev"));
        assert!(!release.to_string_lossy().ends_with("-dev"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_directory_uses_bundle_identifier() {
        let dir = settings_dir_for(false).unwrap();
        assert!(dir.ends_with("Library/Application Support/com.ryan.Glance"));
    }

    #[test]
    fn missing_file_yields_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let config: Config = load(&dir.path().join(CONFIG_FILE)).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn partial_file_fills_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE);
        fs::write(&path, "wrap_long_lines = true\n").unwrap();
        let config: Config = load(&path).unwrap();
        assert!(config.wrap_long_lines);
        assert!(config.highlight_rules.is_empty());
    }

    #[test]
    fn highlight_rules_parse_with_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE);
        fs::write(
            &path,
            "# my rules\n[[highlight_rules]]\npattern = \"timeout\"\ncolor = \"#ffcc00\"\n\n\
             [[highlight_rules]]\npattern = \"req-\\\\d+\"\nregex = true\ncolor = \"#00ccff\"\nscope = \"text\"\n",
        )
        .unwrap();
        let config: Config = load(&path).unwrap();
        assert_eq!(config.highlight_rules.len(), 2);
        assert!(!config.highlight_rules[0].regex);
        assert_eq!(config.highlight_rules[0].scope, HighlightScope::Entry);
        assert_eq!(config.highlight_rules[1].scope, HighlightScope::Text);
    }

    #[test]
    fn malformed_file_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE);
        fs::write(&path, "this is = = not toml [").unwrap();
        assert!(matches!(load::<Config>(&path), Err(SettingsError::Parse(_))));
        // The broken file is left untouched.
        assert_eq!(fs::read_to_string(&path).unwrap(), "this is = = not toml [");
    }

    #[test]
    fn state_round_trips_and_creates_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(STATE_FILE);
        let state = State {
            window: Some(WindowSize { width: 1280, height: 800 }),
            recent_files: vec![PathBuf::from("/var/log/app.log")],
        };
        save(&path, &state).unwrap();
        assert_eq!(load::<State>(&path).unwrap(), state);
    }
}
