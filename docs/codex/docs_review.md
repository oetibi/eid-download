**BLOCKER**

- **ARCHITECTURE §6, v3 note; AGENTS §6; ADR-0004:** §6 still passes cookies through a temporary Netscape file, contradicting “cookies in memory only.” “Errors shown verbatim,” repeated in PLAN P4, also permits credential leakage. **Fix:** remove the temporary-file design; disable cookie-dependent media until a compliant transport is specified. Redact subprocess output before UI/log/event delivery; test sentinel secrets across all outputs and created files.

- **ARCHITECTURE §6; ADR-0003; AGENTS §5:** yt-dlp owns muxing and can launch FFmpeg/runtime descendants itself, bypassing the mandatory `eid_platform::process::spawn` wrapper. A wrapper unit test cannot prove descendant `CREATE_NO_WINDOW` behavior. **Fix:** explicitly define the permitted delegation boundary and descendant launch policy, then test actual process trees, cancellation, and console visibility on Windows.

**MAJOR**

- **PLAN P1/P3, permanent security suite:** P1 is simultaneously an HTTP engine, crash-safe store, dynamic scheduler, limiter, hostile server, CLI, and filesystem-security implementation. Yet every security test applies from P1, including process-launch behavior whose platform support arrives later. **Fix:** seek owner approval to split P1 into sequential transfer/filesystem, persistence/recovery, and segmentation/limiter gates; implement required platform primitives before their first consumer.

- **PLAN P1 tests 2, 8, 10:** `kill -9` is not Windows-portable; killing near 30% does not guarantee ≤15% retransmission with buffered responses/checkpoints. “Time < specified limit” specifies none, and ±10% throughput over six seconds is runner-sensitive. **Fix:** use platform termination, server barriers, acknowledged durable checkpoints, and explicitly bounded in-flight bytes. Gate limiter/split behavior with virtual time and events; record real throughput separately.

- **.github/workflows/ci.yml — test job:** `--list | grep -c security_` only counts names, including ignored tests; PLAN’s named tests generally lack that prefix. CI also never runs release-mode overflow tests, `cargo deny`, or the stated GNU cross-check. **Fix:** maintain an explicit required-test manifest, verify executed/nonignored results per OS, and add release security, dependency-policy, and target-check jobs.

- **PLAN — permanent security tests:** The injection payload exercises POSIX syntax, but lacks PowerShell/cmd-specific attack cases. Windows link creation prerequisites are unspecified despite mandatory failure if links cannot be created. **Fix:** add native-shell payload matrices with sentinel side effects and literal-argument assertions; provision and explicitly verify Windows symlink capability before testing.

- **PLAN — bad server/security; ADR-0010:** Several listed scenarios lack named acceptance tests: malformed `Content-Range`, forced gzip, one-use URLs, redirect loops/downgrades/cross-origin credential stripping, and TLS rejection. A source-text scan cannot establish certificate verification. **Fix:** add named local HTTP/TLS tests, successful trusted-certificate controls, and hostile redirect/header/length cases with bounded resource assertions.

- **PLAN P1 persistence; ADR-0004/0009:** One approximate process-kill test does not exercise crashes between write, sync, checkpoint replacement, and backup recovery. The reservation protocol leaves reservation advancement, failed-write rollback, and cancellation ownership implicit. **Fix:** specify these transitions and add deterministic fault barriers at each persistence/handoff boundary; verify final SHA-256 and untouched external files, including ancestor-junction swaps.

- **PLAN P3; ADR-0002:** IPC acceptance omits wrong/missing tokens, remote-client rejection, and actual DACL isolation; cross-user verification waits until P8. **Fix:** make named authentication tests and a recorded Windows cross-user ACL test prerequisites for IPC approval.

- **PLAN rules/P4/P6; ADR-0001:** Every acceptance test must pass, yet P6 permits “not tested,” and P4 requires a live video download despite the offline-test rule. Building Tauri is also treated as testing its real window. **Fix:** distinguish mandatory automated gates, explicitly deferred manual checks, and ignored online evidence; use local media fixtures for P4.

- **ADR-0008; ARCHITECTURE §6; AGENTS §2:** Separate-process execution does not settle redistribution obligations for bundled/downloaded tool artifacts. Dependency licensing is deferred to P7 despite applying immediately. **Fix:** record artifact-specific licenses, provenance, notices/source obligations, and distribution mode before tooling adoption; add an independently worded provenance ledger for competitor-derived methodology.

**MINOR**

- **README — Ideas/Build; ARCHITECTURE §§1,5,9–10; STATUS:** README lacks per-feature planned labels and claims CI builds the installer, while CI builds only the CLI. Architecture claims “test-verified” handoff and Windows-tested sparse behavior without named results. **Fix:** label these planned/unverified, link actual evidence when available, and add named startup/memory benchmarks before promising REQUIREMENTS §4 limits.

No files modified. Documentation review only; tests were not run.