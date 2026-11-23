<div dir="rtl">

# خطة المراحل

## قواعد
1. **مرحلة واحدة = جلسة Codex جديدة للتنفيذ**، و**مراجعة في جلسة Codex منفصلة** قبل إغلاقها (قالب المراجعة في [AGENTS.md](../AGENTS.md#مراجعة)). لا تُغلق مرحلة إلا إذا نجحت **كل** اختبارات «شرط النجاح» ونُشر ناتجها.
2. شرط النجاح مكتوب مسبقاً بأسماء اختبارات. أي تغيير فيه = موافقة المالك.
3. «خارج المرحلة» = مؤجل لا مرفوض.
4. **ثلاثة أنواع من الأدلة** (لا تُخلط): **(أ) بوابة آلية إلزامية** (اختبار مسمّى ينجح محلياً هنا وفي CI، قائمة `scripts/required_tests.txt` يتحقق منها `scripts/check_required.py` أن كل اسم **نُفّذ ونجح لا متخطّى**) · **(ب) فحص يدوي مؤجل مسجَّل** (في MANUAL-TESTS، لا يُحتسب «نجاحاً» بل يُذكر كـ«لم يُختبر يدوياً») · **(ج) دليل إنترنت حقيقي** (`#[ignore]` ويُسجَّل ناتجه في STATUS، غير إلزامي). بوابات المرحلة = (أ) فقط.
5. CI على Windows وLinux في كل دمج (انظر P0). على هذا الصندوق (Linux فقط) ما لا يمكن اختباره على Windows يُدوَّن في [STATUS.md](STATUS.md) تحت «لم يُختبر على Windows الحقيقي».
6. السيرفر التجريبي السيّئ `eid-testserver` (P1) هو الأساس لكل اختبارات الشبكة؛ لا اختبار يتصل بالإنترنت (ما عدا اختبارات `--ignored` الموسومة «حقيقي»).

## P0 — الأساس (هذا الملف وأخواته)
**تحتوي:** README، REQUIREMENTS، ARCHITECTURE، ADR، PLAN، IDM-COMPARISON، AGENTS، STATUS، قالب CI، مساحة عمل Cargo فارغة.
**نجاح:** مراجعة Codex منفصلة للوثائق ودمج ملاحظاتها (DESIGN_NOTES) · `cargo check --workspace` ينجح · ملف CI يمر في فحص YAML · لا وعد في الوثائق بلا اختبار مسمّى. **خارجها:** أي كود منتج.

## P1 — المحرّك (`eid-engine`) + `eid-testserver` + CLI `eid get/resume` + `eid-platform` (fs/process فقط)
**ثلاث بوابات متتابعة داخل المرحلة** (كل بوابة تُغلق بأدلتها قبل التي بعدها؛ المنصة تُبنى قبل أول مستهلك لها):
### G1 — النقل والملفات (اتصال واحد، استكمال أساسي، أمان الملفات)
`eid-platform::{fs,process}`، probe، إعادة توجيه آمنة، تنظيف الأسماء، كتابة بلا-اتباع-الروابط، Content-Disposition، ترميزات.
`sanitize_*` (جدول الأمان أدناه)، `probe_206_with_content_range`، `probe_200_no_range_single_stream`، `probe_416_empty_file`، `malformed_content_range_rejected`، `forced_gzip_single_stream_raw_bytes`، `single_use_url_not_burned_by_probe`، `redirect_loop_bounded`، `redirect_https_downgrade_refused`، `cross_origin_redirect_drops_credentials`، `lying_content_length_short_body_never_succeeds`، `slow_loris_and_stall_times_out`، `sha256_matches_single_connection`.
### G2 — الحالة والتعافي
حالة JSON ذرية (sidecar `<name>.eid-state` بجانب `.eid-part`)، استكمال، نقاط حفظ، أخطاء حقن عند كل حد:
`state_roundtrip`، `corrupt_or_hostile_state_file_is_rejected` (تالف/مقتطع/checksum/مسار خارجي/أرقام ضخمة/تغطية ناقصة)، `kill_then_resume_requests_only_from_checkpointed_frontier` (يُقتل الـCLI بإنهاء المنصة بعد أن تُظهر ملف الحالة تقدماً ≥30%؛ **يُثبَت بسجل السيرفر أن أي طلب بعد الاستكمال يبدأ من `next` المحفوظ لا قبله**، والملف النهائي SHA مطابق)، `crash_at_every_persistence_fault_point_recovers` (نقاط الأعطال `before_sync`/`after_sync`/`tmp_written`/`dest_moved_to_bak`/`renamed`/`part_renamed_final` عبر ميزة `test-faults`: العملية تنتهي فجأة ثم الاستكمال ينجح بـSHA مطابق)، `etag_change_between_attempts_restarts_with_new_content`، `file_changes_during_download_is_detected` (If-Range يعيد 200: لا خلط)، `cancel_at_many_points_leaves_no_orphans`، `state_has_no_secrets_and_is_private`.
### G3 — التقسيم والتحديد
تقسيم ديناميكي (ADR-0009)، حد الاتصالات المتعلم، backoff، محدّد السرعة:
`sha256_matches_with_1_8_32_connections` (50MB)، `midstream_disconnects_still_complete`، `range_ignored_with_200_falls_back_to_single_stream`، `range_416_falls_back`، `slow_segment_is_split` (حدث `Split` في السجل وجزء المتصل البطيء تقلّص؛ حد زمني = ثلث ما يحتاجه المتصل البطيء وحده، محسوب لا مُخمَّن)، `retry_after_429_respected_and_403_lowers_connections` (بساعة وهمية `start_paused`)، `limiter_rate_within_5_percent_virtual_time` (ساعة وهمية، عام + لكل مهمة + التسلسل الهرمي)، `limiter_real_time_smoke_bounded` (واسع، للكشف الأعمى فقط)، `handoff_randomized_200_seeds` (ADR-0009: تغطية بلا فجوات/تداخل، SHA، لا كتابة خارج النطاق، نقاط yield عشوائية)، `max_in_flight_bytes_bounded`، `numbers_from_network_use_checked_ops` (release).
### G4 — TLS (تُضاف إن اكتملت G1–G3؛ وإلا تُذكر صراحةً «لم تُنجز»)
`tls_rejects_selfsigned_expired_wrong_host` **و**`tls_accepts_with_test_root` (نجاح مقابل لكل رفض).
### ختام المرحلة (أ)
`windows_target_compiles` (`cargo check --target x86_64-pc-windows-gnu`، لا يثبت سلوكاً) · `cargo fmt --check` · `clippy -D warnings` · `cargo test --workspace` (debug **و** `--release` للأمان) · `scripts/check_required.py` أخضر.
**(ج) قياس حقيقي** (`--ignored`): ملف عام ~مئات MB بـ1/8/32 اتصالاً → أرقام في STATUS.
**خارجها:** قوائم/جدولة، IPC، واجهة، فيديو، `.bak` على Windows بـReplaceFileW (مُنفَّذ بالبنية نفسها ولا يُختبر هنا).

### بنود مفتوحة في P1 — ملاحظات المحرّك (مراجعة Codex أثناء PR7؛ غير منفَّذة)
مصدرها مراجعة Codex لاختبار `aborting_the_caller_drains_the_job_and_persists_a_final_checkpoint` أثناء PR7. **كل بند مفتوح.** لا يُنفَّذ أي إصلاح في المحرّك لهذه البنود إلا بعد موافقة المالك عتيبيّ صراحةً.

**(أ) `state::load` ليست للقراءة فقط.** عند فشل قراءة عابر (مثلاً تعارض مشاركة Windows أثناء الاستبدال الذري) قد تُعاد تسمية ملف sidecar أساسي سليم إلى `.broken` ثم تُجرَّب `.bak`. القراءة الفاشلة إذن قادرة على تغيير حالة حيّة؛ ونجاح الرجوع إلى `.bak` يخفي خطأ الملف الأساسي.

**(ب) `stall_timeout` في `fast_cfg` مدته 5 ثوانٍ** (افتراضي المحرّك في الإعداد 30 ث). إيقاف السيرفر لمدة أطول (مثل 30 ث في الاختبار) ليس حاجزاً مطلقاً: العمّال قد يعيدون المحاولة من frontiers متقدمة قبل انتهاء الإيقاف. يُراجَع إن كانت القيمة والدلالة صحيحتين.

**(ج) `run_ranged` يتجاهل خطأ آخر checkpoint.** الكتابة النهائية تستخدم `let _ = persist_checkpoint(...).await`، ففشل الحفظ الحقيقي قد يُسكَت. يُراجَع ما يجب أن يحدث عند فشل الـcheckpoint الأخير.

## P2 — القوائم والجدولة والتصنيف والجملة
queues.json، max_parallel، نوافذ زمنية، إعادة محاولات الطابور، `PowerControl` (mock آلي)، التصنيف، الأنماط `(001-050)`، استيراد/تصدير (ملف خبيث يُرفض)، Grabber لصفحة واحدة (حد حجم/عدد، بدون JS).
**نجاح:** 10 تحميلات بحد 2 لا يتجاوز 2 أبداً · جدولة بعد 5 ث تبدأ خلال ثانية (ساعة وهمية) · kill للطابور ثم الاستكمال · استيراد-تصدير يعيد نفس القائمة وملف خبيث لا يُسقط البرنامج · العد التنازلي للإيقاف قابل للإلغاء (mock) · Grabber يرفض صفحات ضخمة.

## P3 — المنصة وIPC وCLI البعيد ومضيف Native Messaging
`eid-platform` الكاملة (Named Pipe/Unix socket، DPAPI، استبدال ذري، سجل HKCU)، `eid-ipc`، `eid serve`، `eid add/list/pause/resume`، `eid-native-host`.
**نجاح (أ):** رسائل NM حقيقية (طول 4 بايت): الصحيحة تصل، الكبيرة/المشوهة/ذات الحقول الزائدة تُرفض · stdout بروتوكول فقط · طلب مكرر بنفس المعرّف لا يضيف مهمتين · `ipc_rejects_missing_wrong_token` · `ipc_rejects_oversized_and_truncated_frames` · لا `TcpListener` خارج testserver (فحص آلي) · نسخة ثانية تمرّر الأمر وتخرج. **شرط قبول IPC على Windows:** اختبار ACL عبر مستخدم ثانٍ **مسجَّل يدوياً** (MANUAL-TESTS #9) قبل أن تُوصف القناة بأنها «معزولة»؛ قبله الوصف «مصمّمة لتكون معزولة، لم تُختبر».

## P4 — الفيديو (بعد الموافقة)
`eid-media`، نافذة/أمر موافقة، مدير الأدوات (إصدار+SHA-256، تنزيل من المصدر الرسمي بعد الموافقة **لا إعادة توزيع داخل مثبّتنا**)، `-J`، القوائم، التقدم، إلغاء شجرة العمليات، Deno.
**حدّ التفويض:** نحن نطلق yt-dlp فقط (عبر `eid_platform::process::spawn`، CREATE_NO_WINDOW)؛ أحفاده (ffmpeg/deno) يرثون الـconsole المخفي لأنهم برامج console تُطلَق دون `CREATE_NEW_CONSOLE`؛ لا نمرّر `--exec` ولا نسمح بإعدادات المستخدم (`--ignore-config`).
**نجاح (أ):** بلا موافقة لا تشغيل ولا شبكة · بصمة خاطئة تُرفض · لا أداة من PATH · الوسائط لا تُبنى بنص shell (حقن باسم/عنوان خبيث) · **اختبار شجرة عمليات على Windows CI**: أداة اختبار تُطلق حفيداً ويُتحقق أن `GetConsoleWindow()==NULL` للاثنين، وأن الإلغاء يقتل الشجرة كلها · الأخطاء تظهر **بعد تنقيح الأسرار** (اختبار قيم حارسة عبر كل المخرجات والملفات المُنشأة) · اختبارات الوسائط بأداة yt-dlp **مزيّفة محلية** (crate اختبار) تُنتج مقاطع صغيرة يتحقق منها `ffprobe` (فيديو+صوت) · **لا كوكيز للفيديو في v1** (فيديو يحتاج تسجيل دخول غير مدعوم) ⇒ لا ملف كوكيز مؤقت.
**قوائم التشغيل (F11a) — نجاح (أ) بأداة yt-dlp مزيّفة:** قائمة من 3 عناصر ثم 120 عنصراً: (1) نقرة واحدة تُنتج أمر/وسائط تنزيل لكل العناصر المحدّدة افتراضياً بجودة واحدة؛ إلغاء تحديد عنصر يُخرجه · (2) المخرجات داخل مجلد باسم القائمة المنظَّف (قائمة بعنوان خبيث `../x` أو `CON` أو RTL-override لا تُنتج مساراً خارج مجلد التحميل) · (3) الترقيم بترتيب القائمة بأصفار بادئة: `001 - …` لقائمة ≤999 (قرار المالك 2026-11-20؛ كان `01 - …` لقائمة ≤99، والمجلد الذي بدأ بخانتين يبقى عليها) و`0001 - …` لما فوقها، ومطابقة ترتيب الأرقام لترتيب القائمة حتى مع عناوين متطابقة/عربية/خبيثة · (4) قتل العملية في المنتصف ثم الاستكمال: العناصر المكتملة لا تُعاد، ويكتمل الباقي (يُتحقق بسجل استدعاءات الأداة المزيّفة) · (5) مزامنة: بعد إضافة عنصرين للقائمة المزيّفة ينزّل التشغيل الثاني الجديدين فقط عبر `--download-archive` (ملف الأرشيف `.eid-archive.txt` داخل مجلد القائمة — D13 — ولا يحوي روابط بأسرار) · لا `--exec` ولا shell في أي منها.
**(ج) دليل إنترنت:** تنزيل فيديو CC حقيقي + `ffprobe` (`#[ignore]`) ويُسجَّل في STATUS.

### بوابات P4 (D6 — قرارات المالك D1–D16 في [plans/P4.md](plans/P4.md)، اعتُمدت 2026-11-15)
أربع بوابات متتابعة؛ كل بوابة تُغلق بأدلتها (أ) وبموافقة المالك قبل التي بعدها. **لا تُضعِف أي شرط أعلاه:** كل شرط «نجاح (أ)» و«قوائم التشغيل» أعلاه له اسم اختبار هنا. الأسماء قابلة للتعديل الطفيف أثناء التنفيذ بشرط بقاء المعنى، والاسم النهائي يُسجَّل في `scripts/required_tests.txt`. `windows:` = إلزامي على Windows ويُسجَّل «لم يُختبر» على غيره (و`unix:` بالعكس).
**نطاق (D7):** `eid video` تشغيل مستقل داخل العملية فقط؛ أوامر الفيديو عبر IPC/Native Messaging مؤجَّلة إلى P5/P6. عملية yt-dlp واحدة لكل عنصر بـ`yt-dlp.exe` (D9). **أداة الإنترنت (ج)** كلها `#[ignore]` وتُسجَّل في STATUS، ولا تُحتسب بوابة.

#### G1 — الإطلاق المُشرَف عليه ونواة `eid-media` والأداة المزيّفة
`eid_platform::process::spawn` (غلاف async؛ Windows: `CREATE_NO_WINDOW` + `CREATE_SUSPENDED` ⇒ Job `KILL_ON_JOB_CLOSE` ⇒ استئناف؛ Unix: مجموعة عمليات + `killpg`؛ بيئة قائمة مسموحة؛ تفريغ stdout/stderr معاً بحدود)، أداة اختبار تُطلق حفيداً، `eid-media` (قائمة الأعلام المسموحة، بنّاء الوسائط، تحليل `-J` المحدود، التقدم، التنقيح، تلميحات الأخطاء)، أداة yt-dlp مزيّفة (حزمة اختبار) بمقطع mp4 صغير مثبَّت بالبصمة (فيديو+صوت، مُنتَج من lavfi، بلا حقوق للغير).
`media_args_reach_the_tool_byte_exact` (اقتباس، شرطات مائلة، شرطة بادئة، عربي، RTL) · `no_user_text_reaches_a_template_or_path_option` · `percent_and_dollar_in_paths_never_reach_ytdlp` · `child_environment_is_an_allowlist` · `tool_locations_are_explicit` · `every_emitted_flag_is_in_the_allowlist` · `forbidden_flags_are_never_emitted` · `tool_output_is_bounded_and_both_pipes_drain` · `oversized_dash_j_is_refused_without_oom` · `secrets_are_redacted_from_tool_text` (userinfo، الاستعلام، روابط googlevideo بعنوان IP) · `cancel_kills_the_whole_process_tree` · `windows: a_grandchild_spawned_at_startup_is_in_the_job` · `windows: killing_eid_kills_the_tree` · `unix: killpg_kills_the_group` · `windows: child_and_grandchild_have_no_console_window` + ضبطه `windows: the_console_check_can_see_a_window` (إن لم يميّز الضبط على المشغّل: لا يُعلن النجاح ويبقى MANUAL-TESTS #11) · `no_shell_invocations_in_product_code` و`platform_code_and_process_spawning_only_in_eid_platform` (القائمان) يبقيان خضراء.

#### G2 — الموافقة والإعدادات ومدير الأدوات
`settings.json` → `media_consent = {version, text_sha256, ts}` (ملف خاص)؛ مدير الأدوات (manifest، `Fetcher`، تنزيل عبر المحرك بـ`expected_sha256`، zip محدود لمدخلات مسمّاة، أقفال، تثبيت ذري، سحب)؛ تثبيت الإصدارات (D16) **بعد إعادة التحقق يوم البدء** من الملفات الرسمية. خطوة CI تنزّل ffprobe بنفس الإصدار والبصمة المثبّتين في الـmanifest مع `EID_REQUIRE_FFPROBE=1` (D3).
`consent_text_comes_from_locale_files_and_its_hash_is_stored` · `ipc_and_native_messaging_cannot_grant_consent` · `revoking_consent_deletes_the_tools` · `tool_with_wrong_sha256_is_refused_before_run` + نجاح مقابل `tool_with_right_sha256_runs` · `tools_are_never_taken_from_path` · `only_manifest_exe_tools_are_launched_never_bat_or_cmd` · `tool_manifest_rejects_non_https_and_unknown_hosts` · `tool_manifest_pins_immutable_assets` · `tool_install_through_injected_fetcher_installs_and_verifies` · `tool_install_through_injected_fetcher_keeps_the_previous_version_on_failure` · `zip_extracts_only_named_entries` · `zip_slip_entry_is_ignored` · `zip_bomb_is_cut_at_the_size_cap` · `extracted_file_with_wrong_hash_is_refused` · `tool_hash_is_read_through_the_locked_handle` · `windows: tools_are_locked_against_write_delete_rename_while_a_job_runs` · `windows: a_tool_already_open_for_writing_is_refused` · `fake_clip_has_video_and_audio_by_ffprobe` (`EID_REQUIRE_FFPROBE=1` في CI: يفشل بدل التخطي). Linux بلا قفل مكافئ: موثَّق لا مُختبر. **(ج):** `real_tool_install_from_official_release`.

#### G3 — `eid video info|get`
الجودة، صوت mp3، staging ثم النشر بأيدينا، `--limit-rate` من `--limit`، Ctrl-C يقتل الشجرة، الأخطاء بعد التنقيح مع تلميحات.
`video_without_consent_runs_nothing_and_opens_no_connection` (صفر طلبات في سجل الخادم المحلي) · `no_tty_and_no_flag_refuses_consent_without_waiting` · `video_is_published_by_us_from_a_fresh_staging_dir` · `planted_symlink_or_junction_is_never_followed` (يفشل بصوت عالٍ إن تعذّر إنشاء الرابط) · `cancel_leaves_nothing_outside_staging_and_resume_reuses_it` · `video_names_fit_the_path_limit` · `limit_rate_is_passed_from_the_limit_option` · `drm_error_is_refused_with_a_clear_message`. **(ج):** `real_cc_video_has_video_and_audio_by_ffprobe` · `real_ytdlp_with_percent_and_dollar_in_paths` · `real_ytdlp_does_not_load_a_planted_plugin` · `real_ytdlp_startup_time` (لـD9).

#### G4 — قوائم التشغيل F11a (1)–(5) بقواعد الترقيم D8
قائمة 3 عناصر ثم 120 عنصراً بالأداة المزيّفة؛ `.eid-playlist.json` و`.eid-archive.txt` داخل مجلد القائمة (خاص، checksum، حدود حجم، تحقق وحصر عند التحميل)؛ قفل لكل مجلد قائمة (`.eid-lock`).
(1) `playlist_selects_every_item_by_default_with_one_quality_and_a_deselected_item_is_skipped` · (2) `playlist_folder_name_is_sanitized_and_confined` (`../x`، `CON`، RTL) · (3) `playlist_numbering_is_zero_padded_in_playlist_order` (3 ثم 120؛ عناوين متطابقة/عربية/خبيثة) · (4) `playlist_kill_then_resume_skips_finished_items` (سجل استدعاءات الأداة المزيّفة) · (5) `playlist_sync_downloads_only_new_items_via_the_archive` · `numbering_is_stable_when_new_items_are_prepended` · `growing_past_99_keeps_old_names_and_never_collides` · `duplicate_video_in_a_playlist_downloads_once` · `hostile_playlist_state_file_is_rejected_and_confined` · `two_runs_on_one_playlist_folder_cannot_overlap` · `private_or_deleted_item_is_skipped_and_keeps_its_number` · `guard_secrets_never_reach_outputs_logs_state_archive_or_staging` (قيم حارسة عبر كل المخرجات والملفات المُنشأة).

#### ختام P4
كل الأسماء أعلاه في `scripts/required_tests.txt` (بادئات `windows:`/`unix:`) ونجاحها محلياً وفي CI · `cargo deny check` **إلزامي** في CI (D10، `deny.toml`) · تحديث STATUS/IDM-COMPARISON/PROGRESS بأدلة حقيقية · نتائج (ج) في STATUS · فحوص يدوية معلَّقة صراحةً: MANUAL-TESTS #11 و#19 · جلسة مراجعة مستقلة.

## P5 — الواجهة (Tauri + Svelte)
قائمة التحميلات، إضافة، تقدم/سرعة/ETA، خريطة الأجزاء، إعدادات، موافقة الفيديو، مراقبة الحافظة (معطلة افتراضياً)، i18n، ثيمات، tray.
**نجاح:** لقطات (ar/en × فاتح/داكن × هاتف/لوحي/سطح مكتب) من Chrome headless · مفاتيح الترجمة كاملة والجمع العربي (0،1،2،3،11،100) · اسم ملف مختلط عربي/إنجليزي/أرقام يُعرض صحيحاً (مقارنة صورة) · axe بلا أخطاء حرجة · بناء Tauri على windows-latest (يثبت أنه **يُبنى** فقط). **(ب) يدوي:** النافذة الحقيقية وRTL وDPI (MANUAL-TESTS #13) — لا تُحتسب نجاحاً آلياً.

## P6 — الإضافة
MV3 Chrome/Edge (+manifest Firefox)، اعتراض بحسب الامتداد، Bypass، لا إلغاء قبل الإقرار، سنيف الفيديو، overlay بـShadow DOM، قائمة السياق.
**نجاح (أ):** اختبارات وحدة (node) لمنطق الاعتراض والفلاتر والبروتوكول مع host وهمي. **(ب) يدوي:** Chrome/Edge حقيقيان (MANUAL-TESTS #8). أي ميزة إضافة لا تغطيها (أ) تُوصف في الوثائق «لم تُختبر».

## P7 — المثبّت والإصدار
NSIS + bootstrapper + خطافات HKCU، `release.yml` يبني من CI فقط + SHA256SUMS، THIRD-PARTY-NOTICES، `cargo deny`.
**نجاح:** CI على windows-latest ينتج المثبّت وبصمته · تثبيت صامت لمستخدم عادي وفحص HKCU وتشغيل وإلغاء تثبيت **على Windows حقيقي (يدوي، مسجَّل)**.

## P8 — التصليب والتجارب اليدوية على Windows
قائمة [MANUAL-TESTS](MANUAL-TESTS.md): NTFS ممتلئ، ملف مقفول، مضاد فيروسات، مسارات طويلة، SmartScreen، Win10 قديم، Chrome/Edge حقيقي، إيقاف جهاز فعلي.

## السيرفر التجريبي السيّئ `eid-testserver` (يُبنى في P1)
حالات قابلة للضبط لكل مسار: قطع الاتصال في المنتصف (بعد N بايت/عشوائي) · تغيير المحتوى/ETag أثناء التحميل وبين المحاولات · رفض Range (200 كامل / 416 / تجاهل) · `Content-Range` خاطئ · `Content-Length` كاذب (أقصر/أطول) · بطء شديد وتوقف (slow-loris) · 429 + `Retry-After` · 403 فوق حد اتصالات متزامنة · إعادة توجيه (حلقة، downgrade، لمضيف آخر) · `Content-Disposition` خبيث (`../`، `CON`، RTL-override، `a.txt:ads`) · ترميز gzip مفروض · رابط يُستهلك مرة واحدة · أسماء/مسارات عربية. سجل طلبات يُستعلم عنه من الاختبار (بايتات أُرسلت، عدد الاتصالات المتزامنة، الطلبات بحسب Range).

## الأمان الدائم (يعمل في كل مرحلة، P1 فصاعداً)
| اختبار | يمنع | مرجع الحادثة في زاجل |
|---|---|---|
| `traversal_names_never_escape_dir` (`../../x`، مطلق، `\\?\`، UNC، `C:\`، نقطتان) | الكتابة خارج المجلد (بما فيها عند الاستكمال من ملف الحالة) | استكمال يكتب خارج المجلد: [الفيديو 06:50](video/transcript.txt) ومراجعات M1 (#17) |
| `windows_reserved_and_stream_names` (`CON`، `NUL.txt`، `aux.`، `a.txt.`، `a.txt `، `a.txt:ads`، `name:$DATA`) | أسماء محجوزة/ADS | قاعدة التنظيف في AGENTS §6 عندهم |
| `rtl_override_and_control_chars_stripped` (U+202E…) | تزوير الامتداد | نفسه |
| `symlink_and_hardlink_outside_dir_untouched` + `symlink_capability_precheck` (يفشل بصوت عالٍ إن لم يمكن إنشاء الرابط، ويُوفَّر الإذن في CI) | الكتابة عبر روابط رمزية/صلبة (وصلة ancestor-junction على Windows: **لم تُغطَّ**، مسجّلة في MANUAL-TESTS) | #17/#13 |
| `printed_resume_command_cannot_inject_{sh,powershell,cmd}`: مصفوفة حمولات (`` $(touch PWNED)`id` ``، `;`، `&`، `\|`، `%PATH%`، `^`، `"`، `'`، `` ` ``، `$()`، `$env:`، `--` وأسماء تبدأ بـ`-`) تُشغَّل فعلاً في **كل shell أصلي** (sh هنا؛ PowerShell على CI Ubuntu/Windows؛ cmd على Windows CI) مع أثر جانبي مُراقَب (ملف `PWNED`) وتأكيد أن الوسيط وصل حرفياً | حقن أوامر | #18 (الفيديو 06:54) |
| `no_shell_spawn_with_untrusted_args` (فحص آلي) | `sh -c`/`cmd /c` بأسماء ملفات | — |
| `state_file_has_no_password_or_cookie_and_is_private` يبحث في **الملف المكتوب** | كلمة سر في ملف | #15 (الفيديو 06:57) |
| `errors_and_logs_strip_url_credentials_and_query` | تسريب توكن | #28 |
| `all_child_processes_use_create_no_window` (Windows) + فحص spawn الوحيد | نافذة سوداء مع كل فيديو | [الفيديو 07:15](video/transcript.txt)، CHANGELOG #251 |
| `no_tls_verification_bypass` (فحص نصي للكود) + اختبار نجاح مرافق لكل رفض | تعطيل الشهادات | #16 |
| `numbers_from_network_use_checked_ops` (قيم ضخمة، في release) | تجاوز الأعداد | #17 |
| `name_suffixes_stay_within_max_bytes` (197–200 بايت عربي) | رفض ملف حالة ننتجه نحن | #17 |
| `cancel_at_many_points_leaves_no_orphans` | ملفات يتيمة/حذف تحميل لاحق | #15 |
</div>

### بند مؤجّل — `conn-cap-by-construction` (آخر P1 أو أول P2، بعد B1–B4؛ غير منفّذ الآن)
**الموعد:** آخر P1 أو أول P2، **بعد** إغلاق B1–B4 (اعتمد المالك عتيبيّ بتاريخ 2026-11-13 الساعة 10:12 توقيت الكويت، بتوصية Claude). ADR-0012 والتبعيات الجديدة ما زالت **مقترحة** وتحتاج موافقة صريحة. سقف الاتصالات بالبناء: عميل `hyper-util` مع موصِّل (connector) ملفوف يحمل إذن semaphore يُحرَّر في `Drop` للـ`TcpStream` الملفوف (reqwest 0.12 لا يتيح ذلك لأن `Conn` مغلق). يُحذف انتظار الـ2ms. يُنظر في إعادة استعمال الاتصال داخل السقف لإزالة كلفة مصافحة TCP/TLS لكل جزء (بعد `pool_max_idle_per_host(0)`). يُحسب حساب SChannel/native-tls على Windows. الاختبار: عدّاد Drop يساوي ذروة النواة ولا يتجاوز السقف بلا أي نوم.

