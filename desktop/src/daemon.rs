//! `yggproxy daemon`: runs as root (systemd). Keeps tcp_node, the exit (ygghelper server) and the
//! VPN (TUN + ygghelper tun + policy routing) in line with the config, and answers the control
//! socket used by the console commands and the GUI.

use crate::config::{self, Config};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

const TCP_NODE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tcp_node"));
const YGGHELPER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ygghelper"));
const PUBLIC_PEERS_URL: &str = "https://publicpeers.neilalexander.dev/publicnodes.json";
const PEERS_MAX_AGE: Duration = Duration::from_secs(6 * 3600);
const MAX_LOG: usize = 3000;

// ------------------------------------------------------------------------------------------
// Shared state

#[derive(Default)]
pub struct St {
    logs: VecDeque<(u64, String)>,
    log_seq: u64,
    node_running: bool,
    address: Option<String>,
    public_key: Option<String>,
    lan_status: Option<String>,
    server_rtt: Option<String>,
    last_swap: Option<String>,
    exit_active: bool,
    srv: [u64; 5], // active total denied tx rx
    clients: HashMap<String, (u64, u64, u64, Instant)>,
    vpn_active: bool,
    tun_tx: u64,
    tun_rx: u64,
    last_error: Option<String>,
    node_stats: Option<Value>,
    node_peers: Option<Value>,
    /// tcp_node /api/rtt: ping to saved and tracker servers, and to the root.
    node_rtt: Option<Value>,
    /// Exits from the trackers: (address, rtt the tracker measured).
    tracker_exits: Vec<(String, Option<u64>)>,
    tracker_status: Option<String>,
}

type Shared = Arc<Mutex<St>>;

fn now_hms() -> String {
    let t = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_secs();
    // Local time via libc (the daemon has no tz crate).
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let tt = t as libc::time_t;
    unsafe { libc::localtime_r(&tt, &mut tm) };
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

fn log(st: &Shared, tag: &str, msg: &str) {
    let line = format!("{} [{tag}] {msg}", now_hms());
    eprintln!("{line}");
    let mut s = st.lock().unwrap();
    s.log_seq += 1;
    let seq = s.log_seq;
    s.logs.push_back((seq, line));
    while s.logs.len() > MAX_LOG {
        s.logs.pop_front();
    }
}

fn error(st: &Shared, tag: &str, msg: &str) {
    st.lock().unwrap().last_error = Some(format!("[{tag}] {msg}"));
    log(st, tag, &format!("ОШИБКА: {msg}"));
}

// ------------------------------------------------------------------------------------------
// Managed processes (restart with backoff, output lines to a handler)

type LineFn = Arc<dyn Fn(&str) + Send + Sync>;

struct Proc {
    cmd: Vec<String>,
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
}

impl Proc {
    fn start(st: &Shared, tag: &'static str, cmd: Vec<String>, on_line: LineFn) -> Proc {
        let stop = Arc::new(AtomicBool::new(false));
        let child: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
        let (st2, cmd2, stop2, child2) = (st.clone(), cmd.clone(), stop.clone(), child.clone());
        std::thread::spawn(move || {
            let mut backoff = 1u64;
            while !stop2.load(Ordering::Relaxed) {
                let short: Vec<&str> = cmd2.iter().map(|a| a.rsplit('/').next().unwrap_or(a)).collect();
                match Command::new(&cmd2[0]).args(&cmd2[1..]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
                    Ok(mut c) => {
                        log(&st2, tag, &format!("запущен: {}", short.join(" ")));
                        let started = Instant::now();
                        let out = c.stdout.take();
                        let err = c.stderr.take();
                        *child2.lock().unwrap() = Some(c);
                        let f2 = on_line.clone();
                        let errt = err.map(|e| {
                            std::thread::spawn(move || {
                                for l in BufReader::new(e).lines().map_while(Result::ok) {
                                    f2(&l);
                                }
                            })
                        });
                        if let Some(o) = out {
                            for l in BufReader::new(o).lines().map_while(Result::ok) {
                                on_line(&l);
                            }
                        }
                        if let Some(t) = errt {
                            let _ = t.join();
                        }
                        let code = child2.lock().unwrap().take().and_then(|mut c| c.wait().ok()).map(|s| s.to_string()).unwrap_or_default();
                        if stop2.load(Ordering::Relaxed) {
                            break;
                        }
                        error(&st2, tag, &format!("процесс завершился ({code}), перезапуск через {backoff} с"));
                        if started.elapsed() > Duration::from_secs(60) {
                            backoff = 1;
                        }
                    }
                    Err(e) => error(&st2, tag, &format!("не запускается: {e}")),
                }
                for _ in 0..backoff * 10 {
                    if stop2.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                backoff = (backoff * 2).min(30);
            }
        });
        Proc { cmd, stop, child }
    }

    fn stop(&self, st: &Shared, tag: &str, why: &str) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(c) = self.child.lock().unwrap().as_mut() {
            unsafe { libc::kill(c.id() as i32, libc::SIGTERM) };
            let t0 = Instant::now();
            while t0.elapsed() < Duration::from_secs(2) {
                if matches!(c.try_wait(), Ok(Some(_))) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = c.kill();
            let _ = c.wait();
        }
        log(st, tag, &format!("остановлен: {why}"));
    }
}

// ------------------------------------------------------------------------------------------
// Helpers

fn run(st: &Shared, args: &[&str]) -> bool {
    match Command::new(args[0]).args(&args[1..]).stdout(Stdio::null()).stderr(Stdio::piped()).output() {
        Ok(o) if o.status.success() => true,
        Ok(o) => {
            let e = String::from_utf8_lossy(&o.stderr).trim().to_string();
            log(st, "net", &format!("{}: {e}", args.join(" ")));
            false
        }
        Err(e) => {
            log(st, "net", &format!("{}: {e}", args[0]));
            false
        }
    }
}

fn quiet(args: &[&str]) {
    let _ = Command::new(args[0]).args(&args[1..]).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

fn have(cmd: &str) -> bool {
    Command::new("sh").args(["-c", &format!("command -v {cmd}")]).stdout(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
}

/// Write the embedded binaries to state/bin (only when they changed).
fn install_bins(dir: &Path) -> std::io::Result<(PathBuf, PathBuf)> {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin)?;
    let mut out = Vec::new();
    for (name, data) in [("tcp_node", TCP_NODE), ("ygghelper", YGGHELPER)] {
        let p = bin.join(name);
        if std::fs::read(&p).map(|d| d != data).unwrap_or(true) {
            let tmp = bin.join(format!("{name}.tmp"));
            std::fs::write(&tmp, data)?;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
            std::fs::rename(&tmp, &p)?;
        }
        out.push(p);
    }
    Ok((out[0].clone(), out[1].clone()))
}

fn http_json(path: &str) -> Option<Value> {
    let mut s = TcpStream::connect_timeout(&([127, 0, 0, 1], config::API).into(), Duration::from_secs(1)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    s.write_all(format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").as_bytes()).ok()?;
    let mut body = String::new();
    s.read_to_string(&mut body).ok()?;
    serde_json::from_str(body.split_once("\r\n\r\n")?.1).ok()
}

fn parse_kv(line: &str) -> HashMap<&str, u64> {
    line.split(' ').filter_map(|t| t.split_once('=')).map(|(k, v)| (k, v.parse().unwrap_or(0))).collect()
}

// ------------------------------------------------------------------------------------------
// Peers

fn refresh_public(st: &Shared, dir: &Path, force: bool) {
    let p = dir.join("public-peers.json");
    let stale = std::fs::metadata(&p).and_then(|m| m.modified()).map(|t| t.elapsed().unwrap_or_default() > PEERS_MAX_AGE).unwrap_or(true);
    if !force && !stale {
        return;
    }
    let tmp = dir.join("public-peers.json.tmp");
    let ok = Command::new("curl").args(["-fsSL", "--max-time", "30", "-o"]).arg(&tmp).arg(PUBLIC_PEERS_URL).status().map(|s| s.success()).unwrap_or(false)
        || Command::new("wget").args(["-q", "-T", "30", "-O"]).arg(&tmp).arg(PUBLIC_PEERS_URL).status().map(|s| s.success()).unwrap_or(false);
    let valid = ok && std::fs::read_to_string(&tmp).map(|t| t.trim_start().starts_with('{') && (t.contains("tls://") || t.contains("tcp://"))).unwrap_or(false);
    if valid {
        let _ = std::fs::rename(&tmp, &p);
        log(st, "peers", "публичный список загружен");
    } else {
        let _ = std::fs::remove_file(&tmp);
        error(st, "peers", &format!("не удалось скачать публичный список{}", if p.exists() { " — используется сохранённый" } else { "" }));
    }
}

fn write_peer_file(dir: &Path, cfg: &Config) {
    let mut custom = serde_json::Map::new();
    for u in cfg.network.peers.iter().map(|s| s.trim()).filter(|s| !s.is_empty() && !s.starts_with('#')) {
        custom.insert(u.to_string(), json!({"up": true}));
    }
    let mut out = format!("{{\"custom\":{}", Value::Object(custom));
    if cfg.network.public_peers {
        if let Ok(p) = std::fs::read_to_string(dir.join("public-peers.json")) {
            out.push_str(",\"public\":");
            out.push_str(&p);
        }
    }
    // Peers checked by the trackers (works where the public list's site is blocked).
    if let Ok(p) = std::fs::read_to_string(dir.join("tracker-peers.json")) {
        out.push_str(",\"tracker\":");
        out.push_str(&p);
    }
    out.push('}');
    let _ = std::fs::write(dir.join("peers.json"), out);
}

/// Ping list of the node (re-read live): saved servers, the trackers' exits and the trackers.
fn write_monitors(st: &Shared, dir: &Path, cfg: &Config) {
    let mut all: Vec<String> = cfg.client.servers.clone();
    all.extend(cfg.client.server.split_whitespace().map(String::from).filter(|s| !s.is_empty()));
    all.extend(st.lock().unwrap().tracker_exits.iter().map(|e| e.0.clone()));
    // Trackers too: regular pings keep the route to them warm, so the list is fetched fast.
    if cfg.network.use_trackers {
        all.extend(crate::tracker::with_defaults(&cfg.network.trackers));
    }
    let mut seen = std::collections::HashSet::new();
    all.retain(|a| seen.insert(a.clone()));
    let _ = std::fs::write(dir.join("monitors.txt"), all.join("\n"));
}

/// Ask the trackers for exits and more trackers (and, hourly, checked public peers).
fn refresh_trackers(st: Shared, dir: PathBuf, cfg: Arc<Mutex<Config>>, with_peers: bool) {
    let trackers: Vec<std::net::Ipv6Addr> = crate::tracker::with_defaults(&cfg.lock().unwrap().network.trackers).iter().filter_map(|t| config::parse_server(t)).collect();
    let mut exits: Vec<(String, Option<u64>)> = Vec::new();
    let mut found: Vec<String> = Vec::new();
    let mut ok = 0;
    let mut peers_note = String::new();
    for t in &trackers {
        let Ok(v) = crate::tracker::fetch(*t, "/exits") else { continue };
        ok += 1;
        for e in v["exits"].as_array().into_iter().flatten() {
            if let Some(a) = e["address"].as_str().and_then(config::parse_server).map(|a| a.to_string()) {
                if !exits.iter().any(|x| x.0 == a) {
                    exits.push((a, e["rtt_ms"].as_u64()));
                }
            }
        }
        if let Ok(v) = crate::tracker::fetch(*t, "/trackers") {
            found.extend(v["trackers"].as_array().into_iter().flatten().filter_map(|e| e["address"].as_str().and_then(config::parse_server)).map(|a| a.to_string()));
        }
        if with_peers && peers_note.is_empty() {
            if let Ok(v) = crate::tracker::fetch(*t, "/peers") {
                let n = v["peers"].as_array().map(|a| a.len()).unwrap_or(0);
                if n > 0 {
                    let _ = std::fs::write(dir.join("tracker-peers.json"), v.to_string());
                    peers_note = format!(", пиров {n}");
                }
            }
        }
    }
    // New trackers join the list.
    let added = {
        let mut c = cfg.lock().unwrap();
        let new: Vec<String> = found.into_iter().filter(|a| !c.network.trackers.iter().any(|t| config::parse_server(t).map(|x| x.to_string()) == Some(a.clone()))).collect::<std::collections::HashSet<_>>().into_iter().collect();
        if !new.is_empty() {
            c.network.trackers.extend(new.iter().cloned());
            let _ = c.save();
        }
        new.len()
    };
    let msg = if ok == 0 {
        format!("{}: трекеры недоступны", now_hms())
    } else {
        format!("{}: серверов {} от {ok} трекер(ов){peers_note}{}", now_hms(), exits.len(), if added > 0 { format!(", новых трекеров {added}") } else { String::new() })
    };
    {
        let mut s = st.lock().unwrap();
        if ok > 0 {
            s.tracker_exits = exits;
        }
        s.tracker_status = Some(msg.clone());
    }
    log(&st, "trackers", &msg);
    let c = cfg.lock().unwrap().clone();
    write_monitors(&st, &dir, &c);
}

// ------------------------------------------------------------------------------------------
// VPN: TUN + routing + ygghelper tun

struct Vpn {
    tun: std::fs::File,
    helper: Proc,
    ctl: Arc<Mutex<Option<UnixStream>>>,
    ipv6: bool,
    sig: String,
}

fn open_tun(name: &str) -> std::io::Result<std::fs::File> {
    const TUNSETIFF: libc::c_ulong = 0x4004_54ca;
    #[repr(C)]
    struct IfReq {
        name: [u8; 16],
        flags: libc::c_short,
        _pad: [u8; 22],
    }
    let f = std::fs::OpenOptions::new().read(true).write(true).open("/dev/net/tun")?;
    // IFF_TUN with packet information: tun2proxy on Linux reads a 4-byte PI header (on Android
    // the VpnService fd has none, and tun2proxy knows it there).
    let mut req = IfReq { name: [0; 16], flags: 0x0001, _pad: [0; 22] };
    req.name[..name.len()].copy_from_slice(name.as_bytes());
    use std::os::fd::AsRawFd;
    if unsafe { libc::ioctl(f.as_raw_fd(), TUNSETIFF as _, &mut req) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(f)
}

/// Send `fd` over a unix socket connection (SCM_RIGHTS) with one data byte.
fn send_fd(sock: &UnixStream, fd: i32) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    unsafe {
        let mut data = [1u8];
        let mut iov = libc::iovec { iov_base: data.as_mut_ptr() as *mut _, iov_len: 1 };
        let space = libc::CMSG_SPACE(std::mem::size_of::<i32>() as u32) as usize;
        let mut cbuf = vec![0u8; space];
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cbuf.as_mut_ptr() as *mut _;
        msg.msg_controllen = space as _;
        let cm = libc::CMSG_FIRSTHDR(&msg);
        (*cm).cmsg_level = libc::SOL_SOCKET;
        (*cm).cmsg_type = libc::SCM_RIGHTS;
        (*cm).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<i32>() as u32) as _;
        std::ptr::write_unaligned(libc::CMSG_DATA(cm) as *mut i32, fd);
        if libc::sendmsg(sock.as_raw_fd(), &msg, 0) < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

fn routes(st: &Shared, add: bool, ipv6: bool) {
    let t = config::TABLE.to_string();
    let mark = format!("0x{:x}", config::FWMARK);
    let tun = config::TUN_NAME;
    for fam in ["-4", "-6"] {
        if fam == "-6" && !add {
            // fallthrough: delete both families
        }
        if add {
            // Main table without its default route (LAN, links) first; the node's marked peer
            // links use the main table fully; everything else takes the VPN default route.
            run(st, &["ip", fam, "rule", "add", "lookup", "main", "suppress_prefixlength", "0", "priority", "8999"]);
            run(st, &["ip", fam, "rule", "add", "fwmark", &mark, "lookup", "main", "priority", "9000"]);
            run(st, &["ip", fam, "rule", "add", "lookup", &t, "priority", "9001"]);
            if fam == "-4" || ipv6 {
                run(st, &["ip", fam, "route", "replace", "default", "dev", tun, "table", &t]);
            } else {
                // IPv6 off: block it instead of leaking past the tunnel; apps fall back to IPv4.
                run(st, &["ip", fam, "route", "replace", "unreachable", "default", "table", &t]);
            }
        } else {
            for p in ["8999", "9000", "9001"] {
                quiet(&["ip", fam, "rule", "del", "priority", p]);
            }
            quiet(&["ip", fam, "route", "flush", "table", &t]);
        }
    }
}

fn vpn_up(st: &Shared, dir: &Path, helper_bin: &Path, cfg: &Config) -> Result<Vpn, String> {
    let tun = open_tun(config::TUN_NAME).map_err(|e| format!("TUN {}: {e}", config::TUN_NAME))?;
    let dev = config::TUN_NAME;
    run(st, &["ip", "addr", "replace", "10.111.222.2/24", "dev", dev]);
    if cfg.client.ipv6 {
        run(st, &["ip", "-6", "addr", "replace", "fd59:7967:6772::2/64", "dev", dev]);
    }
    run(st, &["ip", "link", "set", dev, "mtu", "1500", "up"]);
    routes(st, true, cfg.client.ipv6);
    if have("resolvectl") {
        run(st, &["resolvectl", "dns", dev, "10.111.222.53"]);
        run(st, &["resolvectl", "domain", dev, "~."]);
        run(st, &["resolvectl", "default-route", dev, "yes"]);
    } else {
        log(st, "vpn", "нет resolvectl (systemd-resolved): DNS идёт мимо VPN-DNS, но запросы всё равно через туннель");
    }

    // Next to the control socket (/run/yggproxy): unix socket paths must stay short.
    let _ = dir;
    let ctl_path = config::socket_path().with_file_name("yggproxy-tun.sock");
    let _ = std::fs::remove_file(&ctl_path);
    let mut cmd = vec![
        helper_bin.display().to_string(),
        "tun".into(),
        "--ctl".into(),
        ctl_path.display().to_string(),
        "--socks".into(),
        format!("127.0.0.1:{}", config::CLIENT_SOCKS),
    ];
    if cfg.client.udp {
        cmd.push("--udpgw".into());
        cmd.push(format!("127.0.0.1:{}", config::UDPGW));
    }
    if cfg.client.ipv6 {
        cmd.push("--ipv6".into());
    }
    cmd.extend(["--mtu".into(), "1500".into(), "-v".into(), "warn".into()]);
    let ctl: Arc<Mutex<Option<UnixStream>>> = Arc::new(Mutex::new(None));
    use std::os::fd::AsRawFd;
    let fd = tun.as_raw_fd();
    let (st2, ctl2, path2) = (st.clone(), ctl.clone(), ctl_path.clone());
    let helper = Proc::start(st, "tun", cmd, Arc::new(move |l: &str| {
        helper_line(&st2, "tun", l);
        if l.starts_with("READY tun") {
            match UnixStream::connect(&path2).and_then(|s| send_fd(&s, fd).map(|_| s)) {
                Ok(s) => *ctl2.lock().unwrap() = Some(s), // kept open: closing it stops the helper
                Err(e) => error(&st2, "tun", &format!("не удалось передать TUN: {e}")),
            }
        } else if l == "STATE tun running" {
            st2.lock().unwrap().vpn_active = true;
        }
    }));
    log(st, "vpn", &format!("интерфейс {dev} поднят, выход через {}", cfg.client.server));
    Ok(Vpn { tun, helper, ctl, ipv6: cfg.client.ipv6, sig: vpn_sig(cfg) })
}

fn vpn_down(st: &Shared, v: Vpn) {
    *v.ctl.lock().unwrap() = None;
    v.helper.stop(st, "tun", "VPN выключен");
    routes(st, false, v.ipv6);
    if have("resolvectl") {
        quiet(&["resolvectl", "revert", config::TUN_NAME]);
    }
    drop(v.tun); // closes the TUN device
    let mut s = st.lock().unwrap();
    s.vpn_active = false;
}

fn vpn_sig(cfg: &Config) -> String {
    format!("{}|{}|{}", cfg.client.server, cfg.client.udp, cfg.client.ipv6)
}

fn helper_line(st: &Shared, tag: &str, l: &str) {
    if let Some(rest) = l.strip_prefix("STAT ") {
        let kv = parse_kv(rest);
        let mut s = st.lock().unwrap();
        if rest.starts_with("tun") {
            s.tun_tx = kv.get("tx").copied().unwrap_or(s.tun_tx);
            s.tun_rx = kv.get("rx").copied().unwrap_or(s.tun_rx);
        } else if rest.starts_with("server") {
            s.srv = ["active", "total", "denied", "tx", "rx"].map(|k| kv.get(k).copied().unwrap_or(0));
        } else if let Some(c) = rest.strip_prefix("client ") {
            if let Some(addr) = c.split(' ').next() {
                s.clients.insert(addr.to_string(), (kv["active"], kv["tx"], kv["rx"], Instant::now()));
            }
        }
    } else if let Some(e) = l.strip_prefix("ERROR ") {
        error(st, tag, e);
    } else if l.starts_with("ygghelper: ") {
        // stderr copy of an ERROR line
    } else {
        log(st, tag, l);
    }
}

fn node_line(st: &Shared, l: &str) {
    {
        let mut s = st.lock().unwrap();
        let val = || l.split_once(':').map(|x| x.1.trim().to_string());
        if l.starts_with("device address:") {
            s.address = val();
        } else if l.starts_with("public key:") {
            s.public_key = val();
        } else if l.starts_with("server rtt:") {
            s.server_rtt = val().map(|v| v.split_once("] ").map(|x| x.1.to_string()).unwrap_or(v));
        } else if l.contains("заменён пир") || l.starts_with("замена ") {
            s.last_swap = Some(format!("{} {l}", now_hms()));
        } else if l.starts_with("lan upstream:") {
            if let Some(v) = val().filter(|v| v.starts_with("joined") || v.starts_with("disabled")) {
                s.lan_status = Some(v);
            }
        }
    }
    // The node prints its peer table every 10 s: keep those out of the log.
    if !(l.starts_with("peers:") || l.starts_with("  out ") || l.starts_with("  in ") || l.starts_with("  cand")) {
        log(st, "node", l);
    }
}

// ------------------------------------------------------------------------------------------
// Reconcile

struct Daemon {
    st: Shared,
    dir: PathBuf,
    node_bin: PathBuf,
    helper_bin: PathBuf,
    node: Option<Proc>,
    exit: Option<Proc>,
    vpn: Option<Vpn>,
}

impl Daemon {
    fn node_args(&self, cfg: &Config, client: bool) -> Vec<String> {
        let n = &cfg.network;
        let d = &self.dir;
        let mut a: Vec<String> = vec![
            self.node_bin.display().to_string(),
            "--key".into(),
            d.join("ygg.key").display().to_string(),
            "--peers".into(),
            d.join("peers.json").display().to_string(),
            "--no-public".into(),
            "--target".into(),
            n.target.clamp(1, 32).to_string(),
            "--swap-interval".into(),
            n.swap_interval.clamp(5, 3600).to_string(),
            "--swap-settle".into(),
            n.swap_settle.clamp(10, 3600).to_string(),
            "--api".into(),
            format!("127.0.0.1:{}", config::API),
            "--html-port".into(),
            config::HTML_PORT.to_string(),
            "--socks".into(),
            format!("127.0.0.1:{}", config::NODE_SOCKS),
            "--monitor-file".into(),
            d.join("monitors.txt").display().to_string(),
        ];
        if n.lan_discovery {
            a.push("--lan-upstream".into());
            if n.lan_target > 0 {
                a.extend(["--lan-target".into(), n.lan_target.to_string()]);
            }
        }
        if client {
            a.extend([
                "--connect".into(),
                format!("{}:[{}]:{}", config::CLIENT_SOCKS, cfg.client.server, config::PORT),
                "--rtt-limit".into(),
                n.rtt_limit.clamp(10, 5000).to_string(),
                "--fwmark".into(),
                format!("0x{:x}", config::FWMARK),
            ]);
        }
        if cfg.server.enabled {
            let p = config::PORT.to_string();
            a.extend([
                "--forward".into(),
                format!("{p}:{}", config::EXIT_SOCKS),
                "--open".into(),
                p.clone(),
                "--proxy-protocol".into(),
                p,
            ]);
        }
        a.extend(n.extra_args.split_whitespace().map(String::from));
        a
    }

    fn reconcile(&mut self, cfg: &Config) {
        let client = cfg.client.enabled && config::parse_server(&cfg.client.server).is_some();
        if cfg.client.enabled && !client {
            error(&self.st, "vpn", "не задан адрес сервера выхода");
        }
        let any = client || cfg.server.enabled || cfg.network.keep_node;

        write_monitors(&self.st, &self.dir, cfg);

        // 1. Node.
        if any {
            refresh_public(&self.st, &self.dir, false);
            write_peer_file(&self.dir, cfg);
            let cmd = self.node_args(cfg, client);
            if self.node.as_ref().map(|p| &p.cmd) != Some(&cmd) {
                if let Some(p) = self.node.take() {
                    p.stop(&self.st, "node", "новые параметры узла");
                }
                let st = self.st.clone();
                self.st.lock().unwrap().node_running = true;
                self.node = Some(Proc::start(&self.st, "node", cmd, Arc::new(move |l: &str| node_line(&st, l))));
            }
        } else if let Some(p) = self.node.take() {
            p.stop(&self.st, "node", "клиент и сервер выключены, узел в фоне отключён");
            let mut s = self.st.lock().unwrap();
            s.node_running = false;
            s.node_stats = None;
            s.node_peers = None;
        }

        // 2. Exit.
        if cfg.server.enabled {
            let limits = self.dir.join("limits.txt");
            let _ = std::fs::write(&limits, cfg.server.limits.file_text()); // re-read live by the exit
            let mut cmd = vec![
                self.helper_bin.display().to_string(),
                "server".into(),
                "--socks".into(),
                format!("127.0.0.1:{}", config::EXIT_SOCKS),
                "--udpgw".into(),
                format!("127.0.0.1:{}", config::UDPGW),
                "--speedtest".into(),
                format!("127.0.0.1:{}", config::SPEEDTEST),
                "--proxy-protocol".into(),
                "--limits".into(),
                limits.display().to_string(),
                "-v".into(),
                "warn".into(),
            ];
            if cfg.server.allow_lan {
                cmd.push("--allow-lan".into());
            }
            if client {
                // Exit traffic must not loop into our own VPN.
                cmd.extend(["--fwmark".into(), format!("0x{:x}", config::FWMARK)]);
            }
            if self.exit.as_ref().map(|p| &p.cmd) != Some(&cmd) {
                if let Some(p) = self.exit.take() {
                    p.stop(&self.st, "exit", "новые параметры сервера");
                }
                let st = self.st.clone();
                self.exit = Some(Proc::start(&self.st, "exit", cmd, Arc::new(move |l: &str| {
                    helper_line(&st, "exit", l);
                    if l.starts_with("READY") {
                        st.lock().unwrap().exit_active = true;
                    }
                })));
            }
        } else if let Some(p) = self.exit.take() {
            p.stop(&self.st, "exit", "сервер выключен");
            let mut s = self.st.lock().unwrap();
            s.exit_active = false;
            s.clients.clear();
        }

        // 3. VPN.
        if client {
            if self.vpn.as_ref().map(|v| v.sig.as_str()) != Some(vpn_sig(cfg).as_str()) {
                if let Some(v) = self.vpn.take() {
                    vpn_down(&self.st, v);
                }
                match vpn_up(&self.st, &self.dir, &self.helper_bin, cfg) {
                    Ok(v) => self.vpn = Some(v),
                    Err(e) => error(&self.st, "vpn", &e),
                }
            }
        } else if let Some(v) = self.vpn.take() {
            vpn_down(&self.st, v);
        }
    }

    fn stop_all(&mut self) {
        if let Some(v) = self.vpn.take() {
            vpn_down(&self.st, v);
        }
        if let Some(p) = self.exit.take() {
            p.stop(&self.st, "exit", "служба завершается");
        }
        if let Some(p) = self.node.take() {
            p.stop(&self.st, "node", "служба завершается");
        }
    }
}

// ------------------------------------------------------------------------------------------
// Control socket

enum Msg {
    Reconcile,
    RefreshPeers,
    RefreshTrackers,
}

fn status_json(st: &Shared, cfg: &Config) -> Value {
    let s = st.lock().unwrap();
    let clients: Vec<Value> = s
        .clients
        .iter()
        .filter(|(_, c)| c.3.elapsed() < Duration::from_secs(10))
        .map(|(a, c)| json!({"address": a, "active": c.0, "tx": c.1, "rx": c.2}))
        .collect();
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "port": config::PORT,
        "node": {"running": s.node_running, "address": s.address, "public_key": s.public_key,
                 "lan": s.lan_status, "stats": s.node_stats, "peers": s.node_peers},
        "server": {"enabled": cfg.server.enabled, "active": s.exit_active,
                   "link": s.address.as_ref().map(|a| config::connect_link(a)),
                   "active_conns": s.srv[0], "total": s.srv[1], "denied": s.srv[2], "tx": s.srv[3], "rx": s.srv[4],
                   "clients": clients, "limits": cfg.server.limits},
        "client": {"enabled": cfg.client.enabled, "server": cfg.client.server, "active": s.vpn_active,
                   "rx": s.tun_rx, "tx": s.tun_tx, "server_rtt": s.server_rtt, "last_swap": s.last_swap,
                   "servers": cfg.client.servers},
        "rtt": s.node_rtt,
        "trackers": {"enabled": cfg.network.use_trackers, "list": cfg.network.trackers, "status": s.tracker_status,
                     "exits": s.tracker_exits.iter().map(|(a, r)| json!({"address": a, "tracker_rtt_ms": r})).collect::<Vec<_>>()},
        "last_error": s.last_error,
    })
}

fn handle_conn(conn: UnixStream, st: Shared, cfg: Arc<Mutex<Config>>, tx: mpsc::Sender<Msg>) {
    let mut r = BufReader::new(match conn.try_clone() {
        Ok(c) => c,
        Err(_) => return,
    });
    let mut w = conn;
    let mut line = String::new();
    while r.read_line(&mut line).map(|n| n > 0).unwrap_or(false) {
        let req: Value = serde_json::from_str(line.trim()).unwrap_or(Value::Null);
        line.clear();
        let resp = match req["cmd"].as_str().unwrap_or("") {
            "status" => status_json(&st, &cfg.lock().unwrap()),
            "config" => serde_json::to_value(&*cfg.lock().unwrap()).unwrap(),
            "set" => match serde_json::from_value::<Config>(req["config"].clone()) {
                Ok(new) => {
                    if let Err(e) = new.save() {
                        json!({"error": format!("не удалось сохранить настройки: {e}")})
                    } else {
                        let old = std::mem::replace(&mut *cfg.lock().unwrap(), new.clone());
                        if old.server.limits != new.server.limits {
                            let _ = std::fs::write(config::state_dir().join("limits.txt"), new.server.limits.file_text());
                        }
                        let _ = tx.send(Msg::Reconcile);
                        json!({"ok": true})
                    }
                }
                Err(e) => json!({"error": format!("неверные настройки: {e}")}),
            },
            "logs" => {
                let since = req["since"].as_u64().unwrap_or(0);
                let s = st.lock().unwrap();
                let lines: Vec<&String> = s.logs.iter().filter(|(n, _)| *n > since).map(|(_, l)| l).collect();
                json!({"seq": s.log_seq, "lines": lines})
            }
            "clear_logs" => {
                st.lock().unwrap().logs.clear();
                json!({"ok": true})
            }
            "refresh_trackers" => {
                let _ = tx.send(Msg::RefreshTrackers);
                json!({"ok": true})
            }
            "refresh_peers" => {
                let _ = tx.send(Msg::RefreshPeers);
                json!({"ok": true})
            }
            other => json!({"error": format!("неизвестная команда {other:?}")}),
        };
        if w.write_all(format!("{resp}\n").as_bytes()).is_err() {
            return;
        }
    }
}

fn listen(path: &Path) -> std::io::Result<UnixListener> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
        let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o755));
    }
    let _ = std::fs::remove_file(path);
    let l = UnixListener::bind(path)?;
    // root:yggproxy 0660 when the group exists (yggproxy install creates it), else root only.
    let grp = std::ffi::CString::new("yggproxy").unwrap();
    let g = unsafe { libc::getgrnam(grp.as_ptr()) };
    let mode = if g.is_null() { 0o600 } else {
        let cp = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        unsafe { libc::chown(cp.as_ptr(), 0, (*g).gr_gid) };
        0o660
    };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    Ok(l)
}

static TERM: AtomicBool = AtomicBool::new(false);

extern "C" fn on_term(_: libc::c_int) {
    TERM.store(true, Ordering::Relaxed);
}

pub fn main() -> Result<(), String> {
    if unsafe { libc::geteuid() } != 0 {
        return Err("служба должна работать от root (sudo yggproxy daemon или systemctl start yggproxy)".into());
    }
    let dir = config::state_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let (node_bin, helper_bin) = install_bins(&dir).map_err(|e| format!("бинарники: {e}"))?;
    let st: Shared = Arc::new(Mutex::new(St::default()));
    let cfg = Arc::new(Mutex::new(Config::load()));
    log(&st, "service", &format!("yggproxy {} запущен, настройки {}", env!("CARGO_PKG_VERSION"), config::config_path().display()));

    unsafe {
        libc::signal(libc::SIGTERM, on_term as *const () as usize);
        libc::signal(libc::SIGINT, on_term as *const () as usize);
    }
    // Leftovers of a crash: stale rules would black-hole traffic.
    routes(&st, false, true);

    let (tx, rx) = mpsc::channel::<Msg>();
    let listener = listen(&config::socket_path()).map_err(|e| format!("{}: {e}", config::socket_path().display()))?;
    {
        let (st, cfg, tx) = (st.clone(), cfg.clone(), tx.clone());
        std::thread::spawn(move || {
            for c in listener.incoming().flatten() {
                let (st, cfg, tx) = (st.clone(), cfg.clone(), tx.clone());
                std::thread::spawn(move || handle_conn(c, st, cfg, tx));
            }
        });
    }

    let mut d = Daemon { st: st.clone(), dir: dir.clone(), node_bin, helper_bin, node: None, exit: None, vpn: None };
    let snapshot = cfg.lock().unwrap().clone();
    d.reconcile(&snapshot);
    let mut last_poll = Instant::now();
    let mut last_peer_check = Instant::now();
    let mut last_tracker_peers: Option<Instant> = None;
    let mut last_trackers: Option<Instant> = None;
    let trackers_busy = Arc::new(AtomicBool::new(false));
    while !TERM.load(Ordering::Relaxed) {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(Msg::Reconcile) => {
                let c = cfg.lock().unwrap().clone();
                d.reconcile(&c);
            }
            Ok(Msg::RefreshPeers) => {
                refresh_public(&st, &dir, true);
                let c = cfg.lock().unwrap().clone();
                write_peer_file(&dir, &c);
                if let Some(p) = d.node.take() {
                    p.stop(&st, "node", "обновление списка пиров");
                }
                d.reconcile(&c);
            }
            Ok(Msg::RefreshTrackers) => last_trackers = None,
            Err(_) => {}
        }
        if last_poll.elapsed() >= Duration::from_secs(2) && d.node.is_some() {
            last_poll = Instant::now();
            let (stats, peers, rtt) = (http_json("/api/stats"), http_json("/api/peers"), http_json("/api/rtt"));
            let mut s = st.lock().unwrap();
            s.node_stats = stats;
            s.node_peers = peers;
            s.node_rtt = rtt;
        }
        // Trackers: exits and more trackers every 2 min, checked public peers hourly (fetched
        // directly through a yggdrasil-go TUN or through the node's --socks).
        let use_trackers = cfg.lock().unwrap().network.use_trackers;
        if use_trackers && d.node.is_some() && st.lock().unwrap().node_stats.is_some()
            && last_trackers.map_or(true, |t| t.elapsed() >= Duration::from_secs(120))
            && !trackers_busy.load(Ordering::Relaxed)
        {
            last_trackers = Some(Instant::now());
            let with_peers = last_tracker_peers.map_or(true, |t| t.elapsed() >= Duration::from_secs(3600));
            if with_peers {
                last_tracker_peers = Some(Instant::now());
            }
            trackers_busy.store(true, Ordering::Relaxed);
            let (st2, dir2, cfg2, busy) = (st.clone(), dir.clone(), cfg.clone(), trackers_busy.clone());
            std::thread::spawn(move || {
                refresh_trackers(st2, dir2, cfg2, with_peers);
                busy.store(false, Ordering::Relaxed);
            });
        }
        if last_peer_check.elapsed() >= Duration::from_secs(600) && d.node.is_some() {
            last_peer_check = Instant::now();
            refresh_public(&st, &dir, false);
        }
    }
    log(&st, "service", "получен сигнал остановки");
    d.stop_all();
    let _ = std::fs::remove_file(config::socket_path());
    Ok(())
}
