//! Settings (/etc/yggproxy/config.json) and fixed values shared with the Android app.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Overlay port of the proxy, the same on every server (and in the Android app).
pub const PORT: u16 = 27080;
pub const CLIENT_SOCKS: u16 = 10808;
pub const EXIT_SOCKS: u16 = 10800;
pub const UDPGW: u16 = 7300;
pub const SPEEDTEST: u16 = 7301;
pub const API: u16 = 19080;
pub const HTML_PORT: u16 = 1081;
/// tcp_node --socks: local SOCKS5 into Yggdrasil (tracker requests).
pub const NODE_SOCKS: u16 = 10810;
/// SO_MARK of the node's peer connections; policy routing keeps them out of the VPN.
pub const FWMARK: u32 = 0x2a7;
/// Routing table of the VPN default route.
pub const TABLE: u32 = 2701;
pub const TUN_NAME: &str = "yggproxy0";

pub fn config_path() -> PathBuf {
    std::env::var("YGGPROXY_CONFIG").map(PathBuf::from).unwrap_or_else(|_| "/etc/yggproxy/config.json".into())
}
pub fn state_dir() -> PathBuf {
    std::env::var("YGGPROXY_STATE").map(PathBuf::from).unwrap_or_else(|_| "/var/lib/yggproxy".into())
}
pub fn socket_path() -> PathBuf {
    std::env::var("YGGPROXY_SOCKET").map(PathBuf::from).unwrap_or_else(|_| "/run/yggproxy/ctl.sock".into())
}

/// Speed limits of the exit in kbit/s (0 = none).
#[derive(Serialize, Deserialize, Clone, Copy, Default, PartialEq, Eq, Debug)]
#[serde(default)]
pub struct Limits {
    pub total_down: u64,
    pub total_up: u64,
    pub client_down: u64,
    pub client_up: u64,
}

impl Limits {
    pub fn file_text(&self) -> String {
        format!(
            "total_down={}\ntotal_up={}\nclient_down={}\nclient_up={}\n",
            self.total_down, self.total_up, self.client_down, self.client_up
        )
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Server {
    pub enabled: bool,
    pub allow_lan: bool,
    pub limits: Limits,
}

impl Default for Server {
    fn default() -> Self {
        Self { enabled: false, allow_lan: false, limits: Limits::default() }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Client {
    pub enabled: bool,
    /// Yggdrasil address of the exit (the port is always [PORT]).
    pub server: String,
    pub udp: bool,
    pub ipv6: bool,
}

impl Default for Client {
    fn default() -> Self {
        Self { enabled: false, server: String::new(), udp: true, ipv6: true }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Network {
    pub public_peers: bool,
    pub peers: Vec<String>,
    pub target: u32,
    pub lan_discovery: bool,
    pub lan_target: u32,
    /// Client: replace peers while the ping to the server is above this (ms).
    pub rtt_limit: u32,
    pub swap_interval: u32,
    pub swap_settle: u32,
    pub extra_args: String,
}

impl Default for Network {
    fn default() -> Self {
        Self {
            public_peers: true,
            peers: Vec::new(),
            target: 3,
            lan_discovery: true,
            lan_target: 0,
            rtt_limit: 100,
            swap_interval: 30,
            swap_settle: 60,
            extra_args: String::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Default, PartialEq, Debug)]
#[serde(default)]
pub struct Config {
    pub server: Server,
    pub client: Client,
    pub network: Network,
}

impl Config {
    pub fn load() -> Config {
        std::fs::read_to_string(config_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let p = config_path();
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).unwrap() + "\n")?;
        std::fs::rename(tmp, p)
    }
}

/// `200:…`, `[200:…]`, `[200:…]:port` (port ignored) or a `yggproxy://c?a=…` link → address.
pub fn parse_server(input: &str) -> Option<std::net::Ipv6Addr> {
    let mut s = input.trim().to_string();
    if let Some(q) = s.strip_prefix("yggproxy://").and_then(|r| r.split_once('?')).map(|(_, q)| q.to_string()) {
        s = q.split('&').find_map(|kv| kv.strip_prefix("a=")).unwrap_or("").to_string();
    }
    let host = if let Some(rest) = s.strip_prefix('[') { rest.split(']').next().unwrap_or("").to_string() } else { s };
    let a: std::net::Ipv6Addr = host.parse().ok()?;
    (a.octets()[0] & 0xfe == 0x02).then_some(a)
}

pub fn connect_link(addr: &str) -> String {
    format!("yggproxy://c?a={addr}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_forms() {
        let a: std::net::Ipv6Addr = "200:99b:4003:b216:f139:24e7:df8f:5d3a".parse().unwrap();
        for s in [
            "200:99b:4003:b216:f139:24e7:df8f:5d3a",
            "[200:99b:4003:b216:f139:24e7:df8f:5d3a]:1080",
            "yggproxy://c?a=200:99b:4003:b216:f139:24e7:df8f:5d3a",
            " yggproxy://c?a=200:99b:4003:b216:f139:24e7:df8f:5d3a&p=1080 ",
        ] {
            assert_eq!(parse_server(s), Some(a), "{s}");
        }
        assert_eq!(parse_server("fd00::1"), None);
        assert_eq!(parse_server("garbage"), None);
    }

    #[test]
    fn old_config_fields_default() {
        let c: Config = serde_json::from_str(r#"{"server":{"enabled":true}}"#).unwrap();
        assert!(c.server.enabled && c.client.udp && c.network.public_peers);
    }
}
