//! Chat between a closed exit's owner and its clients (shared by ygghelper, yggproxy and, through
//! `ygghelper chat …`, the Android app).
//!
//! Two files next to `access.json`, each with a single writer, so nothing is lost to two
//! processes writing at once:
//! - `chat-in.json`: what clients wrote, appended by the exit (ygghelper) only;
//! - `chat-out.json`: the owner's answers, appended by yggproxy or the Android app only.
//!
//! A client is known by its Yggdrasil address, which the node proves (the PROXY line); it may
//! write while it has access or a request waits for the owner.
//!
//! On the wire (method 0x80 of the exit's SOCKS port, see access.rs), after the method reply:
//! - `[2, 1, since: u64 BE]` — the conversation after `since` (ms) →
//!   `[2, status, n: u16 BE, (t: u64 BE, from: u8, len: u16 BE, text) * n]`, `from` 1 = owner;
//! - `[2, 2, len: u16 BE, text]` — a message → `[2, status, t: u64 BE]`.
//!
//! `status`: [ST_OK], [ST_DENIED] (no access and no request), [ST_LIMIT] (too long or too many).

#![allow(dead_code)] // shared with yggproxy, which uses other parts

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const ST_OK: u8 = 0;
pub const ST_DENIED: u8 = 1;
pub const ST_LIMIT: u8 = 2;

/// Longest message, bytes of UTF-8.
pub const MAX_TEXT: usize = 1000;
/// Messages kept per client in each file (older ones go).
pub const KEEP_PER_ADDR: usize = 200;
/// Messages a sync returns at most (the newest).
pub const SYNC_MAX: usize = 50;
/// A client may write this many messages per minute.
pub const PER_MINUTE: usize = 10;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Msg {
    /// The client's Yggdrasil address (whoever wrote it).
    pub addr: String,
    /// Unix time, milliseconds; unique and growing within a file.
    pub t: u64,
    pub text: String,
}

/// One message of a conversation, either side.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Line {
    pub t: u64,
    /// The owner wrote it (else the client).
    pub owner: bool,
    pub text: String,
}

pub fn in_path(access: &Path) -> PathBuf {
    access.with_file_name("chat-in.json")
}

pub fn out_path(access: &Path) -> PathBuf {
    access.with_file_name("chat-out.json")
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub fn load(path: &Path) -> Vec<Msg> {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save(path: &Path, msgs: &[Msg]) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(msgs)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o640));
    }
    std::fs::rename(&tmp, path)
}

/// Trim control characters (except newlines) and the ends; `None` if nothing is left or it is
/// too long.
pub fn clean(text: &str) -> Option<String> {
    let t: String = text.chars().filter(|c| !c.is_control() || *c == '\n').collect();
    let t = t.trim().to_string();
    (!t.is_empty() && t.len() <= MAX_TEXT).then_some(t)
}

/// Append a message for `addr` to `path` (its only writer). Returns its time.
pub fn append(path: &Path, addr: &str, text: &str) -> std::io::Result<u64> {
    let mut msgs = load(path);
    // Unique across both files: a sync asks for "after t", and an answer written in the same
    // millisecond as the client's message would be skipped.
    let other = if path.file_name().is_some_and(|n| n == "chat-in.json") { path.with_file_name("chat-out.json") } else { path.with_file_name("chat-in.json") };
    let last = msgs.last().map_or(0, |m| m.t).max(load(&other).last().map_or(0, |m| m.t));
    let t = now_ms().max(last + 1);
    msgs.push(Msg { addr: addr.to_string(), t, text: text.to_string() });
    let mine = msgs.iter().filter(|m| m.addr == addr).count();
    if mine > KEEP_PER_ADDR {
        let mut drop = mine - KEEP_PER_ADDR;
        msgs.retain(|m| {
            if drop > 0 && m.addr == addr {
                drop -= 1;
                false
            } else {
                true
            }
        });
    }
    save(path, &msgs)?;
    Ok(t)
}

/// Both sides of `addr`'s conversation after `since`, oldest first, at most `max` (the newest).
pub fn conversation(access: &Path, addr: &str, since: u64, max: usize) -> Vec<Line> {
    let mut lines: Vec<Line> = load(&in_path(access))
        .into_iter()
        .filter(|m| m.addr == addr && m.t > since)
        .map(|m| Line { t: m.t, owner: false, text: m.text })
        .chain(load(&out_path(access)).into_iter().filter(|m| m.addr == addr && m.t > since).map(|m| Line { t: m.t, owner: true, text: m.text }))
        .collect();
    lines.sort_by_key(|l| l.t);
    let skip = lines.len().saturating_sub(max);
    lines.drain(..skip);
    lines
}

/// Every conversation: (address, last line, messages from the client after `seen`), newest first.
pub fn conversations(access: &Path, seen: u64) -> Vec<(String, Line, usize)> {
    let inbox = load(&in_path(access));
    let outbox = load(&out_path(access));
    let mut addrs: Vec<String> = inbox.iter().chain(outbox.iter()).map(|m| m.addr.clone()).collect();
    addrs.sort();
    addrs.dedup();
    let mut v: Vec<(String, Line, usize)> = addrs
        .into_iter()
        .filter_map(|a| {
            let last_in = inbox.iter().filter(|m| m.addr == a).last().map(|m| Line { t: m.t, owner: false, text: m.text.clone() });
            let last_out = outbox.iter().filter(|m| m.addr == a).last().map(|m| Line { t: m.t, owner: true, text: m.text.clone() });
            let last = match (last_in, last_out) {
                (Some(i), Some(o)) => if i.t > o.t { i } else { o },
                (i, o) => i.or(o)?,
            };
            let new = inbox.iter().filter(|m| m.addr == a && m.t > seen).count();
            Some((a, last, new))
        })
        .collect();
    v.sort_by(|x, y| y.1.t.cmp(&x.1.t));
    v
}

/// Encode a sync reply body (after `[2, status]`).
pub fn encode_lines(lines: &[Line]) -> Vec<u8> {
    let mut r = Vec::new();
    r.extend_from_slice(&(lines.len() as u16).to_be_bytes());
    for l in lines {
        r.extend_from_slice(&l.t.to_be_bytes());
        r.push(l.owner as u8);
        let b = l.text.as_bytes();
        let b = &b[..b.len().min(u16::MAX as usize)];
        r.extend_from_slice(&(b.len() as u16).to_be_bytes());
        r.extend_from_slice(b);
    }
    r
}

/// Decode what [`encode_lines`] made.
pub fn decode_lines(b: &[u8]) -> Option<Vec<Line>> {
    let n = u16::from_be_bytes(b.get(..2)?.try_into().ok()?) as usize;
    let mut i = 2;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let t = u64::from_be_bytes(b.get(i..i + 8)?.try_into().ok()?);
        let owner = *b.get(i + 8)? == 1;
        let len = u16::from_be_bytes(b.get(i + 9..i + 11)?.try_into().ok()?) as usize;
        let text = String::from_utf8_lossy(b.get(i + 11..i + 11 + len)?).to_string();
        v.push(Line { t, owner, text });
        i += 11 + len;
    }
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_files_make_one_conversation() {
        let dir = std::env::temp_dir().join(format!("chat-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let acc = dir.join("access.json");
        let a = "200:1::1";
        let t1 = append(&in_path(&acc), a, "привет").unwrap();
        let t2 = append(&out_path(&acc), a, "здравствуйте").unwrap();
        append(&in_path(&acc), "200:2::2", "чужое").unwrap();
        let t3 = append(&in_path(&acc), a, "спасибо").unwrap();
        let c = conversation(&acc, a, 0, 50);
        assert_eq!(c.iter().map(|l| (l.owner, l.text.as_str())).collect::<Vec<_>>(), vec![(false, "привет"), (true, "здравствуйте"), (false, "спасибо")]);
        assert!(t1 < t3 && t2 > 0);
        assert_eq!(conversation(&acc, a, t1, 50).len(), 2);
        assert_eq!(conversation(&acc, a, 0, 1)[0].text, "спасибо");
        let all = conversations(&acc, t1);
        assert_eq!(all.len(), 2);
        let mine = all.iter().find(|x| x.0 == a).unwrap();
        assert_eq!((mine.1.text.as_str(), mine.2), ("спасибо", 1));
        let enc = encode_lines(&c);
        assert_eq!(decode_lines(&enc).unwrap(), c);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn clean_text() {
        assert_eq!(clean("  hi\u{7}\n there "), Some("hi\n there".into()));
        assert_eq!(clean(" \n "), None);
        assert_eq!(clean(&"я".repeat(600)), None);
    }

    #[test]
    fn old_messages_go() {
        let dir = std::env::temp_dir().join(format!("chat-keep-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("chat-in.json");
        for i in 0..KEEP_PER_ADDR + 5 {
            append(&p, "200::1", &i.to_string()).unwrap();
        }
        let m = load(&p);
        assert_eq!(m.len(), KEEP_PER_ADDR);
        assert_eq!(m[0].text, "5");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
