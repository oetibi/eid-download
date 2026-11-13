#!/usr/bin/env bash
# ADR-0012: reproduces the loopback TLS-handshake cost numbers (not a gate; Unix/rustls only).
set -euo pipefail
cd "$(dirname "$0")/.."
cargo test -p eid-engine --test bench_tls -- --ignored --nocapture 2>&1 | grep -E "^BENCH|test result|error"
