//! File association helpers (`--register` / `--unregister`).

#[cfg(windows)]
use crate::decode::AUDIO_EXTENSIONS;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const PROG_ID: &str = "Flacura.AudioFile";

#[cfg(windows)]
unsafe extern "system" {
    fn SHChangeNotify(wEventId: i32, uFlags: u32, dwItem1: *const (), dwItem2: *const ());
    fn ShellExecuteW(
        hwnd: *mut core::ffi::c_void,
        lpOperation: *const u16,
        lpFile: *const u16,
        lpParameters: *const u16,
        lpDirectory: *const u16,
        nShowCmd: i32,
    ) -> isize;
}

#[cfg(windows)]
fn notify_shell_associations_changed() {
    const SHCNE_ASSOCCHANGED: i32 = 0x0800_0000;
    const SHCNF_IDLIST: u32 = 0x0000;
    unsafe {
        SHChangeNotify(
            SHCNE_ASSOCCHANGED,
            SHCNF_IDLIST,
            std::ptr::null(),
            std::ptr::null(),
        );
    }
}

#[cfg(windows)]
fn open_uri(uri: &str) -> Result<(), String> {
    let op: Vec<u16> = "open\0".encode_utf16().collect();
    let target: Vec<u16> = uri.encode_utf16().chain(std::iter::once(0)).collect();
    let rc = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            op.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1, // SW_SHOWNORMAL
        )
    };
    if rc > 32 {
        Ok(())
    } else {
        Err(format!("Could not open Windows Settings (code {rc})"))
    }
}

#[cfg(windows)]
fn reg(args: &[&str]) -> Result<(), String> {
    let status = std::process::Command::new("reg")
        .creation_flags(0x08000000)
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
    let exe_name = exe
        .file_name()
        .ok_or("Executable has no filename")?
        .to_string_lossy()
        .into_owned();
    let exe = exe.to_string_lossy();
    let open = format!("\"{exe}\" \"%1\"");
    let icon = format!("{exe},0");
    let classes = r"HKCU\Software\Classes";
    let prog = format!(r"{classes}\{PROG_ID}");
    let prog_app = format!(r"{prog}\Application");
    let app = format!(r"{classes}\Applications\{exe_name}");
    let capabilities = r"HKCU\Software\Clients\Media\Flacura\Capabilities";

    reg(&["add", &prog, "/ve", "/d", "Flacura Audio File", "/f"])?;
    reg(&[
        "add",
        &prog,
        "/v",
        "FriendlyTypeName",
        "/d",
        "Flacura Audio File",
        "/f",
    ])?;
    for (name, value) in [
        ("ApplicationName", "Flacura"),
        ("ApplicationDescription", "Flacura audio player"),
        ("ApplicationIcon", icon.as_str()),
    ] {
        reg(&["add", &prog_app, "/v", name, "/d", value, "/f"])?;
        reg(&["add", capabilities, "/v", name, "/d", value, "/f"])?;
    }
    reg(&["add", &app, "/v", "FriendlyAppName", "/d", "Flacura", "/f"])?;
    reg(&[
        "add",
        &format!(r"{app}\DefaultIcon"),
        "/ve",
        "/d",
        &icon,
        "/f",
    ])?;
    reg(&[
        "add",
        r"HKCU\Software\RegisteredApplications",
        "/v",
        "Flacura",
        "/d",
        r"Software\Clients\Media\Flacura\Capabilities",
        "/f",
    ])?;
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
        reg(&[
            "add",
            &format!(r"{capabilities}\FileAssociations"),
            "/v",
            &format!(".{ext}"),
            "/d",
            PROG_ID,
            "/f",
        ])?;
        let ext_key = format!(r"{classes}\.{ext}");
        reg(&["add", &ext_key, "/ve", "/d", PROG_ID, "/f"])?;
        let key = format!(r"{ext_key}\OpenWithProgids");
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
    notify_shell_associations_changed();
    open_uri("ms-settings:defaultapps?registeredAppUser=Flacura")?;
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
    if let Ok(exe) = std::env::current_exe()
        && let Some(name) = exe.file_name()
    {
        let _ = reg(&[
            "delete",
            &format!(r"{classes}\Applications\{}", name.to_string_lossy()),
            "/f",
        ]);
    }
    let _ = reg(&[
        "delete",
        r"HKCU\Software\RegisteredApplications",
        "/v",
        "Flacura",
        "/f",
    ]);
    let _ = reg(&["delete", r"HKCU\Software\Flacura\Capabilities", "/f"]);
    let _ = reg(&["delete", r"HKCU\Software\Clients\Media\Flacura", "/f"]);
    for ext in AUDIO_EXTENSIONS {
        let key = format!(r"{classes}\.{ext}\OpenWithProgids");
        let _ = reg(&["delete", &key, "/v", PROG_ID, "/f"]);
    }
    notify_shell_associations_changed();
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
