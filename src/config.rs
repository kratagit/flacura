//! Tiny persistent settings file (`key=value` lines).

use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// Volume slider position in 0..=1 (the gain curve is applied by the UI).
    pub volume: f32,
    pub muted: bool,
    pub animated_background: bool,
    /// Adapts background wave speed and responsiveness to the track's tempo.
    pub tempo_adaptive_speed: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            volume: 1.0,
            muted: false,
            animated_background: true,
            tempo_adaptive_speed: true,
        }
    }
}

impl Config {
    pub fn load() -> Config {
        let Some(text) = path().and_then(|p| std::fs::read_to_string(p).ok()) else {
            return Config::default();
        };
        Self::parse(&text)
    }

    fn parse(text: &str) -> Self {
        let mut c = Config::default();
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
                "animated_background" => {
                    if let Ok(enabled) = v.trim().parse() {
                        c.animated_background = enabled;
                    }
                }
                "tempo_adaptive_speed" => {
                    if let Ok(enabled) = v.trim().parse() {
                        c.tempo_adaptive_speed = enabled;
                    }
                }
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
        let _ = std::fs::write(p, self.encode());
    }

    fn encode(&self) -> String {
        format!(
            "volume={:.3}\nmuted={}\nanimated_background={}\ntempo_adaptive_speed={}\n",
            self.volume, self.muted, self.animated_background, self.tempo_adaptive_speed
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_settings_keep_background_enabled() {
        let config = Config::parse("volume=0.500\nmuted=true\n");
        assert!(config.animated_background);
        assert!(config.tempo_adaptive_speed);
        assert_eq!(config.volume, 0.5);
        assert!(config.muted);
        assert!(Config::parse("animated_background=invalid").animated_background);
    }

    #[test]
    fn disabled_background_survives_settings_roundtrip() {
        let config = Config {
            volume: 0.375,
            muted: true,
            animated_background: false,
            tempo_adaptive_speed: false,
        };
        assert_eq!(Config::parse(&config.encode()), config);
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
