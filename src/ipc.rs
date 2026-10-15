//! Single-instance handoff: the first process listens on a per-user local
//! socket, later processes send it their file path and exit immediately.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;

use interprocess::local_socket::{Listener, ListenerOptions, Name, Stream, prelude::*};

/// The socket name, and whether it is a file that can outlive a crashed owner.
///
/// Windows: a named pipe tagged with the user name (pipes vanish with their
/// owner). Unix: a socket file in a per-user directory, `$XDG_RUNTIME_DIR`
/// (mode 0700) or the temp dir (per-user on macOS).
fn socket_name() -> std::io::Result<(Name<'static>, bool)> {
    let user = std::env::var(if cfg!(windows) { "USERNAME" } else { "USER" }).unwrap_or_default();
    let user: String = user
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let file = format!("flacura-{user}.sock");
    #[cfg(windows)]
    {
        use interprocess::local_socket::GenericNamespaced;
        file.to_ns_name::<GenericNamespaced>()
            .map(|n| (n.into_owned(), false))
    }
    #[cfg(not(windows))]
    {
        use interprocess::local_socket::GenericFilePath;
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        dir.join(file)
            .to_fs_name::<GenericFilePath>()
            .map(|n| (n.into_owned(), true))
    }
}

pub enum Role {
    /// We are the running instance; incoming paths will arrive through `listen`.
    Primary(Listener),
    /// Another instance took our path.
    Forwarded,
    /// IPC unavailable; just run standalone.
    Standalone,
}

pub fn acquire(path: Option<&PathBuf>) -> Role {
    let Ok((name, is_file)) = socket_name() else {
        return Role::Standalone;
    };
    for attempt in 0..3 {
        // Fails with AddrInUse (Unix) or PermissionDenied (Windows pipes) when taken.
        if let Ok(l) = ListenerOptions::new().name(name.clone()).create_sync() {
            return Role::Primary(l);
        }
        // Someone owns the name: hand over our path.
        if let Ok(mut s) = Stream::connect(name.clone())
            && s.write_all(&encode(path)).is_ok()
        {
            return Role::Forwarded;
        }
        // A socket file nobody answers on was left by a crashed instance.
        if attempt == 0
            && is_file
            && let Ok(l) = ListenerOptions::new()
                .name(name.clone())
                .try_overwrite(true)
                .create_sync()
        {
            return Role::Primary(l);
        }
        thread::sleep(std::time::Duration::from_millis(30));
    }
    Role::Standalone
}

/// Path bytes terminated by NUL (empty = just focus the window). On Unix the
/// raw bytes are sent, so non-UTF-8 file names survive the handoff.
fn encode(path: Option<&PathBuf>) -> Vec<u8> {
    let mut msg = match path {
        #[cfg(unix)]
        Some(p) => {
            use std::os::unix::ffi::OsStrExt;
            p.as_os_str().as_bytes().to_vec()
        }
        #[cfg(not(unix))]
        Some(p) => p.to_string_lossy().into_owned().into_bytes(),
        None => Vec::new(),
    };
    msg.push(0);
    msg
}

fn decode(mut msg: Vec<u8>) -> Option<PathBuf> {
    if msg.last() == Some(&0) {
        msg.pop();
    }
    if msg.is_empty() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Some(PathBuf::from(std::ffi::OsString::from_vec(msg)))
    }
    #[cfg(not(unix))]
    {
        Some(PathBuf::from(String::from_utf8_lossy(&msg).into_owned()))
    }
}

/// Forwards every received path (`None` = just focus the window).
pub fn listen(listener: Listener, tx: Sender<Option<PathBuf>>, wake: impl Fn() + Send + 'static) {
    thread::Builder::new()
        .name("ipc".into())
        .spawn(move || {
            for conn in listener.incoming().filter_map(Result::ok) {
                let mut msg = Vec::new();
                if BufReader::new(conn).read_until(0, &mut msg).is_ok() {
                    let _ = tx.send(decode(msg));
                    wake();
                }
            }
        })
        .expect("spawn ipc thread");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_roundtrip() {
        let p = PathBuf::from("music/zażółć gęślą jaźń.flac");
        assert_eq!(decode(encode(Some(&p))), Some(p));
        assert_eq!(decode(encode(None)), None);
    }
}
