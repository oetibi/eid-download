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
* **`Last-Modified` alone is a weak validator** (RFC 9110 §8.8.2: second granularity): a same-size change within the same second can still splice. We keep ranged downloads for such servers (otherwise every Last-Modified-only server would lose segmentation); documented debt. ETag-less AND Last-Modified-less servers get one stream and a whole-file restart (B3).
* **B1:** the new generation's state (zero frontier, new validators) replaces the old one and the old `.bak` is deleted before truncation. Not covered: A→B→A with an identical validator across restarts (needs a generation id in the state). Test uses a simulated crash (`fault::set_panic_point`, test-faults).
* **M1:** per-job lock FILE `<name>.eid-lock` (not the part file: it must exist when the part is missing, and the part is deleted/recreated by restarts). Acquired before `state::load`; handed into the restart path; deleted only at the end of a finished job while still locked. The lock file is part of the job's file set (the orphan test allows it). Windows `LockFileEx` untested.
* **M4:** each job runs on its own task; dropping/aborting the caller only cancels (drop guard). Residual: a process kill still relies on checkpoints; hooks `set_pause_point`/`set_panic_point` are `test-faults` only.
* **drop(resp):** probe 416 fallback and single-stream retry now drop the previous response/body before the next request. The kernel may still show the old socket briefly (hyper closes asynchronously) — see STATUS (#[ignore]d 416 test) and ADR-0012 (proposed).
