//! User settings, kept in `settings.toml` in the data directory.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config;
use crate::icons::IconMode;
use crate::images::ImageMode;
use crate::theme;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The theme in use: one of the built-in ones (`T` goes round them).
    pub theme: String,
    /// The tallest video picture to play, in pixels: 2160, 1440, 1080, 720,
    /// 480. Higher needs a faster connection and computer.
    pub max_height: u32,
    /// Remember what you watch here, and where you stopped, so a video
    /// carries on from there. Kept only on this computer.
    pub history: bool,
    /// Show the start of each video's description under it.
    pub descriptions: bool,
    /// How long a channel's videos are kept before its feed and page are
    /// fetched again, in hours. `R` fetches them now.
    pub refresh_hours: u32,
    /// How images are drawn: "auto" (what the terminal says it can do),
    /// "kitty", "sixel", "iterm2" or "blocks". `TT_IMAGES` overrides it.
    pub images: ImageMode,
    /// Which icons: "auto" (Nerd Font icons in Ghostty, kitty and WezTerm,
    /// which have them built in), "nerd" or "plain". `TT_ICONS` overrides it.
    pub icons: IconMode,
    /// The narrowest a video card may be, in columns.
    pub card_width: u16,
    /// The sidebar's width, in columns.
    pub sidebar_width: u16,
    /// Where yt-dlp, mpv and Deno are, if they aren't found on `PATH`.
    pub yt_dlp: Option<PathBuf>,
    pub mpv: Option<PathBuf>,
    pub deno: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: theme::DEFAULT.into(),
            max_height: 1080,
            history: true,
            descriptions: true,
            refresh_hours: 72,
            images: ImageMode::Auto,
            icons: IconMode::Auto,
            card_width: 34,
            sidebar_width: 26,
            yt_dlp: None,
            mpv: None,
            deno: None,
        }
    }
}

impl Settings {
    /// Defaults if the file doesn't exist yet; an error if it can't be read.
    pub fn load(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => {
                return Err(e).with_context(|| format!("cannot read {}", config::shown(path)));
            }
        };
        toml::from_str(&text).with_context(|| format!("{} is invalid", config::shown(path)))
    }

    /// The card width as used: wide enough for a title, whatever the file
    /// says.
    pub fn card_width(&self) -> u16 {
        self.card_width.clamp(20, 80)
    }

    /// How long a channel's videos are kept, in seconds: an hour to 30 days.
    pub fn refresh_every(&self) -> i64 {
        i64::from(self.refresh_hours.clamp(1, 720)) * 3600
    }

    /// The image mode in use: `TT_IMAGES` if it names one, else the file's.
    pub fn image_mode(&self) -> ImageMode {
        config::var("TT_IMAGES")
            .and_then(|v| ImageMode::parse(&v))
            .unwrap_or(self.images)
    }

    /// The icon mode in use: `TT_ICONS` if it names one, else the file's.
    pub fn icon_mode(&self) -> IconMode {
        config::var("TT_ICONS")
            .and_then(|v| IconMode::parse(&v))
            .unwrap_or(self.icons)
    }

    pub fn sidebar_width(&self) -> u16 {
        self.sidebar_width.clamp(16, 50)
    }

    /// Writes the settings: readable only by the user, and all at once, as
    /// a new file renamed over the old one.
    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string(self)?;
        let new = path.with_extension("toml.new");
        let write = || -> std::io::Result<()> {
            let _ = std::fs::remove_file(&new);
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
            let mut file = options.open(&new)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            std::fs::rename(&new, path)
        };
        write().with_context(|| format!("cannot write {}", config::shown(path)))
    }
}

pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("settings.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_means_defaults_and_saving_round_trips() {
        let dir = std::env::temp_dir().join(format!("tuitube-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = path(&dir);
        let _ = std::fs::remove_file(&file);
        assert_eq!(Settings::load(&file).unwrap(), Settings::default());
        let settings = Settings {
            theme: "latte".into(),
            max_height: 720,
            ..Settings::default()
        };
        settings.save(&file).unwrap();
        assert_eq!(Settings::load(&file).unwrap(), settings);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "only yours");
        }
        std::fs::write(&file, "card_width = 2").unwrap();
        assert_eq!(Settings::load(&file).unwrap().card_width(), 20);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
