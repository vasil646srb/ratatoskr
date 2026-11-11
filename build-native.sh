#!/usr/bin/env bash
# Build tcp_node (repo root) and ygghelper for Android ABIs into app/src/main/jniLibs.
# Needs: ANDROID_NDK_HOME, rustup targets, cargo-ndk (cargo install cargo-ndk).
# TCP_NODE_SRC: the yggdrasil-lite checkout with tcp_node (default: the parent directory).
set -euo pipefail
cd "$(dirname "$0")"
SRC=$(cd "${TCP_NODE_SRC:-..}" && pwd)
[ -f "$SRC/Cargo.toml" ] || { echo "tcp_node sources not found in $SRC (set TCP_NODE_SRC)" >&2; exit 1; }
API=26
ABIS=${ABIS:-"arm64-v8a armeabi-v7a x86_64"}
triple() { case $1 in arm64-v8a) echo aarch64-linux-android;; armeabi-v7a) echo armv7-linux-androideabi;; x86_64) echo x86_64-linux-android;; x86) echo i686-linux-android;; esac; }
for abi in $ABIS; do
  t=$(triple "$abi"); out=app/src/main/jniLibs/$abi; mkdir -p "$out"
  (cd "$SRC" && cargo ndk -t "$abi" -P $API build --release --bin tcp_node --features upstream-tls,html)
  cp "$SRC"/target/"$t"/release/tcp_node "$out"/libtcp_node.so
  (cd ygghelper && cargo ndk -t "$abi" -P $API build --release)
  cp ygghelper/target/"$t"/release/ygghelper "$out"/libygghelper.so
  ls -l "$out"
done
