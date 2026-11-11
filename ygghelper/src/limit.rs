//! Bandwidth limits for the exit: a token bucket per direction for the whole server and one
//! per client (its Yggdrasil address, from the PROXY v1 line tcp_node sends). Rates change live:
//! `--limits FILE` is re-read when it changes, open connections included.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Token bucket. `rate` 0 = unlimited. Burst = 250 ms of traffic (at least 64 KiB), so a
/// limited stream stays smooth instead of arriving in one-second lumps.
pub struct Bucket {
    rate: AtomicU64, // bytes per second
    state: Mutex<(f64, Instant)>,
}

impl Bucket {
    pub fn new(rate: u64) -> Self {
        Self { rate: AtomicU64::new(rate), state: Mutex::new((Self::burst(rate), Instant::now())) }
    }

    pub fn set_rate(&self, rate: u64) {
        self.rate.store(rate, Ordering::Relaxed);
    }

    fn burst(rate: u64) -> f64 {
        (rate as f64 / 4.0).max(64.0 * 1024.0)
    }

    /// Take `n` bytes; returns how long to wait before sending them.
    fn take(&self, n: usize) -> Duration {
        let rate = self.rate.load(Ordering::Relaxed);
        if rate == 0 {
            return Duration::ZERO;
        }
        let mut st = self.state.lock().unwrap();
        let now = Instant::now();
        let burst = Self::burst(rate);
        st.0 = (st.0 + now.duration_since(st.1).as_secs_f64() * rate as f64).min(burst);
        st.1 = now;
        st.0 -= n as f64;
        if st.0 >= 0.0 { Duration::ZERO } else { Duration::from_secs_f64(-st.0 / rate as f64) }
    }
}

/// Wait until every bucket lets `n` bytes through.
pub async fn throttle(buckets: &[&Bucket], n: usize) {
    let wait = buckets.iter().map(|b| b.take(n)).max().unwrap_or(Duration::ZERO);
    if !wait.is_zero() {
        tokio::time::sleep(wait).await;
    }
}

/// Limits in kbit/s (0 = none).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Limits {
    pub total_down: u64,
    pub total_up: u64,
    pub client_down: u64,
    pub client_up: u64,
}

impl Limits {
    /// `key=value` lines (kbit/s); unknown keys and `#` comments are ignored.
    pub fn parse(text: &str) -> Limits {
        let mut l = Limits::default();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((k, v)) = line.split_once('=') else { continue };
            let v: u64 = v.trim().parse().unwrap_or(0);
            match k.trim() {
                "total_down" => l.total_down = v,
                "total_up" => l.total_up = v,
                "client_down" => l.client_down = v,
                "client_up" => l.client_up = v,
                _ => {}
            }
        }
        l
    }
}

fn bps(kbit: u64) -> u64 {
    kbit * 1000 / 8
}

/// One client's buckets and counters.
pub struct Client {
    pub down: Bucket,
    pub up: Bucket,
    pub active: AtomicU64,
    pub tx: AtomicU64, // to the client
    pub rx: AtomicU64, // from the client
}

pub struct Limiter {
    pub down: Bucket,
    pub up: Bucket,
    limits: Mutex<Limits>,
    clients: Mutex<HashMap<IpAddr, Arc<Client>>>,
}

impl Limiter {
    pub fn new(l: Limits) -> Self {
        Self {
            down: Bucket::new(bps(l.total_down)),
            up: Bucket::new(bps(l.total_up)),
            limits: Mutex::new(l),
            clients: Mutex::new(HashMap::new()),
        }
    }

    pub fn limits(&self) -> Limits {
        *self.limits.lock().unwrap()
    }

    /// Apply new limits to the server and every known client.
    pub fn set(&self, l: Limits) {
        *self.limits.lock().unwrap() = l;
        self.down.set_rate(bps(l.total_down));
        self.up.set_rate(bps(l.total_up));
        for c in self.clients.lock().unwrap().values() {
            c.down.set_rate(bps(l.client_down));
            c.up.set_rate(bps(l.client_up));
        }
    }

    /// The client entry for `addr` (created on first use).
    pub fn client(&self, addr: IpAddr) -> Arc<Client> {
        let l = self.limits();
        self.clients
            .lock()
            .unwrap()
            .entry(addr)
            .or_insert_with(|| {
                Arc::new(Client {
                    down: Bucket::new(bps(l.client_down)),
                    up: Bucket::new(bps(l.client_up)),
                    active: AtomicU64::new(0),
                    tx: AtomicU64::new(0),
                    rx: AtomicU64::new(0),
                })
            })
            .clone()
    }

    /// Snapshot of clients (address, active, tx, rx); idle ones with no traffic are forgotten
    /// after they were reported once with active=0.
    pub fn clients(&self) -> Vec<(IpAddr, u64, u64, u64)> {
        let mut map = self.clients.lock().unwrap();
        let out: Vec<_> = map
            .iter()
            .map(|(a, c)| (*a, c.active.load(Ordering::Relaxed), c.tx.load(Ordering::Relaxed), c.rx.load(Ordering::Relaxed)))
            .collect();
        map.retain(|_, c| c.active.load(Ordering::Relaxed) > 0 || Arc::strong_count(c) > 1);
        out
    }
}

/// Re-read `path` every 2 s and apply it when its contents change.
pub async fn watch_file(path: String, lim: Arc<Limiter>) {
    let mut last: Option<String> = None;
    loop {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if last.as_deref() != Some(text.as_str()) {
                let l = Limits::parse(&text);
                if Some(l) != last.as_deref().map(Limits::parse) {
                    lim.set(l);
                    crate::out(&format!(
                        "LIMITS total_down={} total_up={} client_down={} client_up={}",
                        l.total_down, l.total_up, l.client_down, l.client_up
                    ));
                }
                last = Some(text);
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_limits() {
        let l = Limits::parse("# exit\ntotal_down=8000\n total_up = 2000\nclient_down=1000\nfoo=1\n");
        assert_eq!(l, Limits { total_down: 8000, total_up: 2000, client_down: 1000, client_up: 0 });
    }

    #[test]
    fn bucket_paces_to_its_rate() {
        // 1 MB/s: the 250 kB burst passes at once, another 500 kB waits about half a second.
        let b = Bucket::new(1_000_000);
        assert!(b.take(250_000).is_zero());
        let w = b.take(500_000);
        assert!(w > Duration::from_millis(400) && w < Duration::from_millis(700), "{w:?}");
        assert!(Bucket::new(0).take(10_000_000).is_zero());
    }
}
