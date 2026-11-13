Added 22 tests covering the required behaviors and edge cases; fixed strict Range parsing and throttle arithmetic.

Validation:
- Formatting: passed.
- Offline Clippy with `-D warnings`: passed.
- `cargo test -p eid-testserver --offline`: **5 passed, 17 failed, 0 ignored**. Every failure occurs because this environment denies loopback binding (`Operation not permitted`).

All mandatory spec behaviors have implementations and tests, but network behavior remains unverified. Optional IPv6 and chunked responses were not added. No commits made.