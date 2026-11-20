<div dir="rtl">

# الحالة — ما اختُبر وما لم يُختبر
آخر تحديث: 2026-11-20. **آخر إصدار منشور: `v0.1.2` نسخة تجريبية (pre-release)، ليست نهائية بقرار المالك.** الوسمان `v0.1.0` و`v0.1.1` بلا إصدار ولا يُحذفان. القسم الأحدث في آخر الملف: «v0.2 — المرحلة 1».

| المرحلة | الحالة | الدليل |
|---|---|---|
| P0 الأساس | قيد التنفيذ | — |
| P1 المحرّك | **مُغلقة بموافقة المالك (2026-11-14)** مع بنود مؤجلة صراحةً: (أ)(ب)(ج) في PLAN تُنفَّذ حين تحتاجها مرحلة لاحقة؛ `conn-cap-by-construction`/ADR-0012 مؤجَّل ولم يبدأ؛ اختبار 429 يبقى بالساعة الحقيقية كما هو | الجدول أدناه |
| P2 القوائم والجدولة | **مُغلقة بموافقة المالك (2026-11-14)**؛ لم تُراجَع في جلسة مراجعة منفصلة (قرار المالك)؛ لم تُختبر على Windows (المالك سيختبر بنفسه) | «P2» أدناه |

> قرارات المالك 2026-11-14: الإغلاق الرسمي لـP1 وP2؛ تأجيل البنود (أ)(ب)(ج) وADR-0012؛ إبقاء اختبار 429 دون تغيير؛ لا طلب دمج الآن (اختبار Windows يدوي لاحقاً)؛ لا بدء لـP3.

> موافقات المالك عتيبيّ (2026-11-13 08:34 توقيت الكويت): بند PLAN `conn-cap-by-construction` وإعادة تسمية اختبار الحدّ الزمني في `required_tests.txt` (ADR-0011).
> موافقة المالك عتيبيّ (2026-11-13 10:12 توقيت الكويت، بتوصية Claude): نقل بند `conn-cap-by-construction` إلى «آخر P1 أو أول P2، بعد B1–B4». ADR-0012 والتبعيات الجديدة ما زالت **مقترحة** (غير مقبولة).

## ديون معروفة (تُحلّ ببند `conn-cap-by-construction`: آخر P1 أو أول P2، بعد B1–B4)
- `pool_max_idle_per_host(0)`: مصافحة TCP/TLS جديدة لكل جزء/تقسيم/إعادة محاولة (كلفة أداء على HTTPS). قرار مؤقت لإبقاء السقف على المقابس الفعلية ≤ السقف.
- **انتظار `2ms` بعد إسقاط الجسم قبل `ResponseClosed` = تخفيف مؤقت موثَّق (TEMPORARY)، ليس ضماناً.** السبب المرجَّح (probable، لا مُثبَت بالهوية — `docs/evidence/conn-cap-diagnosis.md`): hyper يغلق الـfd في مهمة خلفية بعد إسقاط الجسم/الرد، فقد يتصل الجديد قبل إغلاق القديم (حدث مرة في 90 جولة تحت حمل ≈3× الأنوية: `client_sockets=9`؛ 2300 تشغيل مُجهَّز بعدها بلا تكرار). السقف على المقابس **مُقاس** (جدول النواة، ADR-0011) لا مثبت بالبناء. الحل: `conn-cap-by-construction` (PLAN P2؛ المسودة المقترحة ADR-0012) مؤجَّل لآخر P1/أول P2 (وافق المالك على **الموعد** فقط 2026-11-13 10:12؛ قبول ADR-0012 والتبعيات الجديدة **لم يتم** بعد). اختبار SockDiag الصارم يبقى كما هو ولا يُحذف أي اسم من `required_tests.txt`؛ إن فشل في CI يُسجَّل هنا ولا يُعاد تشغيله بصمت.
- اختبارات مقابس النواة (`socket_watch_*`، `socket_closes_mid_download_when_segments_finish_or_split`، `sockdiag_counts_client_sockets_by_ownership_and_sees_closes`) Linux فقط: **لم تُختبر على Windows** (تظهر «NOT TESTED» في `check_required.py`).
- **ثغرة معروفة (اختبار مُتجاهَل):** `g5_one_connection::one_connection_probe_416_fallback_never_holds_two_sockets` (`#[ignore]`): بعد `drop(resp)` ما زال جدول النواة يُظهر مقبسين في ~50% من الجولات تحت الحمل لأن hyper يغلق الـfd لاحقاً؛ لا يُحل إلا بسقف بالبناء (ADR-0012 مقترح). يُشغَّل يدوياً بـ`--ignored`. اختبار الإعادة single-stream (`one_connection_single_stream_retry_drops_old_body_first`) يقيس ترتيب الإسقاط بهامش 75–150ms (backoff فيه jitter)؛ ليس حتمياً بالبناء، وهو في `required_tests.txt` بالبادئة `unix:` (Linux فقط: **لم يُختبر على Windows**).
- **فشل CI وإصلاحه (PR1، التشغيلان 43387076289 لينكس و43387076026 ويندوز):** (1) `socket_watch_catches_a_brief_cap_plus_one_peak` فشل في جزء التقرير (1ms) برسالة `samples == 0`: خيط الـsampler لم يأخذ عيّنة قبل `finish()` لأن `start` كان يعتمد على `sleep(1ms)` (رهان جدولة)، وليس لأن الذروة لم تُلتقط (الجولات الثلاث بـ5ms التقطتها من المحاولة الأولى). **الإصلاح:** `SocketWatch::start` لا يرجع إلا بعد أول عيّنة ناجحة (حاجز جاهزية بمهلة 5 ثوانٍ تُفشل الاختبار)؛ أُزيل `sleep(1ms)`؛ حلقتا التقرير (2ms و1ms) تستعملان `finish_checked` وتعدّان «not measured» بدل الـpanic؛ اختبار جديد `socket_watch_start_returns_only_after_the_first_sample` (يفشل قبل الإصلاح). لم تُطوَّل الـ5ms ولم تُزَد المحاولات ولم يُرخَ أي تأكيد. (2) ويندوز: نجحت `cargo test` لكن `check_required.py` انهار بـ`UnicodeEncodeError` (cp1252) عند طباعة `✗`؛ **الإصلاح:** `sys.stdout.reconfigure(encoding='utf-8', errors='replace')` ويبقى الخروج غير صفري عند أي فشل (تم التحقق محلياً بمحاكاة cp1252 واسم اختبار مفقود: قبل = traceback، بعد = سطر `✗` ظاهر وخروج 1). **التحقق بعد الإصلاح:** 200 تشغيل لاختبار الـsampler السالب تحت 24 حلقة `yes` على 8 أنوية: 199 نجح، 1 فشل (التشغيل 161: `round 1: sampler missed a 5 ms cap+1 peak in all 8 tries`، التأكيد نفسه، أي أن الـsampler حُرم من المعالج ≈8 محاولات متتالية عند تحميل ≈3× الأنوية)؛ لم يظهر أي `samples == 0` ولا «not measured» في 200 تشغيل. **لم يُرخَ التأكيد**؛ تعديل التحمّل (مثلاً محاولات حتى مهلة زمنية بدل K=8) قرار بانتظار Claude. **المتابعة (K=16):** رُفع K في اختبار الـsampler السالب من 8 إلى 16 (مسكة الـ5ms وعدد الجولات 3 كما هما). هذا **تعديل معامل إحصائي بناءً على قياس، وليس تخفيفاً**: المسك للمحاولة ≈0.55 تحت حمل 3× (جولة واحدة من 600 فاتتها كل المحاولات الثماني: 0.45⁸≈1/600) فالافتراض القديم 0.7 كان خاطئاً تحت الحمل؛ احتمال الفشل الكاذب للاختبار كله ≈3×0.45¹⁶≈1×10⁻⁵. اختبار السقف الصارم لم يتغيّر. **التحقق بعد التعديل:** 200 تشغيل تحت 24 حلقة `yes` على 8 أنوية = 200 نجح، 0 فشل (لا `samples == 0`، لا «not measured»). **دَين:** `std::thread::sleep` داخل حلقة الانتظار في `SocketWatch::start` يحجز worker في tokio حتى 5 ثوانٍ في أسوأ حال (كود اختبار فقط).
- **فشل CI الثاني (Linux على 4 PRs؛ Windows على PR1/PR3) وإصلاحه (قرار Claude، موافقة المالك على تعديل الاختبار وADR-0011 المنقولة 11:52):** (1) Linux: `socket_watch_catches_a_brief_cap_plus_one_peak` فاتته جولة كاملة 16/16 (تجويع مترابط لخيط العيّنات على مشغّل 2 vCPU مع اختبارات متوازية في العملية نفسها) — K=16 لا يعالج التجويع المترابط. **الإصلاح:** كل اختبارات `SocketWatch` في ملف مستقل `tests/g9_sockets.rs` تعمل واحداً بعد الآخر (قفل داخلي، و`--test-threads=1` + طباعة `nproc` في CI)؛ الاختبار السالب مقسوم: **مؤكَّد بالحدث** `socket_watch_sees_cap_plus_one_while_held_and_sees_it_closed` (اسم جديد، أُضيف إلى `required_tests.txt`، لم يُحذف اسم) + **تقرير فقط** `socket_watch_catches_a_brief_cap_plus_one_peak` (الاسم محفوظ) بمعدلات 5/2/1ms وأسوأ `max_gap`؛ سطر `SAMPLER` يطبع `max_gap_us`؛ اختبار السقف الصارم لم يتغيّر (ADR-0011 مُحدَّث). (2) Windows: `limiter_real_time_smoke_bounded` فشل بـ4.31s: الاختبار يقيس الوظيفة كلها لا مرحلة النقل. **الإصلاح:** نفس الاسم، الحدّ الأدنى صارم (≥1.5s) والحدّ 1.5–4s يخص مرحلة النقل (أول `ResponseOpened` إلى آخر `ResponseClosed`)، والزمن الكلي والإنهاء تقرير فقط في سطر `LIMITER PHASES`. **غير محلول:** `slow_segment_is_split` على Windows بلغ 31.59s مقابل حدّ 15s؛ الحدّ لم يتغيّر، والتشخيص (توقيت المراحل، أقسام، مدة الـcheckpoints) على فرع تشخيص منفصل؛ لم يُغيَّر سلوك المحرك. **موافقة المالك على A4:** وافق عتيبيّ بكلامه ("موافق على تعديل اختبار الاتصالات وADR-0011 مثل ما قال كلود") بتاريخ 2026-11-13 11:52 (توقيت الكويت)، نقلها «ا». **تشخيص Windows:** `docs/evidence/ci2-windows-diag.md` (التشغيلان 43397872762 كامل و43397876279 معزول)؛ فرع `diag/ci2` يبقى حياً لأجل قبل/بعد PR5 (مهمة checkpoint مستقلة، يقرره Claude).
- **فشل CI الثالث (تشغيلان بعد دفع PR1) وإصلاحه:** (1) تشغيل push `43402351147` فشل خلال 0 ثانية بـ«workflow file issue»: `ci-linux.yml` غير صالح YAML — اسم الخطوة غير المقتبس في السطر 33 يحوي `: ` (`kernel-socket tests alone, one at a time (ADR-0011: the sampler ...)`) فلم يعمل Linux على PR1 إطلاقاً. **الإصلاح:** الاسم مقتبس وبلا نقطتين؛ تم التحقق من الملفين بمحلّل YAML حقيقي (PyYAML `safe_load`: الأول فشل قبل الإصلاح بـ«mapping values are not allowed here» عند السطر 33 والعمود 65، وبعده ci-linux.yml وci-windows.yml سليمان). (2) تشغيل Windows `43402361819`: `check_required` قال `required: 65 ok: 64 bad: 1 — security_hardlinked_part_file_is_refused_on_resume NOT RUN`. السبب (قديم منذ ee20385 وظهر الآن فقط لأن check_required على Windows وصل لهذه النقطة): الاختبار كان داخل `#[cfg(unix)] mod links` بينما `required_tests.txt` يسرده **بلا** بادئة `unix:`. **الإصلاح:** نُقل الاختبار خارج وحدة `cfg(unix)` ليعمل على Windows أيضاً (`std::fs::hard_link` فقط؛ eid-platform يفرض فحص عدد الروابط على Windows عبر `nNumberOfLinks`)، وبقيت اختبارات الروابط الرمزية في وحدة unix؛ لم تُضف بادئة `unix:` ولم يُحذف اسم. **غير مُختبَر على Windows الحقيقي:** سلوك الاختبار المنقول هناك يتأكد فقط من CI (الصندوق يتحقق من التجميع إلى windows-gnu وتشغيله على Linux).
- **دَين أداء على Windows (لا تأكيد عليه):** زمن الإنهاء (finalize: hash+fsync+rename+checkpoint الأخير) بلغ **1.27s** في تشغيل CI رقم 43397872762 (اختبار الـlimiter، الحزمة الكاملة، مشغّل 2 vCPU)؛ أقصى checkpoint 1.58s. إصلاحه المخطط (PR5: checkpoint في مهمة مستقلة) يخص المحرك ولا يُنفَّذ هنا؛ الاختبار يطبع المراحل ولا يؤكد على الإنهاء.
- مراجعة Claude (دَين بسطر لكل بند): كاش ملكية الـinode لا يعيد التحقق من `true` (إنذار كاذب لو احتفظت نواة أحدث بالـinode بعد close)؛ الـdump غير ذرّي (الاتجاهان، ADR-0011 §1)؛ إعادة مسح `/proc/self/fd` تقع لحظة فتح مقابس جديدة (ثغرة عيّنات أطول لحظة الذروة)؛ `RequestStart`/`RequestEnd` في الخادم غير متناظرين (RequestEnd بشرط الجيل) و`now_us` من `SystemTime` غير رتيب؛ `socket_closes_mid_download…` يشترط آخر عيّنة 0 (يعتمد على سرعة إغلاق hyper)؛ الاختبارات تستعمل `std::net::TcpStream::connect` الحاجب داخل async؛ لا اختبار مباشر لسباق pipelining في الخادم؛ اختبارات الالتزامات الوسيطة لم تُشغَّل بمفردها على CI (بُني كل التزام محلياً فقط).

## مراجعة P1 المستقلة (`docs/reviews/P1.md`) — حالة الإصلاحات على `p1/review-fixes`
| البند | الحالة | الاختبار المثبِت |
|---|---|---|
| B1 إعادة البدء تُتلف بيانات يشهد عليها checkpoint قديم | **مُصلَح** (حالة الجيل الجديد تُحفظ وتُحذف `.bak` قبل الاقتطاع) | `crash_right_after_a_restart_truncation_never_corrupts_the_next_resume` (قبل: ملف تالف SHA مختلف؛ بعد: صحيح)، و**حرج لاحقاً:** `restart_retries_a_locked_bak_and_never_truncates_when_it_cannot_delete_it` (حذف `.bak` يعيد المحاولة ويفشل بلا اقتطاع). A→B→A بنفس الـvalidator: لا يحتاج معرّف جيل في v1 (الحجة في DESIGN_NOTES) |
| B2 ملف نهائي غريب بنفس الحجم يُقبل كاكتمال | **مُصلَح** (سجل إنهاء: الاسم+SHA قبل الـrename، يُتحقق بالمحتوى) | `resume_never_adopts_an_unrelated_same_size_final_file`، `crash_after_rename_to_a_numbered_name_is_recovered_by_content_without_the_network` |
| B3 دمج تمثيلين بلا validators | **مُصلَح**؛ `Last-Modified` وحده قوي فقط إذا `Date >= LM+1s` (RFC 9110 §8.8.2.2) وإلا يُعامل كأنه بلا validator (stream واحد) | `no_validator_same_size_change_never_splices_representations`، `last_modified_equal_to_date_is_a_weak_validator_so_the_download_is_one_stream`، `last_modified_one_second_before_date_is_a_strong_validator_so_the_download_is_segmented`، `imf_fixdate_and_strong_last_modified` |
| B4 hard link على Windows | الكود موجود (`GetFileInformationByHandle`)، الاختبار صار على كل المنصات | `open_regular_nofollow_refuses_a_hard_linked_file_and_leaves_it_untouched`، `security_hardlinked_part_file_is_refused_on_resume` — **NOT TESTED على Windows الحقيقي** |
| M1 ملكية الوظيفة | **مُصلَح** بملف قفل ثابت `<name>.eid-lock` يُؤخذ قبل قراءة الحالة ويمتد عبر إعادة البدء | `job_lock_covers_a_missing_part_and_restart_path`، `restart_path_keeps_the_job_lock_for_its_whole_transfer`، `concurrent_resume_of_the_same_job_is_refused_busy`، `lock_taken_on_an_orphaned_inode_is_never_an_ownership` (سباق حذف ملف القفل: مقارنة هوية الـhandle بالمسار) — `LockFileEx` وهوية الملف على Windows **NOT TESTED على Windows الحقيقي** |
| M4 إسقاط المستدعي يترك عمل قرص | **مُصلَح** (الوظيفة تعمل في مهمة مستقلة؛ الإسقاط يطلب الإلغاء فقط) | `aborting_the_caller_drains_the_job_and_persists_a_final_checkpoint`، `abort_at_first_save_write_and_checkpoint_barriers_then_immediate_resume` |
| M2 هوية الملف المنشور عند الإنهاء | **شبه منجز (فرع `p1/m2-finalize-identity`)**: Linux **يمنع** الاستبدال (`linkat` على مقبض الملف الذي تحققنا منه عبر `/proc/self/fd`، فلا يؤثر استبدال اسم الـpart)؛ Windows وUnix الآخر **يكتشف ولا يمنع** (فحص هوية الاسم قبل النقل وبعده، ورجوع آمن فقط لما نملكه). فحص أخير للوجهة قبل `Finished`. إصلاح ثغرة الاستعادة: انهيار بين link وunlink (اسمان لـinode واحد) يُستكمل بالمحتوى. **مخاطرة متبقية:** عملية بنفس المستخدم وصلاحية كتابة بالمجلد قد تسابق الفحص الأخير؛ منع Windows بالكامل (**M2b**: `SetFileInformationByHandle(FileRenameInfoEx)`) **مؤجل** لقرار المالك بعد نتائج ci-windows. **لم يُختبر على Windows الحقيقي.** | `finalization_with_a_swapped_part_publishes_the_original_bytes_on_linux_and_refuses_elsewhere`، `finalization_with_a_symlink_swapped_for_the_part_never_publishes_the_link`، `finalization_detects_a_destination_replaced_before_success`، `crash_between_link_and_unlink_is_recovered_without_the_network` (+`…tampered_inode_is_not_adopted`، `recovery_refuses_a_destination_substituted_while_hashing`، `recovery_refuses_a_third_link_created_while_hashing`)، واختبارات `publish_verified_*` |
| M3 مهلة ترويسات الرد | **مُصلَح**: `Config::header_timeout` (كان معرَّفاً وغير مستعمل) صار يلفّ كل `send()` عبر `http::send_bounded` (يغطي سلسلة إعادة التوجيه) | `headers_that_never_arrive_time_out_in_the_probe` (لا ترويسات / سطر حالة مبتور)، `ranged_requests_with_stuck_headers_are_retried`، ومقابل النجاح `slow_but_in_time_headers_are_accepted`؛ الخادم التجريبي: `header_delay_withholds_headers_then_serves_only_the_selected_requests` |
| M5 خصوصية الملفات | **مُصلَح على Unix**: ملف `.eid-part` يُنشأ `0600` (كان 0644 بحسب umask)؛ الملف النهائي يأخذ `0666 & !umask` قبل النشر؛ اختبارات الخصوصية لم تعد تقبل «غير معروف» دليلاً. **Windows: لم يُنفَّذ** (يرث ACL المجلد؛ DACL المحمي مهمة P3) | `security_generated_part_and_state_files_are_private` (مجلد 0777، يفحص part/state/lock، ثم ملف نهائي بصلاحيات عادية)، `state_roundtrip_and_private` (+`.bak`). قبل الإصلاح فشل: `p.bin.eid-part must be owner-only, is 644`. على Windows هذه الاختبارات `unix:` = «NOT TESTED» + MANUAL-TESTS #15 |
| M6 بوابة Windows | **مُعالَج**: لا اختبار Ctrl-C أجوف على Windows (الاختبار `#[cfg(unix)]` ويظهر NOT TESTED + MANUAL-TESTS #14)؛ `symlink_capability_precheck` يفشل بصوت عالٍ على كل نظام إن تعذر إنشاء الرابط (**مخاطرة:** إن لم يمنح runner ويندوز الصلاحية سيفشل CI وهذا مقصود) | `symlink_capability_precheck`؛ **لم يُشغَّل على Windows هنا** |
| M7 أدلة البوابات | **مُعالَج جزئياً.** (1) `handoff_randomized_200_seeds` صار يكتب **محتوى حقيقياً** ويقارن SHA-256 ويفحص حدود الحجز وانقطاعاً في **منتصف** الحجز. (2) `handoff_engine_randomized_with_jitter` صار **200 جدولاً** على المحرّك الكامل (حجم/اتصالات 2–32/تقسيم/قطع عشوائي/توقف/**فشل قرص حقني أثناء الكتابة بعد الحجز** ثم استكمال بـSHA مطابق)، والجدول يُطبع عند الفشل ويمكن إعادته بـ`EID_SEED`/`EID_REPEAT`؛ قرارات الـjitter دالة نقية في `(seed, ترتيب الاستدعاء)` — **تداخل الخيوط يبقى لجدولة النظام (غير قابل لإعادة بتّية)**. (3) مصفوفة 1/8/32 صارت **50 MiB** مع تأكيد أن 32 مطلوبة تبلغ ≥24 طلباً متزامناً عند الخادم و≥24 عاملاً. (4) عتبة قتل CLI صارت `≥30%` صريحة. **غير منفَّذ:** استعمال `start_paused` لاختبار 429 — انظر DESIGN_NOTES «Round 7» (يتعارض مع مقابس حقيقية)؛ **يحتاج قرار المالك** | `handoff_randomized_200_seeds`، `handoff_engine_randomized_with_jitter` (طباعة `SCHEDULES seeds=200 injected=49 resumed_after_disk_error=49 retries=2590`)، `sha256_matches_with_1_8_32_connections`، `cli_sigkill_then_resume_completes_from_frontier` |
| m1 ترقيم التصادم | **مُصلَح**: `sanitize::numbered_name` يحجز بايتات اللاحقة قبل قصّ الجذع | `numbered_names_keep_their_suffix_for_maximal_names` (فشل قبل الإصلاح: `candidate 1 equals the occupied base`)، `second_download_of_a_maximal_name_gets_a_numbered_free_name` (ASCII/عربي/إيموجي، 3 تحميلات) |
| m2 عمليات حاجبة في async | **مُصلَح للإعداد**: حفظ الحالة الأولية + اقتطاع + `preallocate` + قراءة الحالة في الاستكمال على مجمّع الحجب (الإنهاء كان كذلك). **مفتوح:** `claim_files`/`open` للجزء/القفل ما زالت مباشرة (عمليات بيانات وصفية قصيرة) | `blocking_filesystem_work_does_not_stall_the_executor` (runtime بخيط واحد؛ عملية قرص موقوفة عمداً والمُجدول يتحرك؛ يفشل عند إرجاعها inline) |
| m3 Retry-After بتاريخ HTTP | **مُصلَح**: IMF-fixdate نسبةً إلى `Date` الرد (لا ساعة الجهاز) بحساب مشبَّع وسقف؛ تاريخ ماضٍ/مشوَّه ⇒ `None` (backoff عادي، لا حلقة ساخنة) | `retry_after_http_date_is_honoured_with_bounded_arithmetic`، `http_date_retry_after_is_honoured_relative_to_the_server_date`، `numbers_from_network_use_checked_ops_retry_after_and_backoff` |

## سجل CI لـPR1–PR4 (جرى دمجها في main @ 58d2f42)
آخر تشغيل لرأس كل PR قبل الدمج (من `gh run list`؛ الناجح فقط مذكور كأخضر):
| PR | رأس الفرع | ci-linux | ci-windows |
|---|---|---|---|
| PR1 `p1/conn-cap` | `7e9510c` | success `43404489688` | success `43404489663` |
| PR2 `p1/drop-resp` | `3468e99` | success `43404489392` | success `43404490739` |
| PR3 `p1/adr-0012` | `3ce5c07` | success `43404698952` | success `43404698960` |
| PR4 `p1/review-fixes` | `7e1ef19` | success `43404983213` | success `43404983175` |

- تشغيلان `cancelled` على `3468e99` (`43404489337` linux، `43404489279` windows) أُلغيا (conclusion=cancelled)، وبقي لنفس الرأس تشغيل ناجح؛ لا يُحتسبان فشلاً أو نجاحاً.
- فشلت قبلها (للتاريخ، لا تُحسب خضراء): conn-cap `43402351147` (push، «workflow file issue» — YAML) و`43402361819` (windows، `security_hardlinked_part_file_is_refused_on_resume` NOT RUN) على `45ee840`؛ وجولة `859aa38`/`77a2e1b`/`48d496c`/`540b017` (linux فشل لـ`540b017` `43395030089`، ثم أُصلح).
- تشغيلات push على main: `43406673837` (@ `58d2f42`) قيد التشغيل وقت الكتابة؛ ثلاثة تشغيلات push أخرى على main أُلغيت (`43406428312`، `43406565666`، `43406616357`) لأن الدمجات توالت. لا أُسجّل main أخضر حتى يكتمل `43406673837`.

## PR5: فشل ci-windows `43407352362` @ `74abff2` (kill_then_resume…)
- الفشل: `kill_then_resume_requests_only_from_checkpointed_frontier` عند `g2_state.rs:165` (`matches!(r, Err(Error::Cancelled))` خطأ) داخل `check_required` فقط (نجح في خطوة `cargo test --workspace` بنفس الـcommit: g2_state 13/13؛ ci-linux نجح في الخطوتين) ⇒ متقطع. رسالة الـassert لم تكن تطبع `r`، فالقيمة الفعلية لم تُسجَّل.
- **السبب (تسابق اختبار قائم مسبقاً، لا خلل محرك)** — استنتاج بالأدلة غير المباشرة، لا بقيمة `r` من Windows: الاختبار يلغي عندما تُظهر **الحالة المحفوظة** ≥5 MiB، والتحميل (16 MiB، 4 اتصالات × 2 MB/s ≈ 2 ث) قد يكتمل قبل أن يصبح هذا التقدم مرئياً إذا كان checkpoint بطيئاً (على Windows: fsync+`MoveFileEx`، وسطي ~25ms وأقصى 1.575s، وفي `check_required` تعمل 4 اختبارات بالتوازي على عدّاء بنواتين). عندها `download` يعيد `Ok` وهذا صحيح (كل البايتات كُتبت)، فيفشل الشرط.
- الدليل: (1) قراءة المحرك: لا مسار يُرجع خطأ checkpoint أو `join` بدل `Cancelled` بسبب الإلغاء نفسه (`persist_checkpoint` لا يقرأ الإلغاء)، وعند الاكتمال بعد الإلغاء `run_ranged` يعيد `Ok` عمداً. (2) تجربة مؤقتة (لم تُدمج) على Linux: checkpoint أول معلّق 2.5 ث ⇒ المراقِب رأى ≥5 MiB عند 2.58 ث والنتيجة `Ok(…/k.bin)`. (3) **نفس التجربة على `origin/main` @ 58d2f42 (قبل PR5) أعطت نفس النتيجة** (`Ok` عند 2.73 ث) ⇒ التسابق قائم قبل PR5؛ PR5 يغيّر احتماله فقط (حلقة المنسّق لم تعد تُحجَب بالـcheckpoint فيصل الـramp للسقف أسرع).
- الإصلاح: **الاختبار فقط، لا المحرك**: كل استجابة تتوقف بعد 2 MiB لمدة 30 ث (`Stall`, `once: false`) فلا يمكن أن يكتمل التحميل قبل الإلغاء مهما بلغ زمن الـcheckpoint (~8 MiB تُكتب ثم لا شيء يتحرك). الشرط `matches!(r, Err(Error::Cancelled))` كما هو حرفياً، وأُضيفت رسالة `expected Cancelled, got {r:?}`. لا توسيع حدود زمنية، لا حذف أو تجاهل، لا تغيير في المحرك.
- **لم يُختبر على Windows الحقيقي:** النتيجة بعد الإصلاح من Linux فقط؛ يلزم تشغيل ci-windows الجديد (راجع أرقام التشغيل في PR5).

## PR6 (`ci/windows-on-main-push` @ `5bac5e4`): فشل ci-windows `43413774280` (المحاولة 1) ونجاح المحاولة 2
- **المحاولة 1 (فشل):** `aborting_the_caller_drains_the_job_and_persists_a_final_checkpoint` عند `g2_state.rs:586`، رسالة الـpanic: `state never advanced after the caller was aborted (job was abandoned)`. ci-linux للرأس نفسه نجح (`43413774423`، 96/96). لا علاقة للتعديل بالاختبار (تغيير workflow فقط)، وهو نفسه نجح على Windows في آخر تشغيلين (`43412182501` وغيره).
- **المحاولة 2 (`gh run rerun 43413774280 --failed`، أطلقها ا بموافقة المالك 14:13 بتوقيت الكويت):** **success** (job `131851711773`، اكتمل 14:25 بتوقيت الكويت). نجاح واحد لا يثبت زوال التقطع؛ لذلك الإصلاح التالي.
- **السبب (استنتاج، لا قيمة مسجّلة من Windows):** الرسالة القديمة لا تميّز بين حالتين لأن الحلقة كانت `state::load(..).unwrap_or(0)`: (أ) اكتمل التحميل (16 MiB بـ4×2 MB/s ≈ 2 ث) على مشغّل بطيء قبل أن يصل الإلغاء فأزال المحرك الـsidecar عند الإنهاء فلا حالة تُقرأ بعدها؛ (ب) فشل `state::load` عابر (Windows) عُومل كأنه 0 بايت. لم يُطبع الخطأ الفعلي فلا يمكن الجزم.
- **الإصلاح (الاختبار فقط، فرع `fix/abort-drain-test-deterministic`):** (1) كل استجابة تتوقف بعد 2 MiB لمدة 30 ث (`Stall`, `once:false`، مثل `kill_then_resume…`) فلا يمكن اكتمال التحميل خلال نافذة المراقبة؛ (2) فشل `state::load` يُعاد ولا يُعامل كـ0 مع حفظ آخر خطأ وآخر frontier؛ (3) عند انتهاء المهلة تطبع الرسالة آخر خطأ `load` وآخر frontier؛ (4) مسار الـsidecar معروف (`m4.bin.eid-state`) بدل مسح المجلد (المسح قد يقع في فجوة التدوير primary→`.bak` ويُفشل `unwrap`). **لم يتغير:** الشرط `frontier >= 1 << 20` (حرفياً)، مهلة الـ20 ث، تعطيل الـcheckpoint الدوري، فحص الاستئناف الدقيق (SHA)، المحرك (`src/` بلا تغيير)، `required_tests.txt`.
- **نتائج محلية (Linux فقط):** x20 للاختبار 20/20 ناجح (~1 ث للواحد)، وضغط إضافي 3 جولات × 8 نسخ متزامنة (8 أنوية) كلها ناجحة؛ `cargo test --workspace` 150 ناجح و0 فاشل؛ `check_required` (debug وrelease) 96/96؛ fmt وclippy `-D warnings` وcargo check لـwindows-gnu نظيفة. **لم يُختبر على Windows الحقيقي؛ نتيجة ci-windows لهذا الفرع في وصف الـPR.**

## دمج PR6 وPR7، وأول تشغيل ci-windows على push إلى main
دُمجا في main: PR6 @ `fb829eb` ثم PR7 @ `77ce8da` (2026-11-13، موافقة المالك). `concurrency` لكل workflow على `github.ref` مع `cancel-in-progress`.

| الحدث | الرأس | التشغيل | النتيجة | الوقت (توقيت الكويت) |
|---|---|---|---|---|
| أول ci-windows على push إلى main (دمج PR6) | `fb829eb` | `43418310530` | **cancelled** (التزامن: بدأ تشغيل `77ce8da` بعده بـ~4 دقائق) | بدأ 14:35، أُلغي 14:39 |
| أول ci-linux على push بعد PR6 | `fb829eb` | `43418310554` | **cancelled** (نفس السبب) | بدأ 14:35، أُلغي 14:39 |
| أول ci-windows اكتمل على push إلى main (دمج PR7) | `77ce8da` | `43418779357` (job `131863281120`) | **failure** | بدأ 14:39، اكتمل 14:48 (9د 2ث) |
| ci-linux لنفس الرأس | `77ce8da` | `43418779334` (job `131863348467`) | **success** | بدأ 14:39، اكتمل 14:46 (6د 53ث) |
| إعادة تشغيل ci-windows على main (محاولة 2 لـ`43418779357`، أطلقها ا لجمع دليل) | `77ce8da` | `43418779357` محاولة 2 (job `131868320739`) | **success** | بدأ 14:50، اكتمل 15:02 (11د 50ث) |

- **ci-linux `43418779334`:** `check_required` debug **96/96** و`--release` **96/96**.
- **ci-windows `43418779357`:** خطوة `cargo test --workspace --no-fail-fast` نجحت في نفس التشغيل؛ `check_required` فشل: `required: 82  ok: 81  bad: 1` — `limiter_real_time_smoke_bounded FAILED`. الرسالة: `limiter stalled the transfer: 6.51s (total 9.21s)`. سطر المراحل: `LIMITER PHASES start=0.000s started=0.017s first_open=1.032s last_close=7.539s finished=9.205s returned=9.205s | probe=1.014s transfer=6.508s finalize=1.666s total=9.205s` (حد الاختبار 1.5–4 ث على مرحلة النقل فقط). الاختبار غير الذي أصلحه PR7. هذه المحاولة 1. الحدّ لم يُوسَّع. لا إصلاح في هذا السجل الوثائقي. المحاولة 2 أدناه.
- **ci-windows `43418779357` المحاولة 2** (`gh run rerun --failed`، أطلقها ا لجمع دليل — نجاحه لا يعني أن المشكلة انحلّت): **success** (job `131868320739`، بدأ 14:50 واكتمل 15:02 بتوقيت الكويت). `check_required` **required: 82  ok: 82  bad: 0**. `limiter_real_time_smoke_bounded ... ok`. سطر `LIMITER PHASES` غير موجود في سجل المحاولة 2 (الاختبار نجح؛ خرج المراحل لا يظهر في سجل CI عند النجاح). نجاح مرة واحدة لا يثبت زوال التقطع. الحدّ لم يُوسَّع. لم تُجرَ إعادة تشغيل من هنا.
- أُضيفت في PLAN ثلاث بنود P1 مفتوحة من مراجعة Codex أثناء PR7 (`state::load` ليست للقراءة فقط؛ `stall_timeout` في `fast_cfg` 5 ث؛ `run_ranged` يتجاهل خطأ آخر checkpoint). لا يُنفَّذ إصلاح بالمحرّك إلا بعد موافقة المالك.

## M2 (`p1/m2-finalize-identity`): هوية الملف المنشور — أدلة محلية (Linux فقط)
- **Red-first (على main + hook تجهيزي فقط، commit `e4d1c16`، المخرجات في `reports/p1b1_red_first_output.txt`):** `finalization_with_a_swapped_part_publishes…` فشل: SHA المنشور `712d0f7e…` ≠ الأصلي `ebf58209…`؛ `…symlink_swapped_for_the_part_never_publishes_the_link` فشل: نُشر symlink كملف التحميل؛ `finalization_detects_a_destination_replaced_before_success` فشل: أُبلغ نجاح والوجهة غريبة؛ `crash_between_link_and_unlink_is_recovered…` فشل: `I/O error (open part file): refusing file with multiple hard links`. بعد الإصلاح كلها تنجح.
- **محلياً (الرأس النهائي، بعد إصلاح جولة 3):** `cargo test --workspace` 167 ناجح/0 فاشل؛ `check_required` debug وrelease **113/113** (كان 96)؛ fmt وclippy `-D warnings` نظيفان (Linux وwindows-gnu)؛ `cargo check --all-targets --target x86_64-pc-windows-gnu` نظيف؛ x20 لـ`eid_platform`، `hardlink`، `g12_finalize_swap`، `g8_finalize_crash`، `security`، `g10_lock_race`، `g9_backup_removal`، `g1_transfer`، `g6_restart_crash`: 20/20 لكلٍّ.
- **بنود اكتُشفت أثناء M2:** (1) مسار الاستعادة عند غياب الـpart (قديم، قبل M2) كان يحسب SHA عبر المقبض ثم يعلن `Finished` دون إعادة فحص أن الاسم ما زال نفس الملف (Codex جولة 3، متوسط) — **صُلح بنفس PR** (فحص `handle_is_path` بعد الـhash؛ استبدال أثناء الـhash يُرفض ويُبقي الحالة): red-first `recovery_without_a_part_refuses_a_destination_substituted_while_hashing` فشل بـ«Finished was reported for a destination swapped during verification»، ثم نجح. (2) انظر «تقرير g2_state» أدناه.
- **CI لـ`8937072` (قبل إصلاح Codex جولة 3):** ci-linux `43429633038` **success** (`check_required` debug وrelease **112/112**)؛ ci-windows `43429632845` **success** (`check_required` **89/89**؛ نُفِّذت على Windows فعلياً: `finalization_with_a_swapped_part_publishes…` (فرع «يرفض»)، `publish_verified_fallback_*`، `recovery_open_rejects…`، `fs::tests::windows_rollback_failure_preserves_both_names`). نجاح مرة واحدة لا يثبت غياب التقطع. **ما زال لم يُختبر على Windows الحقيقي خارج CI.**
- **CI لـ`179ccee` (الرأس بعد إصلاح جولة 3):** ci-linux `43434503919` **success** (`check_required` debug وrelease **113/113**؛ خطوة cargo-deny تطبع `licenses FAILED` لكن الـjob نجح — لا علاقة لها بـM2، لم تُفحص هنا). ci-windows `43434504004` **failure**: `check_required` **89/90، bad=1** — `aborting_the_caller_drains_the_job_and_persists_a_final_checkpoint` عند `g2_state.rs:607`، الرسالة: `state never advanced after the caller was aborted (job was abandoned): last observed frontier = Some(0), last state::load error = None` (job `131916018900`). **غير مرتبط بـM2 على الأرجح** (M2 لا يمس الإلغاء/الـcheckpoint؛ الاختبار يُلغى قبل أي إنهاء، ونفس الاختبار فشل سابقاً على main قبل M2 — run `43413774280` محاولة 1)، لكنه **ليس مثبتاً بالقياس**. المعنى: الـsidecar قابل للقراءة (`load` بلا خطأ) لكن الـfrontier بقي 0 طوال 20 ث بعد الإلغاء، أي الـcheckpoint النهائي بعد drain لم يُكتب في النافذة على Windows (إصلاح PR7 منع «اكتمل التحميل» وخطأ `load` لكنه لم يمنع هذا). **لم تُجرَ إعادة تشغيل. قرار المالك مطلوب** (تشخيص وإصلاح منفصل). في وقت `179ccee` لم يُظهر ci-windows نجاحاً كاملاً بعد إصلاح جولة 3؛ نجح `8937072` (دون ذلك الإصلاح). الرأس النهائي `8ed8665` أدناه.
- **CI للرأس النهائي `8ed8665`:** ci-linux `43435698298` **success** (`check_required` debug وrelease **113/113**)؛ ci-windows `43435697809` **success** (`check_required` **90/90**). سبقه على `179ccee` فشل ci-windows `43434504004` (**89/90، bad=1**) في `aborting_the_caller_drains_the_job_and_persists_a_final_checkpoint` عند `g2_state.rs:607` (last frontier `Some(0)`، last state::load error `None`) — تقطع قائم مسبقاً، ظهر أيضاً على main في `43413774280` المحاولة 1. نجاح `8ed8665` مرة واحدة لا يثبت زوال التقطع.
- **تقرير `g2_state` (`job_lock_covers_a_missing_part_and_restart_path`):** فشل مرة واحدة من 20 محلياً (Linux) على `ebdf394`: بقي `m1b.bin.eid-state.broken` فاختلف `only_files` النهائي. **لم يتكرر:** 0/40 على `origin/main` و0/40 على الفرع (نفس الحلقة `--test-threads=4`)، فهو ليس مُثبتاً كتراجع من M2. **التفسير (استنتاج، غير مُثبت بالقياس):** حلقة المراقبة داخل الاختبار (`run_until_progress` تستدعي `state::load`) تُسمّي الملف السليم `.broken` إن قرأته لحظة تدوير الـsidecar — وهو نفس البند المفتوح (أ) «state::load ليس للقراءة فقط» من PR8. يبقى بنداً لـP1 (دفعة (أ)(ب)(ج)). ولم يُغيَّر الاختبار هنا.

- **Codex review (4 جولات، `reports/p1b1_codex_review_*.txt`):** قُبلت النتائج 2–6 وقُبلت 1 جزئياً (انظر DESIGN_NOTES «Round 6»).

## أول تشغيل CI على push إلى main بعد دمج PR9 (`8854c6d`)
دُمج PR9 في main @ `8854c6d` (2026-11-13، موافقة المالك). أول تشغيلات push لهذا الرأس (`gh run list --branch main --event push`؛ لم تُعاد، لم تُوسَّع حدود):

| الحدث | الرأس | التشغيل | النتيجة | الوقت (توقيت الكويت) |
|---|---|---|---|---|
| ci-windows على push إلى main (دمج PR9) | `8854c6d` | `43437514863` (job `131926331303`) | **success** | بدأ 16:52، اكتمل 17:00 (7د 47ث) |
| ci-linux على push إلى main (دمج PR9) | `8854c6d` | `43437514935` | **success** | بدأ 16:52، اكتمل 17:00 (7د 37ث) |

- **ci-linux `43437514935`:** `check_required` debug **113/113** و`--release` **113/113**.
- **ci-windows `43437514863`:** `check_required` **required: 90  ok: 90  bad: 0**. نجاح مرة واحدة لا يثبت زوال التقطع (بما فيه `limiter_real_time_smoke_bounded` و`aborting_the_caller_drains_the_job_and_persists_a_final_checkpoint`).


## P2 — القوائم والجدولة والتصنيف والجملة (فرع `claude/zen-fermat-c6xxgt`، Linux فقط)
**الحالة: منفَّذة وتمر اختباراتها محلياً؛ لم تُراجَع في جلسة مراجعة منفصلة (AGENTS §4) ولم يوافق المالك على إغلاقها.** طلب المالك «إكمال كل المراحل» عُدِّل بقراره (سؤال في الجلسة) إلى «إغلاق P1 ثم P2 فقط».

| بند PLAN P2 | ما بُني | الاختبار (الاسم في `required_tests.txt`) |
|---|---|---|
| 10 تحميلات بحد 2 لا يتجاوز 2 | `queue::QueueSet::plan` (قرار نقي) + إعادة تحقق عند الإطلاق في `Scheduler` (طبقتان) | `ten_downloads_with_a_limit_of_two_never_run_more_than_two` (ساعة وهمية؛ 15 ثانية وهمية لا 30؛ يفشل بـ«peak 10» إن أُزيلت الطبقتان — جُرِّب)، `plan_never_starts_more_than_max_parallel_and_fills_freed_slots`، **وبمحرك حقيقي:** `a_queue_of_real_downloads_respects_its_parallel_limit_and_finishes_everything` (الذروة يقيسها سجل الخادم = 2) |
| جدولة بعد 5 ث تبدأ خلال ثانية (ساعة وهمية) | `Item::not_before` + `wake_at` | `a_job_scheduled_in_five_seconds_starts_within_a_second_of_that` |
| نوافذ زمنية (بدء/إيقاف، ليلية، أيام الأسبوع) | `queue::Window`، إيقاف المهام عند الإغلاق وإعادة تشغيلها عند الفتح | `window_contains_overnight_and_weekday_masks`، `window_next_change_finds_the_next_boundary`، `plan_closed_window_starts_nothing_and_stops_running_jobs`، `a_closing_window_stops_running_jobs_and_the_next_opening_restarts_them` |
| إعادة محاولات الطابور (الفاشل لآخر الطابور) | `RetryPolicy` (تأخير خطي)، تصنيف الأخطاء `queue_runner::is_retryable` | `failures_are_requeued_at_the_end_with_growing_delay_and_then_given_up`، `retry_classification`، `a_panicking_job_fails_alone_and_the_queue_continues` |
| `queues.json` ذرّي/خاص/تالف | `store.rs` (checksum، `.bak`، حجم أقصى، لا اتباع روابط، **قراءة فاشلة لا تُعيد تسمية شيء** — يتفادى بند P1 (أ) في الكود الجديد) | `store_roundtrip_private_and_missing_is_none`، `store_rejects_corrupt_tampered_huge_and_wrong_version_then_uses_the_backup`، `store_never_follows_a_symlink`، `queue_set_validation_rejects_hostile_content` |
| kill للطابور ثم الاستكمال | `Scheduler::open` يُنزل `Running`→`Pending`؛ `EngineRunner` يستأنف من الـsidecar (باسم الملف المسجَّل أو بمطابقة الرابط `state::find_for_url`) | **`cli_queue_sigkill_then_run_resumes_and_never_repeats_finished_items`** (SIGKILL لعملية `eid queue run` بعد ≥30% دائم: المكتمل لا يُطلب ثانية، الكبير يستأنف دون إعادة طلب بايتات دائمة، والباقي يكتمل، SHA مطابق)، `stopping_the_queue_mid_download_and_reopening_resumes_from_the_frontier`، `a_restart_demotes_running_items_and_never_reruns_done_ones` |
| العد التنازلي للإيقاف قابل للإلغاء (mock) | `PowerControl` (trait) + عدّ 60 ث + إعادة فحص قبل التنفيذ؛ لا يُسلَّح إلا لطابور اشتغل في هذه العملية | `shutdown_runs_once_after_the_full_countdown_when_nothing_cancels_it`، `the_countdown_can_be_cancelled_and_then_nothing_is_executed`، `new_work_during_the_countdown_aborts_it`، `no_countdown_without_an_action_for_pending_retries_or_for_a_queue_loaded_finished`، `a_failing_power_action_is_reported_not_swallowed` |
| استيراد/تصدير يعيد نفس القائمة وملف خبيث لا يُسقط البرنامج | `porting.rs` (txt/json/csv؛ حد 8 MiB و50 000 عنصر؛ لا حقل مجلد؛ الأسماء تُنظَّف؛ بيانات الدخول تُحذف؛ تحييد صيغ CSV) | `export_then_import_returns_the_same_list_in_every_format`، `malicious_files_are_rejected_or_neutralized_never_crash`، `csv_export_neutralizes_formulas_and_import_restores_them`، `cli_queue_add_list_export_import_roundtrip_and_no_secrets` |
| الأنماط `(001-050)` | `batch.rs` (أرقام بحشو، حروف، حتى 3 مجموعات، سقف 10 000) | `batch_*` (4 اختبارات) و`text_import_expands_patterns_within_bounds` |
| التصنيف | `category.rs` (الامتداد ثم MIME ثم «Other»؛ المجلد مكوّن واحد منظَّف) + `--categorize` في CLI | `category_*` (2)، `cli_queue_categorize_sorts_downloads_into_sub_folders_inside_the_download_dir` |
| Grabber لصفحة واحدة يرفض الضخمة | `grabber.rs` (ماسح HTML بلا JS، يتجاهل script/style/تعليقات، يحل `<base>`، يحذف javascript:/data:/mailto:، سقف حجم وروابط، مهلة ترويسات) | `grabber_*` (4 + `read_capped…`)، `grabber_refuses_a_huge_page_before_reading_it_and_other_bad_pages` (300 MiB: يُرفض من Content-Length، الخادم لم يرسل إلا ما تتسع له مخازن النواة)، `cli_grab_lists_links_and_adds_them_to_a_queue` |
| ساعة محلية | `eid_platform::time::local_now` (`localtime_r` / `GetLocalTime`) | `from_unix_weekday_minute_and_offsets` (Windows: يُترجَم فقط) |

**أدلة التشغيل المحلي (Linux فقط، هذه الجلسة):** `scripts/check_required.py` في وضع **release: 174/174** (`required: 174  ok: 174  bad: 0`)؛ في وضع debug **173/173** قبل إضافة آخر اختبار (`cli_queue_categorize_…`) الذي نجح منفرداً في debug ولم تُعَد دورة debug الكاملة بعده. `cargo fmt --check` وclippy `-D warnings` و`cargo check --all-targets --target x86_64-pc-windows-gnu` نظيفة. أول تشغيل لـ`check_required` كشف فشل حارس `platform_code_and_process_spawning_only_in_eid_platform` (استعمال `cfg!(unix)` في الـengine) فأُصلح بـ`eid_platform::IS_WINDOWS`. **لم يُشغَّل CI على Windows/Linux (لا PR مفتوح).**

**حدود صادقة لـP2:**
- **`PowerControl` الحقيقي (إيقاف/سبات) غير موجود** — فقط الـtrait والمحاكاة؛ التنفيذ في `eid-platform` مع P3. لذلك لا يعرض CLI خيار `--after-done` إطلاقاً.
- التصنيف في CLI يعتمد **امتداد المسار في الرابط** (اسم الملف الحقيقي من Content-Disposition/MIME لا يُعرف قبل بدء النقل). `Categorizer::classify` يدعم MIME لكنه غير موصول بالمحرك.
- لا «اتصال واحد لكل مضيف» ولا سقف عام عبر الطوابير (غير مطلوب في شروط P2؛ **مخطط**).
- حدّ نافذة الوقت يعتمد على تحقق الحلقة؛ تغيّر التوقيت الصيفي يُلتقط بأقصى `max_idle` (30 ث).
- نافذة صغيرة: قتل العملية بين `rename` النهائي وحفظ `Done` في `queues.json` يُعيد التحميل باسم مرقّم `(1)`.
- لم يُختبر على Windows: `local_now` (GetLocalTime)، اختبارا الروابط الرمزية الجديدان (`store_never_follows_a_symlink`، `job_dir_creation_and_refusals`) يحتاجان صلاحية إنشاء الروابط على الـrunner (يفشلان بصوت عالٍ إن لم تتوفر).


## فشل CI على PR #10 (`etag_change_between_attempts_restarts_with_new_content`، ci-linux release 173/174)
- **الأعراض:** `only_files` وجد `e.bin.eid-state.broken` بجانب `e.bin`.
- **السبب الجذري (مُثبَت بمُعيد إنتاج، لا استنتاجاً):** `state::load` ليست للقراءة فقط (= البند المفتوح (أ) في PLAN): عند فشل قراءة الأساسي تنقل `exists()`-ثم-`rename` الملف إلى `.broken`، فإذا وقع ذلك لحظة تدوير الـcheckpoint (الأساسي→`.bak`→أساسي جديد) أُعيدت تسمية ملف **سليم**. في هذا الاختبار القارئ هو **مراقِب الاختبار** (`run_until_progress` كان يستعمل `state::load` على ملف حي كل 10ms)؛ المحرّك نفسه لا يقرأ ملفاً حياً أثناء العمل. مُعيد الإنتاج `g18_open_item_a.rs` (مُتجاهَل `#[ignore]`، يوثّق السلوك): `saves=9 broken_created=true` في release.
- **لم أستطع إعادة الفشل في الاختبار نفسه:** 0/80 في release تحت حمل 6 عمليات `yes` على 4 أنوية (احتمال ضئيل لنافذة السباق)؛ الجزم بالسبب من مُعيد الإنتاج ومن شكل الفشل لا من إعادة الفشل نفسه.
- **الإصلاح:** كل مراقب اختبار لملف حي صار يستعمل القارئ غير المُغيِّر `state::peek` (`g2_state` مرتان، `g11_ckpt_task`، `cli.rs`). **لم تُغيَّر أي تأكيدات ولا حدود زمنية، ولم يُتجاوَز اختبار.** **لم ألمس المحرّك** لأن (أ) مؤجَّل بقرار المالك؛ العيب في `state::load` باقٍ وموثَّق ومُثبَت بالاختبار المتجاهَل، ويجب إصلاحه قبل أن يقرأ أي مكوّن آخر (واجهة/`eid queue list`/IPC في P3) ملفاً حياً.
- **التحقق (release، Linux، تحت حمل):** `g2_state` كاملاً 0/25 فشل، `g11_ckpt_task` 0/25، `cli` 0/10؛ clippy نظيف. `check_required --release` الكاملة محلياً: **174/174**.


## بند المحرّك (أ): `state::load` لا يعدّل الملفات (فرع `fix/state-load-read-only`)
- **التغيير:** `state::load` صارت قراءة فقط تماماً (أساسي ثم `.bak`، بلا `rename`/إنشاء/حذف). الحجر الصحي `.broken` انتقل إلى دالة صريحة `state::load_repairing` يستعملها **موضع واحد** فقط: بداية `resume_inner` (يمسك قفل الوظيفة ولا توجد مهمة checkpoint بعد). وهي لا تنقل الأساسي إلا إذا فُتح وقُرئ كاملاً و**محتواه** غير صالح (JSON/التحقق/حجم)؛ فشل فتح أو قراءة (مفقود، رابط رمزي/صلب مرفوض، تعارض مشاركة عابر على Windows) لا ينقل شيئاً. `finalize_name` وبقية القرّاء يستعملون `load` القارئة فقط.
- **قبل/بعد (red-first):** الاختبار `state_load_never_quarantines_a_valid_file_during_rotation` (كان `#[ignore]` باسم `open_item_a_…`) على الكود القديم: فشل بـ«a valid, concurrently rotated state file was quarantined as .broken» (`saves=2 loads=5 broken_created=true`)؛ بعد التغيير: نجح (`saves=2295 loads=22391 broken_created=false`، 5 ثوانٍ).
- **اختبارات جديدة:** `load_never_changes_anything_on_disk_in_any_state_of_the_pair` (9 تركيبات أساسي×نسخة احتياطية: سليم/تالف/غائب، لقطة كل الملفات قبل وبعد)، `corrupt_primary_with_good_bak_is_served_from_the_bak_without_touching_the_primary`، `load_repairing_never_moves_a_primary_that_was_merely_unreadable` (مجلد مكان الـsidecar ثم غائب)؛ وعُدِّل `corrupt_or_hostile_state_file_is_rejected` (يُثبت أن `load` لا تحجر وأن `load_repairing` تحجر المحتوى التالف). أُضيفت الأسماء الأربعة إلى `required_tests.txt` ولم يُحذف اسم.
- **الأدلة (Windows 10 محلياً، لا Linux):** `cargo fmt --check` نظيف؛ `cargo clippy --workspace --all-targets -- -D warnings` نظيف؛ `cargo check --workspace --all-targets --target x86_64-pc-windows-gnu` نظيف؛ `cargo test --workspace --no-fail-fast`: كل شيء ناجح ما عدا **6 اختبارات تفشل لأن الجهاز لا يملك صلاحية إنشاء الروابط الرمزية** (خطأ 1314؛ تفشل بصوت عالٍ عمداً حسب AGENTS §6.1): `symlinked_state_path_is_not_followed`، `store_never_follows_a_symlink`، `job_dir_creation_and_refusals`، `symlink_capability_precheck`، `security_open_regular_nofollow_rejects_symlink_and_hardlink`، `security_create_new_refuses_existing_and_dangling_symlink`. لا علاقة لها بالتغيير (لا تمس `load`)، ولم أقارنها بتشغيل على الأساس؛ تحتاج Linux أو وضع المطوّر في Windows.
- **لم يُشغَّل:** `scripts/check_required.py` (لا Python على هذا الجهاز) فلم يُتحقق من العدّ الكلي ولا من اختبارات `unix:`؛ ولا CI. بنداً (ب)(ج) لم يُمسّا.
- **مخاطرة معروفة:** إن وجد `finalize_name` أساسياً تالفاً خارجياً (ليس من تحطّم، فالاستئناف يحجره) فسيقرأ من `.bak` ثم يدوّر الأساسي التالف فوقه عند `save`؛ نافذة تحطّم ضيقة. لم تُعالَج لأن الاستئناف هو الطريق الوحيد لمواجهة ملف خلّفه تحطّم.

## فشل ci-windows على PR #10 (`g13_hardening::second_download_of_a_maximal_name_gets_a_numbered_free_name`)
- **الخطأ (من سجل التشغيل 43576537725):** `Io { context: "replace state", source: Os { code: 3, kind: NotFound, message: "The system cannot find the path specified." } }`. اختبار واحد فشل؛ الباقي في الملف نجح، وci-linux نجح.
- **السبب الجذري:** حد المسار الكامل في Windows (`MAX_PATH` = 260 بدون مسارات طويلة). المحرّك يحدّ الاسم بـ200 بايت (حد المكوّن) لكنه لا يحسب طول **المجلد + اللواحق** (`.eid-state.tmp` وغيرها)؛ المجلد المؤقت على الـrunner ≈ 50 حرفاً فصار مسار ملف الحالة > 259. **عيب حقيقي في المنتج لا في الاختبار** (ليس اختباراً خاصاً بـUnix): دعم المسارات الطويلة (`\\?\` + manifest `longPathAware`) مخطط ولم يُنفَّذ (ARCHITECTURE، MANUAL-TESTS #5)، فكان يجب ألا ينشئ المحرّك مساراً لا يستطيع نظام التشغيل فتحه. أعيد إنتاج الحالة على Linux بحدّ `259` وفشل الاختبار الجديد بـ«266 units» (نفس الطول الذي فشل على Windows) قبل الإصلاح.
- **الإصلاح:** `eid_platform::MAX_PATH_UNITS` (259 على Windows، لا حد في غيره) و`Config::max_path_units`؛ قبل حجز الملفات يُقصَّر الاسم (يُحفظ الامتداد) ليبقى أطول ملف للمهمة (اسم مرقّم ` (999)` + `.eid-state.broken`) ضمن الحد، ومجلد لا يترك مجالاً يُرفض مبكراً برسالة واضحة (`InvalidInput … too long`) بدل خطأ نظام غامض. **لم يُضعَّف أي اختبار ولا شرط:** الاختبار الفاشل كما هو.
- **ما يغطيه الإصلاح وما لا:** ملفات المحرّك ضمن 259 حرفاً. **لا يضيف دعم مسارات أطول من ذلك** (مجلد تحميل عميق جداً يُرفض)، ولا يغيّر سلوك Linux. أسماء الملفات على Windows قد تُقصَّر أكثر من 200 بايت في مجلدات عميقة (موثَّق).
- **الأدلة (Linux فقط):** `every_created_path_stays_within_the_path_limit_and_an_impossible_folder_is_refused` (حد 259 مُحاكى، 4 أسماء قصوى، sidecars كلها + 3 نسخ مرقّمة)، `names_are_fitted_to_a_path_limit_with_room_for_every_sidecar_and_suffix`. **لم يُشغَّل على Windows هنا؛ التحقق الفعلي بإعادة تشغيل ci-windows على PR #10.**

## لم يُختبر على Windows الحقيقي
- **B4 (hard link) و M1 (`LockFileEx` وقفل الملف على Windows):** مُنفَّذان ويُبنيان (`cargo check --target x86_64-pc-windows-gnu --all-targets`) لكن **لم يُشغَّلا على Windows**. تصحيح (M2): قفل الوظيفة (`flock`/`LockFileEx`) يُؤخذ على ملف `<name>.eid-lock` وليس على ملف البيانات، فلا يمنع قراءة الـpart عبر مقبض آخر؛ والـhash يجري عبر المقبض الذي تحققنا منه (`sha256_handle`) لإغلاق استبدال المسار، لا بسبب قفل إلزامي.
كل شيء. الصندوق الحالي Linux بدون Windows/Wine. ما يتحقق منه هنا: بناء المحرك وCLI والاختبارات على Linux و`cargo check`/بناء متقاطع لـWindows (إن نجح). الاختبارات على Windows تنتظر CI على windows-latest (يحتاج مستودع GitHub — لا نربط حساباً) ثم P8 يدوياً.
## P3 — IPC وNative Messaging (فرع `feat/p3-ipc-native`، 7 commits، لم تُراجَع في جلسة منفصلة، لا طلب دمج)
- **ما نُفِّذ:** `eid-platform::{ipc, native_host, dirs, process::spawn_detached}`؛ حزمة `eid-ipc` (إطار ≤1MiB، رسائل صارمة، توكن 256-بت، معرّفات idempotent، خادم/عميل)؛ `eid serve` و`eid add/list/pause/resume/stop`؛ `eid-native-host`.
- **الدليل (Windows 10 محلياً، debug):** `cargo test --workspace --no-fail-fast` = **268 ناجح، 0 فاشل، 1 متجاهَل** (المتجاهَل قديم). أسماء `scripts/required_tests.txt` (202 اسماً بعد إضافة 48) كلها نفّذت ونجحت — تحقّقت منها بحلقة shell لأن **لا Python على الجهاز** فلم يُشغَّل `check_required.py` نفسه (24 اسماً `unix:` لم تُشغَّل هنا). `--release` لـ`eid-ipc` و`eid-native-host` و`eid-platform`: ناجح. `fmt --check` وclippy `-D warnings` وبناء `x86_64-pc-windows-gnu` للمساحة: نظيفة. بناء `x86_64-unknown-linux-gnu` لـ`eid-platform` و`eid-ipc` و`eid-native-host` نظيف (`eid-cli` لا يُبنى لـLinux هنا: `ring` يحتاج مترجم C).
- **عيوب وجدتها الاختبارات (حقيقية):** (1) serde يتجاهل الحقول الزائدة على variants بلا حقول في enum موسوم ⇒ `Ping/List/Shutdown` صارت `{}`. (2) `spawn_detached` على Windows ورّثت أنابيب المضيف للتطبيق فلا يصل المتصفح إلى EOF (الحارس كان يُحرَّر قبل المحاولة الاحتياطية) — أُصلح؛ والاختبار يفشل بصوت عالٍ لا بتعليق. (3) **انحدار سببته أنا:** `eid resume <state-file>` (P1) اصطدم بـ`eid resume <id>` — اكتشفه `cli_sigkill_then_resume_completes_from_frontier` و`crash_at_every_persistence_fault_point_recovers`؛ أُصلح بالتوجيه `is_app_command` (رقم مجرّد أو علم اتصال = عن بعد) وله اختبار.
- **لم يُختبر / غير منفَّذ:** ACL على Windows بمستخدم **ثانٍ** (MANUAL-TESTS #9) — اختبرتُ فقط أن DACL الأنبوب محميّ وبمدخل واحد للمستخدم الحالي (قراءة راجعة `GetSecurityInfo`)، فالوصف الصحيح: «مصمّمة لتكون معزولة، لم تُختبر بمستخدم آخر». كود Unix (socket/peer-uid/مجلد 0700) **يُبنى فقط ولم يُشغَّل** (لا Linux هنا) ⇒ اختبارات `ipc::*` و`nm_*` على Linux لم تُجرَّب. DPAPI، إيقاف الجهاز الفعلي، DACL لملفات `.eid-part/.eid-state/.eid-lock` (#15)، استدعاء `native_host::register` من أي مكان (المثبّت P7)، إضافة المتصفح (P6)، تشغيل المضيف من Chrome/Edge حقيقيين (#8): **لم تُنجَز**. الإصلاح الكامل لوراثة المقابض (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`) لم يُنفَّذ (انظر DESIGN_NOTES Round 8).
- **حد معروف:** معرّفات الطلبات تُحفظ في الذاكرة فقط (1024)؛ إعادة المحاولة بعد إعادة تشغيل التطبيق قد تُنفَّذ مرة ثانية.
- **تصحيح (فشل ci-windows على PR #12):** `windows_pipe_has_a_protected_dacl_with_only_the_current_user` فشل على المشغّل (`D:P(A;;FA;;;LA)`). **السبب الجذري: خطأ في الاختبار لا في الأنبوب** — نص SDDL يكتب حساب Administrator المحلي (RID 500) بالاختصار `LA` و`GA` بعد التحويل `FA`، فمقارنتي النصية (`contains(sid) || contains("OW")`) كانت خاطئة حين يكون المستخدم الحالي هو Administrator المدمج. الأنبوب كان محمياً بمدخل واحد للمستخدم. الإصلاح: الاختبار يقرأ الـDACL هيكلياً (محمي؛ ACE واحد؛ نوعه allow؛ `EqualSid` مع SID المستخدم الحالي؛ قناع غير صفري؛ والأمر نفسه لنسخة الأنبوب التالية) بدل نص SDDL، **دون تضعيف**. واختبار جديد يحرس فرضيات الأول: واصف لـEveryone أو SYSTEM يُرفض، وACE إضافي لـEveryone يظهر، ونص SDDL لـRID-500 يخرج `LA` (أعدتُ إنتاج الاختصار محلياً دون أن أكون Administrator). **لم أستطع تشغيل الاختبار بحساب Administrator المدمج**؛ تحقّقه الفعلي بإعادة تشغيل ci-windows.
- **تصحيح (فشل ci-linux على PR #12، `nm_binary_without_a_running_app_answers_with_an_error_frame`: `Text file busy (os error 26)`):** السبب الجذري **سباق في الاختبار** لا في المنتج: كان الاختبار الوحيد الذي **يكتب ملفاً تنفيذياً (`fs::copy` للمضيف) ثم يشغّله**، داخل عملية اختبار تُشغِّل اختباراً آخر متوازياً (`--test-threads=4`) يُطلق عمليات فرعية؛ `fork()` في خيط آخر يورّث الابن مقبض الكتابة المفتوح لحظياً (إلى أن يُغلقه `exec` بفضل CLOEXEC)، وأي `exec` لملف مفتوح للكتابة يفشل بـETXTBSY على Linux. **الإصلاح بإزالة السبب لا بإعادة المحاولة:** لا نسخ ولا كتابة لأي ملف تنفيذي؛ الاختبار يشغّل المضيف الأصلي مع `EID_NATIVE_HOST_APP` يشير إلى ملف غير موجود (مسار «البرنامج مفقود» الحقيقي) ويتحقق من الإطار الخطأ وصياغة stderr وغياب أي طباعة على stdout. وأُضيف اختبار `the_launcher_refuses_a_missing_program_whether_overridden_or_looked_up_next_to_the_host` يغطي بحث الشقيق `eid` بجانب المضيف (الذي كان الاختبار القديم يقصده بالنسخ). **لم أستطع إعادة إنتاج الفشل هنا (Windows لا يملك ETXTBSY)**؛ الجزم بالسبب من رسالة الخطأ ومن أن هذا هو الموضع الوحيد في الاختبارات الذي يكتب ثم ينفّذ، والتحقق الفعلي بإعادة تشغيل ci-linux.

## P3 — معالجة المراجعة (فرع `fix/p3-review-fixes`، غير مدموج)
- **المصدر:** `docs/reviews/P3.md` (5161c1d). **القرار لكل ملاحظة:** DESIGN_NOTES «Round 9». لم يُحذف أو يُعطَّل أي اختبار؛ اختباران أُعيدت تسميتهما/تقويتهما (`a_symlinked_or_garbage_token_file…` صار يُنشئ الرابط فعلاً؛ `nm_survives_the_app_restarting_between_messages…` أُعيدت تسميته لأنه لم يكن يثبت «بلا تنفيذ مزدوج»).
- **الدليل المحلي (Windows 10، debug، `--test-threads=4`):** 279 ناجح، 0 فاشل، 1 متجاهَل. أسماء `required_tests.txt` المطلوبة على Windows (216 بعد إضافة `windows:`): كلها نفّذت ونجحت (فحص shell؛ لا Python على الجهاز فلم يُشغَّل `check_required.py` المعدَّل نفسه محلياً — **أول تشغيل له على CI**). fmt وclippy `-D warnings` نظيفان. `--release`: `eid-ipc`/`eid-platform`/`eid-native-host` ناجحة (اختبار التشغيل الحقيقي يتطلب بناء `eid` ويفشل بصوت عالٍ إن غاب).
- **تحقّقتُ أن الاختبارات الجديدة تفشل حين يُعطَّل الإصلاح:** `the_client_refuses_a_pipe_whose_server_is_not_the_expected_user` (فحص الخادم معطّلاً) و`a_peer_that_stops_reading_is_dropped_and_its_slot_is_freed` (مهلة الكتابة 600 ث). الباقي لم يُجرَّب بتعطيل الإصلاح.
- **غير مختبر:** كود Unix الجديد (`flock`، فحص `peer_cred`) يُبنى لهدف Linux محلياً ولم يُنفَّذ؛ حساب ثانٍ حقيقي (#17)؛ عميل غير مرتفع (#18).
- **نتائج CI (PR #13):** أبلغ المالك أن `ci-linux` و`ci-windows` **خضراء** ثم دمج PR #13 في `main` (merge `495e246`، شجرة مطابقة لقمة الفرع `d35a8ff` دون أي فرق). **لم أستطع التحقق منها بنفسي**: المستودع خاص، لا `gh`، والمتصفح غير مسجَّل في GitHub (404)؛ فهذه شهادة المالك لا قراءة لسجلات CI. أول تشغيل لـ`check_required.py` المعدَّل (بادئة `windows:`) وأول تنفيذ لكود Unix الجديد حدثا على CI حسب هذه الشهادة؛ سجلاتهما لم أرها.

## P3 — إغلاق (2026-11-15)
- **الدمج:** PR #12 (`6e309cb`، قبل المراجعة) ثم PR #13 (`495e246`، معالجة المراجعة) في `main`. `git diff` بين قمة `fix/p3-review-fixes` و`main` فارغ.
- **فحص محلي على `main` المدموج (Windows 10):** fmt نظيف؛ clippy `-D warnings` نظيف؛ `cargo check --workspace --all-targets --target x86_64-pc-windows-gnu` نظيف؛ `cargo test --workspace --no-fail-fast -- --test-threads=4` = **279 ناجح، 0 فاشل، 1 متجاهَل** (قديم)؛ أسماء `required_tests.txt` المطلوبة على Windows (216) نفّذت كلها ونجحت (حلقة shell؛ لا Python محلياً).
- **CI:** خضراء بشهادة المالك (انظر أعلاه)؛ غير متحقَّق منها من جهتي.
- **شروط PLAN للمرحلة (أ):** كلها مستوفاة باختبارات مسمّاة (جدول المقارنة في `docs/reviews/P3.md` قسم Resolution).
- **معلَّق (لا يمنع الإغلاق بحسب نص PLAN، لكنه معلَّق):** MANUAL-TESTS #8، #9، #15، #16، #17، #18 كلها «لم يُختبر». قيد PLAN: قبل #9 تُوصف القناة بأنها «**مصمّمة لتكون معزولة، لم تُختبر بمستخدم آخر**» ولا يُقال «معزولة».

## P4 — G0 (فرع `docs/p4-g0`، دُفع، لم يُدمج)
- تطبيق القرارات D1–D16 على الوثائق المعتمدة (PLAN «بوابات P4»، ADR-0003، ADR-0007، ARCHITECTURE §6، REQUIREMENTS F11a، DEPENDENCIES، MANUAL-TESTS #19) و`deny.toml` مع جعل `cargo deny check` إلزامياً في `ci-linux`.
- **الدليل (Windows 10 محلياً):** fmt وclippy `-D warnings` نظيفان؛ `cargo test --workspace --no-fail-fast -- --test-threads=4` = **279 ناجح، 0 فاشل، 1 متجاهَل**؛ الأسماء المطلوبة على Windows (216) نفّذت كلها ونجحت (حلقة shell، لا Python).
- **لم يُختبر:** `cargo deny` لم يُشغَّل محلياً (غير مثبّت)؛ أول تشغيل له في CI.

## P4 — G1 (فرع `feat/p4-g1-process` فوق `docs/p4-g0`، غير مدفوع)
- **ما نُفِّذ:** `eid_platform::process::spawn` المُشرَف عليها (Windows: `CREATE_NO_WINDOW|CREATE_SUSPENDED` ⇒ Job `KILL_ON_JOB_CLOSE` ⇒ استئناف؛ Unix: مجموعة عمليات + `killpg`؛ بيئة مُفرَّغة يحددها المستدعي؛ تفريغ الأنبوبين معاً بسقف)؛ حزمة `eid-media` (قائمة الأعلام المسموحة والممنوعة، بنّاء الوسائط، بيئة القائمة المسموحة، تحليل `-J` المحدود، التقدم، التنقيح، التلميحات، `Runner`)؛ حزمة الاختبار `eid-fake-ytdlp` بمقطعَي mp4/mp3 مثبَّتين بالبصمة (`crates/eid-fake-ytdlp/fixtures/README.md`).
- **FFmpeg للمقطع (بإذن المالك 2026-11-15):** نُزِّل `ffmpeg-n8.1.3-35-gf1acb0a266-win64-lgpl-8.1.zip` من BtbN (`autobuild-2026-11-14-12-37`) إلى مجلد مؤقت للجلسة فقط، وطابقت بصمته `9a1c8c9f…e56c` ملف `checksums.sha256` للوسم؛ لم يُثبَّت شيء ولم يُضف إلى PATH. المقطع حتمي (تشغيلان = بصمة واحدة).
- **الدليل (Windows 10 محلياً، debug، `--test-threads=4`، مع `EID_FFPROBE` للـffprobe المتحقَّق منه):** fmt نظيف؛ clippy `--workspace --all-targets -D warnings` نظيف؛ clippy لهدف `x86_64-unknown-linux-gnu` لـeid-platform/eid-media/eid-fake-ytdlp نظيف (بناء فقط، لم يُشغَّل)؛ `cargo check --workspace --all-targets --target x86_64-pc-windows-gnu` نظيف؛ `cargo test --workspace --no-fail-fast` = **308 ناجح، 0 فاشل، 1 متجاهَل** (القديم)؛ الأسماء المطلوبة على Windows **241 كلها نفّذت ونجحت** (منها 26 اسماً جديداً لـG1).
- **فحص «أحمر» (الاختبار يفشل حين يُعطَّل الإصلاح):** بلا `CREATE_NO_WINDOW` يفشل `child_and_grandchild_have_no_console_window`؛ بلا الإسناد إلى Job تفشل `killing_eid_kills_the_tree` و`cancel_kills_the_whole_process_tree` و`a_grandchild_spawned_at_startup_is_in_the_job`؛ تمرير `PYTHONPATH` يُفشل `child_environment_is_an_allowlist`؛ حذف فحص `$`/`%` يُفشل `percent_and_dollar_in_paths_never_reach_ytdlp`. **وجد الفحص الأحمر عيبين في اختباراتي وأصلحتهما** (DESIGN_NOTES Round 10): اختبار النافذة كان ينجح بلا العلم لأن عملية الاختبار بلا نافذة console، واختبار «الأحفاد المتبقية» كان حفيده ينام بقدر الابن.
- **لم يُختبر:** `unix:killpg_kills_the_group` وكل مسار Unix (لا Linux هنا؛ أول تنفيذ في CI)؛ سلوك اختبار النافذة على مشغّل CI (إن فشل الضبط هناك لا يُعلن النجاح ويبقى MANUAL-TESTS #11)؛ `fake_clip_has_video_and_audio_by_ffprobe` نجح محلياً لكنه **ليس إلزامياً بعد** (يصير إلزامياً في G2 مع خطوة CI ‏D3)؛ لا أداة حقيقية شُغِّلت.

## P4 — G2 (فرع `feat/p4-g2-tools` فوق `feat/p4-g1-process`، غير مدفوع)
- **ما نُفِّذ:** الموافقة (`consent`: نص من `locales/{ar,en}/consent.txt` + قائمة الأدوات من الـmanifest، تخزين `{version, text_sha256, ts}` في `settings.json` خاص، سحب يحذف الأدوات)؛ `tools/manifest.json` المثبَّت (الإصدارات D16 بعد إعادة التحقق، بصمات الأرشيفات والملفات المفكوكة)؛ `Fetcher` + `EngineFetcher` (HTTPS، مضيفو الـmanifest على كل قفزة، حجم مضبوط، SHA-256 قبل النشر)؛ فك zip محدود (`zipx`)؛ مخزن الأدوات (تثبيت ذري مع إبقاء السابق، قفل مشاركة-قراءة-فقط والبصمة عبر المقبض)؛ في المحرك `allowed_hosts`/`max_total_bytes` الاختياريان؛ خطوة ffprobe في `ci-windows` (D3).
- **تنزيلات بإذن المالك (2026-11-15):** أرشيف Deno `v2.9.8` الرسمي (طابق `.sha256sum`) لقراءة بصمة `deno.exe`؛ قراءة `SHA2-256SUMS` لـyt-dlp وواجهة إصدارات GitHub (بلا تنزيل yt-dlp). لم يُثبَّت ولم يُشغَّل أي شيء منها.
- **الدليل (Windows 10 محلياً، debug، `--test-threads=4`، مع `EID_FFPROBE`/`EID_REQUIRE_FFPROBE=1`):** fmt نظيف بعد commit التنسيق الأخير (سطران عُدِّلا بعد آخر `cargo fmt` لم يكونا منسّقين في commit المنصة والوسائط؛ التنسيق فقط، بلا تغيير سلوك)؛ clippy `--workspace --all-targets -D warnings` نظيف؛ clippy لهدف Linux لـeid-platform/eid-fake-ytdlp نظيف (لـeid-media تعذّر محلياً: `ring` يحتاج مترجم C لـLinux؛ CI فقط)؛ `cargo check --target x86_64-pc-windows-gnu` نظيف؛ `cargo test --workspace --no-fail-fast` = **331 ناجح، 0 فاشل، 2 متجاهَل** (القديم + `real_tool_install_from_official_release` (ج) لم يُشغَّل)؛ الأسماء المطلوبة على Windows **264 كلها نفّذت ونجحت** (23 جديدة لـG2، منها `windows:fake_clip_has_video_and_audio_by_ffprobe`).
- **فحص أحمر:** بلا `share_mode(FILE_SHARE_READ)` تفشل `a_tool_already_open_for_writing_is_refused` و`tool_hash_is_read_through_the_locked_handle` و`tools_are_locked_against_write_delete_rename_while_a_job_runs`؛ بلا سقف الحجم في zip يفشل `zip_bomb_is_cut_at_the_size_cap`؛ تجاهل بصمة نص الموافقة يُفشل `consent_text_comes_from_locale_files_and_its_hash_is_stored`. خطوة CI لـffprobe جُرِّبت محلياً بـPowerShell 5.1 على الأرشيف المتحقَّق منه (CI يستعمل pwsh 7).
- **لم يُختبر:** أول تشغيل لخطوة ffprobe ولـ`cargo deny` مع `zip` وتبعياتها في CI؛ كل مسارات Unix الجديدة (لا Linux هنا)؛ DACL ملف `settings.json` على Windows يُقرأ فقط عبر اختبار التوكن الذي يستعمل الكاتب نفسه؛ (ج) التثبيت الحقيقي من GitHub لم يُشغَّل (~230 MB، ينتظر إذن المالك)؛ MANUAL-TESTS #19.

## P4 — G3 (فرع `feat/p4-g3-video` فوق `feat/p4-g2-tools`، غير مدفوع)
- **ما نُفِّذ:** `eid video info|get|consent|revoke` (الجودة best/ارتفاع/audio=mp3، `--limit` ⇒ `--limit-rate`، staging داخل مجلد الإخراج باسم ثابت لكل (رابط، جودة) يُستأنف منه، النشر بأيدينا عبر `publish_verified` باسم منظَّف ضمن حد المسار، Ctrl-C يقتل الشجرة، أخطاء مُنقَّحة مع تلميحات، رمز خروج 3 بلا موافقة)؛ خطّافات اختبار خلف ميزة `test-hooks` فقط؛ توسيع الأداة المزيّفة (سجل استدعاءات، `.part` واستئناف، توقف).
- **الدليل (Windows 10 محلياً، debug، `--test-threads=4`):** fmt نظيف (بعد `cargo fmt` لسطر في الاختبار)؛ clippy `--workspace --all-targets -D warnings` نظيف؛ `cargo check --target x86_64-pc-windows-gnu` نظيف؛ `cargo build --release -p eid-cli` نجح و**`eid.exe` الإصداري لا يحوي اسمَي متغيري الخطّافات (0 تطابق)**؛ `cargo test --workspace --no-fail-fast` = **342 ناجح، 0 فاشل، 6 متجاهَلة** (القديم + 5 اختبارات (ج) لم تُشغَّل)؛ الأسماء المطلوبة على Windows **275 كلها نفّذت ونجحت** (11 جديدة لـG3).
- **فحص أحمر:** تعطيل فحص الروابط في staging يُفشل `planted_symlink_or_junction_is_never_followed`؛ تجاهل `--limit` يُفشل `limit_rate_is_passed_from_the_limit_option`؛ إزالة الرفض بلا طرفية يجعل `no_tty_and_no_flag_refuses_consent_without_waiting` يفشل بعد 20 ث. **عيب حقيقي وجده اختبار DRM وأُصلح:** فشل `-J` كان يترك مجلد staging فارغاً.
- **لم يُختبر:** Ctrl-C حقيقي في كونسول Windows (الاختبار يقتل `eid` قسراً على Windows ويرسل SIGINT على Unix؛ MANUAL-TESTS #14)؛ مسارات Unix (CI)؛ اختبارات (ج) الخمسة (`tests/g3_real.rs` و`real_tool_install_from_official_release`) — تنتظر إذن المالك (~230 MB أدوات + فيديو CC حقيقي)؛ MANUAL-TESTS #11 و#19.

## P4 — G4 (فرع `feat/p4-g4-playlists` فوق `feat/p4-g3-video`، غير مدفوع، لم يُدمج)
- **ما نُفِّذ:** `eid video playlist <url> [-d] [--quality] [--limit] [--skip 2,5] [--list]`: كل العناصر محدَّدة افتراضياً بجودة واحدة وما في `--skip` يُستثنى (F11a(1))؛ مجلد باسم القائمة المنظَّف داخل مجلد الإخراج (F11a(2))؛ ترقيم D8 (`eid_media::playlist`: العرض 2/3 يُثبَّت عند أول تنزيل، الرقم = الموضع عند أول رؤية ولا يتغير ولا يُعاد، الجديد = أكبر رقم + 1، المكرر مرة واحدة، رقم أوسع من العرض بخاناته الطبيعية)؛ `.eid-playlist.json` (خاص، checksum، حد 8 MiB، تحقق وحصر عند التحميل، يُرفض ولا يُصلَح) و`.eid-archive.txt` داخل مجلد القائمة (D13) و`.eid-lock` قفل مجلد القائمة؛ الاستكمال (العناصر المنتهية لا تُعاد، الجزئي يُكمَّل من staging)؛ المزامنة بالأرشيف؛ العناصر الخاصة/المحذوفة/DRM تُتخطى وتحتفظ برقمها، والأخطاء المؤقتة (429) تفشل التشغيل وتُعاد لاحقاً.
- **الدليل (Windows 10 محلياً، debug، `--test-threads=4`):** fmt نظيف؛ clippy `--workspace --all-targets -D warnings` نظيف (خطأ `type_complexity` في `playlist::plan` أُصلح بنوع `Planned`)؛ `cargo check --workspace --target x86_64-pc-windows-gnu` نظيف؛ `cargo build --release -p eid-cli`: `eid.exe` الإصداري لا يحوي اسمَي متغيري الخطّافات (0 تطابق)؛ `cargo test --workspace --no-fail-fast` = **356 ناجح، 0 فاشل، 6 متجاهَلة** (اختبارات (ج) الحقيقية لم تُشغَّل)؛ اختبارات G4 الـ12 **نجحت 6 من 6 تشغيلات متتالية إضافية** (12/12 كل مرة)؛ الأسماء المطلوبة في `required_tests.txt`: 316 اسماً، **294 نفّذت ونجحت على Windows**، 22 بادئة `unix:` (**تصحيح، مراجعة P4 m11:** العدّ الصحيح لتلك الجولة 289 و27) لا تعمل هنا (تُسجَّل «لم يُختبر» هنا وتبقى مطلوبة في CI على Linux). `scripts/check_required.py` نفسه **لم يُشغَّل** (لا Python على الجهاز)؛ التحقق تم بمطابقة أسماء المطلوبة مع ناتج `cargo test` نفسه بسكربت صدفة.
- **الأسماء المضافة (14):** الـ12 الواردة في PLAN G4 + اختبار الوحدة `numbering_follows_the_d8_rules` + `hostile_titles_never_remove_the_number_prefix`.
- **فحص أحمر:** تعطيل القفل (`Ok(false) => Ok(lock)`) يُفشل `two_runs_on_one_playlist_folder_cannot_overlap` بعد 30 ث («the second run was not refused and kept running»). **عيبان حقيقيان وجدتهما الاختبارات وأُصلحا:** (1) عنوان مثل `../../evil:x` كان **يقصّ الرقم البادئ** من اسم الملف لأن `item_name` كان ينظّف «رقم - عنوان» كله (فتسقط الفرادة D8 قاعدة 6) — يُنظَّف العنوان وحده الآن ثم يُضاف الرقم؛ (2) سباق في اختبار: قراءة سطر سجل استدعاءات ناقص أثناء كتابته (الأداة المزيّفة تكتب السطر بكتابة واحدة والقارئ يتجاهل سطراً أخيراً بلا نهاية سطر).
- **سلوك موثَّق:** قائمتان مختلفتا المعرّف بالاسم المنظَّف نفسه في مجلد إخراج واحد: التشغيل الثاني يُرفض («this folder belongs to a different playlist; choose another folder»)، لا يُدمج في المجلد (مُختبَر ضمن `playlist_folder_name_is_sanitized_and_confined` بمجلد إخراج لكل حالة). عنوان بجهاز محجوز (`CON`) يصير `05 - _CON.mp4`.
- **لم يُختبر:** yt-dlp الحقيقي على قوائم حقيقية؛ سلوك `--download-archive` الحقيقي (الأداة المزيّفة تحاكيه بسطر `fake ID`، وصيغة yt-dlp الحقيقية `extractor id` لم تُفحص)؛ Ctrl-C حقيقي في كونسول Windows (الاختبار يقتل `eid` قسراً على Windows؛ MANUAL-TESTS #14)؛ مسارات Unix (`unix:` أعلاه، CI)؛ التشغيل في CI لم يتم (الفرع غير مدفوع)؛ اختبار `real_ytdlp_startup_time` (ج) وبقية (ج) تنتظر إذن المالك؛ MANUAL-TESTS #11 و#19؛ مراجعة مستقلة لـP4 كلها لم تُجرَ بعد.

## P4 — حالة الإغلاق (2026-11-15) — **P4 غير مغلقة**
**تسلسل الفروع (فُحص بـ`git merge-base --is-ancestor` و`git rev-list` على `origin`):** `main` (`b9aeb9b`، يحوي خطة P4 عبر PR #15) ← `docs/p4-g0` ← `feat/p4-g1-process` ← `feat/p4-g2-tools` ← `feat/p4-g3-video`. خط مستقيم بلا تفرّع. G4 (`feat/p4-g4-playlists`، `9ed022e`) **دُمجت في `feat/p4-g3-video` عبر PR #16** (`e26a64a`)، فرأس `feat/p4-g3-video` يحوي الآن G0–G4. **لا commit ضائع:** عدد الـcommits في كل فرع من P4 وفي `main` وفي `docs/p4-plan` وليس في رأس `feat/p4-g3-video` = 0 لكلها. الفرق بين `main` ورأس `feat/p4-g3-video`: 65 ملفاً، +9222/−23. **الخطوة الباقية للوصول إلى `main`:** PR واحد `feat/p4-g3-video` → `main` (لم يُفتح ولم يُدمج؛ بموافقة المالك فقط). لم يُحذف أي فرع.

**ما اكتمل (بدليل):** البوابات G0–G4 (انظر أقسامها أعلاه)؛ كل أسماء PLAN/P4 في `scripts/required_tests.txt` عدا اختبارات (ج) الحقيقية (قورنت آلياً: الناقص فقط `real_*` و`real_tool_install_from_official_release` وهي `#[ignore]` عمداً)؛ `cargo deny check` إلزامي في `ci-linux.yml` (D10)؛ خطوة ffprobe في `ci-windows.yml` (D3).
**CI:** أفاد المالك أن PR #16 اجتاز فحوص Linux وWindows. **لم أطّلع على سجلات CI بنفسي** (لا `gh` ولا دخول للمتصفح)؛ فهذا «بإفادة المالك» لا دليل مباشر. لم يُتحقق من أن خطوة `cargo deny` ومسارات `unix:` (مثل `unix:killpg_kills_the_group`) نفّذت فعلاً في تلك الجولة؛ **يُرجى لصق/تأكيد سطر «required: N ok: N bad: 0» من سجل كل من الجولتين.**

**المعلَّق قبل إغلاق P4:**
| البند | الحالة |
|---|---|
| (ج) `real_tool_install_from_official_release`، `real_cc_video_has_video_and_audio_by_ffprobe`، `real_ytdlp_with_percent_and_dollar_in_paths`، `real_ytdlp_does_not_load_a_planted_plugin`، `real_ytdlp_startup_time` | **لم تُشغَّل** — تنزيل ~230 MB أدوات + فيديو CC حقيقي؛ تنتظر إذن المالك الصريح. نتائجها تُسجَّل هنا في STATUS (غير إلزامية للنجاح لكن مطلوبة للتوثيق) |
| MANUAL-TESTS #11 (لا نافذة سوداء على سطح مكتب حقيقي) | لم يُختبر |
| MANUAL-TESTS #14 (Ctrl-C حقيقي في كونسول Windows) | لم يُختبر |
| MANUAL-TESTS #19 (أدوات حقيقية على Windows: موافقة، تنزيل، مضاد فيروسات/SmartScreen على `yt-dlp.exe`) | لم يُختبر |
| أول تشغيل لـ`cargo deny` مع `zip` وتبعياتها، وخطوة ffprobe في CI | بإفادة المالك فقط (انظر أعلاه) |
| ما لم يُفحص قط: صيغة `--download-archive` الحقيقية لـyt-dlp (الأداة المزيّفة تكتب `fake ID`)؛ yt-dlp/FFmpeg/Deno الحقيقية | لم يُختبر |
| مراجعة مستقلة لـP4 (`docs/reviews/P4.md`) | **لم تُجرَ** — البرومبت جاهز في `docs/prompts/review-p4.md`؛ تُشغَّل في جلسة جديدة منفصلة |
| معالجة ملاحظات المراجعة (أو تسجيلها «مرفوضة + السبب» في DESIGN_NOTES) | بعد المراجعة |
| دمج `feat/p4-g3-video` → `main` | **تم** عبر PR #17 (`ce80cc9`) — انظر القسم التالي |

## P4 — بعد الدمج في main (2026-11-15) — **P4 ما زالت غير مغلقة**
- **الدمج:** دُمج PR #17 (`docs/p4-closure`، مبنيّ فوق `feat/p4-g3-video`) في `main` عند `ce80cc9`، فدخلت G0–G4 كلها. فُحص على `origin` بـ`git merge-base --is-ancestor`: `docs/p4-g0` و`feat/p4-g1-process` و`feat/p4-g2-tools` و`feat/p4-g3-video` و`feat/p4-g4-playlists` و`docs/p4-closure` كلها أسلاف لـ`origin/main`؛ الفرق بين رأس `feat/p4-g3-video` و`main` أربعة ملفات وثائق فقط؛ بين `b9aeb9b` و`main` 67 ملفاً (+9271/−24؛ **تصحيح m11:** +9278 بعد PR #18). (الجملتان «لم يُدمج شيء في main» و«PR لم يُفتح» أعلاه كُتبتا قبل PR #17 وقد تقادمتا.)
- **فحص ثابت على `origin/main` (قراءة فقط، لم يُشغَّل أي اختبار):** كل أسماء `required_tests.txt` (315 اسماً بعد إزالة البادئات) لها دالة في المصدر؛ لا اسم مطلوب عليه `#[ignore]`؛ المتجاهَل عمداً هو (ج) فقط (`real_*` الخمسة) إضافة إلى اختبارين قديمين في `bench_tls` وواحد في `g5_one_connection`؛ لا `std::process::Command` ولا `tokio::process` ولا `cfg(windows)` في `src` خارج `eid-platform` (الاستثناءات في `tests/` فقط). **هذا ليس تشغيلاً للاختبارات ولا بديلاً عن المراجعة المستقلة.**
- **CI على `main`:** لم تُفحص السجلات (لا `gh` ولا متصفح بلا إذن). المطلوب من المالك: سطر `required: N ok: N bad: 0` لـ`ci-linux` و`ci-windows` على آخر push إلى `main`، وتأكيد أن `cargo deny` وخطوة ffprobe ومسارات `unix:` نفّذت.
- **ما يمنع الإغلاق:** (1) المراجعة المستقلة لم تُجرَ (`docs/reviews/P4.md` غير موجود) ثم معالجة ملاحظاتها أو تسجيلها «مرفوضة + السبب»؛ (2) (ج) الخمسة لم تُشغَّل؛ (3) MANUAL-TESTS #11 و#14 و#19 «لم يُختبر»؛ (4) CI على `main` بإفادة المالك فقط؛ (5) صيغة `--download-archive` الحقيقية وyt-dlp/FFmpeg/Deno الحقيقية لم تُفحص.
- **للمراجعة:** الفرق بعد الدمج = `git diff b9aeb9b origin/main` (انظر `docs/prompts/review-p4.md`).

## P4 — معالجة ملاحظة المراجعة M3: فقدان `.eid-playlist.json` (فرع `fix/p4-m3-lost-playlist-state` فوق `main` `cc73384`، 2026-11-16)
- **القرار (المالك):** إن غاب ملف الحالة وفي مجلد القائمة أرشيف تنزيل يذكر عناصر أو ملفات ظاهرة بشكل `<أرقام> - …` يُرفض التشغيل برسالة تشرح الاستعادة من نسخة احتياطية أو `-d` لمجلد جديد؛ لا حذف ولا إعادة تسمية ولا استبدال؛ `--list` مسموح مع تحذير. التفاصيل والحدود المعروفة: DESIGN_NOTES «Round 16» (Round 15 محجوز لـM4).
- **الاختبارات الجديدة (في `required_tests.txt`، لم يُحذف اسم):** `playlist_lost_state_with_archive_and_numbered_files_is_refused`، `playlist_lost_state_restored_from_backup_continues_with_the_old_numbers`، `playlist_without_state_in_a_folder_without_earlier_runs_starts_normally`، `playlist_lost_state_leftovers_are_found_read_only`، `playlist_lost_state_archive_that_is_a_link_or_folder_counts_and_is_not_followed`، `numbered_names_are_recognised_by_shape_only`، `archive_name_is_the_file_the_tool_is_told_to_use`، `playlist_lost_state_message_neutralises_names_and_list_survives_an_odd_archive`. **اختبار معدَّل:** ذيل `playlist_sync_downloads_only_new_items_via_the_archive` فقط (صار يتوقع الرفض، لا تنزيل، الملفات الخمسة والأرشيف كما هي، ولا حالة جديدة).
- **أحمر أولاً:** مع إزالة تعديل `crates/eid-cli/src/video.rs` فقط فشلت الاختبارات الطرفية الثلاثة (3/3 FAILED)، ثم نجحت بعد إعادته.
- **محلياً على الكود النهائي (Windows 10 19045، rustc 1.100.0، debug):** `cargo fmt --all -- --check` نظيف؛ `cargo clippy --workspace --all-targets -- -D warnings` نظيف؛ `cargo check --workspace --target x86_64-pc-windows-gnu` نجح؛ `cargo test --workspace --no-fail-fast -- --test-threads=4`: **379 نجح، 0 فشل، 6 متجاهَلة** (الخمسة `real_*` (ج) و`one_connection_probe_416_fallback_never_holds_two_sockets`، كلها متجاهَلة قبل هذا التغيير). فحص الأسماء المطلوبة على ذلك السجل: **required: 312 ok: 312 bad: 0** و29 اسم `unix:` «NOT TESTED» على Windows. **تنبيه:** Python غير مثبت هنا، فطُبّق منطق `scripts/check_required.py` نفسه بسكربت bash على سجل التشغيل؛ `check_required.py` الحقيقي يعمل في CI فقط.
- **مراجعة قراءة فقط بوكيل منفصل داخل جلسة التنفيذ (ليست جلسة المراجعة المستقلة التي تطلبها AGENTS §4):** لا مانع؛ ملاحظة كبيرة واحدة (نسخة احتياطية أقدم من آخر تشغيل) **مفتوحة لقرار المالك** — DESIGN_NOTES Round 16 الحدّ (5)؛ عولجت: أرشيف لا يُقرأ أو بروابط صلبة يوقف التشغيل ولا يوقف `--list`، تحييد أحرف التنسيق الخفية في الأسماء المعروضة، تغطية `--skip` وملف حالة تالف، وتوثيق سباق خارج القفل.
- **فحص الأمان الذاتي (AGENTS §6):** الأسماء من القرص لا تُفتح ولا تُتبع (`symlink_metadata`، والأرشيف عبر `open_regular_nofollow_read` وأول 64 KiB فقط)؛ أرشيف هو رابط أو مجلد يُعدّ موجوداً (رفض، لا اتباع)، والاختبار يفشل إن تعذّر إنشاء الرابط؛ الرسالة تمرّ على `printable` (رموز التحكم وBiDi تُستبدل)؛ لا shell ولا أمر جديد ولا اتصال شبكي ولا مكتبة جديدة؛ لا `unwrap` في الكود غير الاختباري؛ الفحص بعد قفل المجلد وقبل `plan` وأول `save` وأي تنزيل. **مراجعة مستقلة في جلسة منفصلة (AGENTS §4) لم تُجرَ.**
- **لم يُختبر:** CI على Linux وWindows لهذا الفرع؛ مسار Unix (مثلاً أسماء غير UTF-8) إلا عبر CI؛ yt-dlp الحقيقي.

## اختبار متقطع: `etag_change_between_attempts_restarts_with_new_content` (2026-11-16) — **السبب الجذري غير مثبت**
الاختبار في `crates/eid-engine/tests/g2_state.rs:251` (السطر الفاشل 255)، من P1، وهو في `scripts/required_tests.txt`. **لم يُعدَّل ولم يُتجاهَل ولم يُحذف.**
- **العَرَض على CI:** `ci-windows` على PR #24 (P4-M4، commit `e56fa75`)، خطوة `cargo test --workspace --no-fail-fast`، `eid-engine --test g2_state`: **12 نجح، 1 فشل**، برسالة `assertion failed: matches!(r, Err(Error::Cancelled))`. `ci-linux` نجح على الـcommit نفسه. (بإفادة المالك؛ لم أطّلع على السجل بنفسي.)
- **ليس من M4:** الفرق بين `main` (`1ae303f`) و`e56fa75` في `crates/eid-engine` و`crates/eid-platform` و`crates/eid-testserver` **فارغ**، فهذا الاختبار يُبنى من الكود نفسه على الفرعين.
- **ما يفعله الاختبار:** يبدأ تنزيلاً (16 MiB، 4 اتصالات بسرعة 2 MB/s لكل اتصال، checkpoint كل 40 ms)، ويلغيه حين يُظهر ملف الحالة ≥ 2 MiB محفوظة (يقرؤه بـ`state::peek` كل 10 ms)، ويتوقع `Err(Error::Cancelled)`، ثم يغيّر المحتوى (ETag جديد) ويستكمل. الفشل يعني أن المحاولة الأولى انتهت **بشيء غير** `Cancelled`.
- **إعادة الإنتاج المحلية (Windows 10 19045، debug، `--offline`):**
  - أمر CI نفسه (`cargo test --workspace --no-fail-fast`، الخيوط الافتراضية 8): 371 نجح، 0 فشل (تشغيل واحد).
  - ثنائي `g2_state` كاملاً تحت ضغط CPU (8 حلقات `yes`): **فشل 1 من 30** بالتأكيد نفسه (`g2_state.rs:255`).
  - نسخة تشخيص مؤقتة (لم تُحفظ) للنصف الأول من الاختبار، 104 تشغيلات (4 عمليات متوازية + 6 حلقات انشغال): **0 فشل**. الإلغاء وصل دائماً بين ~0.3 و~0.7 ث عند 2–2.4 MiB محفوظة؛ قراءة ملف الحالة الحي لم تفشل مرة واحدة؛ التنزيل الكامل يحتاج ~2 ث.
  - الثنائي كاملاً مع طباعة `r` مؤقتاً في رسالة التأكيد (أُعيد الاختبار إلى أصله بعدها)، 36 تشغيلاً (3 عمليات متوازية + 4 حلقات انشغال): **0 فشل**.
  - **المجموع: 1 من 66** تشغيلاً للثنائي الكامل تحت ضغط. **لم تُلتقط قيمة `r` في التشغيل الفاشل.**
- **ما يدل عليه القياس (استنتاج، غير مثبت):** هامش التوقيت واسع، فاحتمال «اكتمل التنزيل قبل الإلغاء» ضعيف؛ الأرجح أن **خطأً آخر حلّ محل `Cancelled`**.
- **فرضيات في الكود (غير مثبتة):** في `crates/eid-engine/src/download.rs`:
  1. السطر 1148: خطأ من عامل يصل **بعد** الإلغاء يستبدل `Cancelled` (`if fatal.is_none() || matches!(fatal, Some(Error::Cancelled)) { fatal = Some(e); }`). القصد منه تفضيل الخطأ الحقيقي على `Cancelled` الإخوة حين يُلغي عاملٌ فاشلٌ إخوته، لكنه لا يفرّق بين إلغاء المستخدم والإلغاء الداخلي.
  2. الأسطر 1205–1210: خطأ من مهمة الـcheckpoint بعد الإلغاء (`fatal.get_or_insert`) يصير النتيجة إن لم يكن هناك خطأ قبله.
- **لماذا لا يكفي سجل CI:** التأكيد لا يطبع `r`، فالسجل لا يكشف أيّ خطأ أُعيد.
- **الخطوات المطلوبة (مهمة مستقلة، خارج P4-M4؛ لا يبدأ أي منها دون موافقة المالك):**
  1. إضافة `{r:?}` إلى رسالة التأكيد في `g2_state.rs:255` (وفي اختبارات `g2_state` المشابهة) **دون إضعاف** التأكيد، ليكشف الفشل التالي الخطأ الفعلي.
  2. إعادة الإنتاج حتى تُلتقط قيمة `r` (محلياً تحت ضغط، أو من CI).
  3. إن ثبتت الفرضية 1 أو 2: إصلاح المحرك بحيث يُعاد `Cancelled` حين يكون إلغاء المستخدم أسبق من أي خطأ حقيقي، **مع اختبار يُفشله قبل الإصلاح** ويغطي أيضاً حالة «خطأ حقيقي ثم إلغاء الإخوة» حتى لا يُخفى خطأ حقيقي.
  4. إن ظهر سبب آخر: يُوثَّق هنا ويُعرض على المالك قبل أي تعديل.
- **تنبيه:** نجاح إعادة تشغيل `ci-windows` على PR #24 **ليس** دليلاً على حل المشكلة.

## P4 — ملاحظات المراجعة الصغرى m2–m12 وتسوية كل ملاحظات P4 (فرع `fix/p4-review-minors` فوق `main` `921ffc6`، 2026-11-17)
- **تنبيه:** القسم «P4 — بعد الدمج في main (2026-11-15)» أعلاه كُتب قبل المراجعة. جملته «`docs/reviews/P4.md` غير موجود» قديمة: المراجعة موجودة، وتسوية كل ملاحظة في DESIGN_NOTES «Round 17».
- **دمج M3:** PR #26 دُمج في `main` عند `921ffc6`. شجرة الدمج مطابقة لـ`68c3105` (`git diff` فارغ). **CI:** «2 checks passed» بإفادة المالك؛ لم أطّلع على السجلات بنفسي، فلا `gh` ولا دخول للمتصفح (GitHub يعطي 404 لمستودع خاص). الوظيفتان `ci-linux` و`ci-windows` تفشلان عند فشل أي خطوة. لذلك نجاحهما يعني نجاح fmt (Linux) وclippy والاختبارات و`check_required.py` (و`--release` على Linux) وفحص gnu و`cargo deny`. **لم يُتحقق** من عدد «required: N ok: N» ولا من تنفيذ مسارات `unix:` في تلك الجولة.
- **ما أُصلح (commit لكل ملاحظة):** m2 `8998aca`، m3 وm4 `c177d0a`، m7 `9c62581`، m9 `c3123dd`، m10 `fca1111`، m11 `0ec4c63`، m12 `b56e5cb`، ومتابعة المراجعة `29be8a0`.
- **أحمر أولاً:** m2 فشل بالخروج 1 دون الإصلاح. m4 فشل لأن `attempts` بقيت 0. m7 فشل لأن الأداة عملت في مجلد staging فيه رابط على عمق مستويين. m3 يعتمد على خطاف `EID_TEST_PUBLISH_FAIL` الجديد، فلم يُشغَّل أحمر منفصلاً.
- **مراجعة قراءة فقط بوكيل منفصل داخل هذه الجلسة (ليست مراجعة AGENTS §4 المستقلة):**
  - **وجدت مشكلة كبيرة واحدة وعولجت:** أول نسخة من m3 جعلت كشف تبديل الملف أثناء النشر (`InvalidData`) مجرد عنصر فاشل يُعاد. الآن يوقف **هذا** التشغيل (التشغيل التالي غير محمي ولا يُبلَّغ بالتبديل؛ حدّ معروف في Round 17 — تصحيح بعد المراجعة المستقلة n2)، والاختبار `a_swap_while_publishing_stops_the_run_other_errors_fail_one_item`، والنوع مثبت في اختبار eid-platform.
  - **عولجت أيضاً:** اسم المجلد في أخطاء الفحص، وصياغة رسالة إلغاء القائمة، وفحص البادئات `EID_TEST_`/`EID_FAULT` في CI، وإثبات أن التشغيل الثاني ينشر الملف المحفوظ نفسه.
  - **عُدّل بعد المراجعة المستقلة (n4):** ملف staging مقفل (مضاد فيروسات) يُعاد فتحه حتى 10 مرات بانتظار async ‏200 ms (AGENTS §6.9)، ثم يفشل العنصر وحده ويستمر التشغيل.
- **محلياً على `29be8a0` (Windows 10 19045، debug):**
  - fmt نظيف، وclippy `-D warnings` نظيف، و`check --workspace --all-targets --target x86_64-pc-windows-gnu` نجح.
  - `cargo test --workspace --no-fail-fast`: **386 نجح، 0 فشل، 6 متجاهَلة** (الستة نفسها قبل هذا الفرع).
  - **required: 319 ok: 319 bad: 0**، و29 اسم `unix:` «NOT TESTED» هنا. Python غير مثبت، فطُبّق منطق `check_required.py` بسكربت bash على السجل.
  - فحص m10 بالسكربت نفسه محلياً: الضابط 5/5، والإصدار 0، ولا بادئة.
- **مدة التشغيل:** `handoff_engine_randomized_with_jitter` (`eid-engine/tests/g14_schedules.rs`، من P1) يستغرق وحده نحو **157 ث** في كل تشغيل (158.07 و157.65 و156.68). عملية cargo تنتظره بلا استهلاك CPU، وهذا ليس تعليقاً.
- **مفتوح لقرار المالك:** M6 (دليل جُمع أدناه)، وm5، وm6، وM3 الحدّ 5. m8 موثّق ولم يتغير.
- **M6، دليل قراءة فقط (2026-11-17):**
  - `https://github.com/BtbN/FFmpeg-Builds` يعيد **301** إلى `https://github.com/FFmpeg-Builds/FFmpeg-Builds`، والـAPI يعيد 301 إلى `repositories/292087234`.
  - `FFmpeg-Builds/FFmpeg-Builds` له المعرّف نفسه **292087234** (أُنشئ 2020-09-01).
  - أصل الإصدار `autobuild-2026-11-14-12-37/ffmpeg-n8.1.3-35-gf1acb0a266-win64-lgpl-8.1.zip`: الحجم **167647869** والـ`digest` المنشور من GitHub `sha256:9a1c8c9f…e56c`. **كلاهما يطابق `tools/manifest.json`.** لم يُنزَّل الملف ولم تُفحص بصمة `ffprobe.exe` الداخلية من مصدر رسمي.
  - تعديل ADR-0007 D1 ينتظر موافقة المالك.
- **لم يُختبر:** CI لهذا الفرع؛ أول تشغيل لخطوة m10 ولكل مسارات `unix:`؛ yt-dlp الحقيقي؛ MANUAL-TESTS #11 و#14 و#19؛ (ج) الخمسة.
- **المراجعة المستقلة (AGENTS §4، جلسة منفصلة، `docs/reviews/P4-minors.md` على الفرع `docs/p4-minors-review` @ `bb7630c`، غير مدفوع وغير مدموج):** لا مانع ولا ملاحظة كبيرة؛ 4 صغرى وn5–n9. معالجتها في هذا الفرع: n1 (اختبار طرفي لتبديل أثناء النشر `a_swap_while_publishing_one_item_stops_the_playlist_run`)، n2 (الصياغة صُحّحت: التشغيل التالي غير محمي)، n3 (الروابط الصلبة في مجلد staging المعاد تُرفض قبل تشغيل الأداة `hard_link_in_a_reused_staging_folder_is_refused_before_the_tool_runs`؛ **أحمر أولاً:** على الكود القديم عملت الأداة على `.part` مربوط صلباً)، n4 (إعادة محاولة الملف المقفل `a_locked_staged_file_is_retried_then_fails_only_the_item`)، n5 (رسالة فشل النشر لم تعد تدّعي أن الملف في staging)، n6 (رسالة حدّ 10 000 تقول ما العمل)، n7 (مسجّل لـP5)، n8 (في وصف PR)، n9 (الصياغة في Round 17). نتائج الفحوص على `ed7d06b` (Windows 10، debug): fmt وclippy `-D warnings` وفحص gnu نظيفة؛ `cargo test --workspace --no-fail-fast` **389 نجح، 0 فشل، 6 متجاهَلة**؛ **required: 322 ok: 322 bad: 0** و29 `unix:` «NOT TESTED» هنا (سكربت bash، لا Python).

## فشل CI على PR #27: `midstream_disconnects_still_complete` (ci-windows، run 54 على `ccf1300`، 2026-11-17) — **السبب الجذري مثبت ومُصلَح**
- **العَرَض (من سجل GitHub نفسه):** `eid-engine --test g3_segments`: 9 نجح، 1 فشل، `g3_segments.rs:54`: «cut #4: disconnects must have been exercised»، والثنائي استغرق 41.84 ث (محلياً نحو 15 ث). `ci-linux` نجح على الـcommit نفسه، وci-windows نجح على `7222645` (run 55). الفرع لا يغيّر `eid-engine` ولا `eid-testserver`، فالعيب قديم في الاختبار.
- **السبب:** الحالة #4 تقطع الاتصال رقم 1 فقط (اتصال الـprobe، مالك المقطع 0) بعد 1 000 000 بايت. سرقة العمل تقصّر المقطع 0 حين يبطؤ هذا الاتصال، وأدنى ما يبقى له هو `min_split` (64 KiB في `fast_cfg`)، فقد ينتهي قبل 1 MB فلا يحدث قطع أبداً. هذا سلوك المحرك المقصود (`slow_segment_is_split`)، والخطأ في افتراض الاختبار.
- **إعادة إنتاج حتمية:** بإبطاء الاتصال 1 وحده (3 MB/s) بالإعداد القديم: 0 إعادة محاولة في 3 من 3، والاتصال 1 حمل نحو 400 KB فقط. بلا إبطاء وتحت حمل CPU: 48 تشغيلاً للاختبار و40 للحالة #4، 0 فشل (لهذا لم يظهر محلياً).
- **الإصلاح (اختبار فقط، لا تخطٍّ ولا حذف ولا إضعاف):** لحالات القطع الواحد `min_split = 1 MiB ≥ 1 000 000`. قاعدة `Table::split_largest` تضمن عندها أن يحمل الاتصال 1 مليون بايت على أي جدولة. أُضيفت الحالة #5: القطع نفسه مع اتصال probe بطيء، أي ظرف CI بعينه. وأُضيف تأكيد أن الاتصال 1 هو المقطوع بعد 1 000 000 بايت بالضبط. الحالات #0–#3 لم تتغير، وفيها هامش كبير (15–507 قطعاً في التشغيل).
- **أحمر أولاً:** إزالة سطر `min_split` تُفشل الحالة #5 بالرسالة نفسها التي ظهرت في CI. مع الإصلاح: `g3_segments` 10 من 10، وfmt وclippy `-D warnings` نظيفان.

## P4 — M6: التحقق من مصدر FFmpeg وتعديل ADR-0007 (2026-11-17، بموافقة المالك المشروطة)
- **شرط المالك:** التحقق من هوية المستودع من معلومات GitHub العامة فقط، دون تنزيل أو تشغيل أي ملف تنفيذي. ويبقى الإصدار وبصماته كما هي.
- **النتائج:**
  - **المستودع:** `FFmpeg-Builds/FFmpeg-Builds` رقمه 292087234، أُنشئ 2020-09-01، ليس fork، رخصته MIT.
  - **المالك:** المنظمة `FFmpeg-Builds` (رقمها 353825010)، أُنشئت 2026-11-07، `is_verified: false`، وقائمة أعضائها العلنيين فارغة.
  - **التحويل:** `github.com/BtbN/FFmpeg-Builds` يعيد 301 إلى العنوان الجديد، و`api.github.com/repos/BtbN/FFmpeg-Builds` يعيد 301 إلى `repositories/292087234`.
  - **الاستمرارية:**
    - commit `6a4fd78e` (2026-11-07 18:03Z): «FFmpeg-Builds repo moved into its own org»، كتبه الحساب `BtbN` (رقم 294293، منذ 2010).
    - كل commit بعده من الحساب نفسه.
    - كل الـcommits غير موقَّعة.
  - **الإصدار `autobuild-2026-11-14-12-37`:**
    - بناه `github-actions[bot]` ونُشر 2026-11-14، وفيه 49 أصلاً، و`immutable: false`.
    - الأرشيف `ffmpeg-n8.1.3-35-gf1acb0a266-win64-lgpl-8.1.zip`: بصمة GitHub `9a1c8c9f…e56c`، والحجم 167 647 869 بايت.
  - **`checksums.sha256`:**
    - ملف نصي من 5 728 بايت، قُرئ إلى الشاشة ولم يُحفظ.
    - بصمته تطابق بصمة GitHub له (`66b81ba7…`).
    - سطر الأرشيف فيه هو `9a1c8c9f…e56c`.
  - **المطابقة:** القيم الثلاث متطابقة: سطر `checksums.sha256`، وبصمة GitHub، و`tools/manifest.json`. ولم يتغير `tools/manifest.json`.
- **ما تغيّر:** ADR-0007 (تعديل D1: مصدر طرف ثالث، والأدلة، والحدود، وإعادة الفحص عند أي تحديث)، وصف M6 في DESIGN_NOTES Round 17، وهذا القسم.
- **لم يتغير:**
  - الكود و`tools/manifest.json` والإصدار والبصمات.
  - `docs/plans/P4.md` (D1 وD16) و`DEPENDENCIES.md` و`ARCHITECTURE.md` ما زالت تسمّي «BtbN». هذا اسم المطوّر، وهو صحيح، وتعديلها خارج نطاق M6 الذي وافق عليه المالك.
- **حدود:** لا تُعرف هوية من يملك صلاحيات المنظمة. ولم يُنزَّل الأرشيف في هذا الفحص؛ بصمات `ffmpeg.exe` و`ffprobe.exe` من فحص G2 السابق، وCI يتحقق من `ffprobe.exe` في كل تشغيل.

## P4 — اختبارات (ج) الحقيقية (2026-11-17، بإذن المالك، Windows 10 19045، debug، على `main` `bf4d62b`)
- **الإذن:** وافق المالك على تنزيل الأدوات الحقيقية وتشغيل الاختبارات. نُزّلت الأدوات مرتين، نحو 460 MB، لأن `real_tool_install…` يثبّت دائماً في مجلد مؤقت، و`g3_real` ثبّت مرة واحدة في `EID_REAL_TOOLS_DIR`.
- **النتائج:**
  | الاختبار | النتيجة |
  |---|---|
  | `real_tool_install_from_official_release` | **نجح** (148.9 ث: تنزيل وتحقق بالبصمة وقفل الأدوات الأربع) |
  | `real_cc_video_has_video_and_audio_by_ffprobe` | **نجح**: «Big Buck Bunny» (61 صيغة)، 240p في 17.0 ث، وffprobe يرى `video` و`audio` |
  | `real_ytdlp_does_not_load_a_planted_plugin` | **نجح**: لم تُحمَّل الإضافة المزروعة (بعد الالتفاف أدناه) |
  | `real_ytdlp_startup_time` | **نجح**: بدء yt-dlp مع فشل فوري، 5 تشغيلات: 2.50–3.09 ث (لـD9) (بعد الالتفاف أدناه) |
  | `real_ytdlp_with_percent_and_dollar_in_paths` | **نجح في المحاولة الثانية** (بعد الالتفاف أدناه). الأولى فشلت بخطأ من YouTube (`HTTP Error 403: Forbidden` أثناء تنزيل الصوت)، والثانية نجحت والمجلد `a%TEMP%b$HOME c${x}` سليم |
- **عيب حقيقي وجدته هذه الاختبارات (مانع لاستعمال الفيديو أكثر من مرة):**
  - **العَرَض:** في التشغيل الكامل الأول نجح الاختبار الأول، ثم فشلت الثلاثة التالية عند قفل الأدوات بـ`refusing file with multiple hard links`.
  - **السبب (مثبت):**
    - yt-dlp يشغّل Deno لحل تحدي JavaScript في YouTube، وDeno ينشئ `%LOCALAPPDATA%\deno\node_compat_bin\node.exe` **رابطاً صلباً إلى `deno.exe`** (`fsutil hardlink list` يُظهر الاسمين؛ أُنشئ 10:05:31 أثناء التنزيل الأول).
    - قاعدة القفل D11 مع `reject_if_not_plain` ترفض أي أداة لها أكثر من اسم، فيُرفض **كل تشغيل بعد أول فيديو من YouTube** على جهاز المستخدم.
    - ويكتب Deno أيضاً ذاكرته المؤقتة في `%LOCALAPPDATA%\deno`، خارج مجلداتنا، وسحب الموافقة لا يحذفها.
  - **الالتفاف أثناء الاختبار (لا تغيير في الكود):** حُذف الاسم الإضافي `node.exe` الذي أنشأه Deno (لا `deno.exe`) قبل كل اختبار من الثلاثة، وشُغّل كل واحد وحده.
  - **تجربة محلية لم تُحفظ:**
    - تمرير `DENO_DIR` إلى مجلد نختاره جعل Deno يضع ذاكرته **كلها** و`node_compat_bin\node.exe` فيه، ولم يكتب شيئاً في `%LOCALAPPDATA%`، ونجح الفيديو.
    - الرابط الصلب يبقى، لكن داخل مجلدنا، فحذفه قبل كل قفل يعيد `deno.exe` إلى اسم واحد.
    - أُعيد الكود كما كان (`git status` فارغ).
  - **الإصلاح (بموافقة المالك 2026-11-17، تعديل ADR-0003):**
    - `DENO_DIR` دائماً `<جذر الأدوات>\deno-cache`.
    - `ToolStore::lock` يحذف `deno-cache\node_compat_bin` قبل كل قفل. رابط صلب في أي مكان آخر ما زال مرفوضاً.
  - **أحمر أولاً:** قبل الإصلاح فشل `deno_link_in_our_cache_is_removed_before_the_lock_and_others_still_refuse` بالرسالة الحقيقية نفسها `refusing file with multiple hard links`. وفشل `child_environment_is_an_allowlist` لأن `DENO_DIR` كان غائباً.
  - **بعد الإصلاح (محلياً، Windows 10، debug):**
    - fmt وclippy `-D warnings` نظيفان.
    - `eid-media`: الوحدة 14، g1_media 12، g2_tools 21 (و1 متجاهَل، (ج)).
    - `eid-cli`: الوحدة 17، cli 5، playlist 23، queue 5، serve 5، shell_injection 3، video 17. كلها نجحت.
  - **التحقق الحاسم:** `g3_real` كاملاً **في تشغيل واحد بلا أي حذف يدوي**: **4 من 4 نجحت** في 194.9 ث.
    - ffprobe: فيديو وصوت.
    - بدء yt-dlp: 2.54–2.60 ث.
    - المجلد `a%TEMP%b$HOME c${x}` سليم.
    - رابط Deno صار في `real-tools\deno-cache` فقط، ولم يُكتب شيء في `%LOCALAPPDATA%\deno`.
- **لم يُختبر:** MANUAL-TESTS #11 و#14 و#19 (تحتاج سطح المكتب الحقيقي)، وyt-dlp الحقيقي عبر `eid video` نفسه، فهذه الاختبارات تستدعي `eid-media` مباشرة.

## P4 — قرارات المالك الأخيرة ودليل الاختبارات اليدوية (2026-11-17)
- **القرارات:**
  - **m5(a):** تحذير حين يتخطى الأرشيف عنصراً لم يُنشر ملفه. نُفِّذ مع اختبار `an_archived_item_without_a_published_file_is_reported`، الذي **فشل قبل التغيير ونجح بعده**.
  - **m6(a):** استثناء موثَّق لأمر `eid video` في P4. نقل العمليات الحاجبة إلى `spawn_blocking` **مطلوب في بداية P5**.
  - **M3 الحد 5(a):** يبقى التحذير الحالي، وهو حدّ مقبول.
  - التفاصيل في DESIGN_NOTES Round 17.
- **دليل الاختبارات اليدوية #11 و#14 و#19:** `docs/MANUAL-TESTS-P4-GUIDE.md`، خطوة بخطوة في PowerShell: تنزيل الأداة من GitHub Actions، والتحقق من البصمات، والأوامر، وجدول نتائج.
- **الاختبارات اليدوية الثلاثة: «لم يُختبر».** لا تُسجَّل ناجحة إلا بعد أن ينفّذها المالك ويرسل نتائجها.

## P5 — الواجهة الرسومية `EidDownload.exe` (فرع `feat/p5-gui` فوق `main` `ac964eb`، 2026-11-17، بطلب المالك)
**ما بُني:** حزمة `crates/eid-gui` (Tauri 2، نافذة WebView2) تشغّل نفس المحرّك والقائمة (`queues.json`) ونقطة IPC التي يستعملها `eid serve`، فـ`eid add/list/pause/resume/stop` ومضيف المتصفح يتحكمون بالنافذة المفتوحة. الواجهة عربية أولاً (RTL) مع الإنجليزية، فاتح/داكن حسب النظام. إضافة رابط أو عدة روابط، اختيار مجلد الحفظ (كل مجلد = قائمة خاصة به فلا يتغير مجلد تحميل بدأ)، اسم ملف اختياري، التقدم والسرعة والوقت المتبقي، إيقاف مؤقت/استكمال/إعادة محاولة، إزالة من القائمة (لا تحذف الملف)، مسح المكتملة، «إظهار في المجلد» (لا يفتح الملف ولا يشغّله)، رسائل خطأ مفهومة لكل فئة من أخطاء المحرّك. تشغيل نسخة ثانية لا يفتح مالكاً ثانياً للقائمة: يُظهر النافذة المفتوحة ويخرج. إن كان `eid serve` يعمل في الخلفية، تطلب النافذة إيقافه وتستلم القائمة (`serve` صار يكتب القائمة **قبل** إغلاق نقطة IPC). تغيير في المحرّك: `RunJob.item` و`EngineRunner.progress` (اختياري) لتمرير تقدم كل عنصر.

**أدلة آلية (أ) — محلياً على Windows 10 19045 (هذا الجهاز):**
- `cargo test -p eid-gui`: **11 نجحت** (`speed_and_eta_from_samples`، `a_stalled_transfer_decays_to_zero_and_a_restart_resets_the_window`، `progress_numbers_never_overflow`، `error_kinds_cover_the_engine_texts`، `links_are_split_deduplicated_and_bounded`، `folders_must_be_absolute_existing_directories`، `add_into_main_or_a_picked_folder_and_refuse_bad_links` (يتحقق أن كلمة السر و`?token=` لا تصل للقائمة)، `unsafe_file_names_are_sanitized_by_the_engine` (`..\..\evil.exe`، `CON`، U+202E)، `remove_refuses_running_items_and_clear_keeps_unfinished_ones`، `view_statuses_errors_and_finished_files`، `display_names_fall_back_to_the_url`).
- `scripts/gui_smoke.ps1` على **EidDownload.exe الحقيقي (release)**: النافذة تُفتح بعنوانها · نسخة ثانية لا تُغلق الأولى · `eid add` لملفين من `eid-testserver` المحلي (عادي، وآخر يُقطع اتصاله كل 32 KiB) ⇒ SHA-256 مطابق · ملف بطيء: لقطة أثناء التحميل (تقدم + سرعة + وقت متبقٍ)، إيقاف مؤقت ⇒ «متوقف مؤقتاً»، استكمال ⇒ اكتمل وSHA-256 مطابق · إغلاق النافذة أثناء تحميل ثم فتح البرنامج ⇒ اكتمل وSHA-256 مطابق · `eid stop` ⇒ خروج بالرمز 0 والقائمة محفوظة. يعمل في ملف تعريف مؤقت ونقطة IPC خاصة فلا يلمس نسخة حقيقية. **النتيجة: PASSED** (اللقطات في artifact `gui-smoke-screenshots` في CI).
- الواجهة في متصفح (بديل وهمي `mock.js`): عربي/إنجليزي، فاتح/داكن، نافذة الإضافة مع رابط مرفوض — فُحصت بالنظر (لقطات في الجلسة).

**مراجعة PR #32 وإصلاحاتها (2026-11-17):**
- **تشغيل ثانٍ بالضغط المزدوج:** كان يُظهر رسالة «مفتوح بالفعل» ولا يُظهر النافذة المفتوحة. صار يرسل أمر IPC جديداً `show` (غير مسموح من المتصفح؛ `eid serve` يرفضه لأنه بلا نافذة)، فتأتي النافذة المفتوحة إلى الأمام ويخرج التشغيل الثاني بالرمز 0 بلا رسالة.
- **تعذّر إنشاء النافذة (مثل غياب WebView2):** كان البرنامج يُنهي نفسه بـ`expect` بلا أي شيء على الشاشة، لأنه بلا console. صار يعرض رسالة عربية/إنجليزية (`eid_platform::ui`، MessageBoxW) ثم يخرج بالرمز 1 بعد إيقاف التحميلات وكتابة القائمة.
- اختبارات جديدة: `show_wakes_the_window_and_everything_else_goes_to_the_shared_handler`، `the_background_server_refuses_show_and_the_app_answers_it`، `fatal_text_keeps_the_reason_and_drops_control_characters`، وحالة `show` في اختبارات البروتوكول.
- أُضيفت إلى `gui_smoke.ps1` خطوتان:
  - التشغيل الثاني يخرج وحده بالرمز 0.
  - بلا WebView2 (`WEBVIEW2_BROWSER_EXECUTABLE_FOLDER` يشير إلى مجلد فارغ): تظهر رسالة WebView2 ثم رسالة «Eid Download»، ويخرج البرنامج بالرمز 1 والقائمة محفوظة.
- **النتائج محلياً:**
  - `cargo test --workspace`: **407 نجح، 0 فشل، 6 متجاهَلة**.
  - fmt وclippy `-D warnings` وفحص gnu: نظيفة.
  - `gui_smoke.ps1` على release: **PASSED** (9 خطوات).
- **m6(a):** شرطه «قبل أن يعمل كود الفيديو داخل البرنامج». الواجهة الحالية لا تشغّل `eid video`، فلم يتحقق الشرط بعد. يُنفَّذ النقل إلى `spawn_blocking` حين يُضاف الفيديو للواجهة.

**لم يُختبر يدوياً:** النقر الحقيقي بالفأرة داخل النافذة (اختبار الدخان يقود البرنامج عبر `eid` لا عبر النقر؛ أزرار الصفحة نفسها جُرّبت في المتصفح مع البديل الوهمي فقط) · منتقي المجلد الحقيقي · «إظهار في المجلد» · Windows 11 · شاشات DPI مختلفة · جهاز بلا WebView2 حقيقياً (المحاكاة بمتغير البيئة نجحت) · روابط إنترنت حقيقية من الواجهة · SmartScreen.
**غير منفَّذ من P5 في PLAN (مؤجل):** Svelte/npm (الواجهة HTML/JS بلا أدوات بناء)، مصفوفة لقطات headless، axe، اختبارات الجمع العربي، خريطة الأجزاء، الإعدادات، موافقة الفيديو من الواجهة (الفيديو يبقى عبر `eid video`)، مراقبة الحافظة، tray، المثبّت NSIS (P7) — النسخة الحالية ZIP.

## v0.1 — التكامل في `dev/v0.1` (2026-11-19)

الفروع المدموجة في `dev/v0.1` (بلا حذف ولا force push): `v0.1/engine-perf`، `v0.1/playlists`،
`v0.1/ui-redesign`، `v0.1/browser-ext`، `v0.1/ci-installer`. راجعتُ قبل الدمج العمل غير المراجَع الذي
توقف بسبب حد الاستخدام (`a13c2df` في ui-redesign، والعمل الأخير في ci-installer).

| البند | الحالة | الدليل |
|---|---|---|
| 1. محرّك أسرع | **مقاس**: 256 MiB/32 اتصالاً من 6.63 إلى 3.35 ث (ضعف السرعة) مقابل v0.0.1 | `docs/BENCHMARKS.md` §1 |
| 2. قوائم يوتيوب | **مختبر حقيقياً**: Windows (PeerTube، run 44502065611) ويوتيوب حقيقي (Linux) | `docs/BENCHMARKS.md` §5، `crates/eid-cli/tests/real_jobs.rs` |
| 3. زر المتصفح | **مختبر في Chrome حقيقي** حتى ملف الفيديو؛ Edge/Firefox وصفحة يوتيوب الحقيقية يدوياً فقط | `docs/EXTENSION.md` |
| 4. الخفة والسلاسة | **مقاس**: تحديث 0.2 مللي ث مع 3000 تحميل (3.8 في v0.0.1)، 34 صفاً في DOM | `docs/BENCHMARKS.md` §3–4 |
| 5. اختبارات متكررة | كل ما سبق بأوامر قابلة للإعادة؛ CI على Linux وWindows | `.github/workflows/` |
| 6. واجهة احترافية | تصميم جديد بلا أصول منسوخة، إعدادات المحرّك، المجدول، فتح الملف الآمن | `docs/UI-DESIGN.md` |

إصلاحات مهمة أثناء التكامل: تعليق اختبار على Windows بسبب `is_terminal` على أنبوب تقرأه خيط آخر؛
تشغيل العمليات من `media_jobs` عبر `eid-platform` (قاعدة الحارس)؛ زر «تثبيت» القديم في الإعدادات كان
يثبّت الأدوات بلا موافقة — صار يمر بنافذة الموافقة نفسها؛ تقدم القائمة كان يعرض بايتات العنصر الحالي.

**غير مختبر يدوياً:** الواجهة على Windows بالفأرة، الإضافة في Edge/Firefox، الزر على صفحة يوتيوب
الحقيقية، مقارنة IDM، التوقيع الرقمي، نشر المتاجر — كلها في `docs/OWNER-ACTIONS.md`.

### سجل تقدم v0.1 (للاستئناف بعد أي انقطاع؛ الأوقات بتوقيت الكويت)
- 13:20 إعدادات المحرّك + المجدول + فتح الملف (`9b24b55`)؛ clippy لـeid-gui محلياً عبر `--target x86_64-pc-windows-gnu` مع windres وهمي في `/tmp/stubbin` (فحص فقط، بلا ربط).
- 13:35 سكربت مقارنة IDM + خطوة قياس المحرّك على Windows في CI (`778ef73`).
- 13:45 حزم الإضافة للمتاجر (`ef2719b`)، الوثائق وملاحظات الإصدار (`1958ef6`).
- التالي: CI أخضر على PR #3 ← تعبئة `ARTIFACT_NUMBERS` في `docs/BENCHMARKS.md` من artifacts (`perf-windows`، `engine-windows`) ← وصف PR ← دمج ← وسم `v0.1.0` (إصدار تجريبي) ← فحص الأصول والبصمات.
- 14:35 ci-windows أخضر على `f61e174` (run 44510265157)؛ أرقام Windows أُضيفت إلى `docs/BENCHMARKS.md` §4. وصف PR #3 كُتب. بانتظار ci-linux (apt بطيء على المشغّل ~17 د).
- 15:30 نشر `v0.1.0` فشل في خطوة release (ملف البصمات من Windows بنهايات CRLF فلم يطابق `grep ...exe$`؛ كل اختبارات Windows نجحت، run 44518461406). الإصلاح في فرع `fix/release-v0.1.1`: تطبيع CRLF + رفع الرقم إلى 0.1.1 (الوسم v0.1.0 يبقى بلا حذف). التالي: PR ← CI ← دمج ← وسم `v0.1.1`.
- 15:55 ci-windows على PR #4 علق 25+ د في `video_jobs_end_to_end_with_the_fake_tool` (run 44522531035، أُلغي). السبب المرجح: فشل/انتظار ثم انتظار وقت إيقاف tokio لخيوط قراءة أنابيب على Windows. الإصلاح: الاختبار يفشل بمكانه بعد 300 ث ويحدّ إيقاف الـruntime بـ20 ث؛ و`drive` لا ينتظر stderr أكثر من 5 ث بعد خروج العملية.
- 17:40 **نُشر `v0.1.2` كإصدار تجريبي** (ci-windows 44540983741، 17:32 بتوقيت الكويت): https://github.com/oetibi/eid-download/releases/tag/v0.1.2 — 6 أصول (المثبّت، ZIP، 3 حزم إضافة، SHA256SUMS)؛ نزّلتها وطابقت البصمات كلها، وملفات exe داخل الـZIP تطابق البصمات، وحزم الإضافة 0.1.2 (المتجر بلا key، sideload مع key، Firefox بمعرّف gecko)؛ المثبّت فُحص بالتثبيت والإزالة الصامتين في نفس التشغيل. فرع `ci/release-dry-run`: dry run لخطوة النشر في كل PR + فحص بعد النشر + `scripts/ci/simulate_release.py` (نجح محلياً بالوضعين على artifact الإصدار).

## v0.2 — المرحلة 1: خريطة الأجزاء في تفاصيل التحميل (فرع `claude/loving-tesla-35550x` من `main` `cc05970`، 2026-11-19)
**قرار المالك (2026-11-19):** `v0.1.2` يبقى نسخة تجريبية، لا إصدار نهائي، لا حذف ولا تعديل لأي وسم أو تاريخ. الدمج والنشر بموافقته فقط.
**PR #6 (`ci/release-dry-run`):** كل فحوصه خضراء (ci-linux 44545924738: linux + scripts-js؛ ci-windows 44545925197: windows + release dry run). دمجه المالك في `main` (merge `1477342`، 2026-11-19 14:58 UTC). أثره: خطوة release تعمل في كل PR بلا نشر، والمثبّت يُبنى بوضع release دائماً.

**ما نُفّذ:**
- `crates/eid-gui/src/segmap.rs` (جديد، بلا Tauri): خريطة الأجزاء من ملف حالة التحميل نفسه (`.eid-state`، آخر نقطة حفظ). قراءة فقط عبر `state::load` (لا إعادة تسمية ولا إنشاء ولا حذف، لا يتبع الروابط، حد 8 MiB). الملف يُقبل فقط إن طابق رابطه رابط العنصر، واسمه اسماً آمناً بسيطاً. 120 خانة، حساب `u128` بلا تجاوز، خطي في عدد الأجزاء. اتصال واحد: شريط من التقدم الحي.
- أمر Tauri جديد `segments({ id })` (`app.rs`)، يعمل خارج خيط الواجهة (`spawn_blocking`).
- نافذة الخصائص: لوحة رسم (canvas) للخريطة، جدول أول 32 جزءاً، نص الحالة؛ تُطلب كل ثانية للتحميل النشط و4 ث لغيره، **فقط والنافذة مفتوحة**. عربي/إنجليزي. البايتات من اليسار لليمين في اللغتين.
- `logic.js`: `segmentView` لا يثق بشكل الرد (حدود وقيم مقصوصة).

**الأدلة (هذا الصندوق، Linux، بعد تثبيت WebKitGTK للبناء):**
- `cargo test -p eid-gui`: **26 نجحت** (منها 9 جديدة في `segmap`، أُضيفت إلى `scripts/required_tests.txt`).
- `node --test` في `crates/eid-gui/ui`: **7 نجحت** (منها `segment map: shapes from the backend, clamped and never trusted`).
- `scripts/ui/ui_e2e.mjs` في Chrome headless مع البديل الوهمي: **15/15** (منها الفحص الجديد «details window: segment map…»: 8 صفوف، بكسلات مرسومة، نص عربي وإنجليزي).
- `python3 scripts/check_required.py`: **required 381، ok 381، bad 0** (اختبارات `windows:` لا تُحتسب هنا وتعمل في ci-windows).
- `cargo test --workspace`: 431 نجح وفشل `guard` مرة لأن الاختبار الجديد استعمل `cfg(unix)` خارج `eid-platform`؛ صُحّح باستعمال `eid_platform::fs::symlink_file` (يفشل بصوت عالٍ إن تعذّر الرابط) ثم `guard` 4/4.
- `cargo fmt --check`، `cargo clippy --workspace --all-targets -D warnings`، `cargo check --workspace --target x86_64-pc-windows-gnu`: نظيفة.

**لم يُختبر:** الخريطة على Windows حقيقي وفي WebView2 (يُتحقق في ci-windows عند فتح PR)، اختبار الرابط الرمزي على Windows (ci-windows)، التحميل الحي الحقيقي مع النافذة مفتوحة (الفحص الآلي بالبديل الوهمي فقط).
**الوثائق:** `docs/UI-GAP-v0.2.md` (جديد: فجوات الواجهة مقابل README Ezoo العام وما سُجّل من وثائق IDM العامة؛ موقع IDM غير متاح من شبكة هذه الجلسة)، `docs/IDM-COMPARISON.md` (عمود Eid صار يعكس v0.1.2 بأسماء الاختبارات).
**المتبقي:** مراقبة الحافظة وأيقونة الإشعارات (معطلتان افتراضياً)، قياس المحرّك قبل أي تغيير للاتصالات (ADR-0012 مقترح، البند (ج) `let _ = persist_checkpoint` ما زال مفتوحاً، (ب) مفتوح، (أ) مُنجز)، إدارة القوائم والحد لكل ملف من الواجهة، مراجعة زر الفيديو مقابل GAP-ANALYSIS البند 3.

## v0.2 — المراحل 2 إلى 5 (نفس الفرع، 2026-11-19)
**المرحلة 2 — الواجهة:** بحث بالاسم أو الرابط (Ctrl+F)، أعمدة قابلة للإخفاء وإعادة الترتيب بالسحب وتغيير العرض (محفوظة)، تخصيص شريط الأدوات (إخفاء أزرار، أيقونات فقط)، سحب وإفلات الروابط على النافذة (الملفات تُرفض)، تنبيه عند اكتمال تحميل أو فشله (قابل للتعطيل)، وإصلاح المسافة الناقصة في رسالة الروابط المرفوضة (بند من PR #2). نُطفئ معالج الإفلات الأصلي في Tauri لتصل أحداث HTML إلى الصفحة. الأدلة: `node --test` (12)، `ui_e2e.mjs` (5 فحوص جديدة).
**المرحلة 3 — الجودة غير المتوفرة:** خيار المستخدم «إن لم تتوفر هذه الجودة في مقطع»: أقرب أقل (الافتراضي، سلوك v0.1)، أقرب أعلى حتى 4K، أو تخطّي المقطع (يفشل برسالة واضحة `QualityMissing`، ولا يُعاد آلياً). في CLI (`--if-missing`)، ومهام الفيديو (المهام المحفوظة من v0.1 تُقرأ «أقل»)، وIPC (حقل اختياري)، ونافذة القائمة والإعدادات، وصفحة خيارات الإضافة (يُرسَل فقط إن لم يكن الافتراضي، لأن التطبيق الأقدم يرفض الحقول المجهولة). **دليل حقيقي (ليس يوتيوب):** `g5_fallback_real_tool` شغّل yt-dlp 2026.10.26 الحقيقي دون شبكة على قائمة صيغ مصنوعة (`--load-info-json`): كل خيار اختار الصيغة الموعودة، ولا 8K في أي حالة — **نجح** (`#[ignore]`، يحتاج `EID_YTDLP`). اختبار الأمان `ipc_and_native_messaging_cannot_grant_consent` كشف أن `media_jobs` استدعى `eid_media` فصُحّح ليتحقق من قائمة `eid_ipc::VIDEO_FALLBACKS`.
**المرحلة 4 — قياس المحرّك قبل أي تغيير:** `docs/BENCHMARKS.md` §6: مع تأخير مصافحة مصطنع 0/50/150 م.ث، التحميل الكبير يفتح اتصالات بعدد الاتصالات المطلوب بالضبط (32 = 32 طلباً)، فإعادة الاستخدام لا توفّر شيئاً هنا. **لا تغيير في المحرّك**؛ ADR-0012 يبقى مؤجلاً. بند المحرّك (ج) (`let _ = persist_checkpoint` في آخر نقطة حفظ) و(ب) ما زالا مفتوحين بانتظار موافقة المالك (قرار سابق: لا إصلاح لهما بلا موافقة صريحة).
**المرحلة 5 — سطح المكتب:** أيقونة منطقة الإشعارات (تشغيل افتراضياً: إظهار/إيقاف الكل/استكمال الكل/خروج)، الإغلاق إلى منطقة الإشعارات (معطل افتراضياً)، مراقبة الحافظة (معطلة افتراضياً؛ عدّاد تغيّر كل ثانية ثم قراءة النص فقط بعد التغيّر، لا حفظ ولا تسجيل؛ يُقترح رابط ملف أو يوتيوب فقط). `cargo deny check` ناجح.

**الأدلة المجمّعة (Linux، هذا الصندوق):** `check_required.py` **387/387**؛ `ui_e2e.mjs` **21/21**؛ `node --test` الواجهة 12 والإضافة 23؛ `ext_e2e.mjs` 4/4؛ fmt وclippy (Linux وx86_64-pc-windows-gnu) نظيفة؛ `cargo deny check` ok.
**لم يُختبر بعد:** كل ما سبق على Windows حقيقي (ci-windows شُغّل يدوياً على الفرع؛ النتيجة تُسجَّل هنا)، الأيقونة والإغلاق إلى منطقة الإشعارات بالنقر الحقيقي، الحافظة مع متصفح حقيقي، يوتيوب الحقيقي مع خيار «تخطّي/أعلى» (الدليل الحقيقي الوحيد هو yt-dlp دون شبكة)، Edge وFirefox يدوياً.

**ci-windows على الفرع (workflow_dispatch):**
- run 44557942324 (`9225083`): **فشل** اختبار واحد `windows_clipboard_round_trip_and_sequence_changes`: عدّاد الحافظة كان 0 على المشغّل (لم يُنسخ شيء بعد) وكان الكود يعدّ 0 «غير متاح». كل الاختبارات الأخرى نجحت على Windows.
- الإصلاح `4e6fce9`: الصفر قيمة عادية؛ الاختبار يكتب الحافظة أولاً ولا يتخطى نفسه.
- run 44561581088 (`4e6fce9`): **نجح بالكامل**: clippy، `cargo test --workspace`، `check_required.py`، مصفوفة حقن PowerShell/cmd، بناء release، GUI smoke على النافذة الحقيقية (WebView2 + المحرّك + SHA-256)، إشعارات الرخص، المثبّت NSIS وفحص تثبيته وإزالته الصامتين، قياس الإقلاع والذاكرة، قياس المحرّك. خطوة release تخطّت (ليست وسماً).
**ما زال لم يُختبر يدوياً:** الأيقونة والإغلاق إلى منطقة الإشعارات ومراقبة الحافظة بالنقر الحقيقي، الأعمدة والسحب والإفلات بالفأرة داخل WebView2 (فُحصت في Chrome headless فقط).

## v0.2 — البنود الثلاثة الأخيرة قبل طلب الدمج (بطلب المالك 2026-11-19)
- **قوائم الانتظار من الواجهة:** نافذة «قوائم الانتظار…» (قائمة التحميلات): لكل قائمة عدد التحميلات معاً، وتشغيل/إيقاف القائمة (الإيقاف يوقف تحميلاتها الجارية مؤقتاً، والتشغيل يستكمل الموقوفة)، و«عند الانتهاء». اختبار: `queues_are_listed_main_first_and_patches_are_validated_all_or_nothing` وفحص `ui_e2e`.
- **حد سرعة لكل تحميل:** حقل جديد في المحرّك `Item.limit_kib` (تحت الحد العام، يُرفض 0 وما فوق السقف عند التحميل) وفي نافذة التفاصيل؛ التحميل الجاري يُستكمل من حيث توقف. **دليل حقيقي:** `a_per_download_speed_limit_slows_only_that_download` (المحرّك الحقيقي + الخادم التجريبي: ملف 768 KiB بحد 256 KiB/s استغرق ≥ 2.2 ث وجاره بلا حد < 1.5 ث، والاثنان SHA-256 مطابق).
- **ما يحدث عند انتهاء التحميلات (بموافقة المالك):** لا شيء/إغلاق البرنامج/سكون/إسبات/إيقاف تشغيل، بعد عدّ تنازلي 60 ث موجود في المحرّك ومختبر سابقاً، مع شريط تحذير وزر «إلغاء». **مرة واحدة فقط:** يُعاد الاختيار إلى «لا شيء» ويُحفظ قبل أي إجراء وعند الإلغاء (`the_choice_is_reset_and_saved_before_the_action_runs`). على Windows: `eid-platform::power` (صلاحية SeShutdownPrivilege ثم SetSuspendState/ExitWindowsEx غير قسري). **اختبار Windows:** تفعيل الصلاحية فقط (`windows_shutdown_privilege_can_be_enabled`)؛ **لم يُنفَّذ إيقاف أو سكون فعلي في أي اختبار** — يحتاج تجربة يدوية.
- الأدلة على Linux: `check_required.py` **391/391**؛ `ui_e2e.mjs` **22/22**؛ clippy نظيف (Linux وWindows-gnu).

## v0.2 — قرارات المالك بعد PR #7 (2026-11-19)
- لا دمج لـPR #7 قبل تجربة المالك للمثبّت على Windows حقيقي (دليل: `docs/OWNER-TEST-v0.2-beta.md`). الإصدار القادم `v0.2.0-beta` بعد اكتمال الاختبارات، **غير منشور**؛ لا مساس بأي وسم أو بالتاريخ.
- إعادة استخدام الاتصال تبقى معطلة (القياس لم يثبت فائدة، BENCHMARKS §6).
- **سكون/إسبات/إيقاف التشغيل معطّلة افتراضياً:** إعداد جديد `power_actions` (افتراضي false). وهي معطّلة تُرفض في نافذة القوائم وفي `queue_set` (`power_actions_off`)، ويرفضها `GuiPower` نفسه في اللحظة الأخيرة، وإطفاء الإعداد يمحو أي اختيار منها من كل القوائم. «إغلاق البرنامج» مسموح دائماً. اختبار: `queues_are_listed_main_first_and_patches_are_validated_all_or_nothing` (حالة الرفض) و`ui_e2e`. **لا اختبار يطفئ الجهاز فعلياً.**
- بندا المحرّك (ب) و(ج): شُرحا للمالك ولم يُعدَّلا بانتظار قراره.

## بندا المحرّك (ب) و(ج) من PLAN P1 — مُنفَّذان بموافقة المالك (2026-11-19)
- **(ب) مهلة التوقف في الاختبارات:** `kill_then_resume_requests_only_from_checkpointed_frontier` و`aborting_the_caller_drains_the_job_and_persists_a_final_checkpoint` يوقفان الخادم 30 ث ويفترضان أنه «حاجز»، بينما `fast_cfg` يستسلم بعد 5 ث. الآن مهلة هذين الاختبارين 120 ث (إعداد الاختبار فقط؛ `Config::default` = 30 ث بلا تغيير ومؤكَّد في اختبار)، ويُثبَت الحاجز بتحقق جديد: **لا حدث `Retry`** أثناء الملاحظة. **اختبار ضابط جديد** `a_stall_is_a_barrier_only_while_the_stall_timeout_outlasts_it`: نفس الخادم ونفس النافذة: بمهلة 300 م.ث يحدث `Retry` (وقد يكتمل الملف)، وبمهلة 60 ث لا يحدث شيء (5/5 تشغيلات).
- **(ج) فشل حفظ آخر نقطة استكمال:** كان يُتجاهل بصمت (`let _ = persist_checkpoint`). الآن حدث جديد `Event::CheckpointFailed { message }` (منقّح من الأسرار)؛ **لا يغيّر نتيجة التشغيل**: التحميل المكتمل المتحقق يبقى نجاحاً، والإلغاء يبقى `Cancelled`، والخطأ السابق يبقى نفسه. نقطة الحفظ السابقة تبقى فيكرر الاستكمال آخر ثوانٍ فقط. CLI يطبع تحذيراً حتى مع `-q`؛ النافذة تعرض تحذيراً يسمّي التحميل (`warnings` في رد `list`). فشل نقاط الحفظ الدورية بقي خطأً قاتلاً كما كان (اختبار g11 (e)).
  - **الاختبار** `a_failed_final_checkpoint_warns_and_never_changes_the_outcome` (تحميلات حقيقية مع الخادم التجريبي، الفشل محقون في نداء الحفظ الحقيقي `fault::fail("final_checkpoint")` في بناء الاختبار فقط): (1) مكتمل ⇒ نجاح + SHA-256 + تحذير واحد؛ (2) إلغاء ⇒ `Cancelled` + تحذير + استكمال مطابق؛ (3) فشل كتابة قرص محقون + فشل الحفظ ⇒ الخطأ الأصلي `write_data` لا خطأ الحفظ + استكمال مطابق؛ (4) عبر قائمة الانتظار ⇒ العنصر `Done` والتحذير يصل باسم العنصر.
  - **red-first:** بإرجاع الكود القديم مؤقتاً فشل الاختبار عند «(1) the failure must be reported» (0 تحذيرات)، ثم أُعيد الإصلاح.
  - `fault::add_fail_point` جديد (نقطتا فشل معاً)؛ `set_fail_point` بقي بنفس معناه للاختبارات القديمة.
- الأدلة على Linux: `check_required.py` **393/393**؛ `ui_e2e` 24/24؛ clippy نظيف (Linux وWindows-gnu).

## v0.2 — تحسينات الواجهة الأربعة (بموافقة المالك 2026-11-19)
1. **نافذة الإضافة:** اختيار القائمة (يضبط المجلد) و«مجلد حسب النوع» (بلا/تلقائي/تصنيف محدد). **فحص الرابط قبل البدء (الحجم والاسم) لم يُنفَّذ:** يحتاج اتصالاً شبكياً جديداً من البرنامج (AGENTS §8) وقد يستهلك الروابط ذات الاستعمال الواحد؛ بانتظار قرار المالك.
2. **التصنيف التلقائي إلى مجلدات:** إعداد «ضع كل تحميل جديد في مجلد فرعي حسب نوعه» (معطل افتراضياً) يجعل «تلقائي» الاختيار الافتراضي في نافذة الإضافة. يستعمل `Categorizer` الموجود في المحرّك؛ الأنواع المجهولة بلا مجلد فرعي. **حدود:** يطبَّق على ما يُضاف من النافذة؛ الروابط القادمة من الإضافة أو الحافظة عبر IPC تبقى في المجلد نفسه (لم يُغيَّر بروتوكول IPC).
3. **استيراد/تصدير القوائم:** من قائمة التحميلات؛ txt/json/csv عبر `eid_engine::porting` (الملف غير موثوق: بلا تتبع روابط، ≤ 8 MiB، أسماء ومجلدات منظّفة، كلمات السر تُحذف).
4. **الإعدادات:** تبويب جديد «سطح المكتب» (الأيقونة، الإغلاق إلى منطقة الإشعارات، الحافظة، أفعال الطاقة).
- الأدلة على Linux: `category_folders_auto_fixed_and_none`، `import_and_export_lists_are_untrusted_bounded_and_round_trip`، `ui_e2e` 25/25 (فحص جديد)؛ لا تراجع في أرقام الاستجابة مع 3000 تحميل.

## إصلاح مشاكل Windows الحقيقية الخمس (فرع `fix/windows-real-usage`، بموافقة المالك 2026-11-19) — غير مدموج
وجدها المالك على Windows حقيقي بنسخة ZIP، وقد شغّل البرنامج **من داخل ملف ZIP دون استخراج**.
- **1 و2: «os error 2» عند فحص الفيديو وتنزيل الأدوات.**
  - **السبب المثبت:** Explorer يُخرج الملف المنقور وحده (`EidDownload.exe`) إلى `%TEMP%\Temp1_<zip>.zip\`، والنافذة كانت تشغّل `eid.exe` المجاور لكل أمر فيديو.
  - **الإصلاح:** البرنامج صار عامل الفيديو لنفسه (`EidDownload.exe --eid-video-worker video ...`، يقبل `video` فقط). ما زال عملية مستقلة، فالإيقاف يقتل شجرة yt-dlp/FFmpeg كما كان. `eid.exe` والمحرّك والاستكمال بلا تغيير.
  - رسالة واضحة تسمّي الملف الناقص إن غاب العامل نفسه.
  - شريط عند البدء إن فُتح البرنامج من داخل ZIP (`Temp1_*.zip` و`7zO*` و`Rar$EX*`) أو غاب `eid-native-host`.
  - الاختبارات: `the_app_alone_installs_the_tools_and_downloads_a_video_as_its_own_worker`، `the_worker_flag_only_runs_video_commands`، `the_worker_runs_video_commands_only`، `the_startup_check_sees_a_zip_folder_and_a_missing_native_host`.
- **خطأ ثانٍ كان مخفياً خلف os error 2، وجده اختبار النافذة الحقيقية:** «تثبيت الأدوات» كان **يغلق البرنامج**.
  - `tools_install` كان أمر Tauri عادياً يعمل في خيط النافذة خارج runtime، فتشغيل العامل يسبب panic.
  - **الإصلاح:** الأمر صار async.
  - `VideoSetup::install` يرفض بخطأ خارج runtime ولا ينهار. اختبار `install_outside_the_runtime_is_an_error_not_a_crash` (red-first: فشل بـpanic قبل الإصلاح).
- **3: رابط يوتيوب في «إضافة» حفظ صفحة HTML باسم `watch`.**
  - **السبب:** كل رابط http كان يُقبل ملفاً.
  - **الإصلاح:** `eid_engine::links::video_page` (وقاعدة مطابقة في `logic.js`). في نافذة الإضافة تُفتح نافذة «فيديو» بالرابط كاملاً. عبر IPC (المتصفح و`eid serve --add`) يصير مهمة فيديو.
  - `Config::refuse_web_pages` (مفعّل في النافذة و`eid serve`، ومعطّل في `eid get`) يرفض رداً `text/html` لا يحمل اسم صفحة، قبل كتابة أي شيء، برسالة تقول ما العمل.
  - **خطأ وجده اختبار النافذة الحقيقية:** الرابط وصل إلى نافذة الفيديو بلا `?v=` (التنقيح يحذف الاستعلام). أُصلح، والاختبار يتحقق من الرابط حرفياً.
  - الاختبارات: `youtube_video_and_playlist_pages_are_recognized_and_nothing_else`، `web_pages_are_told_apart_from_files_and_page_names`، `a_web_page_is_refused_before_anything_is_written_and_files_still_download`، `a_queued_web_page_fails_once_with_the_message_and_leaves_nothing`، `video_pages_are_not_queued_as_files`، و`serve_video` (التوجيه عبر IPC حقيقي).
- **4: Firefox وإضافة المتصفح.** الأسباب ثلاثة:
  - الـartifact بلا إضافة.
  - نسخة ZIP لا تسجّل Native Messaging؛ المثبّت وحده يفعل.
  - Firefox العادي لا يقبل إضافة غير موقّعة إلا مؤقتاً.
  
  **الإصلاح:**
  - الإعدادات ← سطح المكتب ← «إضافة المتصفح»: حالة كل متصفح، و«ربط المتصفحات بهذه النسخة» (HKCU، بلا مدير)، و«إلغاء الربط» (لما يشير إلى هذه النسخة فقط). يُرفض الربط من داخل ZIP.
  - الـZIP والمثبّت يحملان `browser-extension\chrome-edge` و`browser-extension\firefox` مع `INSTALL-BROWSER-EXTENSION.txt`، والـartifact يحمل الحزم.
  - الاختبارات: `connect_this_copy_see_another_and_never_remove_it` (تسجيل حقيقي)، `the_host_of_a_registered_manifest_is_resolved`، `the_built_in_ids_make_a_valid_manifest_for_every_browser`.
  - **يحتاج المالك:** توقيع Firefox عبر AMO، ومتاجر Chrome وEdge.
- **5: التحميل العادي.** لم يُحدَّد خطأ آخر غير ما سبق. المسار كله مختبر على النافذة الحقيقية: إضافة، حفظ، SHA-256، إيقاف واستكمال من شريط الأدوات.

**اختبار النافذة الحقيقية على Windows (`scripts/windows/window_e2e.mjs`):**
- ينقر أزرار نافذة WebView2 الحقيقية عبر منفذ DevTools.
- المنفذ يُفتح بسياسة WebView2 `AdditionalBrowserArguments` في HKCU/HKLM للاختبار فقط ثم تُعاد. السبب: wry يمرر وسائطه فيتجاهل WebView2 المتغير `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`، كما ثبت في أول تشغيل.
- يعمل في ci-windows بلا إنترنت، وفي `real-video.yml` (مهمة `window`) بـ`--online`.

**أدلة CI على Windows:**
- ci-windows run 44635361276 @ `1ea8219`: **نجح بالكامل**. يشمل clippy و`cargo test --workspace` و`check_required` وGUI smoke واختبار النافذة الحقيقية (9/9) وحزمة ZIP بالإضافة والمثبّت وفحص تثبيته وإزالته والقياسات وSHA-256.
- real-video run 44637121343 @ `ac6a24e`، مهمة `window` بالإنترنت الحقيقي، والبرنامج وحده في مجلد `Temp1_*.zip`:
  - **نجح:** تثبيت الأدوات من النافذة (yt-dlp وFFmpeg وffprobe وDeno في ~10 ث)، وفحص فيديو PeerTube حقيقي، وتحميله 360p (`What is PeerTube_.mp4`، 5,166,155 بايت).
  - فشل فحص واحد في الاختبار نفسه: البحث عن الصف تم بينما النافذة تعرض مهام الفيديو. أُصلح في `96c3c36`.
- **التشغيلان النهائيان @ `96c3c36`:** ci-windows run 44639184379 **نجح بالكامل** (artifact `eid-windows-x64`). real-video run 44639189013: مهمة `window` بالإنترنت **نجحت كل فحوصها**، و`real (peertube)` نجحت.
- مهمة `real (peertube)` نجحت في كل التشغيلات.
- مهمة `real (youtube)` فشلت كما هو متوقع: يوتيوب طلب إثبات أنها ليست روبوتاً على مشغّل GitHub، وظهرت الرسالة الواضحة.

**الأدلة على Linux (هذا الصندوق):**
- `cargo test --workspace`: 495 نجح، 0 فشل، 9 متجاهَلة.
- `check_required.py`: 408/408.
- `ui_e2e.mjs`: كل الفحوص ناجحة.
- `node --test`: الواجهة 13/13، والإضافة 23/23.
- clippy (Linux وwindows-gnu) نظيف، و`cargo deny check` ok.

**لم يُختبر يدوياً:**
- تثبيت الإضافة فعلياً في Chrome وEdge وFirefox على Windows.
- يوتيوب الحقيقي (يطلب إثبات أنه ليس روبوتاً على مشغّلات GitHub).
- «فتح الملف» و«إظهار في المجلد» بنقرة حقيقية.
- خطوات المالك: `docs/OWNER-TEST-windows-fixes.md`.

## «تنزيل الأدوات» يغلق البرنامج على جهاز المالك (تقرير 2026-11-20) — فرع `fix/windows-real-usage`، غير مدموج
**ما وُجد:**
- على الفرع حزمتان باسم `eid-windows-x64`:
  - run 44635361276 @ `1ea8219`: **قبل** إصلاح `tools_install`. هذا الزر فيها يغلق البرنامج، وهو العرَض نفسه. بصمة `EidDownload.exe` فيها `a3d0d48b…`، وبصمة الـZIP `bbe99d3c…`.
  - run 44639184379 @ `96c3c36`: **بعد** الإصلاح. بصمة `EidDownload.exe` فيها `de36c1fc…`.
- لا يمكن من هنا معرفة أيهما شغّله المالك.
- الإصلاح كان موجوداً في الثانية، وجُرّب فيها التثبيت الحقيقي من النافذة.
- السبب على جهاز المالك **غير مثبت** إن كان قد شغّل الحزمة الثانية.

**ما أُضيف (`92130b9`):**
- **سجل:** `%LOCALAPPDATA%\EidDownload\logs\eid-download.log`. يسجّل:
  - البدء (الإصدار ورقم البناء ومسار البرنامج)؛
  - كل خطوة في تثبيت الأدوات ونتيجتها؛
  - الخروج الطبيعي؛
  - كل panic.
- **سجل الانهيار:** ملف `crash.txt` يكتبه panic hook في النافذة وفي عامل الفيديو، فيه الرسالة والمكان والخيط والـbacktrace. في البدء التالي يظهر شريط «أُغلق البرنامج بشكل غير متوقع» مع المسار وزر «فتح مجلد السجلات».
- **أخطاء التثبيت تظهر:** النافذة تتابع التثبيت حتى نهايته وتعرض الخطأ بنصه. قبلها كانت تقول «بدأ» فقط، والفشل لا يظهر أبداً.
- **رمز الخروج:** العامل الذي ينتهي بلا رسالة يُبلَّغ عنه برمز خروجه (مثل `0xC0000005`).
- **رقم البناء:** الإعدادات ← عام ← «عن هذه النسخة» تعرض الإصدار ورقم البناء ومسار البرنامج.
- الاختبارات: `utc_dates_are_exact`، `the_log_redacts_rotates_and_the_crash_record_is_reported_once`، `a_panic_is_written_with_its_message_and_place`، `a_failed_setup_always_says_why`، وفحوص ui_e2e.

**الاختبار على الحزمة النهائية نفسها:**
- ci-windows run 44699973441 @ `82c2109`: **نجح بالكامل**. اختبار النافذة صار يعمل على الملفات المستخرجة من الـZIP النهائي.
  - بصمة الـZIP: `bc86be0e447be07666794adf48b27ff6e7367f81ee4e8e8f15d28a7573ba5295`.
  - بصمة `EidDownload.exe`: `f72939a40c0e9093a1b6dfc87c0a018074960750474dc28ed022a2b8e2855535`.
- real-video run 44704267385 بالمدخل `package_run=44699973441`: نزّل حزمة ذلك التشغيل نفسها، وطابق بصمة الـZIP مع `SHA256SUMS`، واستخرجها. **نجحت الفحوص الـ11 كلها بالإنترنت الحقيقي:**
  - تثبيت الأدوات من النافذة؛
  - تحميل فيديو PeerTube بجودة 360p (5,166,155 بايت)؛
  - فحص 10: تثبيت يفشل عمداً (مجلد الأدوات لا يمكن إنشاؤه). النافذة عرضت «تعذّر تثبيت الأدوات: … (os error 183)»، وبقي البرنامج مفتوحاً، والسجل يحوي start وrequested وworker started وFAILED.
- `check_required` علِق مرة لمدة 40 دقيقة (run 44693133755) ولم يتكرر في التشغيل التالي. لم يُعرف الاختبار المعلّق. السكربت صار يطبع أسماء الاختبارات أثناء التشغيل، وللخطوة حد 25 دقيقة.

**لم يُختبر:** جهاز المالك نفسه. ما زال يحتاج تجربته بالحزمة الجديدة.

## زر الإضافة وقوائم التشغيل والتكامل (تقرير المالك 2026-11-20، Firefox 160) — فرع `fix/browser-button-playlists` فوق `fix/windows-real-usage`، غير مدموج

**السبب الجذري لغياب الزر في Firefox (مثبت):**
- `content.js` كان يقرأ المنطق المشترك من `window.EidCore`.
- `lib/core.js` يضعه على المتغيّرات العامة لسكربت المحتوى.
- في Chrome وEdge هما الكائن نفسه.
- في Firefox متغيّرات سكربت المحتوى في صندوق منفصل، و`window` هو نافذة الصفحة عبر غلاف (Xray). فكان `window.EidCore` غير معرّف، والسكربت يخرج في سطره الأول بصمت، في كل صفحة.
- «اختبار الاتصال» كان ينجح لأنه من صفحة الإعدادات، لا من سكربت المحتوى.
- الأدلة (ci-windows، Firefox 159.0.1 على Windows، صفحة يوتيوب بديلة تُقدَّم عند `www.youtube.com` باعتراض طلبات المتصفح نفسه، بلا اتصال بيوتيوب):
  - run 44717375680، الإضافة دون تغيير: لا زر، ولا طلب واحد وصل للمضيف. Chrome وEdge نجحا.
  - run 44717763588، مع `host_permissions` لـFirefox فقط: ما زال لا زر. إذن الصلاحيات وحدها لا تحلّ.
  - run 44718053772، مع إصلاح `EidCore`: Firefox نجح في الفحوص الخمسة.
- الإصلاح: الاسم المجرّد `EidCore` (يعمل في العالمين)، ورسالة في console الصفحة إن غاب بدلاً من التوقف الصامت. نسخة Firefox تعلن `host_permissions` مساوية لمطابقات سكربت المحتوى، وصفحة الإعدادات تقول إن نقص الوصول للمواقع وتعرض زر «السماح».

**التنقّل داخل يوتيوب:** يُتابَع بـ`yt-navigate-finish` و`yt-page-data-updated` و`popstate` وNavigation API (`currententrychange`).

**قوائم التشغيل:**
- الإضافة: `watch?v=…&list=…` يعرض «هذا الفيديو» أو «القائمة كاملة»؛ القائمة بالاسم والعدد والعناوين والترتيب؛ اختيار الكل أو بعضها.
- التطبيق (نافذة الفيديو): اختيار «هذا الفيديو فقط / قائمة التشغيل كاملة»؛ نطاق مثل `1-10, 15, 20-25`؛ الجودة والصيغة والمجلد.
- رابط `watch?v=` وحده لا يُخمَّن له قائمة: التطبيق يطلب رابط `playlist?list=…`.
- قوائم المكتبة الخاصة (`WL` و`LL` و`LM` و`RD…`) لا تُعرض كقائمة.
- التسمية `001 - عنوان الحلقة.mp4` (قرار المالك 2026-11-20، تعديل REQUIREMENTS F11a(3) وPLAN P4)؛ 4 أرقام فوق 999؛ المجلدات القديمة بعرض رقمين تبقى كما هي.
- الإيقاف والاستئناف وإعادة المحاولة دون إعادة الملفات المكتملة، وتخطّي غير المتاح مع سببه: كانت موجودة واختباراتها (`eid-cli/tests/playlist.rs`) ما زالت تنجح بالترقيم الجديد.

**التكامل:**
- «فتح في Eid Download» من قائمة الزر: أمر IPC جديد `open_video` يفتح نافذة الفيديو في التطبيق بالرابط، مع اختيارات القائمة.
- مضيف الرسائل يشغّل التطبيق المكتبي نفسه إن لم يكن يعمل (نسخة واحدة)، لا `eid serve`.
- Brave: تسجيل مضيف الرسائل في مفتاحه (`HKCU\Software\BraveSoftware\Brave-Browser\NativeMessagingHosts`) بمعرّفات Chrome.

**الاختبارات:**
- `scripts/ext/browser_e2e.mjs` يشغّل Firefox (WebDriver BiDi) وChrome وEdge (DevTools) بمضيف بديل يسجّل الطلبات. ستة فحوص: الزر عند التمرير، قائمة الجودات وطلب `add_video`، التنقّل إلى قائمة (120 فيديو) وطلب `add_playlist` بالحلقات المختارة، الاختفاء في الصفحة الرئيسية والعودة بـ«رجوع»، حفظ الإعدادات وتطبيقها، «فتح في Eid Download».
- ci-windows run 44721795106 @ `5a88e81`: Firefox 159.0.1 6/6، Chrome 6/6، Edge 6/6.
- @@RUN@@
- محلياً (Linux): `cargo test --workspace` 502 ناجح 0 فاشل؛ `check_required` 416/416؛ اختبارات الإضافة 24/24؛ `logic.test` 15/15؛ ui_e2e كلها؛ browser_e2e Chrome 6/6.

**لم يُختبر:**
- يوتيوب الحقيقي من الإضافة (الصفحات بديلة؛ يوتيوب يطلب إثبات أنه ليس روبوتاً على المشغّلات).
- Brave نفسه: التسجيل فقط يُفحص، لا يوجد Brave على المشغّل.
- Firefox 160 على جهاز المالك (المختبر 159.0.1).
- تثبيت دائم في Firefox يحتاج توقيع AMO (التثبيت المؤقت فقط مختبر).

**مؤجَّل (بقرار تقني، يحتاج موافقة المالك):** تحميل عدة حلقات من القائمة نفسها في الوقت نفسه. القائمة الواحدة تكتب ملف أرشيف واحداً (`.eid-archive.txt`) وملف حالة واحداً؛ عدة عمليات yt-dlp تكتب فيهما معاً، والإلحاق المتزامن غير آمن على Windows. القوائم المختلفة تعمل معاً كما كانت.

</div>
