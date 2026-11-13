//! Talking to trackers as a client (console commands and the service): the built-in tracker
//! list, `GET`/`POST` over Yggdrasil — directly when this host has a Yggdrasil TUN, else through
//! the service's node (`tcp_node --socks`).

use crate::config;
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{Ipv6Addr, SocketAddr, TcpStream};
use std::time::Duration;

/// Overlay port of the tracker API.
pub const TRACKER_PORT: u16 = 27081;
/// Built-in tracker(s): the project's own server; others are found and synced.
pub const DEFAULT_TRACKERS: &[&str] = &["200:379c:ac8b:ad9f:2fbd:c38c:4be:51a0"];

/// `list` with the built-in trackers added (they are always asked, also on old configs).
pub fn with_defaults(list: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in DEFAULT_TRACKERS.iter().map(|s| s.to_string()).chain(list.iter().cloned()) {
        if let Some(a) = config::parse_server(&t).map(|a| a.to_string())
            && !out.contains(&a) {
                out.push(a);
            }
    }
    out
}

/// This host has a Yggdrasil address (a yggdrasil-go TUN): the kernel can reach 200::/7 itself.
fn have_ygg_route() -> bool {
    #[cfg(unix)]
    return std::fs::read_to_string("/proc/net/if_inet6").map(|t| t.lines().any(|l| l.starts_with("02") || l.starts_with("03"))).unwrap_or(false);
    // Windows: the source address the system would pick for 200::/7 is a Yggdrasil one.
    #[cfg(windows)]
    {
        let Ok(u) = std::net::UdpSocket::bind("[::]:0") else { return false };
        u.connect("[200::1]:9").is_ok() && u.local_addr().map(|a| matches!(a.ip(), std::net::IpAddr::V6(v) if (v.octets()[0] & 0xfe) == 0x02)).unwrap_or(false)
    }
}

fn io_text(e: &std::io::Error, tracker: Ipv6Addr, secs: u64) -> String {
    match e.kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => format!(
            "трекер {tracker} не ответил за {secs} с (первое соединение через Yggdrasil, пока ищется путь, бывает долгим — повторите)"
        ),
        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::UnexpectedEof => {
            format!("трекер {tracker} недоступен (на нём не запущен трекер или закрыт порт {TRACKER_PORT})")
        }
        _ => format!("трекер {tracker}: {e}"),
    }
}

fn parse_http(body: &[u8], tracker: Ipv6Addr) -> Result<Value, String> {
    let text = String::from_utf8_lossy(body);
    let (_, b) = text.split_once("\r\n\r\n").ok_or_else(|| format!("трекер {tracker} закрыл соединение без ответа (недоступен или закрыт порт {TRACKER_PORT})"))?;
    serde_json::from_str(b).map_err(|e| format!("трекер {tracker}: неверный ответ ({e})"))
}

/// Directly through a yggdrasil-go TUN.
fn fetch_direct(tracker: Ipv6Addr, path: &str) -> Result<Value, String> {
    request_direct(tracker, &format!("GET {path} HTTP/1.0\r\nHost: [{tracker}]\r\n\r\n"))
}

fn request_direct(tracker: Ipv6Addr, req: &str) -> Result<Value, String> {
    let mut s = TcpStream::connect_timeout(&SocketAddr::from((tracker, TRACKER_PORT)), Duration::from_secs(20)).map_err(|e| io_text(&e, tracker, 20))?;
    s.set_read_timeout(Some(Duration::from_secs(30))).ok();
    s.write_all(req.as_bytes()).map_err(|e| io_text(&e, tracker, 30))?;
    let mut body = Vec::new();
    s.take(4 * 1024 * 1024).read_to_end(&mut body).map_err(|e| io_text(&e, tracker, 30))?;
    parse_http(&body, tracker)
}

/// Through tcp_node --socks (the yggproxy service).
fn fetch_socks(tracker: Ipv6Addr, path: &str) -> Result<Value, String> {
    request_socks(tracker, &format!("GET {path} HTTP/1.0\r\nHost: [{tracker}]\r\n\r\n"))
}

fn request_socks(tracker: Ipv6Addr, req: &str) -> Result<Value, String> {
    let mut s = TcpStream::connect(("127.0.0.1", config::NODE_SOCKS))
        .map_err(|_| "трекер недоступен: нет yggdrasil-go и не запущена служба yggproxy (sudo yggproxy install)".to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(40))).ok();
    let mut b = [0u8; 22];
    s.write_all(&[5, 1, 0]).map_err(|e| e.to_string())?;
    s.read_exact(&mut b[..2]).map_err(|e| e.to_string())?;
    let mut sreq = vec![5, 1, 0, 4];
    sreq.extend_from_slice(&tracker.octets());
    sreq.extend_from_slice(&TRACKER_PORT.to_be_bytes());
    s.write_all(&sreq).map_err(|e| e.to_string())?;
    s.read_exact(&mut b).map_err(|e| e.to_string())?;
    if b[1] != 0 {
        return Err(format!("SOCKS узла: отказ {}", b[1]));
    }
    s.write_all(req.as_bytes()).map_err(|e| io_text(&e, tracker, 40))?;
    let mut body = Vec::new();
    s.take(4 * 1024 * 1024).read_to_end(&mut body).map_err(|e| io_text(&e, tracker, 40))?;
    parse_http(&body, tracker)
}

/// `GET path` from a tracker: directly through a yggdrasil-go TUN when this host has one, else
/// through the yggproxy service's tcp_node SOCKS. Two tries: the first connection to a new
/// address can take a while (path discovery).
pub fn fetch(tracker: Ipv6Addr, path: &str) -> Result<Value, String> {
    let direct = have_ygg_route();
    let mut last = String::new();
    for _ in 0..2 {
        let r = if direct { fetch_direct(tracker, path).or_else(|e| fetch_socks(tracker, path).map_err(|_| e)) } else { fetch_socks(tracker, path) };
        match r {
            Ok(v) => return Ok(v),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// `POST path` with a JSON body to a tracker (same transport as [`fetch`]).
pub fn post(tracker: Ipv6Addr, path: &str, body: &Value) -> Result<Value, String> {
    let b = body.to_string();
    let req = format!("POST {path} HTTP/1.0\r\nHost: [{tracker}]\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{b}", b.len());
    if have_ygg_route() { request_direct(tracker, &req).or_else(|_| request_socks(tracker, &req)) } else { request_socks(tracker, &req) }
}

/// The first of `trackers` that answers, with its address.
pub fn fetch_any(trackers: &[String], path: &str) -> Result<(Ipv6Addr, Value), String> {
    let mut errs = Vec::new();
    for t in trackers.iter().filter_map(|t| config::parse_server(t)) {
        match fetch(t, path) {
            Ok(v) => return Ok((t, v)),
            Err(e) => errs.push(e),
        }
    }
    Err(if errs.is_empty() { "нет трекеров".into() } else { errs.join("\n") })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_always_in_the_list() {
        let l = with_defaults(&["200:1::1".into(), DEFAULT_TRACKERS[0].into()]);
        assert_eq!(l.len(), DEFAULT_TRACKERS.len() + 1);
        assert_eq!(l[0], DEFAULT_TRACKERS[0]);
    }
}
