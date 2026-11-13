//! Peer statistics: how well each Yggdrasil peer actually carried traffic.
//!
//! Clients keep a journal of their own peers (link ping, ping to the exit server while the peer
//! carried the traffic, speed from real traffic, uptime, drops) and rank them; the ranking goes
//! to the node as `--prefer-file`. Clients send anonymous per-peer summaries to the trackers
//! (`POST /report`: no client address, no exit server), trackers merge them with decay, share
//! their own aggregates (`GET /peerstats`) and rank the public peers they serve in `/peers`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

/// A peer counts once it was watched this many 10 s ticks.
pub const MIN_SAMPLES: u64 = 6;
/// Traffic below this is idle chatter, not a speed sample (bytes/s).
const SPEED_MIN_BPS: u64 = 20_000;

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()
}

/// `scheme://host:port` (no `?key=`): the identity of a peer across reports.
pub fn peer_id(uri: &str) -> String {
    uri.split('?').next().unwrap_or(uri).trim().to_string()
}

/// Ranking value in "milliseconds" (lower is better): the ping that matters, made worse by drops
/// and better by speed (up to ×2 at 20 Mbit/s and above).
pub fn score(ping_ms: f64, fail_rate: f64, speed_kbps: f64) -> f64 {
    let mbps = (speed_kbps / 1000.0).min(20.0);
    ping_ms * (1.0 + fail_rate.min(1.0)) / (1.0 + mbps / 20.0)
}

// ------------------------------------------------------------------------------------------
// Client journal

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(default)]
pub struct PeerRecord {
    pub uri: String,
    pub samples: u64,
    pub up_s: u64,
    pub sessions: u64,
    /// Sessions that ended within 2 minutes.
    pub drops: u64,
    pub rtt_sum: f64,
    pub rtt_n: u64,
    /// Ping to the exit server while this peer carried most of the traffic.
    pub srv_sum: f64,
    pub srv_n: u64,
    /// Smoothed speed while it carried traffic (bytes/s).
    pub speed_bps: f64,
    pub last_seen: u64,
    #[serde(skip)]
    pub session_start: Option<u64>,
}

impl PeerRecord {
    pub fn ping_ms(&self) -> Option<f64> {
        if self.srv_n >= 3 {
            Some(self.srv_sum / self.srv_n as f64)
        } else if self.rtt_n > 0 {
            // Without a server sample: link ping plus a typical rest of the path.
            Some(self.rtt_sum / self.rtt_n as f64 + 60.0)
        } else {
            None
        }
    }

    pub fn fail_rate(&self) -> f64 {
        if self.sessions == 0 { 0.0 } else { self.drops as f64 / self.sessions as f64 }
    }

    pub fn score(&self) -> Option<f64> {
        if self.samples < MIN_SAMPLES {
            return None;
        }
        Some(score(self.ping_ms()?, self.fail_rate(), self.speed_bps * 8.0 / 1000.0))
    }
}

#[derive(Serialize, Deserialize, Default, Debug)]
#[serde(default)]
pub struct Journal {
    pub peers: HashMap<String, PeerRecord>,
    pub last_report: u64,
}

impl Journal {
    pub fn load(path: &std::path::Path) -> Journal {
        std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &std::path::Path) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, serde_json::to_vec(self).unwrap_or_default()).is_ok() {
            let _ = std::fs::rename(tmp, path);
        }
    }

    /// One tick (every ~10 s): `/api/peers` of the node and the current ping to the server.
    pub fn tick(&mut self, peers: &Value, server_rtt_ms: Option<f64>) {
        let t = now();
        let list = peers["peers"].as_array().cloned().unwrap_or_default();
        let mut seen = Vec::new();
        for p in &list {
            // Outbound, not LAN: the peers we choose.
            if p["dir"].as_str() != Some("out") || p["lan"].as_bool() == Some(true) {
                continue;
            }
            let uri = p["uri"].as_str().unwrap_or("");
            if uri.is_empty() {
                continue;
            }
            let id = peer_id(uri);
            seen.push(id.clone());
            let r = self.peers.entry(id).or_default();
            r.uri = uri.to_string();
            r.samples += 1;
            r.last_seen = t;
            if r.session_start.is_none() {
                r.session_start = Some(t);
                r.sessions += 1;
            }
            r.up_s += 10;
            if let Some(rtt) = p["rtt_ms"].as_f64() {
                r.rtt_sum += rtt;
                r.rtt_n += 1;
            }
            let bps = p["rx_bps"].as_u64().unwrap_or(0) + p["tx_bps"].as_u64().unwrap_or(0);
            let share = p["share"].as_f64().unwrap_or(0.0);
            if share >= 50.0 {
                if let Some(s) = server_rtt_ms {
                    r.srv_sum += s;
                    r.srv_n += 1;
                }
                if bps >= SPEED_MIN_BPS {
                    r.speed_bps = if r.speed_bps == 0.0 { bps as f64 } else { r.speed_bps * 0.8 + bps as f64 * 0.2 };
                }
            }
        }
        // Sessions that ended since the last tick.
        for (id, r) in self.peers.iter_mut() {
            if !seen.contains(id)
                && let Some(s) = r.session_start.take()
                    && t.saturating_sub(s) < 120 {
                        r.drops += 1;
                    }
        }
        // Forget peers not seen for 30 days.
        self.peers.retain(|_, r| t.saturating_sub(r.last_seen) < 30 * 86400);
    }

    /// Our own ranking, best first: (uri, score).
    pub fn ranked(&self) -> Vec<(String, f64)> {
        let mut v: Vec<(String, f64)> = self.peers.values().filter_map(|r| Some((r.uri.clone(), r.score()?))).collect();
        v.sort_by(|a, b| a.1.total_cmp(&b.1));
        v
    }

    /// The anonymous summary for the trackers (no client address, no exit server).
    pub fn report(&self) -> Value {
        let peers: Vec<Value> = self
            .peers
            .values()
            .filter(|r| r.samples >= MIN_SAMPLES)
            .map(|r| {
                json!({"uri": r.uri, "ping_ms": r.ping_ms().map(|p| p.round()), "speed_kbps": (r.speed_bps * 8.0 / 1000.0).round(),
                       "up_s": r.up_s, "sessions": r.sessions, "drops": r.drops})
            })
            .collect();
        json!({"peers": peers})
    }
}

/// The priority list for the node: our own best peers first, then the trackers' ranking.
pub fn prefer_list(journal: &Journal, tracker_peers: Option<&Value>, max: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut ids: Vec<String> = Vec::new();
    let mut push = |uri: &str, out: &mut Vec<String>| {
        let id = peer_id(uri);
        if !uri.is_empty() && !ids.contains(&id) {
            ids.push(id);
            out.push(uri.to_string());
        }
    };
    for (u, _) in journal.ranked().iter().take(max) {
        push(u, &mut out);
    }
    if let Some(tp) = tracker_peers {
        for p in tp["peers"].as_array().into_iter().flatten().take(max) {
            if out.len() >= max * 2 {
                break;
            }
            push(p["uri"].as_str().unwrap_or(""), &mut out);
        }
    }
    out
}

// ------------------------------------------------------------------------------------------
// Tracker aggregates

/// What reports say about one peer (sums, decayed over time).
#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(default)]
pub struct Agg {
    pub uri: String,
    pub reports: f64,
    pub ping_sum: f64,
    pub ping_n: f64,
    pub speed_sum: f64,
    pub speed_n: f64,
    pub sessions: f64,
    pub drops: f64,
    pub updated: u64,
}

/// Reports lose half their weight every 3 days.
const HALF_LIFE_S: f64 = 3.0 * 86400.0;

impl Agg {
    fn decay(&mut self, t: u64) {
        let age = t.saturating_sub(self.updated) as f64;
        if age > 0.0 && self.updated > 0 {
            let f = 0.5f64.powf(age / HALF_LIFE_S);
            self.reports *= f;
            self.ping_sum *= f;
            self.ping_n *= f;
            self.speed_sum *= f;
            self.speed_n *= f;
            self.sessions *= f;
            self.drops *= f;
        }
        self.updated = t;
    }

    fn add(&mut self, other: &Agg) {
        self.reports += other.reports;
        self.ping_sum += other.ping_sum;
        self.ping_n += other.ping_n;
        self.speed_sum += other.speed_sum;
        self.speed_n += other.speed_n;
        self.sessions += other.sessions;
        self.drops += other.drops;
    }

    pub fn ping_ms(&self) -> Option<f64> {
        (self.ping_n >= 0.5).then(|| self.ping_sum / self.ping_n)
    }
    pub fn speed_kbps(&self) -> f64 {
        if self.speed_n >= 0.5 { self.speed_sum / self.speed_n } else { 0.0 }
    }
    pub fn fail_rate(&self) -> f64 {
        if self.sessions >= 0.5 { (self.drops / self.sessions).min(1.0) } else { 0.0 }
    }
}

/// One client report into our aggregates. Values are clamped; at most 200 peers per report.
pub fn ingest(own: &mut HashMap<String, Agg>, report: &Value) -> usize {
    let t = now();
    let mut n = 0;
    for p in report["peers"].as_array().into_iter().flatten().take(200) {
        let uri = p["uri"].as_str().unwrap_or("");
        if !(uri.starts_with("tcp://") || uri.starts_with("tls://") || uri.starts_with("quic://")) || uri.len() > 300 {
            continue;
        }
        let a = own.entry(peer_id(uri)).or_default();
        a.decay(t);
        a.uri = uri.to_string();
        a.reports += 1.0;
        if let Some(ms) = p["ping_ms"].as_f64().filter(|v| *v > 0.0 && *v < 10_000.0) {
            a.ping_sum += ms;
            a.ping_n += 1.0;
        }
        if let Some(k) = p["speed_kbps"].as_f64().filter(|v| *v > 0.0 && *v < 10_000_000.0) {
            a.speed_sum += k;
            a.speed_n += 1.0;
        }
        let sessions = p["sessions"].as_f64().unwrap_or(0.0).clamp(0.0, 1000.0);
        a.sessions += sessions;
        a.drops += p["drops"].as_f64().unwrap_or(0.0).clamp(0.0, sessions);
        n += 1;
    }
    n
}

/// Our own aggregates and those of other trackers (each counted once: trackers share only
/// their own reports, never what they got from others).
pub fn merged(own: &HashMap<String, Agg>, remote: &HashMap<String, Vec<Agg>>) -> HashMap<String, Agg> {
    let t = now();
    let mut out: HashMap<String, Agg> = HashMap::new();
    for a in own.values().cloned().chain(remote.values().flatten().cloned()) {
        let mut a = a;
        a.decay(t);
        let e = out.entry(peer_id(&a.uri)).or_insert_with(|| Agg { uri: a.uri.clone(), updated: t, ..Default::default() });
        e.add(&a);
    }
    out
}

/// `/peers` entries with the clients' view added and sorted by the combined score: the
/// clients' ping where enough reports exist, else the tracker's own connect time.
pub fn rank_checked_peers(peers: &[Value], stats: &HashMap<String, Agg>) -> Vec<Value> {
    let mut v: Vec<(f64, Value)> = peers
        .iter()
        .map(|p| {
            let mut p = p.clone();
            let id = peer_id(p["uri"].as_str().unwrap_or(""));
            let own_rtt = p["rtt_ms"].as_f64().unwrap_or(500.0) + 60.0;
            let s = match stats.get(&id) {
                Some(a) if a.reports >= 1.0 => {
                    p["reports"] = json!((a.reports * 10.0).round() / 10.0);
                    p["client_ping_ms"] = json!(a.ping_ms().map(|x| x.round()));
                    p["speed_kbps"] = json!(a.speed_kbps().round());
                    p["fail_rate"] = json!((a.fail_rate() * 100.0).round() / 100.0);
                    score(a.ping_ms().unwrap_or(own_rtt), a.fail_rate(), a.speed_kbps())
                }
                _ => score(own_rtt, 0.0, 0.0),
            };
            p["score"] = json!(s.round());
            (s, p)
        })
        .collect();
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    v.into_iter().map(|x| x.1).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_ranks_fast_steady_peers_first() {
        let mut j = Journal::default();
        for _ in 0..8 {
            j.tick(
                &json!({"peers": [
                    {"dir": "out", "uri": "tls://a:1?key=00", "rtt_ms": 30, "rx_bps": 500000, "tx_bps": 0, "share": 90},
                    {"dir": "out", "uri": "tcp://b:2", "rtt_ms": 20, "rx_bps": 100, "tx_bps": 0, "share": 10},
                    {"dir": "in", "uri": "", "rtt_ms": 1}
                ]}),
                Some(80.0),
            );
        }
        let r = j.ranked();
        assert_eq!(r.len(), 2);
        // a: server ping 80 ms but 4 Mbit/s; b: 20 + 60 = 80 ms, no speed: a wins.
        assert_eq!(r[0].0, "tls://a:1?key=00");
        let rep = j.report();
        assert_eq!(rep["peers"].as_array().unwrap().len(), 2);
        assert!(rep.to_string().find("share").is_none());
    }

    #[test]
    fn trackers_count_each_report_once() {
        let mut own = HashMap::new();
        ingest(&mut own, &json!({"peers": [{"uri": "tcp://a:1", "ping_ms": 50, "speed_kbps": 8000, "sessions": 2, "drops": 0}]}));
        let mut remote = HashMap::new();
        remote.insert("t2".to_string(), vec![own["tcp://a:1"].clone()]);
        let m = merged(&own, &remote);
        assert!((m["tcp://a:1"].reports - 2.0).abs() < 0.01);
        let ranked = rank_checked_peers(&[json!({"uri": "tcp://b:2", "rtt_ms": 10}), json!({"uri": "tcp://a:1?key=1", "rtt_ms": 100})], &m);
        // a: clients see 50 ms at 8 Mbit/s -> 50 / 1.4 = 36; b: tracker's 10 + 60 = 70 ms.
        assert_eq!(ranked[0]["uri"], "tcp://a:1?key=1");
        assert!(ranked[0]["reports"].as_f64().unwrap() >= 2.0);
    }
}
