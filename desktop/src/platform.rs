//! What differs between Linux and Windows: the control channel, local time, admin rights,
//! child processes and the stop signal.

use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

/// Set when the service is asked to stop (SIGTERM/SIGINT, Ctrl+C or the Windows service manager).
pub static TERM: AtomicBool = AtomicBool::new(false);

pub fn stopping() -> bool {
    TERM.load(Ordering::Relaxed)
}

pub fn request_stop() {
    TERM.store(true, Ordering::Relaxed);
}

/// A command that opens no console window on Windows.
pub fn command(prog: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(prog);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// Write `text` to `path` only when it differs: files the node re-reads live are compared by
/// content, and unchanged ones need no write (flash storage on routers and phones).
pub fn write_if_changed(path: &std::path::Path, text: &str) -> bool {
    if std::fs::read_to_string(path).ok().as_deref() == Some(text) {
        return false;
    }
    std::fs::write(path, text).is_ok()
}

/// Download a text file over HTTP(S) in-process (rustls with the Mozilla root certificates, no
/// curl/wget/PowerShell and no system TLS library). At most 32 MB, 30 s per stage.
pub fn download_text(url: &str) -> Result<String, String> {
    use std::io::Read;
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(15))
        .timeout_read(std::time::Duration::from_secs(30))
        .user_agent(concat!("yggproxy/", env!("CARGO_PKG_VERSION")))
        .build();
    let resp = agent.get(url).call().map_err(|e| match e {
        ureq::Error::Status(code, _) => format!("HTTP {code}"),
        ureq::Error::Transport(t) => t.to_string(),
    })?;
    let mut body = Vec::new();
    resp.into_reader().take(32 << 20).read_to_end(&mut body).map_err(|e| e.to_string())?;
    String::from_utf8(body).map_err(|_| "ответ не текст (UTF-8)".into())
}

// ------------------------------------------------------------------------------------------
// Unix

#[cfg(unix)]
mod imp {
    use crate::config;
    use std::path::Path;

    pub type Stream = std::os::unix::net::UnixStream;
    pub type Listener = std::os::unix::net::UnixListener;

    pub fn connect() -> std::io::Result<Stream> {
        Stream::connect(config::socket_path())
    }

    pub fn ctl_name() -> String {
        config::socket_path().display().to_string()
    }

    pub fn listen() -> std::io::Result<Listener> {
        use std::os::unix::fs::PermissionsExt;
        let path = config::socket_path();
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
            let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o755));
        }
        let _ = std::fs::remove_file(&path);
        let l = Listener::bind(&path)?;
        // root:yggproxy 0660 when the group exists (yggproxy install creates it), else root only.
        let grp = std::ffi::CString::new("yggproxy").unwrap();
        let g = unsafe { libc::getgrnam(grp.as_ptr()) };
        let mode = if g.is_null() {
            0o600
        } else {
            let cp = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
            unsafe { libc::chown(cp.as_ptr(), 0, (*g).gr_gid) };
            0o660
        };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))?;
        Ok(l)
    }

    pub fn unlisten() {
        let _ = std::fs::remove_file(config::socket_path());
    }

    pub fn local_hms(t: u64) -> (u32, u32, u32) {
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let tt = t as libc::time_t;
        unsafe { libc::localtime_r(&tt, &mut tm) };
        (tm.tm_hour as u32, tm.tm_min as u32, tm.tm_sec as u32)
    }

    pub fn is_admin() -> bool {
        unsafe { libc::geteuid() == 0 }
    }

    extern "C" fn on_term(_: libc::c_int) {
        super::request_stop();
    }

    pub fn on_stop_signal() {
        unsafe {
            libc::signal(libc::SIGTERM, on_term as *const () as usize);
            libc::signal(libc::SIGINT, on_term as *const () as usize);
        }
    }

    /// Ask a child to stop (SIGTERM).
    pub fn soft_stop(c: &mut std::process::Child) {
        unsafe { libc::kill(c.id() as i32, libc::SIGTERM) };
    }

    pub fn make_executable(p: &Path) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755))
    }
}

// ------------------------------------------------------------------------------------------
// Windows

#[cfg(windows)]
mod imp {
    use crate::config;
    use std::path::Path;

    /// Control channel on Windows: loopback TCP (only this machine can reach it).
    pub type Stream = std::net::TcpStream;
    pub type Listener = std::net::TcpListener;

    pub fn connect() -> std::io::Result<Stream> {
        Stream::connect_timeout(&([127, 0, 0, 1], config::CTL_PORT).into(), std::time::Duration::from_secs(2))
    }

    pub fn ctl_name() -> String {
        format!("127.0.0.1:{}", config::CTL_PORT)
    }

    pub fn listen() -> std::io::Result<Listener> {
        Listener::bind(("127.0.0.1", config::CTL_PORT))
    }

    pub fn unlisten() {}

    pub fn local_hms(_t: u64) -> (u32, u32, u32) {
        use windows_sys::Win32::System::SystemInformation::GetLocalTime;
        let mut st = unsafe { std::mem::zeroed() };
        unsafe { GetLocalTime(&mut st) };
        (st.wHour as u32, st.wMinute as u32, st.wSecond as u32)
    }

    pub fn is_admin() -> bool {
        unsafe { windows_sys::Win32::UI::Shell::IsUserAnAdmin() != 0 }
    }

    pub fn on_stop_signal() {
        use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
        unsafe extern "system" fn handler(_: u32) -> i32 {
            super::request_stop();
            1
        }
        unsafe { SetConsoleCtrlHandler(Some(handler), 1) };
    }

    /// Ask a child to stop: close its stdin (ygghelper tun removes its adapter on the way out).
    pub fn soft_stop(c: &mut std::process::Child) {
        drop(c.stdin.take());
    }

    pub fn make_executable(_: &Path) -> std::io::Result<()> {
        Ok(())
    }
}

pub use imp::*;

/// `HH:MM:SS` local time.
pub fn now_hms() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    let (h, m, s) = local_hms(t);
    format!("{h:02}:{m:02}:{s:02}")
}
