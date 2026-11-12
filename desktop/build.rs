// Embed tcp_node and ygghelper (and wintun.dll on Windows) into the yggproxy binary: one file to
// install. Paths: TCP_NODE_BIN / YGGHELPER_BIN / WINTUN_DLL, else the usual build outputs.
use std::path::PathBuf;

fn pick(var: &str, default: &str) -> PathBuf {
    println!("cargo:rerun-if-env-changed={var}");
    let p = std::env::var(var).map(PathBuf::from).unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_default();
        PathBuf::from(default.replace('~', &home))
    });
    println!("cargo:rerun-if-changed={}", p.display());
    if !p.is_file() {
        panic!("{var}: {} not found (build tcp_node with --features upstream-tls,html and ygghelper first)", p.display());
    }
    p
}

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let t = "x86_64-pc-windows-gnu";
        let node = pick("TCP_NODE_BIN", &format!("~/yggdrasil-lite/target/{t}/release/tcp_node.exe"));
        let helper = pick("YGGHELPER_BIN", &format!("../ygghelper/target/{t}/release/ygghelper.exe"));
        let wintun = pick("WINTUN_DLL", "~/.cache/ratatoskr-win/wintun/x/wintun/bin/amd64/wintun.dll");
        std::fs::copy(&node, out.join("tcp_node")).unwrap();
        std::fs::copy(&helper, out.join("ygghelper")).unwrap();
        std::fs::copy(&wintun, out.join("wintun.dll")).unwrap();
    } else {
        let node = pick("TCP_NODE_BIN", "~/yggdrasil-lite/target/release/tcp_node");
        let helper = pick("YGGHELPER_BIN", "../ygghelper/target/release/ygghelper");
        std::fs::copy(&node, out.join("tcp_node")).unwrap();
        std::fs::copy(&helper, out.join("ygghelper")).unwrap();
    }
}
