# P4 minor findings: separate review of `fix/p4-review-minors` (AGENTS §4)

Review date: 2026-11-17. Verdict: **nothing blocking, and no Major.** The branch can be merged once the owner accepts or rejects the minors below. Each one is either fixed with a test, or recorded as "rejected + reason" in `docs/DESIGN_NOTES.md`. n1 and n2 are worth doing before the merge, because they touch the fail-closed split this branch introduces.

Prompt: the task given to this session, together with `docs/prompts/review.md`. Read: AGENTS.md, `docs/reviews/P4.md`, `docs/prompts/review.md`, DESIGN_NOTES Round 17, and the last P4 section of STATUS.

Scope: `git diff main..origin/fix/p4-review-minors`, 9 commits (`8998aca`..`ccf1300`), 12 files, +577/−43. Base `main` = `921ffc6`. Read in full:
- `crates/eid-cli/src/video.rs`: `prepare_staging`/`check_staging_tree`, `PublishError`, `publish`, `publish_failure`, the playlist item loop, and the `test_hooks` module;
- `crates/eid-platform/src/fs.rs`: `publish_verified`/`publish_impl` (Unix and Windows), `mismatch`, `path_id`, `reject_if_not_plain`, and `open_regular_nofollow`;
- the new and changed tests in `crates/eid-cli/tests/{video,playlist}.rs` and `video_env/mod.rs`;
- `eid-fake-ytdlp` (`nourl`, `stall_info`);
- the m10 step in `.github/workflows/ci-windows.yml`;
- `scripts/required_tests.txt`.

No code, test or other doc was changed. This file is the only change, on `docs/p4-minors-review` (cut from `fix/p4-review-minors`), committed and not pushed.

## Validation (Windows 10 19045, debug, this machine, `--offline`)
| Check | Result |
|---|---|
| `cargo fmt --all -- --check` | **ran**: clean (exit 0) |
| `cargo clippy --offline --workspace --all-targets -- -D warnings` | **ran**: clean (exit 0) |
| `cargo test --offline --workspace --no-fail-fast` | **ran**: **386 passed, 0 failed, 6 ignored** (exit 0; the same 6 ignored as before the branch). Default thread count; `handoff_engine_randomized_with_jitter` took 169.6 s here, run in parallel with a release build. |
| Names in `scripts/required_tests.txt` | **required: 319 ok: 319 bad: 0** (bash port of `check_required.py`, run on the log above). The **29 `unix:` names cannot run here (NOT TESTED)**. 348 names in all. All seven new names ran and passed. This agrees with STATUS. |
| `scripts/check_required.py` | **not run**: Python is not installed (the `python` on PATH is the Microsoft Store stub, exit 49). Its matching logic was ported to a bash script and run on the log above; see the previous row. |
| m10 scan, by hand | **ran**: `grep -c -a` gives 1 match for each of the five names in `target/debug/eid.exe` (positive control 5/5). A fresh `cargo build --offline --release -p eid-cli` gives 0 for each name and 0 for the prefixes `EID_TEST_` and `EID_FAULT`. This agrees with Round 17 m10. The PowerShell step itself has not run (it runs only in CI). |
| `cargo deny check` | **not run** (not installed) |
| `cargo check --target x86_64-pc-windows-gnu` | **not run** in this review (STATUS reports it clean on `29be8a0`) |
| Red checks | **not repeated**: that would mean editing code. The red-first claims in STATUS (m2, m4, m7) rest on the implementation session. For m3, I checked by reading the old code: it would fail the new test, because the `?` ends the run before item 3 and before the summary. |
| CI for this branch | **not seen**: the branch has no PR and no CI run yet. That includes the first run of the m10 step and every `unix:` name. |

## Blocker
None.

## Major
None.

## Minor

### n1 — The playlist's fail-closed branch has no test
**Where:** `crates/eid-cli/src/video.rs:1213` (`Err(PublishError::Refused(m)) => return Err(Fail::Other(m))`). The classifier `publish_failure` (`:688`) is tested.

**What happens:**
- `a_swap_while_publishing_stops_the_run_other_errors_fail_one_item` tests only that `publish_failure` maps `InvalidData` to `Refused`.
- `planted_symlink_or_junction_is_never_followed` only tests refusals in `prepare_staging`, for `eid video get`.
- No test runs a playlist where `publish` returns `Refused`. That happens when `open_regular_nofollow` refuses the staged file (link, hard link, reparse point) or when a swap is detected.
- **Failure scenario:** a later edit swaps the two match arms, or turns `Refused` into `continue`, as the first version of m3 did. Every test still passes, but a playlist run then keeps going after a planted or swapped file.

**Fix:** let the test hook also produce a swap, for example `EID_TEST_PUBLISH_FAIL=swap:<name>`, so that it returns `publish_failure(&dest, io::Error::new(InvalidData, …))`. That runs the real classifier and the real loop. Add an end-to-end test: items a, b, c, with the swap on b. Assert:
- exit 1 and the "changed while it was being published" message;
- only a and b were downloaded, c was not;
- no summary line;
- b's download is still in staging.

A second test can stage a hard-linked file, which `reject_if_not_plain` refuses on both systems.

### n2 — STATUS and a code comment overstate what the swap follow-up prevents
**Where:** `docs/STATUS.md:289` ("كان التشغيل التالي سينشر الملف الغريب. الآن يوقف التشغيل") and `crates/eid-cli/src/video.rs:684-687` ("the file in staging may now be foreign and must not be published by the next run"). Compare Round 17 m3, "Known limit": "the next run finds the same file in staging and is not told it was swapped".

**What happens:**
- `29be8a0` stops the **current** run. Nothing marks the item or its staging folder.
- **Failure scenario (Windows):** a swap is detected and rolled back ("moved back, nothing published"), so the foreign file now sits under the staged name. The run exits 1 without saving state for that item. The user runs the same command again:
  1. yt-dlp skips the item, because the archive already lists it.
  2. `finished_file` finds the foreign file, and `check_mp4` accepts it if it is a valid MP4.
  3. `publish` publishes it as `NN - Title.mp4`, and the item is marked `done` without any warning.
- So the next run does publish the foreign file. Round 17 says so correctly; STATUS and the comment say the opposite.
- The real risk is small: anyone who can write into the staging folder could plant a file before any run. But the documents must not promise more than the code does (AGENTS §3.6).

**Fix:** pick one.
- Make the claim true. On `Refused`, rename the staging folder to a quarantine name (for example `.refused-<name>`, never deleted by us) or record it in the state, so that the next run refuses that item until the user deals with it. Add a test.
- Or reword STATUS:289 and the comment to match Round 17: the run stops, and the next run is not protected.

### n3 — The staging walk does not refuse hard links; Round 17 says "any link"
**Where:** `crates/eid-cli/src/video.rs:462` (`is_link_or_odd`) and `:495-532` (`check_staging_tree`). Round 17 m7: "any link, junction or special file at any depth refuses the folder".

**What happens:**
- The walk refuses symlinks, junctions (name-surrogate reparse points) and non-file, non-folder entries. A regular file with a link count above 1 passes.
- AGENTS §6.1 lists the external hard link next to the symlink.
- At publish time, `reject_if_not_plain` does refuse multi-link files. But that happens after yt-dlp has worked in the folder.
- **Failure scenario:** a reused staging folder holds `abc.mp4.part`, a hard link to a file outside the download folder. `prepare_staging` accepts it. yt-dlp resumes the `.part` and appends to that outside file. The publish is then refused, but the outside file has already been changed.
- This gap existed at the top level before the branch; it is not new. Planting the link needs write access to the staging folder and, on Windows, write-attribute access to the target.

**Fix:** pick one.
- In the walk, refuse regular files with more than one link. Unix: `MetadataExt::nlink()` from the same `symlink_metadata`. Windows: the handle-based count `eid-platform` already has (`windows_link_count`), exposed behind a function there. Add a "hard link in a reused staging folder" case to `planted_symlink_or_junction_is_never_followed`, failing loudly if the link cannot be made.
- Or narrow the Round 17 m7 text to "symlinks, junctions and special files", and record hard links as a known limit.

### n4 — A locked staged file stops the whole run, with no retry (AGENTS §6.9)
**Where:** `crates/eid-cli/src/video.rs:647-649` and the comment at `:624-628`. Compare STATUS ("قُبلت محافظةً").

**What happens:**
- Every error from `open_regular_nofollow` becomes `Refused`. That includes a sharing violation (Windows errors 32 and 33), for example from an antivirus or indexer that opens the freshly remuxed file without `FILE_SHARE_WRITE`. Our open asks for read and write access.
- The playlist run then stops at that item, and later items are not even tried.
- AGENTS §6.9 requires retrying when a file is locked. `eid-platform` already has `is_lock_contention`, and bounded retries in `replace_atomic`.
- "Accepted conservatively" was decided by the in-session read-only agent, not by the owner.
- This is not a regression: before the branch, the `?` also ended the run.

**Fix:** if `eid_platform::fs::is_lock_contention(&e)`, retry the open a few times with a short pause. If it is still locked, make it `Item` (temporary failure; the file stays in staging). Keep `Refused` for links, hard links and reparse points, which `reject_if_not_plain` reports as `InvalidInput`. Alternatively, record the owner's acceptance in DESIGN_NOTES.

## Nits
- **n5 — An `Item` error can come after the new name already exists.** The cases:
  - `fs.rs:546` and `:568` (Unix): `drop_part_if_ours` fails, or `handle_is_path(dest)` returns an I/O error.
  - `fs.rs:614` (Windows): `path_id(dest)?` fails after `rename_noreplace`.

  These are not `InvalidData`, so they become `Item` with "it is kept in the staging folder". On Linux the file then has both names, and the next run publishes it again as `NN - Title (1).mp4` (a duplicate). On Windows the file has already moved to `dest`. The next run sees an empty staging folder and marks the item `done` with no `file`. That is allowed (`playlist.rs:232`), but the message was wrong. Rare (the post-checks rarely fail with I/O), and it existed before the branch. At least the message should not claim the file is in staging when it is not.
- **n6 — The 10 000-entry limit of the staging walk has no way out** (`video.rs:493`). A reused folder over the limit is refused on every run. The message does not say what to do (delete the staging folder to start that item again). Whether a killed real yt-dlp can leave that many `*.part-Frag*` files was not checked (no real tool). Add a hint to the message.
- **n7 — The m10 scan covers only `target\release\eid.exe`** (`ci-windows.yml:91-105`). The upload step (`:122-131`) will also ship the NSIS installers once P5 adds them. When P5 adds the desktop app, its binary should get the same scan and positive control. Note this for P5.
- **n8 — Commit hygiene (AGENTS §3.2, §9).**
  - `c177d0a` fixes two findings (m3 and m4).
  - The fake's `nourl` mode, used only by the m4 test, came in `8998aca` (m2).
  - The seven new required names were added only in the final docs commit `ccf1300`, so the fix commits themselves do not pin their tests.

  Not worth rewriting history. The PR description should still carry the prompt and the test output (AGENTS §3.9).
- **n9 — The m12 wording.** Round 17 says the restriction test "asserts the exact summary line". `playlist.rs:1208` asserts the line up to "0 failed". The tail ("… not retried automatically …") is not checked. That is fine; the wording is just a little stronger than the test.

## Checked and found sound
- **m2** (`video.rs:1019-1030`): the select now yields `None` on cancel, and that becomes `Fail::Cancelled` (exit 130). The test checks four things:
  - exit 130 and "stopped while listing", with no "error: playlist";
  - the listing tool's pid dies within 15 s while its stall flag still exists (killed, not released);
  - nothing in the output folder;
  - exactly one tool call.

  The `.list-<pid>` folder that may be left in the tools root on Windows is covered by Round 17 m8.
- **m3** (`video.rs:1201-1220`): an `Item` error counts as a failure, saves the state at the end of the loop, keeps staging, and the run goes on to the summary. The test proves that the next run publishes the kept file itself (marked mtime, same SHA-256 as the fixture) under its own number. The hook `EID_TEST_PUBLISH_FAIL` is compiled only with `test-hooks`; it has no effect in a release build (0 markers, above). It matches one candidate name exactly and returns before `publish_verified`, so the real file is never touched. Round 17's claim that a too-long title can no longer cause the path error holds: `fit_name_to_path_limit` (`eid-engine/src/sanitize.rs:90-120`) fails only for the folder itself, and it shortens the title stem while keeping the number prefix.
- **Swap classification:** every refusal in `publish_impl` on both systems goes through `mismatch` (`InvalidData`). The kind is pinned in `publish_verified_fallback_refuses_a_source_replaced_before_promotion`. No non-swap path in `publish_impl` returns `InvalidData`: `CString` errors are `InvalidInput`, and `rename_noreplace`/`hard_link` errors propagate with their own OS kind.
- **m4** (`video.rs:1163-1173`): a missing or non-http(s) address records a temporary failure and saves at once. The test covers 4 runs (pending 1, pending 2, failed 3, and then not asked, with the `--retry-failed 2` hint), the number kept, and no download of it.
- **m7** (`video.rs:483-532`): a bounded recursive walk (depth 8, 10 000 entries) that never enters a link (it uses `symlink_metadata`, and recurses only into real folders). On Windows, `FileType::is_symlink` is true for junctions (name-surrogate reparse tags), so junctions at any depth are refused. The planted-link test now also plants a file symlink two levels down and a junction (Windows) or folder symlink (Unix) one level down. It fails loudly if a link cannot be made, and asserts that the tool never ran and nothing was written outside. Names in the message go through `printable`. The depth test checks the exact boundary (7 nested folders accepted, 8 refused) and that nothing is removed.
- **m9:** the message texts, and `--skip` numbers missing from the plan are reported (also with `-q`), not refused.
- **m10:** the positive control and the prefix scan are sound. The step fails closed if `target\debug\eid.exe` is missing (`ReadAllBytes` throws). The only `EID_TEST_` string outside the hooks (`EID_TEST_DIR_` in `eid-platform/src/dirs.rs`) is inside `#[cfg(test)]` and is not in the release binary (0 matches).
- **m11:** ARCHITECTURE §6 matches what is emitted. The 289/27 and +9278 corrections are added next to the old numbers, not over them.
- **m12:** `video_env::Running` kills and reaps `eid` on drop. `exit_within` takes the child out, then kills it on timeout. `stop_eid` waits for it. On Unix, the tool may outlive `eid` until its stall limit; this is documented.
- **Windows vs Linux:** nothing new is system-specific beyond what is noted in n3 (how the link count is read) and n5 (move on Windows, second name on Linux). No `cfg(windows)` or `Command` was added outside `eid-platform`; the `#[cfg(feature = "test-hooks")]` in `video.rs` is a feature gate, not a system gate.

## What I could not check
- CI for this branch: the first run of the m10 PowerShell step, the `required: N ok: N` lines, and the 29 `unix:` names (including the Unix publish paths in n5 and the Unix side of the new link cases). There is no Linux machine here.
- `scripts/check_required.py` itself (no Python), `cargo deny check` (not installed), and the `x86_64-pc-windows-gnu` check (not run in this review).
- Red checks (they need code edits).
- Real yt-dlp: fragment leftovers (n6), and how flat entries without an address really look (m4 uses the fake's `nourl` shape).
- A real antivirus lock on a staged file (n4).
- M6, m5, m6 and M3 limit 5: open for the owner, outside this review.
