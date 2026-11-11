//! Control socket client (console commands and GUI): one JSON line per request and reply.

use crate::config::{self, Config};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

pub fn request(req: Value) -> Result<Value, String> {
    let path = config::socket_path();
    let s = UnixStream::connect(&path).map_err(|e| match e.kind() {
        std::io::ErrorKind::PermissionDenied => format!(
            "нет доступа к {} — добавьте себя в группу yggproxy (sudo usermod -aG yggproxy $USER, затем перелогиньтесь) или запустите через sudo",
            path.display()
        ),
        _ => format!("служба yggproxy не запущена ({e}). Установите и запустите: sudo yggproxy install"),
    })?;
    s.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut w = s.try_clone().map_err(|e| e.to_string())?;
    w.write_all(format!("{req}\n").as_bytes()).map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_str(line.trim()).map_err(|e| format!("ответ службы: {e}"))?;
    match v.get("error").and_then(|e| e.as_str()) {
        Some(e) => Err(e.to_string()),
        None => Ok(v),
    }
}

pub fn status() -> Result<Value, String> {
    request(json!({"cmd": "status"}))
}

pub fn config() -> Result<Config, String> {
    serde_json::from_value(request(json!({"cmd": "config"}))?).map_err(|e| e.to_string())
}

pub fn set(cfg: &Config) -> Result<(), String> {
    request(json!({"cmd": "set", "config": cfg})).map(|_| ())
}

/// Change the config with `f` and apply it.
pub fn update(f: impl FnOnce(&mut Config)) -> Result<Config, String> {
    let mut c = config()?;
    f(&mut c);
    set(&c)?;
    Ok(c)
}

pub fn human(b: u64) -> String {
    match b {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        b if b < 1024 * 1024 * 1024 => format!("{:.1} MB", b as f64 / 1048576.0),
        b => format!("{:.2} GB", b as f64 / 1073741824.0),
    }
}

pub fn mbit(bytes_per_sec: u64) -> String {
    let bits = bytes_per_sec * 8;
    if bits < 1000 {
        format!("{bits} бит/с")
    } else if bits < 1_000_000 {
        format!("{:.1} кбит/с", bits as f64 / 1000.0)
    } else {
        format!("{:.1} Мбит/с", bits as f64 / 1e6)
    }
}

pub fn dur(s: u64) -> String {
    match s {
        s if s < 60 => format!("{s}с"),
        s if s < 3600 => format!("{}м {}с", s / 60, s % 60),
        s if s < 86400 => format!("{}ч {}м", s / 3600, s % 3600 / 60),
        s => format!("{}д {}ч", s / 86400, s % 86400 / 3600),
    }
}

/// kbit/s → "8" / "1.5" Mbit/s text, 0 → "нет".
pub fn limit_text(kbit: u64) -> String {
    if kbit == 0 {
        "нет".into()
    } else if kbit % 1000 == 0 {
        format!("{} Мбит/с", kbit / 1000)
    } else {
        format!("{:.1} Мбит/с", kbit as f64 / 1000.0)
    }
}

/// QR modules of `text` (dark = true), with its side length.
pub fn qr(text: &str) -> Option<(Vec<bool>, usize)> {
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    let w = code.width();
    Some((code.to_colors().into_iter().map(|c| c == qrcode::Color::Dark).collect(), w))
}

/// SOCKS5 greeting + CONNECT 127.0.0.1:SPEEDTEST through the client tunnel, then the
/// iperf-like test of ygghelper (same protocol as the Android app).
pub fn speed_test(download: bool, secs: u8, progress: &mut dyn FnMut(f64)) -> Result<String, String> {
    use std::io::Read;
    use std::net::TcpStream;
    use std::time::Instant;
    let mut s = TcpStream::connect(("127.0.0.1", config::CLIENT_SOCKS)).map_err(|e| format!("клиент не подключён: {e}"))?;
    s.set_nodelay(true).ok();
    s.set_read_timeout(Some(Duration::from_secs(15))).ok();
    let mut b = [0u8; 10];
    s.write_all(&[5, 1, 0]).map_err(|e| e.to_string())?;
    s.read_exact(&mut b[..2]).map_err(|e| e.to_string())?;
    let p = config::SPEEDTEST.to_be_bytes();
    s.write_all(&[5, 1, 0, 1, 127, 0, 0, 1, p[0], p[1]]).map_err(|e| e.to_string())?;
    s.read_exact(&mut b).map_err(|e| e.to_string())?;
    if b[1] != 0 {
        return Err(format!("сервер не поддерживает тест (код {})", b[1]));
    }
    s.write_all(&[if download { b'D' } else { b'U' }, secs]).map_err(|e| e.to_string())?;
    let mbps = |bytes: u64, ms: u64| if ms == 0 { 0.0 } else { bytes as f64 * 8.0 / ms as f64 / 1000.0 };
    if download {
        let mut buf = vec![0u8; 64 * 1024];
        let (mut total, mut t0, mut tick, mut last) = (0u64, None::<Instant>, Instant::now(), 0u64);
        loop {
            let n = s.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            let t = *t0.get_or_insert_with(Instant::now);
            let _ = t;
            total += n as u64;
            if tick.elapsed() >= Duration::from_millis(500) {
                progress(mbps(total - last, tick.elapsed().as_millis() as u64));
                tick = Instant::now();
                last = total;
            }
        }
        let ms = t0.map(|t| t.elapsed().as_millis() as u64).unwrap_or(0);
        Ok(format!("{:.1} Мбит/с · {} за {:.1} с", mbps(total, ms), human(total), ms as f64 / 1000.0))
    } else {
        s.set_read_timeout(Some(Duration::from_secs(secs as u64 + 20))).ok();
        let mut r = s.try_clone().map_err(|e| e.to_string())?;
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let d2 = done.clone();
        let reader = std::thread::spawn(move || {
            let mut rep = [0u8; 16];
            let res = r.read_exact(&mut rep).map(|_| rep);
            d2.store(true, std::sync::atomic::Ordering::Relaxed);
            res
        });
        let buf = vec![0u8; 64 * 1024];
        let (mut sent, mut tick, mut last) = (0u64, Instant::now(), 0u64);
        while !done.load(std::sync::atomic::Ordering::Relaxed) {
            if s.write_all(&buf).is_err() {
                break;
            }
            sent += buf.len() as u64;
            if tick.elapsed() >= Duration::from_millis(500) {
                progress(mbps(sent - last, tick.elapsed().as_millis() as u64));
                tick = Instant::now();
                last = sent;
            }
        }
        let rep = reader.join().map_err(|_| "поток чтения упал".to_string())?.map_err(|e| format!("нет отчёта сервера: {e}"))?;
        let bytes = u64::from_be_bytes(rep[..8].try_into().unwrap());
        let ms = u64::from_be_bytes(rep[8..].try_into().unwrap());
        Ok(format!("{:.1} Мбит/с · {} за {:.1} с", mbps(bytes, ms), human(bytes), ms as f64 / 1000.0))
    }
}
