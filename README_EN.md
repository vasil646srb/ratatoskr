# Ratatoskr

**Internet over the [Yggdrasil](https://yggdrasil-network.github.io/) network: a VPN client and an exit server for Android, Linux and Windows.**

*Ratatoskr is the squirrel of Norse mythology that runs up and down the world tree Yggdrasil,
carrying messages between its branches and its roots. Here it carries your traffic.*

[Русская версия](README.md)

---

## What it is

Ratatoskr turns any phone or computer connected to Yggdrasil into an **exit server** that shares
its internet connection with other nodes of the network. Another device becomes a **client**: all
of its traffic goes into Yggdrasil and reaches the internet through the exit.

- **Servers are closed, every device has its own key.** The owner makes a key valid for 1–365 days
  and shows its QR code: scan it and connect. Without a key a device can ask the owner for access.
- **Servers find themselves.** Trackers crawl the Yggdrasil network and collect working exit
  servers; the app lists them with the ping to each one.
- **No root and no separate Yggdrasil install on the phone.** Inside is a lightweight node,
  [yggdrasil-lite](https://github.com/vasil646srb/yggdrasil-lite) (`tcp_node`), compatible with
  yggdrasil-go.

```
client device                                                   exit server
apps → VPN (TUN) → tun2proxy ─SOCKS5→ tcp_node ══ Yggdrasil ══ tcp_node ─→ SOCKS5 + udpgw ─→ internet
```

## Features

| | |
|---|---|
| **Client** | all device traffic through the VPN; TCP, UDP (QUIC, calls, games) over udpgw, IPv4 and IPv6 |
| **Exit server** | port **27080**, closed by default: only devices with a key; can be made public after a warning about the risks. The server's LAN, loopback and Yggdrasil itself are closed through the exit |
| **Access keys** | a key (1–365 days) binds to the first device that uses it; renew, unbind, delete; the client shows how long its key is still valid; access requests (with a message to the owner) from devices without a key — approve or deny |
| **Quick connect** | QR code or a `yggproxy://c?a=ADDRESS&k=KEY` link (the system camera opens it too), or the address and key typed in |
| **Yggdrasil and ALFIS sites** | `200::/7` addresses (and `300::/8` subnets) and ALFIS names (`.ygg`, `.anon`, `.btn`, `.conf`, `.index`, `.merch`, `.mirror`, `.mob`, `.screen`, `.srv`) go through the device's own node, next to the internet through the exit; a "Yggdrasil only" mode runs the VPN without an exit server. TCP only: sites, SSH, chats |
| **Updates** | the app and the service check GitHub releases and offer new versions; `sudo yggproxy update` on Linux downloads and installs one |
| **Server list** | saved servers plus servers from trackers, ping to each and to the network root |
| **Peer selection** | the client replaces its Yggdrasil peers while the ping to the server is above a threshold |
| **Speed limits** | for the whole server and per client, download and upload separately; applied live, no reconnects |
| **Statistics** | the exit's clients and their traffic; Yggdrasil peers with their rate and share of traffic |
| **Speed test** | iperf-like: client ↔ server over Yggdrasil, 10 s download and 10 s upload |
| **VPN exclusions** | pick apps that bypass the VPN |
| **Trackers** | find exit servers and other trackers, sync their lists with each other |
| **Background** | the exit does not sleep with the screen off (wake lock, Wi-Fi lock); the node keeps pinging servers when the app is minimized |
| **Logs** | export to a file; every stop is logged with its cause |

## Components

| Part | Where | What it does |
|---|---|---|
| **Ratatoskr** Android app | `app/` | client, server, status, settings (Kotlin, no third-party UI libraries) |
| `ygghelper` | `ygghelper/` | exit server (SOCKS5, udpgw, limits, speed test) and the client's tun2proxy side (Rust) |
| `yggproxy` for Linux and Windows | `desktop/` | service (systemd / Windows service), console commands, a window (egui), the tracker — one file ([details](desktop/README.md)) |
| `tcp_node` | [yggdrasil-lite, branch `ratatoskr`](https://github.com/vasil646srb/yggdrasil-lite/tree/ratatoskr) | userspace Yggdrasil node |

## Quick start

### Android

1. Install the APK.
2. **Server** (a phone with internet): "Сервер" tab → "Запустить сервер". Allow unrestricted battery
   use so the exit does not fall asleep. Under "Доступ" create a key for a device (name and 1–365
   days): a QR code appears. Requests from devices without a key are listed there too.
3. **Client**: simple mode — "Ключ" → "Сканировать QR" → "Подключиться". Expert mode — "Клиент" tab →
   "Сканировать QR сервера" (or type the address and key). Connecting to a tracker's server without
   a key sends its owner an access request.

Network, tracker settings and logs are on the "Настройки" tab.

### Linux

Prebuilt files are in the [releases](https://github.com/vasil646srb/ratatoskr/releases/latest): the APK, a static
server build (any Ubuntu/Debian x86_64) and the desktop build with a window.

```sh
wget -O yggproxy https://github.com/vasil646srb/ratatoskr/releases/latest/download/yggproxy-server-linux-x86_64
chmod +x yggproxy
sudo ./yggproxy install             # systemd service, yggproxy group for control without sudo
yggproxy server on                  # share the internet connection
yggproxy access new Phone 30        # a 30-day key: QR code and link for one device
yggproxy access                     # keys and access requests
yggproxy access approve ADDR 30     # approve a device's request
yggproxy servers                    # servers from trackers, with ping
yggproxy connect 'yggproxy://c?a=…&k=…'   # connect with a key link (or: ADDR KEY)
yggproxy gui                        # window
sudo yggproxy update                # update to the latest release
yggproxy request ADDR Hi, it's Masha      # ask for access with a message
yggproxy ygg                        # VPN only for Yggdrasil and ALFIS sites (wiki.ygg …)
```

Details: [desktop/README.md](desktop/README.md).

### Windows

Download `yggproxy-windows-x86_64.exe` from the [releases](https://github.com/vasil646srb/ratatoskr/releases/latest)
and run it. A window opens; press "Установить и запустить службу" and confirm the administrator
prompt. The program is copied to `C:\Program Files\Ratatoskr`, the **Ratatoskr** service starts
with Windows, and a Start menu shortcut appears. The same console commands work in a command
prompt: `yggproxy status`, `yggproxy connect`, `yggproxy server on`. Windows 10/11 x64; the window
is drawn with DirectX 12 and opens in virtual machines without 3D acceleration too.

## How it works

**Client.** The VPN interface hands packets to tun2proxy, which turns connections into SOCKS5
requests to `127.0.0.1:10808`. `tcp_node` carries them over Yggdrasil to `[server]:27080`. DNS is
virtual: the server resolves names. UDP travels inside the same tunnel through udpgw. On Android
the app itself is excluded from the VPN, so the node's peer links bypass the tunnel; on Linux an
`fwmark` and policy routing rules do the same; on Windows the peer sockets are bound to the main
interface (`IP_UNICAST_IF`) while the tunnel (Wintun) gets the routes `0.0.0.0/1` and `128.0.0.0/1`.

**Server.** `tcp_node --forward 27080:10800 --open 27080 --proxy-protocol 27080` accepts
connections from any node and gives the proxy a PROXY v1 line with the client's Yggdrasil address
(verified against its key). Per-client limits and statistics are keyed on it.

**Peer selection.** The client's node pings the server every 10 s. While the ping is above the
threshold (250 ms by default) it replaces the slowest peer, measures the new one for a while and,
if the swap did not help, waits twice as long before the next one. Threshold, pause and
measuring time are configurable.

**Peer statistics.** Every 10 s a client (the app and the PC service) records how each peer
carried its traffic: ping to the peer, ping to the exit server while that peer carried the
traffic, speed from real traffic, drops. That gives a ranking; the node dials the best peers
first (`tcp_node --prefer-file`) and every 2 minutes replaces its worst peer with a better free
one. Every 30 minutes the client sends a tracker an anonymous per-peer summary (no client
address, no exit server, `POST /report`); trackers merge the reports, share them
(`GET /peerstats`) and serve `/peers` sorted by the combined score. A client's priority list is
its own best peers, then the trackers' ranking.

**Trackers.** A tracker runs its own Yggdrasil node — [yggdrasil-ng](https://github.com/Revertron/Yggdrasil-ng)
as a library, with a TCP stack of its own: no yggdrasil-go, TUN or root. With the remote getPeers
request it crawls the whole network starting from its own peers and checks port
27080 on every node (SOCKS5 plus an allowed CONNECT to the speed-test port, which is how a
Ratatoskr exit is told apart from other proxies) and the tracker port 27081. The list is served
over HTTP: `GET /exits`, `GET /trackers`, `GET /peers` (public peers checked by the tracker, so a
node finds peers even where the list's website is blocked), `GET /info`. Trackers fetch each other's lists and probe
every received entry themselves. The app queries trackers through its own node
(`tcp_node --socks`) every 2 minutes and learns about new trackers on its own.

Tracker schedule (configurable in `/etc/yggproxy/tracker.json`):

| What | How often |
|---|---|
| finding other trackers: asking known ones for `/trackers` | every 5 min |
| finding other trackers and exits: crawl of the whole network (ports 27080 and 27081) | every 30 min (crawl ~8 min, scan ~12 min for ~8,500 nodes) |
| re-check of known exits and trackers | every 5 min |
| sync with other trackers | every 5 min |
| an exit or tracker is hidden from the list | after 2 failed checks in a row (~10 min) or 24 h without an answer; it comes back as soon as it answers |
| public peers: download the list and check every peer | hourly |

## Ports

| Port | Where | Purpose |
|---|---|---|
| **27080** | Yggdrasil | exit server proxy (the same on every server) |
| **27081** | Yggdrasil | tracker API |
| 10808 | 127.0.0.1 | client SOCKS5 → server (`tcp_node --connect`) |
| 10810 | 127.0.0.1 | SOCKS5 into Yggdrasil (`tcp_node --socks`) |
| 10800 / 7300 / 7301 | 127.0.0.1 | exit SOCKS5 / udpgw / speed test |
| 19080 | 127.0.0.1 | node API: `/api/stats`, `/api/peers`, `/api/rtt` |

## Security

- An exit server is **closed**: only devices with your keys reach the internet through it. A key
  binds to one device; delete or unbind a lost one. A public exit is open to anyone who finds it and
  people browse the internet as you, so the app warns about the consequences first. Use speed limits. The server's LAN is closed (unless "Разрешить клиентам доступ в мою LAN"
  is checked).
- Traffic inside Yggdrasil is encrypted by the network; from the exit to a website it is as usual
  (HTTPS stays HTTPS).
- The exit sees where its clients go, like any VPN provider. Client DNS queries go through the
  exit too.

## Building

Requirements: Rust (stable), Android SDK (platform 35), NDK 27, `cargo-ndk`, Gradle 8.11+, JDK 17.

```sh
git clone -b ratatoskr https://github.com/vasil646srb/yggdrasil-lite ~/yggdrasil-lite
rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
cargo install cargo-ndk

# Android
export ANDROID_NDK_HOME=$ANDROID_HOME/ndk/27.3.13750724
TCP_NODE_SRC=~/yggdrasil-lite ./build-native.sh     # tcp_node + ygghelper → app/src/main/jniLibs
gradle assembleRelease                              # signing: KEYSTORE_FILE, KEYSTORE_PASSWORD, KEY_ALIAS, KEY_PASSWORD

# Windows (cross-build on Linux: rustup target add x86_64-pc-windows-gnu, the package mingw-w64;
# wintun.dll from https://www.wintun.net, path in WINTUN_DLL)
(cd ~/yggdrasil-lite && cargo build --release --target x86_64-pc-windows-gnu --bin tcp_node --features upstream-tls,html)
(cd ygghelper && cargo build --release --target x86_64-pc-windows-gnu)
(cd desktop && cargo build --release --target x86_64-pc-windows-gnu)

# Linux
(cd ~/yggdrasil-lite && cargo build --release --bin tcp_node --features upstream-tls,html)
(cd ygghelper && cargo build --release)
(cd desktop && cargo build --release)                                                   # with the window
(cd desktop && cargo build --release --no-default-features --target-dir target/server)  # for servers
```

## Layout

```
app/            Android app (Kotlin)
ygghelper/      exit server and the client's tun2proxy side (Rust)
desktop/        yggproxy for Linux and Windows: service, console, window, tracker (Rust)
build-native.sh builds the native parts for Android
```

## Roadmap

- Static build for older Linux servers (musl).

## Thanks

[Yggdrasil Network](https://yggdrasil-network.github.io/), [tun2proxy](https://github.com/tun2proxy/tun2proxy),
[zxing](https://github.com/zxing/zxing), [egui](https://github.com/emilk/egui), [Wintun](https://www.wintun.net/).
