# Design notes — agreements / disagreements with Codex (and why)

## Round 1 (architecture, `codex/arch_round1.md`)
* Agreed: Tauri+Svelte, native messaging (no localhost), probe with `GET Range`, identity encoding, HTTP/1.1, positional writes, `.part`+rename, ETag/If-Range, token-bucket limiter, ramp start small, clipboard opt-in, bounded Grabber, DPAPI for secrets, Deno for yt-dlp, Windows CI required.
* Adopted from Codex: connection ramp (4→…), bounded grabber, opt-in clipboard, checkpoint discipline, cancellable shutdown countdown.
* Disagreed: separate persistent daemon (rejected: no admin ⇒ no service; tray app + same server code), SQLite (kept atomic JSON for v1: no C toolchain for cross-builds; revisit).
## Round 2 (vs RESEARCH-grok.md, `codex/arch_round2.md`)
* yt-dlp owns YouTube transfer (Codex+me vs grok) → adopted.
* SChannel on Windows (Codex+grok vs me) → adopted; **spike proved** cross-build works for gnu+msvc.
* Handoff: Codex wanted pause/ack; I use reservation-under-lock. Kept mine; randomized test will arbitrate.
* Pipe: Codex corrected my "0-ACL" wording (empty DACL = deny all) → protected user-only DACL + token.
* Rejected grok's 64-byte overlap resume check (Codex).
## Usage
Codex weekly usage at checkpoint: see final report (~/bin/codex_usage.sh).

## Round 3 — docs review (docs/codex/docs_review.md), P0
| ملاحظة Codex | القرار |
|---|---|
| BLOCKER: ARCH §6 cookie temp-file يناقض «كوكيز بالذاكرة» | **قُبلت**: لا كوكيز للفيديو في v1؛ حُذف التصميم؛ تنقيح أسرار لمخرجات الأدوات + اختبار sentinel |
| BLOCKER: yt-dlp يطلق أحفاداً خارج غلافنا | **قُبلت جزئياً**: حدّ التفويض مكتوب؛ الإثبات = اختبار شجرة عمليات على Windows CI (P4)، لا اختبار الغلاف وحده؛ لم يُنفَّذ هنا |
| MAJOR: P1 كبير جداً | **قُبلت**: P1 = بوابات G1 نقل/ملفات → G2 حالة/تعافي → G3 تقسيم/تحديد → G4 TLS؛ بقاء الجميع داخل «المرحلة 1» لتوجيه المنسّق بالتوقف بعدها |
| MAJOR: kill -9/زمن/±10% هشة | **قُبلت**: إثبات الاستكمال بسجل الطلبات (Range يبدأ من `next` المحفوظ) وبنقاط أعطال؛ المحدّد بساعة وهمية؛ القياس الحقيقي واسع/غير إلزامي |
| MAJOR: CI يعدّ الأسماء فقط، لا deny ولا release | **قُبلت**: `required_tests.txt` + `check_required.py` + مهام release/deny/cross-check |
| MAJOR: مصفوفة PowerShell/cmd، قدرة symlink على Windows | **قُبلت** (pwsh على Ubuntu CI؛ cmd على Windows؛ precheck) |
| MAJOR: سيناريوهات سيرفر بلا اختبار مسمّى + TLS | **قُبلت**: أسماء مضافة؛ TLS = G4 مشروطة بإنهاء G1–G3 |
| MAJOR: قصور crash points وانتقالات الحجز | **قُبلت**: `test-faults` عبر `EID_FAULT`، وتوثيق انتقالات الحجز (reserve→write→complete داخل closure blocking واحد، لا إلغاء بينها) في ADR-0009 |
| MAJOR: IPC بلا اختبارات token/ACL | **قُبلت** (P3) |
| MAJOR: أدلة (أ)/(ب)/(ج) تُخلط | **قُبلت** (PLAN §4) |
| MAJOR: رخصة الأدوات وإعادة توزيعها | **قُبلت**: تُنزَّل بعد موافقة ولا تُوزَّع؛ DEPENDENCIES.md وPROVENANCE.md الآن |
| MINOR: README/CI/ARCH ادعاءات | **قُبلت**: وسم «مخطط»؛ أهداف الذاكرة/الإقلاع غير موعودة قبل قياس |

## Round 4 — P1 independent review fixes (docs/reviews/P1.md, Claude review of the fixes)
* **SHA-256 is computed at every finish even when `compute_sha256 = false`** (disagreement with the review note "don't compute unless required"): the finalization record (name + SHA-256, written and fsynced BEFORE the rename, B2) is the only thing that lets a crash after the rename be recognised by CONTENT instead of by file size; without a digest the recovery would again trust a name+size. It is a deliberate cost (one sequential read of the file, now on the blocking pool); the digest is only RETURNED in `Outcome` when requested. Revisit only with a cheaper content identity.
* **`Last-Modified` alone** is a strong validator only if the server's `Date` is >= LM + 1 s (RFC 9110 §8.8.2.2: the resource may change twice within one second). Otherwise (no `Date`, unparsable, `Date <= LM`) it is dropped at the probe: never stored, never sent in `If-Range`, and a server with no other validator gets ONE stream and a whole-file restart (B3). Tests: Date == LM (one stream; 4 live responses before the rule) and Date == LM + 1 s (segmented).
* **B1 (critical follow-up):** `state::remove_backup` returns `Result`, retries a locked `.bak` (8 attempts, growing delay) and does `sync_dir`; a restart that cannot delete the old `.bak` stops BEFORE truncating. A surviving `.bak` would let `load` fall back to an older generation next to new-generation data.
* **A→B→A needs no generation id in v1.** Argument: a restart is `save(new state, zero frontier)` → `remove_backup` (checked, durable) → truncate. After the second step no file on disk (primary or `.bak`) can certify bytes of an older generation, and before the truncation the data on disk is still the old generation, which the still-consistent state describes. So at every instant each state file on disk describes the data currently on disk, or a prefix of it that is still valid; the only way an older state could come back is a surviving `.bak`, which the checked removal closes. A server that returns to the first representation WITH THE SAME validator and length is, by the definition of a validator, indistinguishable from "unchanged": a locally stored generation id cannot detect it, because the id is client-local and the server shows nothing different. If the validator differs, the existing validator comparison already forces a restart. Revisit only if a server class with unstable validators matters (then weak-validator policy above applies).
* **M1:** per-job lock FILE `<name>.eid-lock` (not the part file: it must exist when the part is missing, and the part is deleted/recreated by restarts). Acquired before `state::load`; handed into the restart path; deleted only at the end of a finished job while still locked. The lock file is part of the job's file set (the orphan test allows it). After locking, the handle's identity (dev+ino / volume serial+file index) is compared with the path and an orphaned inode is retried (the file is deleted while locked, so a waiter could otherwise lock an orphan while a third process owns the new file); delete-pending/sharing violations on Windows become `Busy`. Windows `LockFileEx` and these Windows paths are NOT TESTED on real Windows.
* **`is_lock_contention` maps Windows `ERROR_ACCESS_DENIED` (5), `ERROR_SHARING_VIOLATION` (32) and `ERROR_LOCK_VIOLATION` (33) to `Busy`.** Access-denied is what a delete-pending lock file returns, so it must be Busy, but a REAL permission problem on the lock file would also be reported as `Busy` (the user sees "busy" instead of "access denied"). Accepted for v1 because the lock file is created by the same user in the download directory; revisit if such reports appear. Always `false` on Unix. NOT TESTED on real Windows.
* **M4:** each job runs on its own task; dropping/aborting the caller only cancels (drop guard). Residual: a process kill still relies on checkpoints; hooks `set_pause_point`/`set_panic_point` are `test-faults` only.
* **drop(resp):** probe 416 fallback and single-stream retry now drop the previous response/body before the next request. The kernel may still show the old socket briefly (hyper closes asynchronously) — see STATUS (#[ignore]d 416 test) and ADR-0012 (proposed).

## Round 5 — coordinator checkpoint task (PR5, Claude review of the Windows CI diagnosis)
* A slow `checkpoint()` (part-file `sync_data` + state fsync/`MoveFileEx`) used to run *inside* the `run_ranged` `select!`, next to `ramp.tick()` and worker reassignment. Default `interval` missed-tick behavior is Burst, so after a long checkpoint the coordinator could immediately run another and almost never ramp or split. That is the cause of `slow_segment_is_split` (and extra limiter wall-time) on 2-vCPU Windows runners; no ADR and no new dependencies (an internal change that keeps the durability order).
* **Now:** one background task per `run_ranged`. The coordinator only signals it (`Notify` + a coalescing flag) and never awaits the I/O. At most one checkpoint runs at a time; a request that arrives during a run is merged and the next run takes a *fresh* snapshot immediately before `sync_data` (snapshot → sync → save), then save. `MissedTickBehavior::Delay` is set on the checkpoint *and* ramp intervals so a stall cannot enqueue a burst of ticks.
* The task is **joined** (its `spawn_blocking` is joined, never aborted) before any later state write: the final checkpoint, finalize/`remove_all`, a ServerChanged restart, and cancel. Without the join, an in-flight old-generation checkpoint could finish after the restart wrote the new-generation state and overwrite it (B1 returning), or recreate a state file after `remove_all`. A checkpoint error is sent back to the coordinator over a channel. The M4 supervised job drains this join as well.
* Tests (barriers, no property timing bounds) in `g11_ckpt_task.rs`: (a) a parked checkpoint must still allow a `Split` and the ramp to reach the cap; (b) finish while parked ⇒ no leftover state, checkpoint events only after release; (c) parked during ServerChanged ⇒ new-generation state is not written before the join and is not overwritten after (B1); (d) cancel while parked, then resume, exact SHA. NOT TESTED on real Windows.
