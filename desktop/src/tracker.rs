//! `yggproxy tracker`: finds Ratatoskr exit nodes (and other trackers) in Yggdrasil and serves
//! the list to clients.
//!
//! Runs next to a regular yggdrasil-go node (TUN + admin socket):
//! - crawl: from our peers and tree, `debug_remotegetpeers` on every node, breadth first, so the
//!   public peers and everything connected to them get visited;
//! - probe: TCP to `[addr]:27080` through the TUN, a SOCKS5 greeting and a CONNECT to the exit's
//!   speed-test port (only Ratatoskr exits allow that) — an exit; `[addr]:27081 /info` — a tracker;
//! - sync: other trackers' lists are fetched and every entry is probed again before it is listed;
//! - API (HTTP on our Yggdrasil address, port 27081): `GET /exits`, `GET /trackers`, `GET /info`.

use crate::config;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// Overlay port of the tracker API.
pub const TRACKER_PORT: u16 = 27081;
/// Built-in tracker(s): the project's own server; others are found and synced.
pub const DEFAULT_TRACKERS: &[&str] = &["200:fbf3:4681:219d:8a88:4f:9770:ece5"];

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct TrackerConfig {
    pub admin_socket: String,
    pub crawl_interval_min: u64,
    pub recheck_interval_min: u64,
    pub sync_interval_min: u64,
    /// Exits not confirmed for this long are dropped from the list.
    pub expire_hours: u64,
    pub crawl_workers: usize,
    pub probe_workers: usize,
    pub max_nodes: usize,
    pub trackers: Vec<String>,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self {
            admin_socket: "/var/run/yggdrasil/yggdrasil.sock".into(),
            crawl_interval_min: 30,
            recheck_interval_min: 5,
            sync_interval_min: 5,
            expire_hours: 24,
            crawl_workers: 24,
            probe_workers: 64,
            max_nodes: 50_000,
            trackers: DEFAULT_TRACKERS.iter().map(|s| s.to_string()).collect(),
        }
    }
}

fn tracker_config_path() -> std::path::PathBuf {
    config::config_path().with_file_name("tracker.json")
}

fn db_path() -> std::path::PathBuf {
    config::state_dir().join("tracker-db.json")
}

fn now() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_secs()
}

fn log(msg: &str) {
    eprintln!("[tracker] {msg}");
}

// ------------------------------------------------------------------------------------------
// Yggdrasil addresses (address.AddrForKey of yggdrasil-go)

pub fn addr_for_key(key: &[u8; 32]) -> Ipv6Addr {
    let buf: Vec<u8> = key.iter().map(|b| !b).collect();
    let mut temp = Vec::with_capacity(32);
    let (mut done, mut ones, mut bits, mut nbits) = (false, 0u8, 0u8, 0);
    for idx in 0..256 {
        let bit = (buf[idx / 8] >> (7 - idx % 8)) & 1;
        if !done && bit != 0 {
            ones += 1;
            continue;
        }
        if !done && bit == 0 {
            done = true;
            continue;
        }
        bits = (bits << 1) | bit;
        nbits += 1;
        if nbits == 8 {
            nbits = 0;
            temp.push(bits);
            bits = 0;
        }
    }
    let mut a = [0u8; 16];
    a[0] = 0x02;
    a[1] = ones;
    for (i, b) in temp.iter().take(14).enumerate() {
        a[2 + i] = *b;
    }
    Ipv6Addr::from(a)
}

fn parse_hex_key(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut k = [0u8; 32];
    for i in 0..32 {
        k[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(k)
}

// ------------------------------------------------------------------------------------------
// yggdrasil-go admin API

fn admin(sock: &str, request: &str, args: Value) -> Result<Value, String> {
    let path = sock.strip_prefix("unix://").unwrap_or(sock);
    let mut s = UnixStream::connect(path).map_err(|e| format!("admin {path}: {e}"))?;
    s.set_read_timeout(Some(Duration::from_secs(30))).ok();
    let req = json!({"request": request, "arguments": args, "keepalive": false});
    s.write_all(req.to_string().as_bytes()).map_err(|e| e.to_string())?;
    let mut de = serde_json::Deserializer::from_reader(BufReader::new(s)).into_iter::<Value>();
    let v = de.next().ok_or("admin: empty reply")?.map_err(|e| format!("admin: {e}"))?;
    if v["status"].as_str() == Some("success") {
        Ok(v["response"].clone())
    } else {
        Err(v["error"].as_str().unwrap_or("admin error").to_string())
    }
}

/// Keys to start the crawl from: our peers and the known tree.
fn seed_keys(sock: &str) -> Result<(String, Vec<String>), String> {
    let me = admin(sock, "getself", json!({}))?;
    let own = me["key"].as_str().unwrap_or("").to_string();
    let mut keys = Vec::new();
    if let Ok(p) = admin(sock, "getpeers", json!({})) {
        for e in p["peers"].as_array().into_iter().flatten() {
            if e["up"].as_bool() != Some(false) {
                if let Some(k) = e["key"].as_str().filter(|k| k.len() == 64) {
                    keys.push(k.to_string());
                }
            }
        }
    }
    if let Ok(t) = admin(sock, "gettree", json!({})) {
        for e in t["tree"].as_array().into_iter().flatten() {
            if let Some(k) = e["key"].as_str().filter(|k| k.len() == 64) {
                keys.push(k.to_string());
            }
        }
    }
    Ok((own, keys))
}

/// Breadth-first walk of the network with remote getPeers; returns every key seen.
fn crawl(cfg: &TrackerConfig) -> Result<Vec<String>, String> {
    let (own, seeds) = seed_keys(&cfg.admin_socket)?;
    let seen: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(seeds.iter().cloned().chain([own]).collect()));
    let queue: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(seeds.into_iter().collect()));
    let busy = Arc::new(Mutex::new(0usize));
    let t0 = Instant::now();
    let mut handles = Vec::new();
    for _ in 0..cfg.crawl_workers.max(1) {
        let (seen, queue, busy, sock, max) = (seen.clone(), queue.clone(), busy.clone(), cfg.admin_socket.clone(), cfg.max_nodes);
        handles.push(std::thread::spawn(move || loop {
            let next = {
                let mut q = queue.lock().unwrap();
                let k = q.pop_front();
                if k.is_some() {
                    *busy.lock().unwrap() += 1;
                }
                k
            };
            let Some(key) = next else {
                if *busy.lock().unwrap() == 0 {
                    return;
                }
                std::thread::sleep(Duration::from_millis(200));
                continue;
            };
            if let Ok(r) = admin(&sock, "debug_remotegetpeers", json!({"key": key})) {
                let found: Vec<String> = r
                    .as_object()
                    .into_iter()
                    .flat_map(|m| m.values())
                    .flat_map(|v| v["keys"].as_array().cloned().unwrap_or_default())
                    .filter_map(|k| k.as_str().map(String::from))
                    .filter(|k| k.len() == 64)
                    .collect();
                let mut s = seen.lock().unwrap();
                let mut q = queue.lock().unwrap();
                for k in found {
                    if s.len() < max && s.insert(k.clone()) {
                        q.push_back(k);
                    }
                }
            }
            *busy.lock().unwrap() -= 1;
        }));
    }
    for h in handles {
        let _ = h.join();
    }
    let keys: Vec<String> = seen.lock().unwrap().iter().cloned().collect();
    log(&format!("обход сети: {} узлов за {} с", keys.len(), t0.elapsed().as_secs()));
    Ok(keys)
}

// ------------------------------------------------------------------------------------------
// Probes (kernel TCP through the yggdrasil-go TUN)

/// A Ratatoskr exit: SOCKS5 on 27080 that lets us CONNECT to its speed-test port. Connect time (ms).
pub fn probe_exit(addr: Ipv6Addr) -> Option<u64> {
    let sa = SocketAddr::from((addr, config::PORT));
    let t0 = Instant::now();
    let mut s = TcpStream::connect_timeout(&sa, Duration::from_secs(5)).ok()?;
    let rtt = t0.elapsed().as_millis() as u64;
    s.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
    s.write_all(&[5, 1, 0]).ok()?;
    let mut b = [0u8; 10];
    s.read_exact(&mut b[..2]).ok()?;
    if b[..2] != [5, 0] {
        return None;
    }
    let p = config::SPEEDTEST.to_be_bytes();
    s.write_all(&[5, 1, 0, 1, 127, 0, 0, 1, p[0], p[1]]).ok()?;
    s.read_exact(&mut b).ok()?;
    (b[1] == 0).then_some(rtt)
}

fn http_get(addr: Ipv6Addr, port: u16, path: &str) -> Option<Value> {
    let mut s = TcpStream::connect_timeout(&SocketAddr::from((addr, port)), Duration::from_secs(5)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(15))).ok()?;
    s.write_all(format!("GET {path} HTTP/1.0\r\nHost: [{addr}]\r\n\r\n").as_bytes()).ok()?;
    let mut body = Vec::new();
    s.take(4 * 1024 * 1024).read_to_end(&mut body).ok()?;
    let text = String::from_utf8_lossy(&body);
    serde_json::from_str(text.split_once("\r\n\r\n")?.1).ok()
}

/// Another tracker: `/info` names the service.
fn probe_tracker(addr: Ipv6Addr) -> bool {
    http_get(addr, TRACKER_PORT, "/info").map(|v| v["service"].as_str() == Some("ratatoskr-tracker")).unwrap_or(false)
}

fn parallel<T: Send + 'static>(items: Vec<Ipv6Addr>, workers: usize, f: fn(Ipv6Addr) -> Option<T>) -> Vec<(Ipv6Addr, T)> {
    let q = Arc::new(Mutex::new(items));
    let out = Arc::new(Mutex::new(Vec::new()));
    let hs: Vec<_> = (0..workers.max(1))
        .map(|_| {
            let (q, out) = (q.clone(), out.clone());
            std::thread::spawn(move || loop {
                let Some(a) = q.lock().unwrap().pop() else { return };
                if let Some(r) = f(a) {
                    out.lock().unwrap().push((a, r));
                }
            })
        })
        .collect();
    for h in hs {
        let _ = h.join();
    }
    Arc::try_unwrap(out).ok().map(|m| m.into_inner().unwrap()).unwrap_or_default()
}

// ------------------------------------------------------------------------------------------
// Database

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
pub struct Entry {
    pub first_seen: u64,
    /// Last time our own probe confirmed it.
    pub last_ok: u64,
    pub last_check: u64,
    pub fails: u32,
    pub rtt_ms: Option<u64>,
    /// "crawl", "sync:<tracker>" or "default".
    pub source: String,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Db {
    pub exits: HashMap<String, Entry>,
    pub trackers: HashMap<String, Entry>,
    pub last_crawl: u64,
    pub crawl_nodes: usize,
}

impl Db {
    fn load() -> Db {
        std::fs::read_to_string(db_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    fn save(&self) {
        let p = db_path();
        let tmp = p.with_extension("json.tmp");
        if std::fs::write(&tmp, serde_json::to_vec(self).unwrap()).is_ok() {
            let _ = std::fs::rename(tmp, p);
        }
    }

    fn mark(map: &mut HashMap<String, Entry>, addr: &str, ok: Option<u64>, source: &str) {
        let t = now();
        let e = map.entry(addr.to_string()).or_insert_with(|| Entry { first_seen: t, source: source.to_string(), ..Default::default() });
        e.last_check = t;
        match ok {
            Some(rtt) => {
                e.last_ok = t;
                e.fails = 0;
                e.rtt_ms = Some(rtt);
            }
            None => e.fails += 1,
        }
    }
}

fn listed(e: &Entry, cfg: &TrackerConfig) -> bool {
    e.last_ok > 0 && now().saturating_sub(e.last_ok) < cfg.expire_hours * 3600
}

/// Probe these exit candidates and record the results.
fn check_exits(db: &Mutex<Db>, cfg: &TrackerConfig, cands: Vec<(Ipv6Addr, String)>) -> usize {
    let source: HashMap<Ipv6Addr, String> = cands.iter().cloned().collect();
    let addrs: Vec<Ipv6Addr> = source.keys().copied().collect();
    let ok = parallel(addrs.clone(), cfg.probe_workers, probe_exit);
    let okmap: HashMap<Ipv6Addr, u64> = ok.into_iter().collect();
    let mut d = db.lock().unwrap();
    for a in &addrs {
        let s = a.to_string();
        match okmap.get(a) {
            Some(rtt) => Db::mark(&mut d.exits, &s, Some(*rtt), &source[a]),
            // A crawl candidate that never answered is not worth remembering.
            None if d.exits.contains_key(&s) => Db::mark(&mut d.exits, &s, None, ""),
            None => {}
        }
    }
    // Forget long-dead entries.
    let exp = cfg.expire_hours * 3600;
    d.exits.retain(|_, e| now().saturating_sub(e.last_ok.max(e.first_seen)) < exp * 7);
    d.save();
    okmap.len()
}

fn check_trackers(db: &Mutex<Db>, cfg: &TrackerConfig, cands: Vec<(Ipv6Addr, String)>, me: Ipv6Addr) {
    let source: HashMap<Ipv6Addr, String> = cands.into_iter().filter(|(a, _)| *a != me).collect();
    let addrs: Vec<Ipv6Addr> = source.keys().copied().collect();
    let ok: HashSet<Ipv6Addr> = parallel(addrs.clone(), cfg.probe_workers, |a| probe_tracker(a).then_some(())).into_iter().map(|x| x.0).collect();
    let mut d = db.lock().unwrap();
    for a in &addrs {
        let s = a.to_string();
        if ok.contains(a) {
            Db::mark(&mut d.trackers, &s, Some(0), &source[a]);
        } else if d.trackers.contains_key(&s) {
            Db::mark(&mut d.trackers, &s, None, "");
        }
    }
    d.save();
}

// ------------------------------------------------------------------------------------------
// API

fn list_json(map: &HashMap<String, Entry>, cfg: &TrackerConfig, key: &str) -> Value {
    let mut v: Vec<(&String, &Entry)> = map.iter().filter(|(_, e)| listed(e, cfg)).collect();
    v.sort_by_key(|(_, e)| std::cmp::Reverse(e.last_ok));
    let items: Vec<Value> = v
        .into_iter()
        .map(|(a, e)| json!({"address": a, "first_seen": e.first_seen, "last_seen": e.last_ok, "rtt_ms": e.rtt_ms}))
        .collect();
    json!({"service": "ratatoskr-tracker", "port": config::PORT, key: items, "updated": now()})
}

fn serve(listener: TcpListener, db: Arc<Mutex<Db>>, cfg: TrackerConfig, me: Ipv6Addr) {
    for conn in listener.incoming().flatten() {
        let (db, cfg) = (db.clone(), cfg.clone());
        std::thread::spawn(move || {
            let mut c = conn;
            let _ = c.set_read_timeout(Some(Duration::from_secs(10)));
            let mut line = String::new();
            if BufReader::new(&c).take(4096).read_line(&mut line).is_err() {
                return;
            }
            let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
            let (status, body) = {
                let d = db.lock().unwrap();
                match path.as_str() {
                    "/exits" => ("200 OK", list_json(&d.exits, &cfg, "exits")),
                    "/trackers" => {
                        let mut v = list_json(&d.trackers, &cfg, "trackers");
                        // We are a tracker too.
                        if let Some(a) = v["trackers"].as_array_mut() {
                            a.insert(0, json!({"address": me.to_string(), "last_seen": now()}));
                        }
                        ("200 OK", v)
                    }
                    "/" | "/info" => (
                        "200 OK",
                        json!({"service": "ratatoskr-tracker", "version": env!("CARGO_PKG_VERSION"), "address": me.to_string(),
                               "proxy_port": config::PORT, "exits": d.exits.values().filter(|e| listed(e, &cfg)).count(),
                               "trackers": d.trackers.values().filter(|e| listed(e, &cfg)).count(),
                               "last_crawl": d.last_crawl, "crawl_nodes": d.crawl_nodes}),
                    ),
                    _ => ("404 Not Found", json!({"error": "not found"})),
                }
            };
            let body = body.to_string();
            let _ = write!(c, "HTTP/1.0 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        });
    }
}

// ------------------------------------------------------------------------------------------

pub fn main() -> Result<(), String> {
    let cfg: TrackerConfig = std::fs::read_to_string(tracker_config_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    if !tracker_config_path().exists() {
        if let Some(d) = tracker_config_path().parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(tracker_config_path(), serde_json::to_string_pretty(&cfg).unwrap() + "\n");
    }
    let _ = std::fs::create_dir_all(config::state_dir());
    let me_v = admin(&cfg.admin_socket, "getself", json!({}))
        .map_err(|e| format!("нет доступа к yggdrasil-go ({e}): нужен запущенный yggdrasil и права на его admin-сокет"))?;
    let me: Ipv6Addr = me_v["address"].as_str().and_then(|a| a.parse().ok()).ok_or("getself: нет адреса")?;
    log(&format!("трекер {} на [{me}]:{TRACKER_PORT}, выходы ищутся на порту {}", env!("CARGO_PKG_VERSION"), config::PORT));

    let listener = TcpListener::bind((me, TRACKER_PORT)).map_err(|e| format!("[{me}]:{TRACKER_PORT}: {e}"))?;
    let db = Arc::new(Mutex::new(Db::load()));
    {
        let (db, cfg) = (db.clone(), cfg.clone());
        std::thread::spawn(move || serve(listener, db, cfg, me));
    }

    // Seed trackers from the config (the default one first).
    let seeds: Vec<(Ipv6Addr, String)> = cfg.trackers.iter().filter_map(|t| t.parse().ok()).map(|a| (a, "default".to_string())).collect();
    check_trackers(&db, &cfg, seeds, me);

    let (mut last_crawl, mut last_recheck, mut last_sync) = (None::<Instant>, Instant::now(), None::<Instant>);
    loop {
        // Sync: other trackers' exits and trackers, verified by our own probes.
        if last_sync.map_or(true, |t| t.elapsed() >= Duration::from_secs(cfg.sync_interval_min * 60)) {
            last_sync = Some(Instant::now());
            let trackers: Vec<String> = {
                let d = db.lock().unwrap();
                d.trackers.iter().filter(|(_, e)| listed(e, &cfg)).map(|(a, _)| a.clone()).chain(cfg.trackers.iter().cloned()).collect::<HashSet<_>>().into_iter().collect()
            };
            let (mut ex, mut tr) = (Vec::new(), Vec::new());
            for t in trackers.iter().filter_map(|t| t.parse::<Ipv6Addr>().ok()).filter(|a| *a != me) {
                let src = format!("sync:{t}");
                if let Some(v) = http_get(t, TRACKER_PORT, "/exits") {
                    ex.extend(v["exits"].as_array().into_iter().flatten().filter_map(|e| e["address"].as_str()?.parse().ok()).map(|a| (a, src.clone())));
                }
                if let Some(v) = http_get(t, TRACKER_PORT, "/trackers") {
                    tr.extend(v["trackers"].as_array().into_iter().flatten().filter_map(|e| e["address"].as_str()?.parse().ok()).map(|a| (a, src.clone())));
                }
            }
            if !ex.is_empty() || !tr.is_empty() {
                let n = check_exits(&db, &cfg, ex);
                check_trackers(&db, &cfg, tr, me);
                log(&format!("синхронизация с {} трекерами: подтверждено выходов {n}", trackers.len()));
            }
        }
        // Crawl: every node of the network, probed for the exit and tracker ports.
        if last_crawl.map_or(true, |t| t.elapsed() >= Duration::from_secs(cfg.crawl_interval_min * 60)) {
            last_crawl = Some(Instant::now());
            match crawl(&cfg) {
                Ok(keys) => {
                    let addrs: Vec<Ipv6Addr> = keys.iter().filter_map(|k| parse_hex_key(k)).map(|k| addr_for_key(&k)).filter(|a| *a != me).collect();
                    {
                        let mut d = db.lock().unwrap();
                        d.last_crawl = now();
                        d.crawl_nodes = addrs.len();
                    }
                    let t0 = Instant::now();
                    let n = check_exits(&db, &cfg, addrs.iter().map(|a| (*a, "crawl".to_string())).collect());
                    check_trackers(&db, &cfg, addrs.iter().map(|a| (*a, "crawl".to_string())).collect(), me);
                    log(&format!("проверено {} узлов за {} с: выходов {n}, трекеров {}", addrs.len(), t0.elapsed().as_secs(),
                        db.lock().unwrap().trackers.values().filter(|e| listed(e, &cfg)).count()));
                }
                Err(e) => log(&format!("обход не удался: {e}")),
            }
        }
        // Recheck known exits so the list stays fresh between crawls.
        if last_recheck.elapsed() >= Duration::from_secs(cfg.recheck_interval_min * 60) {
            last_recheck = Instant::now();
            let known: Vec<(Ipv6Addr, String)> = db.lock().unwrap().exits.keys().filter_map(|a| a.parse().ok()).map(|a| (a, "recheck".to_string())).collect();
            let n = check_exits(&db, &cfg, known);
            log(&format!("перепроверка: живых выходов {n}"));
        }
        std::thread::sleep(Duration::from_secs(10));
    }
}

/// `yggproxy exits [TRACKER]`: the list from a tracker (directly through a yggdrasil-go TUN, or
/// through the yggproxy service's tcp_node SOCKS).
pub fn fetch(tracker: Ipv6Addr, path: &str) -> Result<Value, String> {
    if let Some(v) = http_get(tracker, TRACKER_PORT, path) {
        return Ok(v);
    }
    // Through tcp_node --socks (the yggproxy service).
    let mut s = TcpStream::connect(("127.0.0.1", config::NODE_SOCKS)).map_err(|_| "трекер недоступен: нет yggdrasil-go и не запущена служба yggproxy".to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(20))).ok();
    let mut b = [0u8; 22];
    s.write_all(&[5, 1, 0]).map_err(|e| e.to_string())?;
    s.read_exact(&mut b[..2]).map_err(|e| e.to_string())?;
    let mut req = vec![5, 1, 0, 4];
    req.extend_from_slice(&tracker.octets());
    req.extend_from_slice(&TRACKER_PORT.to_be_bytes());
    s.write_all(&req).map_err(|e| e.to_string())?;
    s.read_exact(&mut b).map_err(|e| e.to_string())?;
    if b[1] != 0 {
        return Err(format!("SOCKS: отказ {}", b[1]));
    }
    s.write_all(format!("GET {path} HTTP/1.0\r\nHost: [{tracker}]\r\n\r\n").as_bytes()).map_err(|e| e.to_string())?;
    let mut body = Vec::new();
    s.take(4 * 1024 * 1024).read_to_end(&mut body).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&body);
    serde_json::from_str(text.split_once("\r\n\r\n").ok_or("пустой ответ трекера")?.1).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_of_key_matches_yggdrasil_go() {
        // getself of the host's yggdrasil-go 0.5.14
        let k = parse_hex_key("82065cbf6f313abbffd83447898d37c1aa2eb4be82e0e471b2c2c8953ba33724").unwrap();
        assert_eq!(addr_for_key(&k), "200:fbf3:4681:219d:8a88:4f:9770:ece5".parse::<Ipv6Addr>().unwrap());
        let k = parse_hex_key("00000000000193f3eeb75460f85b1ba9b6f1718626aed6f8ef373ff78d5eb75c").unwrap();
        assert_eq!(addr_for_key(&k), "22f:6c0c:1148:ab9f:7a4:e456:490e:8e79".parse::<Ipv6Addr>().unwrap());
    }
}
