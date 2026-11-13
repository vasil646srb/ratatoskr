//! `yggproxy tracker`: finds Ratatoskr exit nodes (and other trackers) in Yggdrasil and serves
//! the list to clients.
//!
//! Runs its own Yggdrasil node (yggdrasil-ng as a library, see `ygg_net`): no yggdrasil-go, TUN or
//! root needed, and it coexists with any other Yggdrasil node on the host.
//! - crawl: from our peers and tree, the remote getPeers request on every node, breadth first, so
//!   the public peers and everything connected to them get visited;
//! - probe: TCP (our own stack) to `[addr]:27080`, a SOCKS5 greeting and a CONNECT to the exit's
//!   speed-test port (only Ratatoskr exits allow that) — an exit; `[addr]:27081 /info` — a tracker;
//! - sync: other trackers' lists are fetched and every entry is probed again before it is listed;
//! - public peers: the public peer list (plus our own working peers), checked from here, served as
//!   `GET /peers` so clients can find peers even where the list's website is blocked;
//! - API (HTTP on our Yggdrasil address, port 27081): `GET /exits`, `GET /trackers`, `GET /peers`,
//!   `GET /info`.

use crate::config;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use crate::tracker_client::{with_defaults, DEFAULT_TRACKERS, TRACKER_PORT};
use crate::ygg_net::Net;
use std::net::{Ipv6Addr, SocketAddr, TcpStream};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct TrackerConfig {
    /// Peers of the tracker's own node; empty = the fastest checked public peers.
    pub node_peers: Vec<String>,
    /// How many of the fastest public peers the node takes when `node_peers` is empty.
    pub node_peer_count: usize,
    pub crawl_interval_min: u64,
    pub recheck_interval_min: u64,
    pub sync_interval_min: u64,
    /// Exits not confirmed for this long are dropped from the list.
    pub expire_hours: u64,
    /// Hide an entry after this many failed checks in a row (0 = only `expire_hours`).
    pub hide_after_fails: u32,
    pub crawl_workers: usize,
    pub probe_workers: usize,
    pub max_nodes: usize,
    /// How often the public peer list is downloaded and checked.
    pub peers_interval_min: u64,
    pub public_peers_url: String,
    pub trackers: Vec<String>,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self {
            node_peers: Vec::new(),
            node_peer_count: 6,
            crawl_interval_min: 30,
            recheck_interval_min: 5,
            sync_interval_min: 5,
            expire_hours: 24,
            hide_after_fails: 2,
            crawl_workers: 24,
            probe_workers: 64,
            max_nodes: 50_000,
            peers_interval_min: 60,
            public_peers_url: "https://publicpeers.neilalexander.dev/publicnodes.json".into(),
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
    // Windows service: no journal, so a log file next to the database.
    #[cfg(windows)]
    {
        use std::io::Write as _;
        let p = config::state_dir().join("tracker.log");
        if std::fs::metadata(&p).map(|m| m.len() > 5 << 20).unwrap_or(false) {
            let _ = std::fs::rename(&p, p.with_extension("log.old"));
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
            let _ = writeln!(f, "{} {msg}", crate::platform::now_hms());
        }
    }
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
// The tracker's own node

static NET: OnceLock<Arc<Net>> = OnceLock::new();

fn net() -> &'static Arc<Net> {
    NET.get().expect("tracker node not started")
}

/// yggdrasil-ng (c3a3aa6) panics on `tls://[IPv6]:port` (the SNI fallback cannot parse the
/// bracketed host), so such peers are not given to the tracker's node.
fn node_can_use(uri: &str) -> bool {
    !(uri.starts_with("tls://[") || uri.starts_with("quic://["))
}

fn key_hex(k: &[u8; 32]) -> String {
    k.iter().map(|b| format!("{b:02x}")).collect()
}

/// Breadth-first walk of the network with remote getPeers; returns every key seen.
fn crawl(cfg: &TrackerConfig) -> Result<Vec<String>, String> {
    let seeds: Vec<String> = net().seed_keys().iter().map(key_hex).collect();
    if seeds.is_empty() {
        return Err("у узла трекера нет пиров".into());
    }
    let own = net().public_key_hex();
    let seen: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(seeds.iter().cloned().chain([own]).collect()));
    let queue: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(seeds.into_iter().collect()));
    let busy = Arc::new(Mutex::new(0usize));
    let t0 = Instant::now();
    let mut handles = Vec::new();
    for _ in 0..cfg.crawl_workers.max(1) {
        let (seen, queue, busy, max) = (seen.clone(), queue.clone(), busy.clone(), cfg.max_nodes);
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
            if let Some(k) = parse_hex_key(&key)
                && let Ok(found) = net().remote_peers(k) {
                    let mut s = seen.lock().unwrap();
                    let mut q = queue.lock().unwrap();
                    for k in found.iter().map(key_hex) {
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
// Probes (TCP over the tracker's own node)

/// Connect timeout for nodes we already know (sync, recheck): the first connection waits for
/// path discovery.
const KNOWN_TIMEOUT: Duration = Duration::from_secs(10);
/// Connect timeout for crawl candidates: thousands of nodes, most of them without our ports.
const CRAWL_TIMEOUT: Duration = Duration::from_secs(6);

/// A Ratatoskr exit: SOCKS5 on 27080 that lets us CONNECT to its speed-test port. Connect time (ms).
fn probe_exit(addr: Ipv6Addr, timeout: Duration) -> Option<u64> {
    let t0 = Instant::now();
    let mut s = net().connect(addr, config::PORT, timeout).ok()?;
    let rtt = t0.elapsed().as_millis() as u64;
    s.set_read_timeout(Some(Duration::from_secs(10)));
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
    http_get_within(addr, port, path, KNOWN_TIMEOUT)
}

fn http_get_within(addr: Ipv6Addr, port: u16, path: &str, timeout: Duration) -> Option<Value> {
    let mut s = net().connect(addr, port, timeout).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(15)));
    s.write_all(format!("GET {path} HTTP/1.0\r\nHost: [{addr}]\r\n\r\n").as_bytes()).ok()?;
    let mut body = Vec::new();
    s.take(4 * 1024 * 1024).read_to_end(&mut body).ok()?;
    let text = String::from_utf8_lossy(&body);
    serde_json::from_str(text.split_once("\r\n\r\n")?.1).ok()
}

/// Another tracker: `/info` names the service.
fn probe_tracker(addr: Ipv6Addr, timeout: Duration) -> bool {
    http_get_within(addr, TRACKER_PORT, "/info", timeout).map(|v| v["service"].as_str() == Some("ratatoskr-tracker")).unwrap_or(false)
}

/// Run `f` over `items` on up to `workers` threads; the items it returned something for.
fn parallel<T, F>(items: Vec<Ipv6Addr>, workers: usize, f: F) -> Vec<(Ipv6Addr, T)>
where
    T: Send + 'static,
    F: Fn(Ipv6Addr) -> Option<T> + Send + Sync + 'static,
{
    let workers = workers.max(1).min(items.len().max(1));
    let q = Arc::new(Mutex::new(items));
    let out = Arc::new(Mutex::new(Vec::new()));
    let f = Arc::new(f);
    let hs: Vec<_> = (0..workers)
        .map(|_| {
            let (q, out, f) = (q.clone(), out.clone(), f.clone());
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
    /// Public peers checked from this tracker: {uri, key, region, rtt_ms, source}.
    pub peers: Vec<Value>,
    pub peers_updated: u64,
    /// Clients' anonymous peer summaries sent to this tracker (`POST /report`), decayed.
    pub peer_reports: HashMap<String, crate::peerstats::Agg>,
    /// Other trackers' own aggregates (`GET /peerstats`), by tracker address.
    pub remote_stats: HashMap<String, Vec<crate::peerstats::Agg>>,
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
    e.last_ok > 0
        && now().saturating_sub(e.last_ok) < cfg.expire_hours * 3600
        && (cfg.hide_after_fails == 0 || e.fails < cfg.hide_after_fails)
}

/// Record exit probe results: `ok` answered (with the connect time), the rest of `source` did not.
fn record_exits(db: &Mutex<Db>, cfg: &TrackerConfig, source: &HashMap<Ipv6Addr, String>, ok: &HashMap<Ipv6Addr, u64>) -> usize {
    let mut d = db.lock().unwrap();
    for (a, src) in source {
        let s = a.to_string();
        match ok.get(a) {
            Some(rtt) => Db::mark(&mut d.exits, &s, Some(*rtt), src),
            // A crawl candidate that never answered is not worth remembering.
            None if d.exits.contains_key(&s) => Db::mark(&mut d.exits, &s, None, ""),
            None => {}
        }
    }
    // Forget long-dead entries.
    let exp = cfg.expire_hours * 3600;
    d.exits.retain(|_, e| now().saturating_sub(e.last_ok.max(e.first_seen)) < exp * 7);
    d.save();
    ok.len()
}

fn record_trackers(db: &Mutex<Db>, source: &HashMap<Ipv6Addr, String>, ok: &HashSet<Ipv6Addr>) {
    let mut d = db.lock().unwrap();
    for (a, src) in source {
        let s = a.to_string();
        if ok.contains(a) {
            Db::mark(&mut d.trackers, &s, Some(0), src);
        } else if d.trackers.contains_key(&s) {
            Db::mark(&mut d.trackers, &s, None, "");
        }
    }
    d.save();
}

/// Probe these exit candidates and record the results.
fn check_exits(db: &Mutex<Db>, cfg: &TrackerConfig, cands: Vec<(Ipv6Addr, String)>) -> usize {
    let source: HashMap<Ipv6Addr, String> = cands.into_iter().collect();
    let ok: HashMap<Ipv6Addr, u64> = parallel(source.keys().copied().collect(), cfg.probe_workers, |a| probe_exit(a, KNOWN_TIMEOUT)).into_iter().collect();
    record_exits(db, cfg, &source, &ok)
}

fn check_trackers(db: &Mutex<Db>, cfg: &TrackerConfig, cands: Vec<(Ipv6Addr, String)>, me: Ipv6Addr) {
    let source: HashMap<Ipv6Addr, String> = cands.into_iter().filter(|(a, _)| *a != me).collect();
    let ok: HashSet<Ipv6Addr> =
        parallel(source.keys().copied().collect(), cfg.probe_workers, |a| probe_tracker(a, KNOWN_TIMEOUT).then_some(())).into_iter().map(|x| x.0).collect();
    record_trackers(db, &source, &ok);
}

/// The crawl's nodes: both ports of every node in one pass. A node's two probes run together,
/// so the pass takes as long as the slower probe, not their sum (two passes over ~9000 nodes
/// took longer than the crawl interval).
fn check_crawled(db: &Mutex<Db>, cfg: &TrackerConfig, addrs: &[Ipv6Addr], me: Ipv6Addr) -> usize {
    let source: HashMap<Ipv6Addr, String> = addrs.iter().filter(|a| **a != me).map(|a| (*a, "crawl".to_string())).collect();
    let found = parallel(source.keys().copied().collect(), cfg.probe_workers, |a| {
        std::thread::scope(|s| {
            let tracker = s.spawn(move || probe_tracker(a, CRAWL_TIMEOUT));
            let exit = probe_exit(a, CRAWL_TIMEOUT);
            let tracker = tracker.join().unwrap_or(false);
            (exit.is_some() || tracker).then_some((exit, tracker))
        })
    });
    let exits: HashMap<Ipv6Addr, u64> = found.iter().filter_map(|(a, (e, _))| e.map(|rtt| (*a, rtt))).collect();
    let trackers: HashSet<Ipv6Addr> = found.iter().filter(|(_, (_, t))| *t).map(|x| x.0).collect();
    record_trackers(db, &source, &trackers);
    record_exits(db, cfg, &source, &exits)
}

// ------------------------------------------------------------------------------------------
// Public peers

/// `tcp://`/`tls://` peers of the public list: (uri with ?key=, key, region).
fn parse_public_list(text: &str) -> Vec<(String, String, String)> {
    let Ok(v) = serde_json::from_str::<Value>(text) else { return Vec::new() };
    let mut out = Vec::new();
    for (region, peers) in v.as_object().into_iter().flatten() {
        for (uri, info) in peers.as_object().into_iter().flatten() {
            if info["up"].as_bool() == Some(false) || !(uri.starts_with("tcp://") || uri.starts_with("tls://")) {
                continue;
            }
            let key = info["key"].as_str().filter(|k| k.len() == 64).unwrap_or("").to_string();
            let full = if !key.is_empty() && !uri.contains('?') { format!("{uri}?key={key}") } else { uri.clone() };
            out.push((full, key, region.trim_end_matches(".md").to_string()));
        }
    }
    out
}

fn download(url: &str) -> Option<String> {
    match crate::platform::download_text(url) {
        Ok(t) if t.trim_start().starts_with('{') => Some(t),
        Ok(_) => {
            log(&format!("{url}: ответ не JSON"));
            None
        }
        Err(e) => {
            log(&format!("{url}: {e}"));
            None
        }
    }
}

/// TCP connect time from here (ms), as a liveness check of a peer URI.
fn probe_uri(uri: &str) -> Option<u64> {
    let rest = uri.split("://").nth(1)?.split('?').next()?;
    let t0 = Instant::now();
    let addrs: Vec<SocketAddr> = std::net::ToSocketAddrs::to_socket_addrs(rest).ok()?.collect();
    let a = addrs.first()?;
    TcpStream::connect_timeout(a, Duration::from_secs(3)).ok()?;
    Some(t0.elapsed().as_millis() as u64)
}

/// Download the public list (or take other trackers' lists), add our own working peers, check
/// every peer from here and keep the ones that answer.
fn refresh_peers(db: &Mutex<Db>, cfg: &TrackerConfig, others: &[Ipv6Addr]) {
    let mut cands: Vec<(String, String, String, String)> = Vec::new(); // uri, key, region, source
    match download(&cfg.public_peers_url) {
        Some(t) => cands.extend(parse_public_list(&t).into_iter().map(|(u, k, r)| (u, k, r, "public".into()))),
        None => {
            log("публичный список пиров не скачивается — беру у других трекеров");
            for t in others {
                if let Some(v) = http_get(*t, TRACKER_PORT, "/peers") {
                    for p in v["peers"].as_array().into_iter().flatten() {
                        if let Some(u) = p["uri"].as_str() {
                            cands.push((u.into(), p["key"].as_str().unwrap_or("").into(), p["region"].as_str().unwrap_or("").into(), format!("sync:{t}")));
                        }
                    }
                }
            }
        }
    }
    // Our own node's peers that are up work for sure.
    if let Some(n) = NET.get() {
        for p in n.peers().into_iter().filter(|p| p.up && !p.inbound) {
            let k = key_hex(&p.key);
            if p.uri.starts_with("tcp://") || p.uri.starts_with("tls://") {
                let full = if p.uri.contains('?') { p.uri.clone() } else { format!("{}?key={k}", p.uri) };
                cands.push((full, k, String::new(), "tracker".into()));
            }
        }
    }
    let mut seen = HashSet::new();
    cands.retain(|c| seen.insert(c.0.clone()));
    if cands.is_empty() {
        log("пиров для проверки нет");
        return;
    }
    let q = Arc::new(Mutex::new(cands));
    let out: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let hs: Vec<_> = (0..cfg.probe_workers.max(1))
        .map(|_| {
            let (q, out) = (q.clone(), out.clone());
            std::thread::spawn(move || loop {
                let Some((uri, key, region, source)) = q.lock().unwrap().pop() else { return };
                if let Some(rtt) = probe_uri(&uri) {
                    out.lock().unwrap().push(json!({"uri": uri, "key": key, "region": region, "rtt_ms": rtt, "source": source}));
                }
            })
        })
        .collect();
    for h in hs {
        let _ = h.join();
    }
    let mut peers = std::mem::take(&mut *out.lock().unwrap());
    peers.sort_by_key(|p| p["rtt_ms"].as_u64().unwrap_or(u64::MAX));
    let n = peers.len();
    let mut d = db.lock().unwrap();
    if n > 0 {
        d.peers = peers;
        d.peers_updated = now();
        d.save();
    }
    log(&format!("публичные пиры: отвечают {n}"));
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

fn serve(listener: crate::ygg_net::NetListener, db: Arc<Mutex<Db>>, cfg: TrackerConfig, me: Ipv6Addr) {
    while let Some(conn) = listener.accept() {
        let (db, cfg) = (db.clone(), cfg.clone());
        std::thread::spawn(move || {
            let mut c = conn;
            c.set_read_timeout(Some(Duration::from_secs(10)));
            let remote = c.remote;
            // Request line, headers and (POST) the body.
            let mut line = String::new();
            let mut body_in = Vec::new();
            {
                let mut r = BufReader::new(&mut c);
                if (&mut r).take(4096).read_line(&mut line).is_err() {
                    return;
                }
                let mut len = 0usize;
                let mut seen = 0usize;
                loop {
                    let mut h = String::new();
                    match (&mut r).take(4096).read_line(&mut h) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => seen += n,
                    }
                    if h.trim().is_empty() || seen > 16 * 1024 {
                        break;
                    }
                    if let Some((k, v)) = h.split_once(':')
                        && k.trim().eq_ignore_ascii_case("content-length") {
                            len = v.trim().parse().unwrap_or(0);
                        }
                }
                if len > 0 && len <= 256 * 1024 {
                    body_in.resize(len, 0);
                    if r.read_exact(&mut body_in).is_err() {
                        body_in.clear();
                    }
                }
            }
            let method = line.split_whitespace().next().unwrap_or("GET").to_string();
            let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
            if method == "POST" && path == "/report" {
                let (status, body) = take_report(&db, remote, &body_in);
                let body = body.to_string();
                let _ = write!(c, "HTTP/1.0 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                return;
            }
            let (status, body) = {
                let d = db.lock().unwrap();
                match path.as_str() {
                    // Our own aggregates only: what other trackers merge (each report counted once).
                    "/peerstats" => ("200 OK", json!({"service": "ratatoskr-tracker", "stats": d.peer_reports.values().collect::<Vec<_>>(), "updated": now()})),
                    "/exits" => ("200 OK", list_json(&d.exits, &cfg, "exits")),
                    "/trackers" => {
                        let mut v = list_json(&d.trackers, &cfg, "trackers");
                        // We are a tracker too.
                        if let Some(a) = v["trackers"].as_array_mut() {
                            a.insert(0, json!({"address": me.to_string(), "last_seen": now()}));
                        }
                        ("200 OK", v)
                    }
                    "/peers" => {
                        // Checked peers, best first by what clients report (merged across trackers).
                        let stats = crate::peerstats::merged(&d.peer_reports, &d.remote_stats);
                        let ranked = crate::peerstats::rank_checked_peers(&d.peers, &stats);
                        (
                            "200 OK",
                            json!({"service": "ratatoskr-tracker", "peers": ranked, "updated": d.peers_updated,
                                   "note": "rtt_ms: TCP connect time from this tracker; client_ping_ms/speed_kbps/fail_rate: clients' reports; sorted by score"}),
                        )
                    }
                    "/" | "/info" => (
                        "200 OK",
                        json!({"service": "ratatoskr-tracker", "version": env!("CARGO_PKG_VERSION"), "address": me.to_string(),
                               "proxy_port": config::PORT, "exits": d.exits.values().filter(|e| listed(e, &cfg)).count(),
                               "trackers": d.trackers.values().filter(|e| listed(e, &cfg)).count(),
                               "peers": d.peers.len(),
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

/// `POST /report`: a client's anonymous peer summary. At most one per client address every
/// 10 minutes; the address itself is not stored.
fn take_report(db: &Mutex<Db>, remote: Option<Ipv6Addr>, body: &[u8]) -> (&'static str, Value) {
    static LAST: OnceLock<Mutex<HashMap<Ipv6Addr, Instant>>> = OnceLock::new();
    let last = LAST.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(a) = remote {
        let mut l = last.lock().unwrap();
        l.retain(|_, t| t.elapsed() < Duration::from_secs(600));
        if l.contains_key(&a) {
            return ("429 Too Many Requests", json!({"error": "one report per 10 minutes"}));
        }
        l.insert(a, Instant::now());
    }
    let Ok(v) = serde_json::from_slice::<Value>(body) else { return ("400 Bad Request", json!({"error": "JSON expected"})) };
    let mut d = db.lock().unwrap();
    let n = crate::peerstats::ingest(&mut d.peer_reports, &v);
    // Bounded: the 5000 most reported peers.
    if d.peer_reports.len() > 5000 {
        let mut v: Vec<(String, f64)> = d.peer_reports.iter().map(|(k, a)| (k.clone(), a.reports)).collect();
        v.sort_by(|a, b| b.1.total_cmp(&a.1));
        let keep: HashSet<String> = v.into_iter().take(5000).map(|x| x.0).collect();
        d.peer_reports.retain(|k, _| keep.contains(k));
    }
    d.save();
    ("200 OK", json!({"ok": true, "peers": n}))
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
    let db = Arc::new(Mutex::new(Db::load()));

    // The tracker's own node: configured peers, else the fastest checked public peers (checked
    // right now when there are none yet).
    let mut peers = cfg.node_peers.clone();
    if peers.is_empty() {
        if db.lock().unwrap().peers.is_empty() {
            log("проверяю публичные пиры для узла трекера…");
            refresh_peers(&db, &cfg, &[]);
        }
        peers = db.lock().unwrap().peers.iter().filter_map(|p| p["uri"].as_str().map(String::from)).filter(|u| node_can_use(u)).take(cfg.node_peer_count.max(1)).collect();
    }
    if peers.is_empty() {
        return Err("нет пиров для узла трекера: задайте node_peers в tracker.json или проверьте доступ в интернет".into());
    }
    let n = Net::start(&config::state_dir().join("tracker-node.key"), peers.clone())?;
    let me = n.address();
    let _ = NET.set(n);
    log(&format!("трекер {} на [{me}]:{TRACKER_PORT} (свой узел yggdrasil-ng, пиров {}), выходы ищутся на порту {}", env!("CARGO_PKG_VERSION"), peers.len(), config::PORT));
    // The first crawl needs peers that are up.
    for _ in 0..60 {
        if net().peers().iter().any(|p| p.up) {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    log(&format!("пиров на связи: {}", net().peers().iter().filter(|p| p.up).count()));

    let listener = net().listen(TRACKER_PORT).map_err(|e| format!("[{me}]:{TRACKER_PORT}: {e}"))?;
    {
        let (db, cfg) = (db.clone(), cfg.clone());
        std::thread::spawn(move || serve(listener, db, cfg, me));
    }

    // Seed trackers from the config (the default one first).
    let seeds: Vec<(Ipv6Addr, String)> = with_defaults(&cfg.trackers).iter().filter_map(|t| t.parse().ok()).map(|a| (a, "default".to_string())).collect();
    check_trackers(&db, &cfg, seeds, me);

    let (mut last_crawl, mut last_recheck, mut last_sync) = (None::<Instant>, Instant::now(), None::<Instant>);
    let mut last_peers = None::<Instant>;
    loop {
        // Public peers: downloaded and checked from here (from other trackers if blocked).
        if last_peers.is_none_or(|t| t.elapsed() >= Duration::from_secs(cfg.peers_interval_min * 60)) {
            last_peers = Some(Instant::now());
            let others: Vec<Ipv6Addr> = {
                let d = db.lock().unwrap();
                d.trackers.iter().filter(|(_, e)| listed(e, &cfg)).filter_map(|(a, _)| a.parse().ok()).chain(cfg.trackers.iter().filter_map(|t| t.parse().ok())).filter(|a| *a != me).collect()
            };
            refresh_peers(&db, &cfg, &others);
        }
        // Sync: other trackers' exits and trackers, verified by our own probes.
        if last_sync.is_none_or(|t| t.elapsed() >= Duration::from_secs(cfg.sync_interval_min * 60)) {
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
                // Their own peer statistics (they count each client report once, so do we).
                if let Some(v) = http_get(t, TRACKER_PORT, "/peerstats") {
                    let stats: Vec<crate::peerstats::Agg> = v["stats"].as_array().into_iter().flatten().take(5000).filter_map(|a| serde_json::from_value(a.clone()).ok()).collect();
                    db.lock().unwrap().remote_stats.insert(t.to_string(), stats);
                }
            }
            if !ex.is_empty() || !tr.is_empty() {
                let n = check_exits(&db, &cfg, ex);
                check_trackers(&db, &cfg, tr, me);
                log(&format!("синхронизация с {} трекерами: подтверждено выходов {n}", trackers.len()));
            }
        }
        // Crawl: every node of the network, probed for the exit and tracker ports.
        if last_crawl.is_none_or(|t| t.elapsed() >= Duration::from_secs(cfg.crawl_interval_min * 60)) {
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
                    let n = check_crawled(&db, &cfg, &addrs, me);
                    log(&format!("проверено {} узлов за {} с: выходов {n}, трекеров {}", addrs.len(), t0.elapsed().as_secs(),
                        db.lock().unwrap().trackers.values().filter(|e| listed(e, &cfg)).count()));
                }
                Err(e) => log(&format!("обход не удался: {e}")),
            }
        }
        // Recheck known exits and trackers so the list stays fresh between crawls.
        if last_recheck.elapsed() >= Duration::from_secs(cfg.recheck_interval_min * 60) {
            last_recheck = Instant::now();
            let known: Vec<(Ipv6Addr, String)> = db.lock().unwrap().exits.keys().filter_map(|a| a.parse().ok()).map(|a| (a, "recheck".to_string())).collect();
            let n = check_exits(&db, &cfg, known);
            let known_tr: Vec<(Ipv6Addr, String)> = db.lock().unwrap().trackers.keys().filter_map(|a| a.parse().ok()).map(|a| (a, "recheck".to_string())).collect();
            check_trackers(&db, &cfg, known_tr, me);
            let tr = db.lock().unwrap().trackers.values().filter(|e| listed(e, &cfg)).count();
            log(&format!("перепроверка: живых выходов {n}, трекеров {tr}"));
        }
        std::thread::sleep(Duration::from_secs(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_after_failed_checks_and_back_on_success() {
        let cfg = TrackerConfig::default();
        let mut m = HashMap::new();
        Db::mark(&mut m, "200::1", Some(10), "t");
        assert!(listed(&m["200::1"], &cfg));
        Db::mark(&mut m, "200::1", None, "");
        assert!(listed(&m["200::1"], &cfg), "one miss is tolerated");
        Db::mark(&mut m, "200::1", None, "");
        assert!(!listed(&m["200::1"], &cfg));
        Db::mark(&mut m, "200::1", Some(12), "");
        assert!(listed(&m["200::1"], &cfg));
    }

    #[test]
    fn address_of_key_matches_yggdrasil_go() {
        // getself of the host's yggdrasil-go 0.5.14
        let k = parse_hex_key("82065cbf6f313abbffd83447898d37c1aa2eb4be82e0e471b2c2c8953ba33724").unwrap();
        assert_eq!(addr_for_key(&k), "200:fbf3:4681:219d:8a88:4f:9770:ece5".parse::<Ipv6Addr>().unwrap());
        let k = parse_hex_key("00000000000193f3eeb75460f85b1ba9b6f1718626aed6f8ef373ff78d5eb75c").unwrap();
        assert_eq!(addr_for_key(&k), "22f:6c0c:1148:ab9f:7a4:e456:490e:8e79".parse::<Ipv6Addr>().unwrap());
    }
}
