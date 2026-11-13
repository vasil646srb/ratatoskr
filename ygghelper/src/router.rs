//! Client-side SOCKS5 router in front of tun2proxy: Yggdrasil next to the normal internet.
//!
//! - `200::/7` (node addresses and `300::/8` subnets) → the local node's SOCKS (`--ygg-socks`),
//!   straight into Yggdrasil, never through the exit (exits refuse Yggdrasil destinations).
//! - Names in the ALFIS zones (`.ygg`, `.anon`, …) are resolved by ALFIS resolvers inside
//!   Yggdrasil (DNS over TCP through the node); a Yggdrasil answer goes to the node, any other
//!   address to the internet.
//! - DNS over TCP to port 53 (tun2proxy `--dns over-tcp`, Windows) is split per query: ALFIS
//!   zones to an ALFIS resolver, the rest to the usual resolver.
//! - Everything else goes to the exit (`--socks`) unchanged, or straight to the internet in the
//!   Yggdrasil-only mode (`--no-exit`), where only Yggdrasil traffic is routed into the tunnel.
//!
//! TCP only: the node's SOCKS carries TCP (sites, SSH, IRC, Matrix…), not UDP or ping.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// ALFIS domain zones (github.com/Revertron/Alfis).
pub const ZONES: &[&str] = &["ygg", "anon", "btn", "conf", "index", "merch", "mirror", "mob", "screen", "srv"];

/// Public resolvers with the ALFIS zones, reachable over Yggdrasil (checked 2026-11).
pub const RESOLVERS: &[&str] = &[
    "300:7c75:d590:c5d8::100",
    "202:cbff:6f7f:fa95:c6e9:3768:a96e:d1c8",
    "301:3423:a49b:f4e0::100",
    "302:7991::53",
    "302:db60::53",
    "300:6223::53",
];

pub fn is_ygg(ip: &IpAddr) -> bool {
    matches!(ip, IpAddr::V6(v6) if v6.octets()[0] & 0xfe == 0x02)
}

pub fn is_alfis(name: &str) -> bool {
    let n = name.trim_end_matches('.').to_ascii_lowercase();
    n.rsplit('.').next().is_some_and(|tld| n.contains('.') && ZONES.contains(&tld))
}

pub struct Router {
    /// The exit server's SOCKS (through the node); None: Yggdrasil-only, the rest goes direct.
    pub exit: Option<SocketAddr>,
    /// The node's SOCKS into Yggdrasil.
    pub ygg: SocketAddr,
    pub resolvers: Vec<Ipv6Addr>,
    /// The resolver that answered last (tried first).
    good: Mutex<usize>,
    /// name → (addresses, until).
    cache: Mutex<HashMap<String, (Vec<IpAddr>, Instant)>>,
}

impl Router {
    pub fn new(exit: Option<SocketAddr>, ygg: SocketAddr, resolvers: Vec<Ipv6Addr>) -> Router {
        Router { exit, ygg, resolvers, good: Mutex::new(0), cache: Default::default() }
    }

    /// Listen on a free loopback port; returns it (for tun2proxy's `--proxy`).
    pub async fn start(self: Arc<Self>) -> std::io::Result<SocketAddr> {
        let l = TcpListener::bind("127.0.0.1:0").await?;
        let addr = l.local_addr()?;
        tokio::spawn(async move {
            while let Ok((c, _)) = l.accept().await {
                let r = self.clone();
                tokio::spawn(async move {
                    if let Err(e) = r.session(c).await {
                        log::debug!("router: {e}");
                    }
                });
            }
        });
        Ok(addr)
    }

    async fn session(&self, mut c: TcpStream) -> std::io::Result<()> {
        let _ = c.set_nodelay(true);
        let mut h = [0u8; 2];
        c.read_exact(&mut h).await?;
        let mut m = vec![0u8; h[1] as usize];
        c.read_exact(&mut m).await?;
        c.write_all(&[5, 0]).await?;
        // The request, kept as sent (replayed to the exit unchanged).
        let mut req = vec![0u8; 4];
        c.read_exact(&mut req).await?;
        let (cmd, atyp) = (req[1], req[3]);
        let host = match atyp {
            1 => {
                let mut b = [0u8; 4];
                c.read_exact(&mut b).await?;
                req.extend_from_slice(&b);
                Host::Ip(IpAddr::from(b))
            }
            4 => {
                let mut b = [0u8; 16];
                c.read_exact(&mut b).await?;
                req.extend_from_slice(&b);
                Host::Ip(IpAddr::from(b))
            }
            3 => {
                let mut l = [0u8; 1];
                c.read_exact(&mut l).await?;
                let mut n = vec![0u8; l[0] as usize];
                c.read_exact(&mut n).await?;
                req.push(l[0]);
                req.extend_from_slice(&n);
                let name = String::from_utf8_lossy(&n).trim_matches(|c| c == '[' || c == ']').to_string();
                match name.parse::<IpAddr>() {
                    Ok(ip) => Host::Ip(ip),
                    Err(_) => Host::Name(name),
                }
            }
            _ => return reply(&mut c, 8).await,
        };
        let mut p = [0u8; 2];
        c.read_exact(&mut p).await?;
        req.extend_from_slice(&p);
        let port = u16::from_be_bytes(p);
        if cmd != 1 {
            return reply(&mut c, 7).await;
        }

        let target: Option<Ipv6Addr> = match &host {
            Host::Ip(IpAddr::V6(v6)) if is_ygg(&IpAddr::V6(*v6)) => Some(*v6),
            Host::Name(n) if is_alfis(n) => match self.resolve(n).await {
                Some(ips) => match ips.iter().find(|i| is_ygg(i)) {
                    Some(IpAddr::V6(v6)) => Some(*v6),
                    _ => {
                        // An ALFIS name with an internet address: on to the internet by address.
                        let ip = ips[0];
                        return self.internet(c, Host::Ip(ip), port, None).await;
                    }
                },
                None => return reply(&mut c, 4).await,
            },
            _ => None,
        };
        if let Some(a) = target {
            return self.to_ygg(c, a, port).await;
        }
        if port == 53 && matches!(host, Host::Ip(_)) {
            return self.dns_split(c, host, &req).await;
        }
        self.internet(c, host, port, Some(&req)).await
    }

    /// Into Yggdrasil through the node.
    async fn to_ygg(&self, mut c: TcpStream, a: Ipv6Addr, port: u16) -> std::io::Result<()> {
        let mut u = match ygg_connect(self.ygg, a, port).await {
            Ok(u) => u,
            Err(_) => return reply(&mut c, 4).await,
        };
        reply(&mut c, 0).await?;
        let _ = tokio::io::copy_bidirectional(&mut c, &mut u).await;
        Ok(())
    }

    /// To the internet: through the exit (the request replayed), or directly without one.
    async fn internet(&self, mut c: TcpStream, host: Host, port: u16, req: Option<&[u8]>) -> std::io::Result<()> {
        match self.exit {
            Some(exit) => {
                let mut u = TcpStream::connect(exit).await?;
                let _ = u.set_nodelay(true);
                u.write_all(&[5, 1, 0]).await?;
                let mut g = [0u8; 2];
                u.read_exact(&mut g).await?;
                if g[1] != 0 {
                    return reply(&mut c, 2).await;
                }
                let built;
                let r = match req {
                    Some(r) => r,
                    None => {
                        built = socks_request(&host, port);
                        &built
                    }
                };
                u.write_all(r).await?;
                // The exit's reply reaches the client through the copy.
                let _ = tokio::io::copy_bidirectional(&mut c, &mut u).await;
                Ok(())
            }
            None => {
                let addrs: Vec<SocketAddr> = match host {
                    Host::Ip(ip) => vec![SocketAddr::new(ip, port)],
                    Host::Name(n) => match tokio::net::lookup_host((n.as_str(), port)).await {
                        Ok(it) => it.collect(),
                        Err(_) => return reply(&mut c, 4).await,
                    },
                };
                for a in addrs.into_iter().take(4) {
                    if let Ok(Ok(mut u)) = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(a)).await {
                        let _ = u.set_nodelay(true);
                        reply(&mut c, 0).await?;
                        let _ = tokio::io::copy_bidirectional(&mut c, &mut u).await;
                        return Ok(());
                    }
                }
                reply(&mut c, 4).await
            }
        }
    }

    /// DNS over TCP: each query to ALFIS (its zones) or to the usual resolver.
    async fn dns_split(&self, mut c: TcpStream, host: Host, req: &[u8]) -> std::io::Result<()> {
        reply(&mut c, 0).await?;
        let mut upstream: Option<TcpStream> = None;
        loop {
            let mut l = [0u8; 2];
            if c.read_exact(&mut l).await.is_err() {
                return Ok(());
            }
            let mut q = vec![0u8; u16::from_be_bytes(l) as usize];
            c.read_exact(&mut q).await?;
            let answer = if qname(&q).is_some_and(|n| is_alfis(&n)) {
                self.alfis_raw(&q).await
            } else {
                if upstream.is_none() {
                    upstream = self.dns_upstream(&host, req).await.ok();
                }
                match upstream.as_mut() {
                    Some(u) => match dns_exchange(u, &q).await {
                        Ok(a) => Some(a),
                        Err(_) => {
                            upstream = None;
                            None
                        }
                    },
                    None => None,
                }
            };
            let a = answer.unwrap_or_else(|| servfail(&q));
            c.write_all(&(a.len() as u16).to_be_bytes()).await?;
            c.write_all(&a).await?;
        }
    }

    async fn dns_upstream(&self, host: &Host, req: &[u8]) -> std::io::Result<TcpStream> {
        let Host::Ip(ip) = host else { return Err(std::io::Error::other("dns: name")) };
        match self.exit {
            Some(exit) => {
                let mut u = TcpStream::connect(exit).await?;
                u.write_all(&[5, 1, 0]).await?;
                let mut g = [0u8; 2];
                u.read_exact(&mut g).await?;
                u.write_all(req).await?;
                read_reply(&mut u).await?;
                Ok(u)
            }
            None => TcpStream::connect(SocketAddr::new(*ip, 53)).await,
        }
    }

    /// One raw DNS query to the ALFIS resolvers (the one that answered last first).
    async fn alfis_raw(&self, q: &[u8]) -> Option<Vec<u8>> {
        let n = self.resolvers.len();
        let first = *self.good.lock().unwrap();
        for k in 0..n {
            let i = (first + k) % n;
            let r = tokio::time::timeout(Duration::from_secs(12), async {
                let mut u = ygg_connect(self.ygg, self.resolvers[i], 53).await?;
                dns_exchange(&mut u, q).await
            })
            .await;
            if let Ok(Ok(a)) = r {
                *self.good.lock().unwrap() = i;
                return Some(a);
            }
        }
        None
    }

    /// AAAA (then A) of an ALFIS name, cached for 5 minutes.
    pub async fn resolve(&self, name: &str) -> Option<Vec<IpAddr>> {
        let key = name.trim_end_matches('.').to_ascii_lowercase();
        if let Some((ips, until)) = self.cache.lock().unwrap().get(&key)
            && *until > Instant::now() {
                return Some(ips.clone());
            }
        let mut ips = Vec::new();
        for qtype in [28u16, 1] {
            if let Some(a) = self.alfis_raw(&query(&key, qtype)).await {
                ips.extend(answers(&a));
            }
            if !ips.is_empty() {
                break;
            }
        }
        if ips.is_empty() {
            return None;
        }
        self.cache.lock().unwrap().insert(key, (ips.clone(), Instant::now() + Duration::from_secs(300)));
        Some(ips)
    }
}

enum Host {
    Ip(IpAddr),
    Name(String),
}

async fn reply(c: &mut TcpStream, rep: u8) -> std::io::Result<()> {
    c.write_all(&[5, rep, 0, 1, 0, 0, 0, 0, 0, 0]).await
}

fn socks_request(host: &Host, port: u16) -> Vec<u8> {
    let mut r = vec![5, 1, 0];
    match host {
        Host::Ip(IpAddr::V4(v4)) => {
            r.push(1);
            r.extend_from_slice(&v4.octets());
        }
        Host::Ip(IpAddr::V6(v6)) => {
            r.push(4);
            r.extend_from_slice(&v6.octets());
        }
        Host::Name(n) => {
            r.push(3);
            r.push(n.len().min(255) as u8);
            r.extend_from_slice(&n.as_bytes()[..n.len().min(255)]);
        }
    }
    r.extend_from_slice(&port.to_be_bytes());
    r
}

/// Read a SOCKS5 reply (any address type); error unless it succeeded.
async fn read_reply(u: &mut TcpStream) -> std::io::Result<()> {
    let mut r = [0u8; 4];
    u.read_exact(&mut r).await?;
    let n = match r[3] {
        1 => 6,
        4 => 18,
        3 => {
            let mut l = [0u8; 1];
            u.read_exact(&mut l).await?;
            l[0] as usize + 2
        }
        _ => return Err(std::io::Error::other("bad SOCKS reply")),
    };
    let mut rest = vec![0u8; n];
    u.read_exact(&mut rest).await?;
    if r[1] != 0 {
        return Err(std::io::Error::other(format!("SOCKS reply {}", r[1])));
    }
    Ok(())
}

/// A TCP connection into Yggdrasil through the node's SOCKS.
async fn ygg_connect(node: SocketAddr, a: Ipv6Addr, port: u16) -> std::io::Result<TcpStream> {
    let mut u = TcpStream::connect(node).await?;
    let _ = u.set_nodelay(true);
    u.write_all(&[5, 1, 0]).await?;
    let mut g = [0u8; 2];
    u.read_exact(&mut g).await?;
    let mut r = vec![5, 1, 0, 4];
    r.extend_from_slice(&a.octets());
    r.extend_from_slice(&port.to_be_bytes());
    u.write_all(&r).await?;
    read_reply(&mut u).await?;
    Ok(u)
}

async fn dns_exchange(u: &mut TcpStream, q: &[u8]) -> std::io::Result<Vec<u8>> {
    u.write_all(&(q.len() as u16).to_be_bytes()).await?;
    u.write_all(q).await?;
    let mut l = [0u8; 2];
    u.read_exact(&mut l).await?;
    let mut a = vec![0u8; u16::from_be_bytes(l) as usize];
    u.read_exact(&mut a).await?;
    Ok(a)
}

/// A DNS query for `name` (recursion desired).
pub fn query(name: &str, qtype: u16) -> Vec<u8> {
    let mut q = vec![0x52, 0x54, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for l in name.split('.').filter(|l| !l.is_empty()) {
        q.push(l.len().min(63) as u8);
        q.extend_from_slice(&l.as_bytes()[..l.len().min(63)]);
    }
    q.push(0);
    q.extend_from_slice(&qtype.to_be_bytes());
    q.extend_from_slice(&[0, 1]);
    q
}

/// Skip a (possibly compressed) name at `i`; the index after it.
fn skip_name(m: &[u8], mut i: usize) -> Option<usize> {
    loop {
        let l = *m.get(i)? as usize;
        if l == 0 {
            return Some(i + 1);
        }
        if l & 0xc0 == 0xc0 {
            return Some(i + 2);
        }
        i += l + 1;
    }
}

/// The question's name of a DNS message.
pub fn qname(m: &[u8]) -> Option<String> {
    let mut i = 12;
    let mut parts = Vec::new();
    loop {
        let l = *m.get(i)? as usize;
        if l == 0 {
            break;
        }
        if l & 0xc0 != 0 {
            return None;
        }
        parts.push(String::from_utf8_lossy(m.get(i + 1..i + 1 + l)?).to_string());
        i += l + 1;
    }
    Some(parts.join("."))
}

/// A and AAAA addresses in a DNS answer.
pub fn answers(m: &[u8]) -> Vec<IpAddr> {
    let mut out = Vec::new();
    if m.len() < 12 || m[3] & 0x0f != 0 {
        return out;
    }
    let qd = u16::from_be_bytes([m[4], m[5]]);
    let an = u16::from_be_bytes([m[6], m[7]]);
    let mut i = 12;
    for _ in 0..qd {
        let Some(j) = skip_name(m, i) else { return out };
        i = j + 4;
    }
    for _ in 0..an {
        let Some(j) = skip_name(m, i) else { return out };
        let Some(h) = m.get(j..j + 10) else { return out };
        let (t, len) = (u16::from_be_bytes([h[0], h[1]]), u16::from_be_bytes([h[8], h[9]]) as usize);
        let Some(d) = m.get(j + 10..j + 10 + len) else { return out };
        match (t, len) {
            (28, 16) => out.push(IpAddr::from(<[u8; 16]>::try_from(d).unwrap())),
            (1, 4) => out.push(IpAddr::from(<[u8; 4]>::try_from(d).unwrap())),
            _ => {}
        }
        i = j + 10 + len;
    }
    out
}

/// SERVFAIL for a query nobody answered.
fn servfail(q: &[u8]) -> Vec<u8> {
    let mut a = q.to_vec();
    if a.len() >= 12 {
        a[2] |= 0x80; // response
        a[3] = (a[3] & 0xf0) | 0x80 | 2; // recursion available, SERVFAIL
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zones_and_addresses() {
        assert!(is_alfis("wiki.ygg") && is_alfis("Search.YGG.") && is_alfis("a.b.anon"));
        assert!(!is_alfis("ygg") && !is_alfis("example.com") && !is_alfis("ygg.com"));
        assert!(is_ygg(&"200::1".parse().unwrap()) && is_ygg(&"326:c728::2".parse().unwrap()));
        assert!(!is_ygg(&"2001:db8::1".parse().unwrap()) && !is_ygg(&"1.2.3.4".parse().unwrap()));
    }

    #[test]
    fn dns_messages() {
        let q = query("wiki.ygg", 28);
        assert_eq!(qname(&q).as_deref(), Some("wiki.ygg"));
        // An answer: the question, then one AAAA with a compressed name.
        let mut a = q.clone();
        a[2] = 0x81;
        a[3] = 0x80;
        a[7] = 1;
        a.extend_from_slice(&[0xc0, 12, 0, 28, 0, 1, 0, 0, 0, 60, 0, 16]);
        a.extend_from_slice(&"202:976c::1".parse::<Ipv6Addr>().unwrap().octets());
        assert_eq!(answers(&a), vec!["202:976c::1".parse::<IpAddr>().unwrap()]);
        assert_eq!(servfail(&q)[3] & 0x0f, 2);
    }
}
