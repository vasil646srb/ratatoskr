//! ygghelper — the two non-Yggdrasil halves of YggProxy.
//!
//! `ygghelper server --socks 127.0.0.1:10800 --udpgw 127.0.0.1:7300 [--speedtest 127.0.0.1:7301] [--allow-lan]`
//!     Exit node: SOCKS5 (CONNECT, no auth) to the internet plus a udpgw server for UDP.
//!     tcp_node exposes the SOCKS port over Yggdrasil (`--forward 1080:10800`).
//!     Loopback/LAN destinations are refused, except the udpgw and speed-test addresses.
//!
//!     `--proxy-protocol`: every connection starts with tcp_node's PROXY v1 line (the client's
//!     Yggdrasil address); `--limits FILE` (`total_down= total_up= client_down= client_up=`,
//!     kbit/s, 0 = none) caps the server and each client, re-read live. With PROXY lines the
//!     server also prints `STAT client ADDR active=N tx=N rx=N` for active clients.
//!
//!     Speed test (iperf-like, client <-> exit over the overlay), reached through SOCKS CONNECT:
//!     the client sends `[mode, seconds]`; `D` = the server sends data for that long and closes,
//!     `U` = the server reads for that long from the first byte and answers with
//!     `bytes:u64be, ms:u64be`. At most 2 tests at once, 30 s each.
//!
//! `ygghelper tun --ctl PATH --socks 127.0.0.1:10808 [--udpgw 127.0.0.1:7300] [--mtu 1500] [--ipv6]`
//!     Client: listens on the unix socket PATH, receives the VpnService tun fd via SCM_RIGHTS,
//!     then runs tun2proxy over the local SOCKS port (tcp_node `--connect 10808:[server]:1080`).
//!     `--udpgw` is the udpgw address as seen by the exit node. Exits when the control
//!     connection closes.
//!
//! Status lines on stdout, for the app: `STAT tun tx=N rx=N`,
//! `STAT server active=N total=N denied=N tx=N rx=N`, `READY ...`, `ERROR ...`.

mod limit;
mod udpgw_server;

use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::os::fd::RawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

pub(crate) fn out(line: &str) {
    let mut o = std::io::stdout().lock();
    let _ = writeln!(o, "{line}");
    let _ = o.flush();
}

fn die(msg: &str) -> ! {
    out(&format!("ERROR {msg}"));
    eprintln!("ygghelper: {msg}");
    std::process::exit(1);
}

const USAGE: &str = "usage:
  ygghelper server --socks ADDR [--udpgw ADDR] [--speedtest ADDR] [--allow-lan]
                   [--proxy-protocol] [--limits FILE] [--fwmark N] [-v LEVEL]
  ygghelper tun --ctl PATH --socks ADDR [--udpgw ADDR] [--mtu N] [--ipv6] [-v LEVEL]";

struct Opts {
    socks: Option<SocketAddr>,
    udpgw: Option<SocketAddr>,
    speedtest: Option<SocketAddr>,
    proxy_protocol: bool,
    limits: Option<String>,
    fwmark: u32,
    ctl: Option<String>,
    mtu: u16,
    ipv6: bool,
    allow_lan: bool,
    verbosity: String,
}

fn parse(args: &[String]) -> Opts {
    let mut o = Opts { socks: None, udpgw: None, speedtest: None, proxy_protocol: false, limits: None, fwmark: 0, ctl: None, mtu: 1500, ipv6: false, allow_lan: false, verbosity: "info".into() };
    let mut it = args.iter();
    let addr = |v: Option<&String>, f: &str| -> SocketAddr {
        v.and_then(|s| s.parse().ok()).unwrap_or_else(|| die(&format!("{f}: want IP:PORT")))
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--socks" => o.socks = Some(addr(it.next(), "--socks")),
            "--udpgw" => o.udpgw = Some(addr(it.next(), "--udpgw")),
            "--speedtest" => o.speedtest = Some(addr(it.next(), "--speedtest")),
            "--proxy-protocol" => o.proxy_protocol = true,
            "--fwmark" => {
                o.fwmark = it
                    .next()
                    .and_then(|v| v.strip_prefix("0x").map(|h| u32::from_str_radix(h, 16).ok()).unwrap_or_else(|| v.parse().ok()))
                    .unwrap_or_else(|| die("--fwmark: want N or 0xN"))
            }
            "--limits" => o.limits = Some(it.next().cloned().unwrap_or_else(|| die("--limits: want FILE"))),
            "--ctl" => o.ctl = Some(it.next().cloned().unwrap_or_else(|| die("--ctl: want PATH"))),
            "--mtu" => o.mtu = it.next().and_then(|s| s.parse().ok()).filter(|m| *m >= 576).unwrap_or_else(|| die("--mtu: want >= 576")),
            "--ipv6" => o.ipv6 = true,
            "--allow-lan" => o.allow_lan = true,
            "-v" | "--verbosity" => o.verbosity = it.next().cloned().unwrap_or_else(|| die("-v: want level")),
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => die(&format!("unknown option {other}\n{USAGE}")),
        }
    }
    o
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else { die(USAGE) };
    let o = parse(&args[1..]);
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(o.verbosity.clone()))
        .format_timestamp_secs()
        .init();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .unwrap_or_else(|e| die(&format!("runtime: {e}")));
    let res = match cmd.as_str() {
        "server" => rt.block_on(server_main(o)),
        "tun" => tun_main(&rt, o),
        _ => die(USAGE),
    };
    if let Err(e) = res {
        die(&e);
    }
}

// ------------------------------------------------------------------------------------------
// server: SOCKS5 exit + udpgw
// ------------------------------------------------------------------------------------------

#[derive(Default)]
struct ServerStats {
    active: AtomicU64,
    total: AtomicU64,
    denied: AtomicU64,
    tx: AtomicU64,
    rx: AtomicU64,
}

async fn server_main(o: Opts) -> Result<(), String> {
    let socks = o.socks.ok_or("server: --socks required")?;
    let token = CancellationToken::new();
    if let Some(gw) = o.udpgw {
        let args = udpgw_server::UdpGwArgs {
            listen_addr: gw,
            udp_mtu: 10240,
            udp_timeout: 3,
            daemonize: false,
            verbosity: log::LevelFilter::Info,
        };
        let t = token.clone();
        tokio::spawn(async move {
            if let Err(e) = udpgw_server::run(args, t).await {
                out(&format!("ERROR udpgw: {e}"));
                std::process::exit(1);
            }
        });
    }
    if let Some(st) = o.speedtest {
        let l = TcpListener::bind(st).await.map_err(|e| format!("bind {st}: {e}"))?;
        tokio::spawn(speedtest_serve(l));
    }
    let listener = TcpListener::bind(socks).await.map_err(|e| format!("bind {socks}: {e}"))?;
    let show = |a: Option<SocketAddr>| a.map(|a| a.to_string()).unwrap_or("-".into());
    out(&format!("READY server socks={socks} udpgw={} speedtest={}", show(o.udpgw), show(o.speedtest)));

    let stats = Arc::new(ServerStats::default());
    let limiter = Arc::new(limit::Limiter::new(limit::Limits::default()));
    if let Some(path) = o.limits.clone() {
        tokio::spawn(limit::watch_file(path, limiter.clone()));
    }
    {
        let s = stats.clone();
        let lim = limiter.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let mut clients = lim.clients();
                clients.sort_by_key(|c| std::cmp::Reverse(c.2 + c.3));
                for (a, active, tx, rx) in clients.into_iter().take(32) {
                    out(&format!("STAT client {a} active={active} tx={tx} rx={rx}"));
                }
                out(&format!(
                    "STAT server active={} total={} denied={} tx={} rx={}",
                    s.active.load(Ordering::Relaxed),
                    s.total.load(Ordering::Relaxed),
                    s.denied.load(Ordering::Relaxed),
                    s.tx.load(Ordering::Relaxed),
                    s.rx.load(Ordering::Relaxed)
                ));
            }
        });
    }
    let policy = Arc::new(Policy { allow_lan: o.allow_lan, local_ok: [o.udpgw, o.speedtest].into_iter().flatten().collect(), fwmark: o.fwmark });
    let limit = Arc::new(tokio::sync::Semaphore::new(1024));
    loop {
        let (conn, _) = listener.accept().await.map_err(|e| format!("accept: {e}"))?;
        let Ok(permit) = limit.clone().try_acquire_owned() else {
            continue; // over the limit: drop the connection
        };
        let (stats, policy, lim) = (stats.clone(), policy.clone(), limiter.clone());
        let pp = o.proxy_protocol;
        tokio::spawn(async move {
            stats.total.fetch_add(1, Ordering::Relaxed);
            stats.active.fetch_add(1, Ordering::Relaxed);
            if let Err(e) = socks_session(conn, &policy, &stats, &lim, pp).await {
                log::debug!("socks: {e}");
            }
            stats.active.fetch_sub(1, Ordering::Relaxed);
            drop(permit);
        });
    }
}

struct Policy {
    allow_lan: bool,
    /// Our own services reachable through SOCKS (udpgw, speed test).
    local_ok: Vec<SocketAddr>,
    /// SO_MARK for outbound connections (Linux): keeps them out of a VPN on the same host.
    fwmark: u32,
}

/// Outbound TCP connect, with `--fwmark` set before connecting.
async fn connect_out(a: SocketAddr, mark: u32) -> std::io::Result<TcpStream> {
    if mark == 0 {
        return TcpStream::connect(a).await;
    }
    let sock = if a.is_ipv6() { tokio::net::TcpSocket::new_v6()? } else { tokio::net::TcpSocket::new_v4()? };
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let m = mark as libc::c_int;
        // SAFETY: setsockopt with a c_int on a socket we own.
        if unsafe { libc::setsockopt(sock.as_raw_fd(), libc::SOL_SOCKET, libc::SO_MARK, &m as *const _ as *const libc::c_void, 4) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    sock.connect(a).await
}

impl Policy {
    fn allowed(&self, sa: &SocketAddr) -> bool {
        if self.local_ok.contains(sa) {
            return true;
        }
        let ip = match sa.ip() {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)),
            v4 => v4,
        };
        if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() {
            return false;
        }
        let local = match ip {
            IpAddr::V4(v4) => {
                let o = v4.octets();
                v4.is_private() || v4.is_link_local() || v4.is_broadcast() || (o[0] == 100 && (o[1] & 0xc0) == 64) // CGNAT
            }
            IpAddr::V6(v6) => {
                let s = v6.segments()[0];
                (s & 0xfe00) == 0xfc00   // ULA
                    || (s & 0xffc0) == 0xfe80 // link-local
                    || (s & 0xfe00) == 0x0200 // Yggdrasil 200::/7: never via the exit
            }
        };
        !local || self.allow_lan
    }
}

const REP_OK: u8 = 0;
const REP_FAIL: u8 = 1;
const REP_DENIED: u8 = 2;
const REP_HOST: u8 = 4;
const REP_REFUSED: u8 = 5;
const REP_CMD: u8 = 7;
const REP_ATYP: u8 = 8;

async fn reply(c: &mut TcpStream, rep: u8) -> std::io::Result<()> {
    c.write_all(&[5, rep, 0, 1, 0, 0, 0, 0, 0, 0]).await
}

/// Read tcp_node's `PROXY TCP6 SRC DST SPORT DPORT\r\n` line: the client's Yggdrasil address.
async fn read_proxy_v1(c: &mut TcpStream) -> std::io::Result<IpAddr> {
    let mut line = Vec::with_capacity(108);
    let mut b = [0u8; 1];
    while line.len() < 108 {
        c.read_exact(&mut b).await?;
        line.push(b[0]);
        if line.ends_with(b"\r\n") {
            let s = String::from_utf8_lossy(&line);
            let mut f = s.split_whitespace();
            if f.next() == Some("PROXY") {
                f.next();
                if let Some(Ok(ip)) = f.next().map(|a| a.parse::<IpAddr>()) {
                    return Ok(ip);
                }
            }
            break;
        }
    }
    Err(std::io::Error::other("bad PROXY line"))
}

/// Copy `r` to `w` through the buckets; counts into `n` and `extra`.
async fn pump(
    mut r: tokio::net::tcp::ReadHalf<'_>,
    mut w: tokio::net::tcp::WriteHalf<'_>,
    buckets: &[&limit::Bucket],
    count: &[&AtomicU64],
) -> std::io::Result<()> {
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = r.read(&mut buf).await?;
        if n == 0 {
            return w.shutdown().await;
        }
        limit::throttle(buckets, n).await;
        w.write_all(&buf[..n]).await?;
        for c in count {
            c.fetch_add(n as u64, Ordering::Relaxed);
        }
    }
}

async fn socks_session(mut c: TcpStream, policy: &Policy, stats: &ServerStats, lim: &limit::Limiter, pp: bool) -> Result<(), String> {
    let _ = c.set_nodelay(true);
    let client = if pp {
        let ip = tokio::time::timeout(Duration::from_secs(10), read_proxy_v1(&mut c))
            .await
            .map_err(|_| "no PROXY line".to_string())?
            .map_err(|e| e.to_string())?;
        Some(lim.client(ip))
    } else {
        None
    };
    let hs = async {
        // greeting
        let mut h = [0u8; 2];
        c.read_exact(&mut h).await?;
        if h[0] != 5 {
            return Err(std::io::Error::other("not socks5"));
        }
        let mut m = vec![0u8; h[1] as usize];
        c.read_exact(&mut m).await?;
        if !m.contains(&0) {
            c.write_all(&[5, 0xff]).await?;
            return Err(std::io::Error::other("no acceptable auth"));
        }
        c.write_all(&[5, 0]).await?;
        // request
        let mut r = [0u8; 4];
        c.read_exact(&mut r).await?;
        let (cmd, atyp) = (r[1], r[3]);
        let host: Result<Vec<SocketAddr>, u8> = match atyp {
            1 => {
                let mut b = [0u8; 6];
                c.read_exact(&mut b).await?;
                let ip = std::net::Ipv4Addr::new(b[0], b[1], b[2], b[3]);
                Ok(vec![SocketAddr::new(ip.into(), u16::from_be_bytes([b[4], b[5]]))])
            }
            4 => {
                let mut b = [0u8; 18];
                c.read_exact(&mut b).await?;
                let mut a = [0u8; 16];
                a.copy_from_slice(&b[..16]);
                Ok(vec![SocketAddr::new(std::net::Ipv6Addr::from(a).into(), u16::from_be_bytes([b[16], b[17]]))])
            }
            3 => {
                let mut l = [0u8; 1];
                c.read_exact(&mut l).await?;
                let mut name = vec![0u8; l[0] as usize + 2];
                c.read_exact(&mut name).await?;
                let port = u16::from_be_bytes([name[name.len() - 2], name[name.len() - 1]]);
                let name = String::from_utf8_lossy(&name[..name.len() - 2]).to_string();
                match tokio::time::timeout(Duration::from_secs(10), tokio::net::lookup_host((name.as_str(), port))).await {
                    Ok(Ok(it)) => Ok(it.collect()),
                    _ => Err(REP_HOST),
                }
            }
            _ => Err(REP_ATYP),
        };
        Ok::<_, std::io::Error>((cmd, host))
    };
    let (cmd, host) = tokio::time::timeout(Duration::from_secs(20), hs)
        .await
        .map_err(|_| "handshake timeout".to_string())?
        .map_err(|e| e.to_string())?;
    if cmd != 1 {
        let _ = reply(&mut c, REP_CMD).await;
        return Err(format!("command {cmd} not supported"));
    }
    let addrs = match host {
        Ok(a) => a,
        Err(rep) => {
            let _ = reply(&mut c, rep).await;
            return Err("bad address".into());
        }
    };
    let allowed: Vec<SocketAddr> = addrs.iter().copied().filter(|a| policy.allowed(a)).collect();
    if allowed.is_empty() {
        stats.denied.fetch_add(1, Ordering::Relaxed);
        let _ = reply(&mut c, if addrs.is_empty() { REP_HOST } else { REP_DENIED }).await;
        return Err(format!("denied {addrs:?}"));
    }
    let mut last = REP_FAIL;
    let mut upstream = None;
    for a in allowed.iter().take(4) {
        match tokio::time::timeout(Duration::from_secs(10), connect_out(*a, policy.fwmark)).await {
            Ok(Ok(s)) => {
                upstream = Some(s);
                break;
            }
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => last = REP_REFUSED,
            _ => last = REP_HOST,
        }
    }
    let Some(mut up) = upstream else {
        let _ = reply(&mut c, last).await;
        return Err(format!("connect {allowed:?} failed"));
    };
    let _ = up.set_nodelay(true);
    reply(&mut c, REP_OK).await.map_err(|e| e.to_string())?;
    if let Some(cl) = &client {
        cl.active.fetch_add(1, Ordering::Relaxed);
    }
    let (cr, cw) = c.split();
    let (ur, uw) = up.split();
    // internet -> client is "down", client -> internet is "up".
    let (down_b, up_b): (Vec<&limit::Bucket>, Vec<&limit::Bucket>) = match &client {
        Some(cl) => (vec![&lim.down, &cl.down], vec![&lim.up, &cl.up]),
        None => (vec![&lim.down], vec![&lim.up]),
    };
    let (down_c, up_c): (Vec<&AtomicU64>, Vec<&AtomicU64>) = match &client {
        Some(cl) => (vec![&stats.tx, &cl.tx], vec![&stats.rx, &cl.rx]),
        None => (vec![&stats.tx], vec![&stats.rx]),
    };
    let r = tokio::try_join!(pump(ur, cw, &down_b, &down_c), pump(cr, uw, &up_b, &up_c));
    if let Some(cl) = &client {
        cl.active.fetch_sub(1, Ordering::Relaxed);
    }
    r.map(|_| ()).map_err(|e| e.to_string())
}

// ------------------------------------------------------------------------------------------
// speed test (iperf-like): see the module docs for the protocol
// ------------------------------------------------------------------------------------------

const SPEEDTEST_MAX_SECS: u64 = 30;

async fn speedtest_serve(l: TcpListener) {
    let limit = Arc::new(tokio::sync::Semaphore::new(2));
    loop {
        let Ok((c, _)) = l.accept().await else { continue };
        let Ok(permit) = limit.clone().try_acquire_owned() else {
            continue; // busy: drop the connection
        };
        tokio::spawn(async move {
            if let Err(e) = speedtest_session(c).await {
                log::debug!("speedtest: {e}");
            }
            drop(permit);
        });
    }
}

async fn speedtest_session(mut c: TcpStream) -> std::io::Result<()> {
    use tokio::time::{timeout, timeout_at, Instant};
    let _ = c.set_nodelay(true);
    let mut h = [0u8; 2];
    timeout(Duration::from_secs(10), c.read_exact(&mut h)).await.map_err(|_| std::io::Error::other("no header"))??;
    let secs = (h[1] as u64).clamp(1, SPEEDTEST_MAX_SECS);
    let dur = Duration::from_secs(secs);
    let mut buf = vec![0u8; 64 * 1024];
    match h[0] {
        b'D' => {
            let start = Instant::now();
            let send = async {
                while start.elapsed() < dur {
                    c.write_all(&buf).await?;
                }
                c.shutdown().await
            };
            timeout(dur + Duration::from_secs(10), send).await.map_err(|_| std::io::Error::other("send stalled"))?
        }
        b'U' => {
            // The clock starts with the first byte, so the overlay round trip does not count.
            let n = timeout(Duration::from_secs(10), c.read(&mut buf)).await.map_err(|_| std::io::Error::other("no data"))??;
            if n == 0 {
                return Ok(());
            }
            let first = Instant::now();
            let mut total = n as u64;
            loop {
                match timeout_at(first + dur, c.read(&mut buf)).await {
                    Ok(Ok(0)) | Err(_) => break,
                    Ok(Ok(n)) => total += n as u64,
                    Ok(Err(e)) => return Err(e),
                }
            }
            let ms = first.elapsed().as_millis() as u64;
            let mut rep = [0u8; 16];
            rep[..8].copy_from_slice(&total.to_be_bytes());
            rep[8..].copy_from_slice(&ms.to_be_bytes());
            timeout(Duration::from_secs(10), c.write_all(&rep)).await.map_err(|_| std::io::Error::other("report stalled"))??;
            // Let the client read the report before we close (unread data would turn into a RST).
            let _ = timeout(Duration::from_secs(5), async {
                while let Ok(n) = c.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                }
            })
            .await;
            Ok(())
        }
        m => Err(std::io::Error::other(format!("bad mode {m}"))),
    }
}

// ------------------------------------------------------------------------------------------
// tun: receive the VpnService fd and run tun2proxy
// ------------------------------------------------------------------------------------------

/// Receive one fd over a connected unix stream (SCM_RIGHTS).
fn recv_fd(sock: RawFd) -> std::io::Result<RawFd> {
    unsafe {
        let mut data = [0u8; 16];
        let mut iov = libc::iovec { iov_base: data.as_mut_ptr() as *mut _, iov_len: data.len() };
        let space = libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) as usize;
        let mut cbuf = vec![0u8; space * 4];
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cbuf.as_mut_ptr() as *mut _;
        msg.msg_controllen = cbuf.len() as _;
        loop {
            let n = libc::recvmsg(sock, &mut msg, 0);
            if n < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            if n == 0 {
                return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "control socket closed"));
            }
            let mut cm = libc::CMSG_FIRSTHDR(&msg);
            while !cm.is_null() {
                if (*cm).cmsg_level == libc::SOL_SOCKET && (*cm).cmsg_type == libc::SCM_RIGHTS {
                    let fd = std::ptr::read_unaligned(libc::CMSG_DATA(cm) as *const RawFd);
                    return Ok(fd);
                }
                cm = libc::CMSG_NXTHDR(&msg, cm);
            }
            // Data without an fd: keep waiting.
        }
    }
}

unsafe extern "C" fn traffic_cb(st: *const tun2proxy::TrafficStatus, _ctx: *mut std::ffi::c_void) {
    if let Some(st) = unsafe { st.as_ref() } {
        out(&format!("STAT tun tx={} rx={}", st.tx, st.rx));
    }
}

fn tun_main(rt: &tokio::runtime::Runtime, o: Opts) -> Result<(), String> {
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixListener;

    let ctl = o.ctl.ok_or("tun: --ctl required")?;
    let socks = o.socks.ok_or("tun: --socks required")?;
    let _ = std::fs::remove_file(&ctl);
    let listener = UnixListener::bind(&ctl).map_err(|e| format!("bind {ctl}: {e}"))?;
    out(&format!("READY tun ctl={ctl}"));
    let (stream, _) = listener.accept().map_err(|e| format!("accept ctl: {e}"))?;
    let _ = std::fs::remove_file(&ctl);
    let fd = recv_fd(stream.as_raw_fd()).map_err(|e| format!("recv fd: {e}"))?;
    out(&format!("STATE tun fd={fd}"));

    let mut cli: Vec<String> = vec![
        "tun2proxy".into(),
        "--tun-fd".into(),
        fd.to_string(),
        "--close-fd-on-drop".into(),
        "true".into(),
        "--proxy".into(),
        format!("socks5://{socks}"),
        "--dns".into(),
        "virtual".into(),
        "--mtu".into(),
        o.mtu.to_string(),
        "--max-sessions".into(),
        "2000".into(),
        "--verbosity".into(),
        o.verbosity.clone(),
    ];
    if o.ipv6 {
        cli.push("--ipv6-enabled".into());
    }
    if let Some(gw) = o.udpgw {
        cli.push("--udpgw-server".into());
        cli.push(gw.to_string());
    }
    let args = <tun2proxy::Args as clap::Parser>::try_parse_from(&cli).map_err(|e| format!("tun2proxy args: {e}"))?;

    unsafe { tun2proxy::tun2proxy_set_traffic_status_callback(2, Some(traffic_cb), std::ptr::null_mut()) };

    let token = CancellationToken::new();
    // The app closes the control connection to stop us.
    {
        let t = token.clone();
        let mut s = stream;
        std::thread::spawn(move || {
            use std::io::Read;
            let mut b = [0u8; 64];
            loop {
                match s.read(&mut b) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            t.cancel();
        });
    }
    {
        let t = token.clone();
        rt.spawn(async move {
            if let Ok(mut sig) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                sig.recv().await;
                t.cancel();
            }
        });
    }
    out("STATE tun running");
    let r = rt.block_on(tun2proxy::general_run_async(args, o.mtu, false, token));
    out("STATE tun stopped");
    r.map(|_| ()).map_err(|e| format!("tun2proxy: {e}"))
}
