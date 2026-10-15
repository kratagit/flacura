//! File association helpers (`--register` / `--unregister`).

#[cfg(windows)]
use crate::decode::AUDIO_EXTENSIONS;

#[cfg(windows)]
const PROG_ID: &str = "Flacura.AudioFile";

#[cfg(windows)]
fn reg(args: &[&str]) -> Result<(), String> {
    let status = std::process::Command::new("reg")
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("reg {} failed", args.join(" ")))
    }
}

/// Registers Flacura under HKCU as a handler for audio files and opens the
/// "Default apps" settings page (Windows does not allow apps to set defaults).
#[cfg(windows)]
pub fn register() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.to_string_lossy();
    let open = format!("\"{exe}\" \"%1\"");
    let icon = format!("\"{exe}\",0");
    let classes = r"HKCU\Software\Classes";
    let prog = format!(r"{classes}\{PROG_ID}");
    let app = format!(r"{classes}\Applications\flacura.exe");

    reg(&["add", &prog, "/ve", "/d", "Audio file", "/f"])?;
    reg(&[
        "add",
        &format!(r"{prog}\DefaultIcon"),
        "/ve",
        "/d",
        &icon,
        "/f",
    ])?;
    reg(&[
        "add",
        &format!(r"{prog}\shell\open\command"),
        "/ve",
        "/d",
        &open,
        "/f",
    ])?;
    reg(&[
        "add",
        &format!(r"{app}\shell\open\command"),
        "/ve",
        "/d",
        &open,
        "/f",
    ])?;
    for ext in AUDIO_EXTENSIONS {
        let key = format!(r"{classes}\.{ext}\OpenWithProgids");
        reg(&["add", &key, "/v", PROG_ID, "/t", "REG_NONE", "/f"])?;
        reg(&[
            "add",
            &format!(r"{app}\SupportedTypes"),
            "/v",
            &format!(".{ext}"),
            "/d",
            "",
            "/f",
        ])?;
    }
    let _ = std::process::Command::new("cmd")
        .args(["/c", "start", "ms-settings:defaultapps"])
        .status();
    Ok(())
}

#[cfg(windows)]
pub fn unregister() -> Result<(), String> {
    let classes = r"HKCU\Software\Classes";
    let _ = reg(&["delete", &format!(r"{classes}\{PROG_ID}"), "/f"]);
    let _ = reg(&[
        "delete",
        &format!(r"{classes}\Applications\flacura.exe"),
        "/f",
    ]);
    for ext in AUDIO_EXTENSIONS {
        let key = format!(r"{classes}\.{ext}\OpenWithProgids");
        let _ = reg(&["delete", &key, "/v", PROG_ID, "/f"]);
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
const MIME_TYPES: &[&str] = &[
    "audio/flac",
    "audio/x-flac",
    "audio/mpeg",
    "audio/mp3",
    "audio/wav",
    "audio/x-wav",
    "audio/vnd.wave",
    "audio/ogg",
    "audio/x-vorbis+ogg",
    "audio/mp4",
    "audio/x-m4a",
    "audio/aac",
    "audio/aiff",
    "audio/x-aiff",
    "audio/x-matroska",
    "audio/x-caf",
    "audio/webm",
];

/// Installs a .desktop entry in ~/.local/share/applications and makes it the
/// default handler for common audio MIME types.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn register() -> Result<(), String> {
    use std::process::Command;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share"))
        })
        .ok_or("HOME not set")?;
    let dir = data.join("applications");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let desktop = format!(
        "[Desktop Entry]\nType=Application\nName=Flacura\nComment=Minimal audio player\n\
         Exec=\"{}\" %f\nIcon=audio-x-generic\nTerminal=false\nCategories=AudioVideo;Audio;Player;\n\
         MimeType={};\nNoDisplay=false\n",
        exe.display(),
        MIME_TYPES.join(";")
    );
    std::fs::write(dir.join("flacura.desktop"), desktop).map_err(|e| e.to_string())?;
    let _ = Command::new("update-desktop-database").arg(&dir).status();
    let mut cmd = Command::new("xdg-mime");
    cmd.args(["default", "flacura.desktop"]).args(MIME_TYPES);
    let _ = cmd.status();
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn unregister() -> Result<(), String> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share"))
        })
        .ok_or("HOME not set")?;
    let dir = data.join("applications");
    let _ = std::fs::remove_file(dir.join("flacura.desktop"));
    let _ = std::process::Command::new("update-desktop-database")
        .arg(&dir)
        .status();
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn register() -> Result<(), String> {
    Err("Finder file associations are not supported on macOS yet; \
         run `flacura <file>` or drop files on the window"
        .into())
}

#[cfg(target_os = "macos")]
pub fn unregister() -> Result<(), String> {
    Ok(())
}
