//! User settings, persisted as JSON in the OS config directory.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use streamlens_core::CacheWindow;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Decoded frames kept after the playhead (decode order).
    pub frames_ahead: u32,
    /// Decoded frames kept before the playhead (decode order).
    pub frames_behind: u32,
}

impl Default for Settings {
    fn default() -> Self {
        let w = CacheWindow::default();
        Self {
            frames_ahead: w.ahead,
            frames_behind: w.behind,
        }
    }
}

impl Settings {
    /// The saved settings; defaults when there is no (readable) file.
    pub fn load() -> Self {
        path().map_or_else(Self::default, |p| Self::load_from(&p))
    }

    pub fn save(&self) {
        if let Some(p) = path() {
            self.save_to(&p);
        }
    }

    fn load_from(path: &Path) -> Self {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                log::warn!("ignoring unreadable {}: {e}", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    fn save_to(&self, path: &Path) {
        let result = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| {
                let json = serde_json::to_vec_pretty(self).expect("settings serialize");
                std::fs::write(path, json)
            });
        if let Err(e) = result {
            log::warn!("could not save {}: {e}", path.display());
        }
    }

    pub fn window(&self) -> CacheWindow {
        CacheWindow::new(self.frames_ahead, self.frames_behind)
    }
}

/// `settings.json` in the platform's per-user config directory.
fn path() -> Option<PathBuf> {
    let dir = if cfg!(target_os = "macos") {
        PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support/StreamLens")
    } else if cfg!(windows) {
        PathBuf::from(std::env::var_os("APPDATA")?).join("StreamLens")
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| Some(PathBuf::from(std::env::var_os("HOME")?).join(".config")))?
            .join("streamlens")
    };
    Some(dir.join("settings.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_falls_back_to_defaults() {
        let dir = std::env::temp_dir().join(format!("streamlens-settings-{}", std::process::id()));
        let path = dir.join("nested/settings.json");
        assert_eq!(Settings::load_from(&path), Settings::default());

        let s = Settings {
            frames_ahead: 256,
            frames_behind: 16,
        };
        s.save_to(&path);
        assert_eq!(Settings::load_from(&path), s);

        // Unknown or missing fields keep defaults; garbage falls back.
        std::fs::write(&path, br#"{"frames_behind": 7, "future": true}"#).unwrap();
        let partial = Settings::load_from(&path);
        assert_eq!(partial.frames_behind, 7);
        assert_eq!(partial.frames_ahead, Settings::default().frames_ahead);
        std::fs::write(&path, b"not json").unwrap();
        assert_eq!(Settings::load_from(&path), Settings::default());
        std::fs::remove_dir_all(&dir).ok();
    }
}
