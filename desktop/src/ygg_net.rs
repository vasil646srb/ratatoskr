//! An embedded Yggdrasil node (yggdrasil-ng as a library) with a TCP stack of its own (smoltcp):
//! the tracker crawls the network, probes exits and serves its API without yggdrasil-go, TUN or
//! root, next to any other Yggdrasil node on the same host.
//!
//! Threads: a tokio runtime runs the node and moves IPv6 packets between it and the stack thread;
//! the stack thread owns the smoltcp interface. Callers get blocking `NetStream`s (Read + Write).

use smoltcp::iface::{Config as IfConfig, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::tcp;
use smoltcp::time::Instant as SInstant;
use smoltcp::wire::{HardwareAddress, IpAddress, IpCidr, IpEndpoint};
use std::collections::{HashMap, VecDeque};
use std::io::{ErrorKind, Read, Write};
use std::net::Ipv6Addr;
use std::path::Path;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use yggdrasil::core::Core;
use yggdrasil::ipv6rwc::ReadWriteCloser;

/// MTU of our TCP stack (MSS 1340): safe on every Yggdrasil path.
const MTU: usize = 1400;
const BUF: usize = 64 * 1024;
/// Listening sockets kept open per listened port (accepted ones are replaced at once).
const BACKLOG: usize = 16;

pub struct PeerInfo {
    pub uri: String,
    pub key: [u8; 32],
    pub up: bool,
    pub inbound: bool,
}

/// The node and its TCP stack.
pub struct Net {
    rt: tokio::runtime::Runtime,
    core: Arc<Core>,
    ev: mpsc::Sender<Ev>,
    address: Ipv6Addr,
}

enum Ev {
    Packet(Vec<u8>),
    Connect { addr: Ipv6Addr, port: u16, timeout: Duration, reply: mpsc::Sender<std::io::Result<NetStream>> },
    Listen { port: u16, accept: mpsc::Sender<NetStream> },
    Write { id: u64, data: Vec<u8> },
    Close { id: u64 },
}

enum Chunk {
    Data(Vec<u8>),
    End,
    Error(std::io::Error),
}

/// A TCP connection over Yggdrasil. Blocking, like `std::net::TcpStream`.
pub struct NetStream {
    /// The other side's address (accepted connections).
    pub remote: Option<Ipv6Addr>,
    id: u64,
    ev: mpsc::Sender<Ev>,
    rx: mpsc::Receiver<Chunk>,
    pending: Vec<u8>,
    eof: bool,
    read_timeout: Option<Duration>,
}

impl NetStream {
    pub fn set_read_timeout(&mut self, t: Option<Duration>) {
        self.read_timeout = t;
    }
}

impl Read for NetStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while self.pending.is_empty() {
            if self.eof {
                return Ok(0);
            }
            let c = match self.read_timeout {
                Some(t) => self.rx.recv_timeout(t).map_err(|e| match e {
                    mpsc::RecvTimeoutError::Timeout => std::io::Error::new(ErrorKind::TimedOut, "read timed out"),
                    mpsc::RecvTimeoutError::Disconnected => std::io::Error::new(ErrorKind::ConnectionAborted, "stack stopped"),
                })?,
                None => self.rx.recv().map_err(|_| std::io::Error::new(ErrorKind::ConnectionAborted, "stack stopped"))?,
            };
            match c {
                Chunk::Data(d) => self.pending = d,
                Chunk::End => self.eof = true,
                Chunk::Error(e) => {
                    self.eof = true;
                    return Err(e);
                }
            }
        }
        let n = buf.len().min(self.pending.len());
        buf[..n].copy_from_slice(&self.pending[..n]);
        self.pending.drain(..n);
        Ok(n)
    }
}

impl Write for NetStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.ev
            .send(Ev::Write { id: self.id, data: buf.to_vec() })
            .map_err(|_| std::io::Error::new(ErrorKind::BrokenPipe, "stack stopped"))?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for NetStream {
    fn drop(&mut self) {
        let _ = self.ev.send(Ev::Close { id: self.id });
    }
}

/// Accepted connections of `Net::listen`.
pub struct NetListener {
    rx: mpsc::Receiver<NetStream>,
}

impl NetListener {
    pub fn accept(&self) -> Option<NetStream> {
        self.rx.recv().ok()
    }
}

/// The node key: a yggdrasil-ng private key (hex) in `path`, created on first start.
fn load_key(path: &Path) -> Result<String, String> {
    if let Ok(t) = std::fs::read_to_string(path) {
        let t = t.trim().to_string();
        if t.len() == 128 {
            return Ok(t);
        }
        return Err(format!("{}: ожидается ключ из 128 hex-символов", path.display()));
    }
    let key = yggdrasil::config::Config::generate().private_key;
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    std::fs::write(path, format!("{key}\n")).map_err(|e| format!("{}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(key)
}

impl Net {
    /// Start the node with `peers` (tcp:// / tls:// / quic:// URIs) and its key in `key_path`.
    pub fn start(key_path: &Path, peers: Vec<String>) -> Result<Arc<Net>, String> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("ygg-node")
            .build()
            .map_err(|e| e.to_string())?;
        let mut cfg = yggdrasil::config::Config::default();
        cfg.private_key = load_key(key_path)?;
        cfg.peers = peers;
        cfg.listen = Vec::new(); // outbound peers only: nothing to open on this host
        cfg.if_name = "none".into();
        let signing = cfg.signing_key()?;
        let (ev_tx, ev_rx) = mpsc::channel::<Ev>();
        let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        let (core, address) = rt.block_on(async {
            let core = Core::new(signing, cfg);
            core.init_links().await;
            core.start().await;
            let mtu = core.mtu();
            let rwc = ReadWriteCloser::new(core.clone(), mtu, None);
            core.set_path_notify(rwc.clone());
            // Directly connected peers need no lookup (as yggdrasil-ng's own daemon does).
            {
                let (core, rwc) = (core.clone(), rwc.clone());
                tokio::spawn(async move {
                    let mut t = tokio::time::interval(Duration::from_secs(2));
                    loop {
                        t.tick().await;
                        for k in core.get_peer_keys().await {
                            rwc.update_key(k).await;
                        }
                    }
                });
            }
            // Node -> stack. Reading also runs the node's protocol handler (remote getPeers).
            {
                let (rwc, ev) = (rwc.clone(), ev_tx.clone());
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 65536];
                    loop {
                        match rwc.read(&mut buf).await {
                            Ok(p) => {
                                if ev.send(Ev::Packet(p.to_vec())).is_err() {
                                    return;
                                }
                            }
                            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                        }
                    }
                });
            }
            // Stack -> node.
            {
                let rwc = rwc.clone();
                tokio::spawn(async move {
                    while let Some(p) = out_rx.recv().await {
                        let _ = rwc.write(&p).await;
                    }
                });
            }
            let address: Ipv6Addr = core.address().0.into();
            (core, address)
        });
        let ev_for_streams = ev_tx.clone();
        std::thread::Builder::new()
            .name("ygg-stack".into())
            .spawn(move || stack(address, ev_rx, ev_for_streams, out_tx))
            .map_err(|e| e.to_string())?;
        Ok(Arc::new(Net { rt, core, ev: ev_tx, address }))
    }

    pub fn address(&self) -> Ipv6Addr {
        self.address
    }

    pub fn public_key_hex(&self) -> String {
        self.core.public_key().iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn peers(&self) -> Vec<PeerInfo> {
        self.rt
            .block_on(self.core.get_peers())
            .into_iter()
            .map(|p| PeerInfo { uri: p.uri, key: p.key, up: p.up, inbound: p.inbound })
            .collect()
    }

    /// Keys of our peers and of the spanning tree we know: where a crawl starts.
    pub fn seed_keys(&self) -> Vec<[u8; 32]> {
        let mut v = self.rt.block_on(self.core.get_peer_keys());
        v.extend(self.rt.block_on(self.core.get_tree_keys()));
        v
    }

    /// Peers of a remote node (the debug getPeers request, as `debug_remotegetpeers`).
    pub fn remote_peers(&self, key: [u8; 32]) -> Result<Vec<[u8; 32]>, String> {
        self.rt.block_on(self.core.proto_handler().send_get_peers_request(key))
    }

    pub fn connect(&self, addr: Ipv6Addr, port: u16, timeout: Duration) -> std::io::Result<NetStream> {
        let (tx, rx) = mpsc::channel();
        self.ev
            .send(Ev::Connect { addr, port, timeout, reply: tx })
            .map_err(|_| std::io::Error::new(ErrorKind::NotConnected, "stack stopped"))?;
        rx.recv_timeout(timeout + Duration::from_secs(5))
            .map_err(|_| std::io::Error::new(ErrorKind::TimedOut, "connect timed out"))?
    }

    pub fn listen(&self, port: u16) -> std::io::Result<NetListener> {
        let (tx, rx) = mpsc::channel();
        self.ev
            .send(Ev::Listen { port, accept: tx })
            .map_err(|_| std::io::Error::new(ErrorKind::NotConnected, "stack stopped"))?;
        Ok(NetListener { rx })
    }
}

// ------------------------------------------------------------------------------------------
// The TCP stack thread

struct QueueDev {
    rx: VecDeque<Vec<u8>>,
    tx: Vec<Vec<u8>>,
}

struct Rx(Vec<u8>);
struct Tx<'a>(&'a mut Vec<Vec<u8>>);

impl RxToken for Rx {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

impl TxToken for Tx<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut b = vec![0u8; len];
        let r = f(&mut b);
        self.0.push(b);
        r
    }
}

impl Device for QueueDev {
    type RxToken<'a> = Rx;
    type TxToken<'a> = Tx<'a>;

    fn receive(&mut self, _: SInstant) -> Option<(Rx, Tx<'_>)> {
        let p = self.rx.pop_front()?;
        Some((Rx(p), Tx(&mut self.tx)))
    }

    fn transmit(&mut self, _: SInstant) -> Option<Tx<'_>> {
        Some(Tx(&mut self.tx))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut c = DeviceCapabilities::default();
        c.medium = Medium::Ip;
        c.max_transmission_unit = MTU;
        c
    }
}

enum Phase {
    Connecting { deadline: Instant, reply: mpsc::Sender<std::io::Result<NetStream>> },
    Open,
    /// Our side closed (FIN sent); dropped once the socket is closed or after `until`.
    Closing { until: Instant },
}

struct Conn {
    handle: SocketHandle,
    to_user: mpsc::Sender<Chunk>,
    out: VecDeque<u8>,
    phase: Phase,
    user_closed: bool,
    ended: bool,
}

struct Listener {
    port: u16,
    accept: mpsc::Sender<NetStream>,
    handles: Vec<SocketHandle>,
}

fn new_socket() -> tcp::Socket<'static> {
    let mut s = tcp::Socket::new(tcp::SocketBuffer::new(vec![0u8; BUF]), tcp::SocketBuffer::new(vec![0u8; BUF]));
    s.set_timeout(Some(smoltcp::time::Duration::from_secs(60)));
    s.set_keep_alive(Some(smoltcp::time::Duration::from_secs(20)));
    s
}

fn now_s(t0: Instant) -> SInstant {
    SInstant::from_millis(t0.elapsed().as_millis() as i64)
}

fn stack(me: Ipv6Addr, ev: mpsc::Receiver<Ev>, stream_tx: mpsc::Sender<Ev>, out: tokio::sync::mpsc::UnboundedSender<Vec<u8>>) {
    let t0 = Instant::now();
    let mut dev = QueueDev { rx: VecDeque::new(), tx: Vec::new() };
    let mut iface = Interface::new(IfConfig::new(HardwareAddress::Ip), &mut dev, now_s(t0));
    iface.update_ip_addrs(|a| {
        // 200::/7 is "on link": every Yggdrasil address is reached through the node.
        let _ = a.push(IpCidr::new(IpAddress::Ipv6(me), 7));
    });
    let mut sockets = SocketSet::new(Vec::new());
    let mut conns: HashMap<u64, Conn> = HashMap::new();
    let mut listeners: Vec<Listener> = Vec::new();
    let mut next_id: u64 = 1;
    let mut next_port: u16 = 33000;

    // YGGPROXY_NET_DEBUG=1: packet counters every 10 s on stderr.
    let debug = std::env::var_os("YGGPROXY_NET_DEBUG").is_some();
    let (mut n_in, mut n_out, mut last_dbg) = (0u64, 0u64, Instant::now());
    let mut wait = Duration::from_millis(1);
    loop {
        if debug && last_dbg.elapsed() > Duration::from_secs(10) {
            last_dbg = Instant::now();
            eprintln!("[net] пакетов из сети {n_in}, в сеть {n_out}, соединений {}", conns.len());
        }
        // 1. Events: block for the first one up to `wait`, then take everything queued.
        let mut batch = Vec::new();
        match ev.recv_timeout(wait) {
            Ok(e) => batch.push(e),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        batch.extend(ev.try_iter());
        for e in batch {
            match e {
                Ev::Packet(p) => {
                    n_in += 1;
                    if debug && n_in <= 20 {
                        eprintln!("[net] in {} байт: {}", p.len(), describe(&p));
                    }
                    dev.rx.push_back(p)
                }
                Ev::Connect { addr, port, timeout, reply } => {
                    let mut s = new_socket();
                    next_port = if next_port >= 60999 { 33000 } else { next_port + 1 };
                    let local = next_port;
                    let remote = IpEndpoint::new(IpAddress::Ipv6(addr), port);
                    match s.connect(iface.context(), remote, local) {
                        Ok(()) => {
                            let handle = sockets.add(s);
                            let (to_user, from_stack) = mpsc::channel();
                            let id = next_id;
                            next_id += 1;
                            let stream = NetStream { remote: Some(addr), id, ev: stream_tx.clone(), rx: from_stack, pending: Vec::new(), eof: false, read_timeout: None };
                            // The stream is handed over once connected (kept in PENDING meanwhile).
                            conns.insert(id, Conn {
                                handle,
                                to_user,
                                out: VecDeque::new(),
                                phase: Phase::Connecting { deadline: Instant::now() + timeout, reply },
                                user_closed: false,
                                ended: false,
                            });
                            PENDING.with(|p| p.borrow_mut().insert(id, stream));
                        }
                        Err(e) => {
                            let _ = reply.send(Err(std::io::Error::other(format!("connect: {e}"))));
                        }
                    }
                }
                Ev::Listen { port, accept } => {
                    let mut handles = Vec::new();
                    for _ in 0..BACKLOG {
                        let mut s = new_socket();
                        if s.listen(port).is_ok() {
                            handles.push(sockets.add(s));
                        }
                    }
                    listeners.push(Listener { port, accept, handles });
                }
                Ev::Write { id, data } => {
                    if let Some(c) = conns.get_mut(&id) {
                        c.out.extend(data);
                    }
                }
                Ev::Close { id } => {
                    if let Some(c) = conns.get_mut(&id) {
                        c.user_closed = true;
                    }
                }
            }
        }

        // 2. Run TCP.
        let now = now_s(t0);
        iface.poll(now, &mut dev, &mut sockets);

        // 3. Listeners: established sockets become connections, fresh ones take their place.
        for l in &mut listeners {
            for h in l.handles.iter_mut() {
                let s = sockets.get_mut::<tcp::Socket>(*h);
                if s.is_listening() {
                    continue;
                }
                let accepted = *h;
                let remote = s.remote_endpoint().and_then(|e| match e.addr {
                    IpAddress::Ipv6(a) => Some(a),
                    #[allow(unreachable_patterns)]
                    _ => None,
                });
                let mut fresh = new_socket();
                let _ = fresh.listen(l.port);
                *h = sockets.add(fresh);
                let (to_user, from_stack) = mpsc::channel();
                let id = next_id;
                next_id += 1;
                let stream = NetStream { remote, id, ev: stream_tx.clone(), rx: from_stack, pending: Vec::new(), eof: false, read_timeout: None };
                conns.insert(id, Conn { handle: accepted, to_user, out: VecDeque::new(), phase: Phase::Open, user_closed: false, ended: false });
                let _ = l.accept.send(stream);
            }
        }

        // 4. Connections: deliver data, push queued output, finish closes.
        let mut gone = Vec::new();
        for (id, c) in conns.iter_mut() {
            let s = sockets.get_mut::<tcp::Socket>(c.handle);
            if let Phase::Connecting { deadline, .. } = &c.phase {
                if s.state() == tcp::State::Established {
                    if let Phase::Connecting { reply, .. } = std::mem::replace(&mut c.phase, Phase::Open)
                        && let Some(stream) = PENDING.with(|p| p.borrow_mut().remove(id))
                            && reply.send(Ok(stream)).is_err() {
                                c.user_closed = true;
                            }
                } else if s.state() == tcp::State::Closed || Instant::now() > *deadline {
                    let refused = s.state() == tcp::State::Closed;
                    s.abort();
                    if let Phase::Connecting { reply, .. } = std::mem::replace(&mut c.phase, Phase::Closing { until: Instant::now() }) {
                        let e = if refused {
                            std::io::Error::new(ErrorKind::ConnectionRefused, "connection refused")
                        } else {
                            std::io::Error::new(ErrorKind::TimedOut, "connect timed out")
                        };
                        let _ = reply.send(Err(e));
                    }
                    PENDING.with(|p| p.borrow_mut().remove(id));
                    gone.push(*id);
                    continue;
                }
            }
            if matches!(c.phase, Phase::Connecting { .. }) {
                continue;
            }
            // Incoming data.
            while s.can_recv() {
                // Straight from the socket's buffer: one allocation of exactly the data's size.
                match s.recv(|data| (data.len(), data.to_vec())) {
                    Ok(data) if !data.is_empty() => {
                        let _ = c.to_user.send(Chunk::Data(data));
                    }
                    _ => break,
                }
            }
            if !c.ended && !s.may_recv() && !matches!(s.state(), tcp::State::Listen | tcp::State::SynReceived) {
                c.ended = true;
                let _ = c.to_user.send(if s.state() == tcp::State::Closed && !c.user_closed {
                    Chunk::Error(std::io::Error::new(ErrorKind::ConnectionReset, "connection reset"))
                } else {
                    Chunk::End
                });
            }
            // Outgoing data.
            while !c.out.is_empty() && s.can_send() {
                let (a, _) = c.out.as_slices();
                match s.send_slice(a) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        c.out.drain(..n);
                    }
                }
            }
            // Close once everything queued has been handed to TCP.
            if c.user_closed && c.out.is_empty() {
                match c.phase {
                    Phase::Open => {
                        s.close();
                        c.phase = Phase::Closing { until: Instant::now() + Duration::from_secs(30) };
                    }
                    Phase::Closing { until } => {
                        if matches!(s.state(), tcp::State::Closed | tcp::State::TimeWait) || Instant::now() > until {
                            s.abort();
                            gone.push(*id);
                        }
                    }
                    Phase::Connecting { .. } => {}
                }
            } else if s.state() == tcp::State::Closed && c.ended {
                // The peer is gone and the user has seen it: wait only for the user's close.
                if c.user_closed {
                    gone.push(*id);
                }
            }
        }
        for id in gone {
            if let Some(c) = conns.remove(&id) {
                sockets.remove(c.handle);
            }
        }

        // 5. Packets out.
        for p in dev.tx.drain(..) {
            n_out += 1;
            if debug && n_out <= 20 {
                eprintln!("[net] out {} байт: {}", p.len(), describe(&p));
            }
            let _ = out.send(p);
        }
        // Sleep until smoltcp has a timer due, or 20 ms while connections are being set up.
        let delay = iface.poll_delay(now_s(t0), &sockets).map(|d| Duration::from_millis(d.total_millis())).unwrap_or(Duration::from_millis(200));
        let busy = conns.values().any(|c| !c.out.is_empty() || matches!(c.phase, Phase::Connecting { .. }));
        wait = if busy { delay.min(Duration::from_millis(20)) } else { delay.min(Duration::from_millis(200)) };
        if !dev.rx.is_empty() {
            wait = Duration::ZERO;
        }
    }
}

/// "src > dst proto" of an IPv6 packet, for debugging.
fn describe(p: &[u8]) -> String {
    if p.len() < 40 || p[0] >> 4 != 6 {
        return format!("не IPv6 (первый байт {:#x})", p.first().copied().unwrap_or(0));
    }
    let src = Ipv6Addr::from(<[u8; 16]>::try_from(&p[8..24]).unwrap());
    let dst = Ipv6Addr::from(<[u8; 16]>::try_from(&p[24..40]).unwrap());
    format!("{src} > {dst} next={}", p[6])
}

thread_local! {
    /// Streams of connections still being set up (handed to the caller once established).
    static PENDING: std::cell::RefCell<HashMap<u64, NetStream>> = std::cell::RefCell::new(HashMap::new());
}
