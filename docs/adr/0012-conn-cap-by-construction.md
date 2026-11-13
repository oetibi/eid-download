# ADR-0012 — سقف الاتصالات بالتصميم (hyper-util + PermitStream)

**الحالة: مقترح — بانتظار موافقة المالك. لا شيء من هذا منفَّذ.** (AGENTS §8: قرار معماري جديد = ADR «مقترح» ثم موافقة المالك.) إن قُبل يحلّ جزئياً محل بند «مُقاس لا بالتصميم» في ADR-0011 §1 ويُعلَّم ADR-0011 «استُبدل جزئياً بـ0012». سياسة القياس نفسها (لا تخطٍّ صامت، حدود المُعيِّن، اختبار المُعيِّن السلبي) تبقى سارية.

## السياق
- ADR-0011 قرّر سقفاً **مُقاساً** على مقابس النواة (`SocketWatch`/`SockDiag`) مع تخفيف زمني: `pool_max_idle_per_host(0)` و`sleep(2ms)` بعد إسقاط الـbody. هذا ليس ضماناً: hyper يغلق الـfd في مهمة خلفية بعد إسقاط الـbody، فقد يتصل الجديد قبل إغلاق القديم، والـ2ms مجرد رهان على الجدولة.
- فشل متقطع مُسجَّل (حلقة الـ200 تحت الحمل، التشغيل 90: `PEAK conns=8 client_sockets=9`، `/tmp/cap200_FAIL_90.log`). وللمسار نفسه دليل مباشر: في اختبار `g5_one_connection` (سقف 1) بعد `drop(resp)` ما زال القياس يُظهر مقبسين في ~50% من الجولات تحت الحمل، أي أن الإسقاط لا يعني إغلاق الـfd.
- `connector_layer` في reqwest لا يكفي (راجَعنا reqwest 0.12.28 `src/connect.rs:1288-1305`): `Conn` داخل `mod sealed` وحقوله `pub(super)` بلا constructor عام، فالطبقة تستطيع حجز permit قبل الاتصال لكن لا تستطيع ربطه بـDrop الـstream. و`ConcurrencyLimitLayer` يحدّ مرحلة الاتصال لا عمر المقبس. كما أن `connect_timeout` طبقة خارجية فانتظار الـpermit كان سيُحسب منها.

## القرار المقترح
تحميل المقاطع (والـprobe) عبر **`hyper-util` legacy `Client`** مع connector خاص:
1. `Semaphore` بعدد permits = السقف. الـconnector يحجز `acquire_owned()` **قبل إنشاء أي مقبس**، **بلا مهلة** (انتظار الـpermit لا يُحسب من `connect_timeout`)، ثم يتصل داخل `tokio::time::timeout(connect_timeout, …)`.
2. يُغلَّف المقبس: `struct PermitStream { io: Io, _permit: OwnedSemaphorePermit }` ويطبّق `hyper::rt::Read/Write` و`Connection`. **ترتيب الحقول مقصود: `io` قبل `_permit`** (Rust يُسقط بترتيب التعريف) فيُغلق الـfd ثم يُحرَّر الـpermit. وفي دالة connect الـasync يُعرَّف الـpermit أولاً ليُسقط أخيراً عند فشل/إلغاء الاتصال.
3. الـpermit يعيش **طول عمر المقبس لا الطلب**: يُحرَّر عند `close(fd)` الفعلي في مهمة اتصال hyper، فلا سباق بين «انتهى الرد» و«أُغلق المقبس». بعدها يُحذف `sleep(2ms)`.
4. `pool_max_idle_per_host(0)` يبقى (مقبس خامل يحجز permit فيحجب الطلبات الجديدة). نتيجة checkout سباقية قد تُكمل اتصالاً في الخلفية ممسكاً permit حتى يسقط: داخل السقف، لكن يُختبر.
5. `http1_only` وALPN يعرض `http/1.1` فقط. مع HTTP/2 يصير الـpermit لكل مقبس لا لكل مقطع ويتغير معنى السقف.

## ما نفقده من reqwest وما نبقيه
| الجانب | الخطة |
|---|---|
| TLS | Linux: `tokio-rustls`. **Windows حالياً `native-tls-alpn` (SChannel) لا rustls** (`crates/eid-engine/Cargo.toml:24-27`، ADR-0010) فعلى Windows `tokio-native-tls` (يبقى SChannel)؛ توحيد rustls قرار منفصل. الـhandshake يجري والـpermit ممسوك؛ وفشله يُسقط TCP أولاً. |
| Proxy | يضيع الدعم الجاهز. `hyper-util` 0.1.21 فيه `client-proxy` و`client-proxy-system` (`Matcher::from_system()`) و`Tunnel` لـCONNECT. الحد الأدنى: CONNECT لـhttps، وhttp-proxy بصيغة absolute-form (`Connected::proxy(true)`). الأقرب أن يبقى `use_system_proxy=false` افتراضياً كما الآن (`http.rs:43`). |
| Redirects | المقاطع ترسل إلى `final_url` الذي حسمه الـprobe، فلا تتبّع تحويلات: أي 3xx في مقطع = خطأ «تغيّر سلوك الخادم». الـprobe يستمر بـ`redirect_verdict` (دالة نقية مُختبَرة) في حلقة صغيرة. |
| Timeouts | `connect_timeout` داخل الـconnector بعد الـpermit؛ `stall_timeout` قائم حول `stream.next()`؛ **`header_timeout` معرّف في `config.rs:42` لكنه غير مستخدم** اليوم فيُلفّ حول `send` هنا (يتقاطع مع M3 في المراجعة). |
| Cookies/gzip/… | لا نستعملها أصلاً (`Accept-Encoding: identity`). |
| تبعيات | مباشرة جديدة: hyper، hyper-util، http-body-util، tokio-rustls/tokio-native-tls. تحديث `docs/DEPENDENCIES.md` ونجاح `cargo deny` شرط. |

## فخاخ يجب أن يغطيها التنفيذ
1. **الـprobe:** رده يُمرَّر للمقطع 0 كـ`first`، وهو اليوم `reqwest::Response` بمقبس **خارج الـpermit** فيصير السقف الحقيقي cap+1 حتى يُغلق. الحل: الـprobe يمر على العميل نفسه.
2. **`drop(resp)` قبل أي طلب جديد** (تصليح مستقل نُفِّذ على `p1/drop-resp`): 416 في `probe` (`download.rs` ≈689)، ومحاولة single-stream (≈1088)، و`send_retrying`. بدونه يحدث **deadlock** عند السقف 1 لأن العامل يمسك permit وينتظر ثانياً. ويجب إسقاط الـResponse قبل `sleep` إعادة المحاولة.
3. **Happy Eyeballs:** `HttpConnector` يطلق محاولة ثانية بالتوازي بعد 300ms (مقبسان على permit واحد). الحل `set_happy_eyeballs_timeout(None)` (محاولات متتالية) أو حلقة connect خاصة؛ والـpermit قبل أول مقبس ويبقى عبر كل المحاولات. (الاختبارات الحالية على `127.0.0.1` فقط.)
4. **لا `try_clone`/`into_std`** في الـconnector (المقابس تُنشأ `SOCK_CLOEXEC`/بلا وراثة مقبض على Windows فلا يرثها yt-dlp). اختبار: تشغيل عملية فرعية أثناء التحميل وعدم تغيّر عدد المقابس.
5. **لا `SO_LINGER > 0`** (يحجز `close` داخل async). الافتراضي يعيد فوراً والمقبس اليتيم inode=0 فلا يُعدّ.
6. **الإلغاء/الفشل:** مقبس غير مكتمل يسقط قبل الـpermit.
7. **Windows:** لا `SockDiag` هناك؛ الدليل هناك هو اختبار الـpermit الحتمي. قياس `GetExtendedTcpTable` لاحقاً وحالات FIN_WAIT المنسوبة للعملية لا تُقارن مباشرة بـSOCK_DIAG. يُكتب في STATUS: قياس المقابس على Windows لم يُختبر.

## خطة الاختبار
1. **حتمي:** connector وهمي يُرجع stream بعلم Drop؛ `available_permits()` لا يعود إلا بعد Drop الـstream، والطلب cap+1 ينتظر حتى ذلك.
2. **الثابت:** في كل عيّنة: مقابس النواة ≤ permits المحجوزة، والذروة ≤ السقف.
3. **سقف = 1** يمر على: 416، إعادة محاولة (429/503 + انقطاع body + stall)، تحويل في الـprobe، إلغاء، والـprobe نفسه، مع مهلة صريحة (الفشل = تعليق).
4. Happy Eyeballs: خادم على `::1` بطيء القبول؛ لا مقبسان لنفس الـpermit.
5. يبقى اختبار `SockDiag` الصارم كما هو (دليل مستقل) وحلقة الـ200 تحت الحمل مع طباعة البذرة؛ تُحذف الـ2ms.
6. اختبار عملية فرعية (نقطة 4 أعلاه) على Linux؛ ما يخص Windows يُسجَّل «لم يُختبر على Windows الحقيقي».

## البدائل
- إبقاء الوضع (مقاس + 2ms): رخيص لكن متقطع (فشل 1/90 تحت حمل شديد) ولا يمكن إثباته.
- تخفيف إضافي (رفع الـ2ms): رهان زمني لا يُغلق الباب.
- فرع reqwest: غير ممكن بلا تعديل المكتبة (`Conn` مغلق).

## العواقب
+ سقف حتمي على المقابس الفعلية على ويندوز ولينكس بلا `cfg` خارج `eid-platform`.
− إعادة كتابة طبقة HTTP (≈ مئات الأسطر) وتبعيات جديدة وفقدان بروكسي/تحويلات reqwest الجاهزة.
− يتقاطع مع B3/M3 في `docs/reviews/P1.md`: يُفضَّل حسم سلوك مسار «بلا validators» (B3) قبل بناء العميل الجديد، وبعد موافقة المالك فقط.

## أسئلة للمالك
1. هل يُقدَّم البند (PLAN P2 `conn-cap-by-construction`) إلى آخر P1، أم يبقى دَيناً موثّقاً؟
2. Windows: `tokio-native-tls` (SChannel) أم توحيد rustls؟
3. مدى دعم البروكسي المطلوب في الإصدار الأول.
