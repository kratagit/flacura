//! Tiny persistent settings file (`key=value` lines).

use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// Volume slider position in 0..=1 (the gain curve is applied by the UI).
    pub volume: f32,
    pub muted: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            volume: 1.0,
            muted: false,
        }
    }
}

impl Config {
    pub fn load() -> Config {
        let mut c = Config::default();
        let Some(text) = path().and_then(|p| std::fs::read_to_string(p).ok()) else {
            return c;
        };
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            match k.trim() {
                "volume" => {
                    if let Ok(x) = v.trim().parse::<f32>()
                        && x.is_finite()
                    {
                        c.volume = x.clamp(0.0, 1.0);
                    }
                }
                "muted" => c.muted = v.trim() == "true",
                _ => {}
            }
        }
        c
    }

    pub fn save(&self) {
        let Some(p) = path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let text = format!("volume={:.3}\nmuted={}\n", self.volume, self.muted);
        let _ = std::fs::write(p, text);
    }
}

fn path() -> Option<PathBuf> {
    let env = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let dir = if cfg!(windows) {
        env("APPDATA")?.join("Flacura")
    } else if cfg!(target_os = "macos") {
        env("HOME")?.join("Library/Application Support/Flacura")
    } else {
        env("XDG_CONFIG_HOME")
            .or_else(|| env("HOME").map(|h| h.join(".config")))?
            .join("flacura")
    };
    Some(dir.join("config.txt"))
}
