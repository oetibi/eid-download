# Diagnosis (evidence copy inside the repo; originals lived outside it): run-90 `client_sockets=9` (cap 8) — provisional, updated at end of run

## What was done
- Worktree `EidDownload-diag` (branch `p1/conn-cap-diag`, DIAG ONLY, never to be merged). Each response's LOCAL TCP port is read from reqwest's `HttpInfo` extension and emitted as `Event::DiagPort(seg, port)`; a diag sampler (50 µs) uses `SockDiag::client_sockets_detail` to dump the FULL raw set (inode, local_port, state, owned) per sample WITH duplicates preserved. On the first sample with > cap owned sockets the test prints every socket, its segment, first/last-seen time, the segment's engine events (send/sent_ok/Opened/body_dropped/Closed) and the duplicate count. Test: `diag_hunt_for_cap_overshoot` (g3_segments.rs on that branch).
- Test server binds `127.0.0.1:0` only (`eid-testserver/src/server.rs:46`), base_url is `http://127.0.0.1:<port>`, the hunt asserts the prefix → IP literal, no DNS, **no Happy Eyeballs possible** (hypothesis 4 excluded).

## Evidence
1. Original failure log (the original run-90 log (not preserved: the loop script deleted it on the next invocation; only the recorded numbers survive)): no retry/416/restart events in the run (grep = 0), so the engine took only the plain 206 path: probe → segments → split. The `drop(resp)` bug (probe 416 fallback, single-stream retry) is NOT on that path ⇒ **it is not the cause of run 90.**
2. From the earlier timing capture (t0 = first sample > cap): kernel sockets 8→9 at t0, ResponseClosed for segs 0,1,3 at −6.8…−1.4 ms, ResponseOpened for new segs 9,8 only AFTER t0 (+2.6, +6.1 ms) and 9→5 at t0+0.76 ms (four fds vanished within one sample). ⇒ the new connects were already established several ms BEFORE their `ResponseOpened` event (the event is emitted after headers), and old fds were still open ~2–7 ms after `ResponseClosed`. Both are consistent with hypothesis (i)/(ii), not with a phantom.
3. Direct, deterministic-ish proof that `drop` ≠ closed fd: `g5_one_connection::one_connection_probe_416_fallback…` (cap 1): after `drop(resp)` was added, the kernel still showed 2 owned sockets in ~50% of rounds under load (PEAK conns=1 client_sockets=2). hyper closes the fd in its connection task AFTER the Response/body is dropped; nothing orders that close before the next `connect()`. The `2 ms` sleep exists only on the segment path (after body drop), not on the probe path.
4. Identity hunts: **no overshoot reproduced** in 2300 instrumented runs — see section "Hunt 3" below.

## Verdict
Not (iii) phantom: the diag dump keeps duplicates; no overshoot was captured, so no duplicate-inode evidence either way (see Hunt 3). Most consistent with **(i)+(ii): an old segment's fd closed asynchronously by hyper after `ResponseClosed`/permit-release while the replacement connect had already completed (the `ResponseOpened` event lags the real connect)** — the 2 ms settle can't make that correct; it is a scheduling bet, it loses under heavy load (1/90 of loop runs at load ≈ 3× cores). The 9th socket could not be pinned to a specific (inode, port) in a live capture (not reproduced in 2300 instrumented runs: instrumentation slows the sampler and shifts timing), so this verdict rests on evidence 2+3, not on a captured identity. Honest status: **probable, not proven by identity.**

## Recommendation
Cap by construction (permit tied to the TcpStream's Drop) — see PROPOSED `docs/adr/0012-conn-cap-by-construction.md` (branch `p1/adr-0012`). Owner decision needed. Until then ADR-0011's "measured, not by construction" stands and the strict test keeps rejecting 9.

## Hunt 3 — instrumented identity hunts (results)
Test: `diag_hunt_for_cap_overshoot` (branch `p1/conn-cap-diag`, worktree `../EidDownload-diag`), 8 connections, 20 MiB generated file, test server on `127.0.0.1`, 10 (hunt 3) / 30 (hunt 2) CPU-burner `yes` loops on 8 cores, `--test-threads=1`. On the first sample with > cap owned sockets it would print every socket (inode, local port, state), the segment bound to that port and its engine events, and the dump-duplicate count, then panic.

| Hunt | Log | Runs | Sampler | Load | Result | Wall time |
|---|---|---|---|---|---|---|
| hunt2 | hunt2 log (excerpt below) | 800 | diag sampler re-scanning `/proc/self/fd` per sample | 30 `yes` loops | `HUNT: no overshoot in 800 runs` | 533.97 s |
| hunt3 | hunt3 log (excerpt below) | 1500 | diag sampler with cached ownership (`client_sockets_detail` + `owned` cache) | 10 `yes` loops | `HUNT: no overshoot in 1500 runs` | 974.38 s |

Interpretation (honest): 0 overshoots in 2300 runs vs 1 in 90 loop runs (≈ 270 downloads) of the uninstrumented test. The instrumentation slows the sampler (detail dump + per-sample set building) and shifts timing, so the absence is NOT evidence that the race is gone; it only means the socket identity of the 9th socket was never captured. The failing-run timeline (evidence 2) and the deterministic cap-1 probe result (evidence 3, ≈50 % of rounds with 2 sockets after `drop(resp)`) remain the basis of the verdict: **probable cause = hyper closes the fd asynchronously after the Response/body drop, while the replacement connect (whose `ResponseOpened` is emitted later, after headers) is already established.** Not proven by identity. Branch `p1/conn-cap-diag` is DIAG ONLY and must never be pushed/merged.

## Log excerpts (copied verbatim; no machine paths)
Hunt logs, final lines:
```
hunt2: test diag_hunt_for_cap_overshoot ... HUNT: no overshoot in 800 runs
       test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 13 filtered out; finished in 533.97s
hunt3: test diag_hunt_for_cap_overshoot ... HUNT: no overshoot in 1500 runs
       test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 13 filtered out; finished in 974.38s
```
Original failing line (as recorded when it happened; the full log was lost, see above): `PEAK conns=8 client_sockets=9 ...` with cap 8, 1 failure in 90 loop runs under load of about 3x the cores.

## Limits of this evidence
- The run-90 log itself is NOT in the repo; the timeline in "Evidence 2" is from a capture whose raw file is also gone. Treat the verdict as **probable**.
- The identity hunts live on a diag-only branch that is not part of any PR.
