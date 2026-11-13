//! Who may use a closed exit (`access.json`, shared by ygghelper, yggproxy and the Android app).
//!
//! The exit is closed by default. A client's Yggdrasil address is derived from its node key and
//! arrives with every connection (tcp_node's PROXY line), so access is granted to an address:
//! - the owner makes a key (QR, link or address + key typed by hand) valid for 1–365 days; the
//!   first device that uses it binds it to its own address — one key per device;
//! - a device without a key can send a request; the owner approves it (a key already bound to
//!   that address) or deletes it;
//! - the owner may make the exit public (anyone may use it) after accepting the risk.
//!
//! On the wire (SOCKS5 port of the exit): an address with access gets method 0 (no auth); any
//! other client must pick method 2 (RFC 1929 user/password): user = device name, password = the
//! key, or empty for a request. The status byte tells the client what happened ([Auth]).
//! A client offering only method 0 without access gets 0xff — nothing is recorded.

#![allow(dead_code)] // shared with yggproxy, which uses the rest

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const MAX_REQUESTS: usize = 50;
pub const MAX_DAYS: u32 = 365;

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
#[serde(default)]
pub struct Key {
    pub id: String,
    /// The owner's note ("Phone of …").
    pub name: String,
    pub key: String,
    /// Validity in days (1–365), used again on renewal.
    pub days: u32,
    pub created: u64,
    pub expires: u64,
    /// The device's Yggdrasil address once the key was used (or approved from a request).
    pub addr: Option<String>,
    pub bound: u64,
    /// The name the device sent.
    pub device: String,
}

#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
#[serde(default)]
pub struct Request {
    pub addr: String,
    pub name: String,
    pub first: u64,
    pub last: u64,
    pub count: u32,
}

#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
#[serde(default)]
pub struct Access {
    /// Anyone may use the exit.
    pub public: bool,
    /// When the owner accepted the risk of a public exit (0: never).
    pub public_accepted: u64,
    pub keys: Vec<Key>,
    pub requests: Vec<Request>,
}

/// Result of an access check, sent as the RFC 1929 status byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Auth {
    Ok = 0,
    /// No access: the request was recorded, the owner decides.
    Pending = 1,
    BadKey = 2,
    Expired = 3,
    /// The key is already bound to another device.
    OtherDevice = 4,
}

impl Auth {
    pub fn from_code(c: u8) -> Option<Auth> {
        Some(match c {
            0 => Auth::Ok,
            1 => Auth::Pending,
            2 => Auth::BadKey,
            3 => Auth::Expired,
            4 => Auth::OtherDevice,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Auth::Ok => "ok",
            Auth::Pending => "pending",
            Auth::BadKey => "bad_key",
            Auth::Expired => "expired",
            Auth::OtherDevice => "other_device",
        }
    }
}

/// Canonical text form of an address (so "0200:…" and "200:…" match).
pub fn norm_addr(a: &str) -> Option<String> {
    let ip: std::net::Ipv6Addr = a.trim().trim_matches(|c| c == '[' || c == ']').parse().ok()?;
    (ip.octets()[0] & 0xfe == 0x02).then(|| ip.to_string())
}

/// Device and key names: printable, at most 40 characters.
pub fn clean_name(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(40).collect::<String>().trim().to_string()
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::getrandom(&mut b).expect("no system randomness");
    b
}

const B32: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";

/// 20 base32 characters (100 bits): short enough to type, too many to guess.
pub fn new_secret() -> String {
    let b = random_bytes::<13>();
    let mut bits: u128 = 0;
    for x in b {
        bits = (bits << 8) | x as u128;
    }
    (0..20).map(|i| B32[((bits >> (i * 5)) & 31) as usize] as char).collect()
}

/// Keys are typed by hand: case, spaces and dashes don't matter.
pub fn norm_secret(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect()
}

fn eq_ct(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// `yggproxy://c?a=ADDR&k=KEY`: what the QR code holds.
pub fn link(server: &str, key: &str) -> String {
    format!("yggproxy://c?a={server}&k={key}")
}

impl Access {
    pub fn load(path: &Path) -> Access {
        std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).unwrap() + "\n")?;
        std::fs::rename(tmp, path)
    }

    /// Read-modify-write under a lock file (the exit and the app both write).
    pub fn update<R>(path: &Path, f: impl FnOnce(&mut Access) -> R) -> std::io::Result<R> {
        let lock = PathBuf::from(format!("{}.lock", path.display()));
        let mut held = false;
        for _ in 0..100 {
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&lock) {
                Ok(_) => {
                    held = true;
                    break;
                }
                Err(_) => {
                    // A writer that died leaves the lock behind: older than 10 s is stale.
                    let stale = std::fs::metadata(&lock).and_then(|m| m.modified()).ok()
                        .and_then(|t| t.elapsed().ok()).is_some_and(|e| e > Duration::from_secs(10));
                    if stale {
                        let _ = std::fs::remove_file(&lock);
                    } else {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                }
            }
        }
        let mut a = Access::load(path);
        let r = f(&mut a);
        let res = a.save(path);
        if held {
            let _ = std::fs::remove_file(&lock);
        }
        res.map(|_| r)
    }

    /// The address may use the exit now (public, or a valid key bound to it).
    pub fn granted(&self, addr: &str, t: u64) -> bool {
        self.public || self.keys.iter().any(|k| k.addr.as_deref() == Some(addr) && k.expires > t)
    }

    /// A client asked with method 2. Returns the answer and whether the file must be saved.
    pub fn check(&mut self, addr: &str, device: &str, secret: &str, t: u64) -> (Auth, bool) {
        if self.granted(addr, t) {
            return (Auth::Ok, false);
        }
        let secret = norm_secret(secret);
        if secret.is_empty() {
            let had = self.keys.iter().any(|k| k.addr.as_deref() == Some(addr));
            self.add_request(addr, device, t);
            return (if had { Auth::Expired } else { Auth::Pending }, true);
        }
        let Some(k) = self.keys.iter_mut().find(|k| eq_ct(&k.key, &secret)) else { return (Auth::BadKey, false) };
        if k.expires <= t {
            return (Auth::Expired, false);
        }
        if k.addr.as_deref().is_some_and(|a| a != addr) {
            return (Auth::OtherDevice, false);
        }
        k.addr = Some(addr.to_string());
        k.bound = t;
        k.device = clean_name(device);
        self.requests.retain(|r| r.addr != addr);
        (Auth::Ok, true)
    }

    fn add_request(&mut self, addr: &str, device: &str, t: u64) {
        if let Some(r) = self.requests.iter_mut().find(|r| r.addr == addr) {
            r.last = t;
            r.count = r.count.saturating_add(1);
            r.name = clean_name(device);
            return;
        }
        if self.requests.len() >= MAX_REQUESTS {
            // Full: the oldest request goes.
            if let Some(i) = self.requests.iter().enumerate().min_by_key(|(_, r)| r.last).map(|(i, _)| i) {
                self.requests.remove(i);
            }
        }
        self.requests.push(Request { addr: addr.into(), name: clean_name(device), first: t, last: t, count: 1 });
    }

    /// New key; `addr` given = approved request (bound at once).
    pub fn create(&mut self, name: &str, days: u32, addr: Option<&str>, t: u64) -> Key {
        let days = days.clamp(1, MAX_DAYS);
        let mut id = String::new();
        while id.is_empty() || self.keys.iter().any(|k| k.id == id) {
            id = random_bytes::<3>().iter().map(|b| format!("{b:02x}")).collect();
        }
        let mut k = Key { id, name: clean_name(name), key: new_secret(), days, created: t, expires: t + days as u64 * 86400, ..Default::default() };
        if let Some(a) = addr {
            if let Some(r) = self.requests.iter().find(|r| r.addr == a) {
                k.device = r.name.clone();
                if k.name.is_empty() {
                    k.name = r.name.clone();
                }
            }
            // One key per device: an approved address replaces its old key.
            self.keys.retain(|x| x.addr.as_deref() != Some(a));
            k.addr = Some(a.to_string());
            k.bound = t;
            self.requests.retain(|r| r.addr != a);
        }
        self.keys.push(k.clone());
        k
    }

    /// Extend a key by `days` (or its own period) from now.
    pub fn renew(&mut self, id: &str, days: Option<u32>, t: u64) -> Option<Key> {
        let k = self.keys.iter_mut().find(|k| k.id == id)?;
        if let Some(d) = days {
            k.days = d.clamp(1, MAX_DAYS);
        }
        k.expires = t + k.days.max(1) as u64 * 86400;
        Some(k.clone())
    }

    /// Unbind: the same key may be used by a new device (e.g. after reinstalling).
    pub fn unbind(&mut self, id: &str) -> Option<Key> {
        let k = self.keys.iter_mut().find(|k| k.id == id)?;
        k.addr = None;
        k.bound = 0;
        k.device.clear();
        Some(k.clone())
    }

    pub fn revoke(&mut self, id: &str) -> bool {
        let n = self.keys.len();
        self.keys.retain(|k| k.id != id);
        self.keys.len() != n
    }

    pub fn deny(&mut self, addr: &str) -> bool {
        let n = self.requests.len();
        self.requests.retain(|r| r.addr != addr);
        self.requests.len() != n
    }

    pub fn set_public(&mut self, on: bool, accept_risk: bool, t: u64) -> Result<(), &'static str> {
        if on && !accept_risk {
            return Err("public exit: the risk must be accepted");
        }
        self.public = on;
        if on {
            self.public_accepted = t;
        }
        Ok(())
    }
}

/// What the owner is told before making the exit public (also shown by the apps).
pub const PUBLIC_RISK_RU: &str = "Публичный сервер может использовать любой, кто найдёт его через трекер.\n\
• Весь их трафик выходит в интернет с вашего IP-адреса: за нарушения закона, спам, взломы, \
пиратство и блокировки отвечать придётся вам — жалобы и запросы придут вашему провайдеру.\n\
• Чужие люди займут ваш канал и могут исчерпать лимит трафика.\n\
• Сайты и сервисы могут заблокировать ваш IP или показывать капчу.\n\
• Вы не узнаете, кто именно пользуется сервером.\n\
Включайте, только если понимаете и принимаете эти последствия.";

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "200:1111::1";
    const B: &str = "200:2222::2";

    #[test]
    fn key_binds_to_one_device() {
        let mut a = Access::default();
        let k = a.create("phone", 30, None, 1000);
        assert_eq!(k.key.len(), 20);
        assert!(!a.granted(A, 1000));
        assert_eq!(a.check(A, "Pixel", &k.key.to_uppercase(), 1000), (Auth::Ok, true));
        assert!(a.granted(A, 1001));
        assert_eq!(a.check(B, "other", &k.key, 1001), (Auth::OtherDevice, false));
        assert_eq!(a.check(A, "Pixel", "", 1001), (Auth::Ok, false));
        assert!(!a.granted(A, 1000 + 30 * 86400));
        assert_eq!(a.check(A, "Pixel", &k.key, 1000 + 30 * 86400), (Auth::Expired, false));
        a.renew(&k.id, Some(7), 5_000_000);
        assert!(a.granted(A, 5_000_001));
        a.unbind(&k.id);
        assert_eq!(a.check(B, "new", &k.key, 5_000_002), (Auth::Ok, true));
    }

    #[test]
    fn request_and_approve() {
        let mut a = Access::default();
        assert_eq!(a.check(A, "laptop\n", "", 10), (Auth::Pending, true));
        assert_eq!(a.check(A, "laptop", "", 11), (Auth::Pending, true));
        assert_eq!(a.requests.len(), 1);
        assert_eq!(a.requests[0].count, 2);
        assert_eq!(a.check(A, "x", "wrongkey", 12), (Auth::BadKey, false));
        let k = a.create("", 400, Some(A), 20);
        assert_eq!((k.days, k.name.as_str()), (365, "laptop"));
        assert!(a.requests.is_empty() && a.granted(A, 21));
        assert!(a.set_public(true, false, 30).is_err());
        a.set_public(true, true, 30).unwrap();
        assert!(a.granted(B, 31));
    }

    #[test]
    fn requests_are_capped() {
        let mut a = Access::default();
        for i in 0..(MAX_REQUESTS + 5) {
            a.check(&format!("200::{:x}", i + 1), "d", "", i as u64);
        }
        assert_eq!(a.requests.len(), MAX_REQUESTS);
    }

    #[test]
    fn addresses_and_secrets() {
        assert_eq!(norm_addr("[0200:0000::1]").as_deref(), Some("200::1"));
        assert_eq!(norm_addr("fd00::1"), None);
        assert_eq!(norm_secret(" AbCd-ef 12 "), "abcdef12");
        assert_ne!(new_secret(), new_secret());
    }
}
