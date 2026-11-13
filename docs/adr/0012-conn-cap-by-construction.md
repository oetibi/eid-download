# ADR-0012 — سقف الاتصالات بالتصميم (hyper-util + PermitStream)

**الحالة: مقترح — غير مقبول، وغير منفَّذ.** موافقة المالك (2026-11-13 10:12 الكويت) تخص **موعد** البند في PLAN (آخر P1 أو أول P2، بعد B1–B4) فقط؛ هذا الـADR والتبعيات الجديدة يحتاجان موافقة صريحة (AGENTS §8). إن قُبل يحلّ جزئياً محل بند «مُقاس لا بالتصميم» في ADR-0011 §1، وتبقى سياسة القياس وحدودها سارية.

## السياق (السبب **مرجَّح** لا مُثبَت)
- فشل واحد في 90 جولة تحت حمل ≈3× الأنوية: `client_sockets=9` مع سقف 8 (سجل الجولة 90 الأصلي (غير محفوظ؛ المقتطفات في ملف الدليل)). **السبب المرجَّح (probable):** hyper يغلق الـfd في مهمة خلفية بعد إسقاط الرد/الجسم، فيتصل البديل قبل إغلاق القديم؛ `ResponseOpened` يتأخر عن الـconnect الفعلي. لم يُلتقط الـinode بالهوية في 2300 تشغيل مُجهَّز (`docs/evidence/conn-cap-diagnosis.md`، قسم Hunt 3).
- دليل مباشر على أن «أُسقط الرد» لا يعني «أُغلق المقبس»: اختبار سقف 1 لمسار 416 (`g5_one_connection`) يبقى **غير مستقر** حتى بعد `drop(resp)` بسبب الإغلاق غير المتزامن في hyper (مُتجاهَل `#[ignore]` ومُسجَّل في STATUS).
- `connector_layer` في reqwest 0.12.28 لا يكفي: `Conn` مغلق (`src/connect.rs:1288-1305`) فلا يمكن ربط permit بـDrop الـstream.
- الـ2ms (ADR-0011) رهان زمني، لا ضمان.

## القرار المقترح
عميل `hyper-util` (legacy `Client`) + connector خاص:
1. `Semaphore(cap)`؛ الـconnector يحجز `acquire_owned()` **قبل أي مقبس**، بلا مهلة، ثم `timeout(connect_timeout, connect+TLS)` (انتظار الـpermit لا يُحسب من مهلة الاتصال).
2. `PermitStream { io, _permit }` (**`io` قبل `_permit`**) يطبّق `hyper::rt::Read/Write`؛ الـpermit يُحرَّر عند `close(fd)` الفعلي.
3. `http1_only`، ALPN `http/1.1` فقط، `Accept-Encoding: identity`، `set_happy_eyeballs_timeout(None)` (مقبس واحد لكل permit)، لا `try_clone`/`into_std`، لا `SO_LINGER>0`.
4. الـprobe يمر على العميل نفسه (وإلا يصير الحقيقي cap+1 حتى يُغلق مقبس الـprobe).
5. `drop(resp)` قبل أي طلب جديد وقبل `sleep` إعادة المحاولة (مُنفَّذ على `p1/drop-resp`)؛ وإلا deadlock عند السقف 1. تُحذف الـ2ms بعد التنفيذ.

### التعارض مع PLAN:86 (إعادة استعمال الاتصال) و`pool_max_idle_per_host(0)`
PLAN:86 «يُنظر في إعادة استعمال الاتصال داخل السقف»؛ هذا الـADR يثبّت **عدم الإعادة في الإصدار الأول** (`pool_max_idle_per_host(0)`): مقبس خامل في pool يمسك permit فيحجب طلباً جديداً (deadlock) ما لم يُملك الاتصال الخامل لعامل واحد. الإعادة تحسين لاحق بقياس، لا شرط. فلا تعارض في الترتيب: v1 بلا إعادة، والبند في PLAN يبقى «يُنظر».
**كلفة المصافحة (قياس محلي، loopback، rustls TLS1.3، شهادة ECDSA مولَّدة بـrcgen، reqwest مع pool=0):** 2.24 ms/طلب https مقابل 0.55 ms/طلب http (اتصال جديد لكل طلب) ⇒ **≈1.7 ms زيادة TLS للطلب** تسلسلياً، و≈0.31 ms/طلب عند تواز 8 (400 طلب، بعد إحماء). هذا CPU فقط؛ على الشبكة الحقيقية تُضاف ≈2 RTT (TCP+TLS1.3) لكل مقطع، **ولم تُقَس على الإنترنت**. المصافحة على SChannel (Windows) **لم تُقَس**. القياس مؤقت (لم يُلتزم ملفه).

## ما نفقده/نبقيه من reqwest
| الجانب | الخطة |
|---|---|
| **الأمن: التحويلات وبيانات الدخول** (AGENTS §6.3) | حلقة تحويلات يدوية في الـprobe: `basic_auth` فقط للـorigin الأصلي (scheme+host+port)؛ **يُحذف `Authorization` عند أي تغيّر في الـorigin** ولا يعود؛ `Location` النسبي يُحَلّ على الـURL الحالي؛ **يُمنع https→http** (`redirect_verdict`)؛ يُحترم `max_redirects` (10). المقاطع تستعمل `final_url` **بلا بيانات دخول إن اختلف الـorigin**؛ أي 3xx في مقطع = خطأ «تغيّر السلوك». المحرك الحالي سليم (reqwest يحذفها): `cross_origin_redirect_drops_credentials`. |
| **الشهادات** (parity) | Linux: نفس جذور reqwest الحالي (`rustls-tls` = webpki-roots) + `Config::extra_root_certs_pem`؛ Windows: مخزن النظام عبر SChannel + نفس `extra_root_certs_pem`. SNI والتحقق من اسم المضيف من الـURL (تُرفض الشهادات الخاطئة قبل إرسال أي طلب HTTP كما اليوم). `user_agent` من `Config` و`Accept-Encoding: identity`. |
| **TLS على Windows** | `tokio-native-tls` (يبقى SChannel، ADR-0010)؛ توحيد rustls قرار منفصل. الـpermit يبقى ممسوكاً طوال المصافحة، وفشلها يُسقط الـstream فيُحرَّر. |
| **Proxy** | **الافتراضي الحالي `use_system_proxy = true` (`config.rs:69`)**؛ الاستبدال بلا بروكسي **تراجع سلوك** لمن خلف بروكسي. `hyper-util` فيه `client-proxy-system` (`Matcher::from_system()`) و`Tunnel` (CONNECT). **قرار للمالك** (سؤال 2). |
| Timeouts | `connect_timeout` داخل الـconnector بعد الـpermit؛ `stall_timeout` قائم؛ `header_timeout` (غير مستخدم اليوم، M3) يُلفّ حول `send`. |
| تبعيات | مباشرة جديدة: hyper، hyper-util، http-body-util، tokio-rustls/tokio-native-tls؛ تحديث `DEPENDENCIES.md` ونجاح `cargo deny`. |

## خطة التراجع (Windows)
الكود الجديد في وحدة واحدة (`capped_client`) خلف `Config::client_backend` (`Reqwest` | `Capped`) لإصدار واحد على الأقل: الافتراضي يبقى reqwest + الـ2ms حتى ينجح على Windows (CI + P8 اليدوي: التحميل، شهادة خاطئة مرفوضة، بروكسي إن لزم، قياس الـpermits). فشل على Windows ⇒ يبقى الافتراضي القديم على Windows ويُسجَّل دَين؛ التراجع الكامل = `git revert` لالتزام الوحدة. قياس مقابس Windows (`GetExtendedTcpTable`) لاحق؛ الدليل هناك عدّاد permits حتمي.

## اختبارات ستُضاف إلى `required_tests.txt` (لا يُحذف شيء)
`permit_is_held_until_the_stream_drops` · `request_cap_plus_one_waits_for_stream_drop` · `kernel_sockets_never_exceed_reserved_permits` (unix) · `one_connection_probe_416_fallback_never_holds_two_sockets` (يخرج من `#[ignore]`) · `one_connection_retry_429_503_cancel_redirect_paths` · `happy_eyeballs_never_opens_two_sockets_per_permit` · `child_process_does_not_inherit_download_sockets` (unix) · `redirect_cross_origin_strips_authorization` · `redirect_relative_location_resolved` · `redirect_https_to_http_blocked` · `redirect_respects_max_redirects` · `tls_sni_and_hostname_verified` · `user_agent_and_identity_encoding_sent` · `windows_cap_by_permit_counter` (cfg windows، NOT TESTED حتى Windows الحقيقي) + الموجود في `g4_tls`. **شرط القبول:** 200 جولة تحت حمل بلا أي فشل وبلا أي `sleep`؛ `SockDiag` الصارم يبقى كما هو؛ والثابت «مقابس النواة ≤ permits المحجوزة».

## أسئلة للمالك
1. قبول الـADR والتبعيات الجديدة (موعده: آخر P1/أول P2 بعد B1–B4).
2. البروكسي: هل يُقبل فقدان بروكسي النظام في أول إصدار بالعميل الجديد، أم يُشترط CONNECT + `from_system()` من اليوم الأول؟
3. Windows: `tokio-native-tls` (SChannel) أم توحيد rustls؟
