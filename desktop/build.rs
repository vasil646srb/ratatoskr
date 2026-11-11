// Embed tcp_node and ygghelper (Linux builds) into the yggproxy binary: one file to install.
// Paths: TCP_NODE_BIN / YGGHELPER_BIN, else the usual build outputs next to this repo.
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
    let node = pick("TCP_NODE_BIN", "~/yggdrasil-lite/target/release/tcp_node");
    let helper = pick("YGGHELPER_BIN", "../ygghelper/target/release/ygghelper");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    std::fs::copy(&node, out.join("tcp_node")).unwrap();
    std::fs::copy(&helper, out.join("ygghelper")).unwrap();
}
