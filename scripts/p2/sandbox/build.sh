#!/bin/sh
set -eu
test "$(uname -s)" = Linux
test ! -e /workspace
test ! -e /var/run/docker.sock
mkdir -p /work/source /work/target
cp -R /input/. /work/source/
cd /work/source
export CARGO_HOME=/opt/cargo RUSTC=/opt/rust/bin/rustc
export PATH=/opt/rust/bin:/usr/bin:/bin HOME=/work
export CARGO_TARGET_DIR=/work/target CARGO_INCREMENTAL=0
export CARGO_BUILD_JOBS=2
export CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_PROFILE_RELEASE_OPT_LEVEL=1
# Source hashes and the lock are verified outside this untrusted workload.
# Frozen Cargo cannot consult the network or rewrite Cargo.lock.
cargo build --frozen --release -p kyro-app --features factory-artifact --bins
for name in kyro-app kyro-app-worker kyro-app-migrate; do
    test -f "/work/target/release/$name"
    test ! -L "/work/target/release/$name"
    size=$(stat -c %s "/work/target/release/$name")
    test "$size" -gt 0 && test "$size" -le 134217728
    cp "/work/target/release/$name" "/output/$name"
    chmod 0555 "/output/$name"
done
echo KYRO_FACTORY_BUILD_COMPLETED
