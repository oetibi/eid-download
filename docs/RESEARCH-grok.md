# تقرير بحث لتقوية معماريّة «Eid Download»

- **التاريخ:** الجمعة 13 نوفمبر 2026، توقيت الكويت (Asia/Kuwait، UTC+3)
- **الغرض:** توجيه بناء مدير تحميل احترافي لويندوز 10 و11 اسمه **Eid Download**: محرك Rust، واجهة Tauri+Svelte أو Slint، سطر أوامر، إضافة Chrome/Edge/Firefox عبر Native Messaging، وyt-dlp + FFmpeg للفيديو وقوائم التشغيل، مع عربي RTL وإنجليزي ووضع فاتح/داكن.
- **النطاق:** دراسة IDM وAnt Download Manager وEzoo Download (أفكار فقط)، وأفضل ممارسات Rust وويندوز وManifest V3 وyt-dlp.
- **تحذير قانوني مهم جداً:** مستودع [Ezoo Download](https://github.com/AhmedEzo686/ezoo-download) ظاهر على GitHub، لكن رخصته **ليست مفتوحة المصدر**. ملف `LICENSE` في الإصدار 1.0.11 (commit `20b201c646e1e05b214d03af93734504e964745b`، 12 نوفمبر 2026) يقول صراحة إن الكود المصدري والاسم والشعار وإضافة المتصفح ملك لـ Ezoo Tech (أحمد عزت)، ويُمنع بدون إذن مكتوب: نسخ الكود أو إعادة استخدامه أو نشره كله أو جزء منه، وإعادة التوزيع، ونشر نسخ معدّلة تحت أي اسم. حقل `license = "MIT"` في `Cargo.toml` **مضلّل** ولا يلغي ملف الرخصة. **Eid Download يُكتب من الصفر.** نأخذ أفكاراً وسلوكاً وتجربة مستخدم فقط، ولا ننقل دوالاً ولا ملفات ولا نصوص واجهة ولا مفاتيح توقيع ولا أسماء crate الداخلية (`swiftget`, `ezoo-host`, ...).

---

## 1) Internet Download Manager (IDM): ماذا يفعل المحترف فعلياً؟

المصادر الأساسية: [صفحة التقسيم الديناميكي الرسمية](https://www.internetdownloadmanager.com/support/segmentation.html)، [صفحة المزايا](https://www.internetdownloadmanager.com/features2.html)، [الطوابير](https://www.internetdownloadmanager.com/support/idm-scheduler/idm_queues.html)، [الجدولة](https://www.internetdownloadmanager.com/support/idm-scheduler/idm_scheduler.html)، [خيارات البرنامج](https://secure.internetdownloadmanager.com/support/options.html)، [التصنيفات](https://www.internetdownloadmanager.com/register/new_faq/functions12.html)، [لوحة الفيديو](https://www.internetdownloadmanager.com/welcome.html) و[موضع اللوحة](https://www.internetdownloadmanager.com/register/new_faq/video3.html)، و[Range requests في MDN](https://developer.mozilla.org/en-US/docs/Web/HTTP/Guides/Range_requests).

### 1.1 التقسيم الديناميكي (Dynamic Segmentation)

IDM **لا** يقسم الملف إلى أجزاء ثابتة مرة واحدة قبل بدء التحميل (هذا أسلوب المسرّعات الأضعف). المنطق الرسمي:

1. عند البداية لا يُعرف كم اتصالاً سينجح فعلاً مع الخادم.
2. عندما يصبح اتصال جديد متاحاً، يبحث IDM عن **أكبر جزء ما زال قيد التنزيل** ويقسمه **إلى نصفين**. الاتصال الجديد يبدأ من منتصف ذلك الجزء.
3. عندما ينتهي اتصال من جزئه: إن كان الجزء التالي لم يبدأ بعد، يُعاد إسناد ذلك الجزء لنفس الاتصال (إعادة استخدام بدون مرحلة وصل/دخول جديدة). وإن كان التالي قد بدأ، يساعد الاتصال الفارغ الأجزاء الأبطأ بتقسيم أكبر جزء متبقٍّ إلى نصفين.
4. لا يُقسَّم الجزء إذا صار أصغر من حد مناسب لنوع الاتصال (وإلا تزداد كلفة التفاوض أكثر من الفائدة).
5. بعد اكتمال الأجزاء يُجمَّع الملف. عملياً الأفضل اليوم: ملف واحد مُخصص مسبقاً مع كتابة موضعِيّة (`write_at`)، لا ملفات `.part1`…`.partN` ثم لصقها في النهاية.
6. تُحفظ مواضع البايت **عدة مرات في الدقيقة** حتى يبقى الاستئناف صحيحاً بعد إيقاف أو انقطاع الكهرباء.

هذه القاعدة (in-half division + work-stealing + connection reuse) هي أهم درس من IDM. التقسيم المتساوي الثابت يترك اتصالاً سريعاً ينتظر اتصالاً بطيئاً في آخر الملف.

### 1.2 إعادة استخدام الاتصالات وResume

- الهدف: تقليل مصافحة TCP/TLS وتسجيل الدخول. الاتصال الذي انتهى لا يُغلق إن أمكن؛ يُطلب منه `Range` جديد (HTTP keep-alive).
- الاستئناف يعتمد على [HTTP Range](https://developer.mozilla.org/en-US/docs/Web/HTTP/Guides/Range_requests): طلب `Range: bytes=START-END`، ورد ناجح `206 Partial Content` مع `Content-Range`.
- لا تعتمد على وجود `Accept-Ranges: bytes` وحده. بعض الخوادم تعلن الدعم ثم ترد `200 OK` وتعيد الملف كاملاً، أو ترد `416`. إذا جاء `200` لجسم كامل رغم طلب Range فالسيرفر **لا يدعم** التجزئة: حمّل باتصال واحد، وعطّل الإيقاف/الاستئناف أو أوضح ذلك للمستخدم.
- للاستئناف الآمن استخدم `If-Range` مع **ETag قوي** (وليس `W/...` الضعيف) أو `Last-Modified`. إذا تغيّر الملف يرسل الخادم `200` بالجسم كاملاً؛ عندها أوقف الدمج وأعد التحميل من الصفر بعد تنبيه المستخدم.
- بعض الخوادم تغيّر `Last-Modified` في كل طلب بالخطأ؛ لذلك يوجد في IDM خيار "Ignore file modification time changes when resuming". لا تعتمد على التاريخ وحده.

### 1.3 تكامل المتصفح ولوحة «Download this video»

- الإضافة الحديثة (IDM Integration Module) تعتمد على صلاحيات واسعة: `webRequest` و`cookies` و`tabs` و`nativeMessaging` وغيرها، وتنقل الرابط والكوكيز وبيانات الجلسة إلى البرنامج عبر Native Messaging. الأسلوب القديم (NPAPI embed) **مات** ولا يُقلَّد. انظر [نقاش آلية الإضافة](https://stackoverflow.com/questions/17608498/how-does-internet-download-managers-chrome-extension-work) و[وصف الوحدة](https://chrome-stats.com/d/tonec-idm-integration-module).
- اعتراض التحميلات العادية: قائمة امتدادات (File Types) + قائمة استثناءات للمواقع والعناوين + مفاتيح لوحة مفاتيح لإجبار التحميل بالبرنامج أو منعه (مثل الضغط على Ctrl أثناء النقر). هذا أهم من اعتراض «كل شيء».
- الفيديو: المستخدم غالباً يحتاج **تشغيل** المقطع أولاً حتى يظهر الـstream. بعدها تظهر لوحة عائمة قرب المشغّل تعرض الجودات المتاحة. إذا تعذّر إيجاد المشغّل في DOM تُوضع اللوحة في زاوية النافذة، ويمكن سحبها. التحميل يكمل حتى لو أوقف المستخدم التشغيل في الصفحة.
- حوار البداية يعرض المسار، «تحميل الآن» أو «تحميل لاحقاً» (إلى الطابور/الجدولة). IDM يبدأ التحميل أثناء ظهور الحوار لتوفير الوقت؛ هذا خيار ممتاز لكن يجب أن يكون قابلاً للإيقاف حتى لا يُستهلك رابط لمرة واحدة أو يُضاف للجدولة وهو قد بدأ.

### 1.4 الطوابير والجدولة والتصنيفات

- طابوران رئيسان: **Download queue** (يُحذف الملف منه بعد اكتماله) و**Synchronization queue** (يبقى ويُعاد فحصه دورياً إن تغيّر على الخادم). يمكن إنشاء طوابير إضافية.
- لكل طابور: وقت بدء/إيقاف، عدد ملفات متزامنة، عدد إعادة المحاولة، وإجراء بعد الانتهاء (إطفاء الجهاز، تشغيل برنامج، قطع الاتصال). الملف الفاشل يُؤخَّر لنهاية الطابور.
- مواقع ترفض أكثر من اتصال: طابور خاص بعدد 1، مع قائمة استثناءات في خيارات الاتصال.
- حدود يومية/ساعية (Download Limits / الحصص) مفيدة للشبكات ذات سياسة الاستخدام العادل.
- التصنيفات: اسم + مجلد حفظ افتراضي + قائمة امتدادات. المستخدم يضيف تصنيفاً من القائمة الجانبية. مجلد مؤقت منفصل إن كان قرص الوجهة ممتلئاً أو بطيئاً.
- Grabber لمواقع كاملة أو أنواع ملفات من موقع: مفيد كمرحلة لاحقة، لا في الإصدار الأول.

---

## 2) Ant Download Manager: الإضافة وكشف الفيديو والاتصالات

المصادر: [دليل Chrome](https://antdownloadmanager.com/uguide/module_6_1.htm)، [تكامل المتصفح](https://antdownloadmanager.com/uguide/module_15_1_1.htm)، [نافذة رابط التحميل](https://antdownloadmanager.com/uguide/module_8_3.htm)، [إعادة الاتصال](https://antdownloadmanager.com/uguide/module_15_3.htm)، [التقاط الفيديو](https://antdownloadmanager.com/downloads/help/antDM_en.pdf)، وملاحظات Ant Video Downloader عن تخزين الاكتشاف في الذاكرة.

### 2.1 أسلوب الإضافة

- وحدتان: إضافة المتصفح + برنامج Native Messaging (`antch.exe`) يربط الإضافة بتطبيق سطح المكتب. هذا هو النموذج الصحيح لـ Eid أيضاً: الإضافة لا تتحدث مع المحرك مباشرة عبر منفذ مكشوف دون جسر موقَّع.
- أوضاع التقاط:
  - **Proactive / in-flight:** سريع لكن قد يخطئ ويحسب مورداً للعرض أنه ملف تحميل.
  - **Soft / بعد قرار المتصفح:** أبطأ وأدق، لأن المتصفح هو من يعرف أن الطلب تحميل حقيقي.
- إن رفض المستخدم أو لم يقرر خلال 25–30 ثانية، يُعاد التحميل للمتصفح (ما لم يُمنع ذلك). هذا يقلل غضب المستخدم من «سرقة» تحميلاته.
- خيار إغلاق التبويب الفارغ الذي يفتحه الموقع بعد بدء التحميل.
- مفاتيح تعديل لتجاوز النافذة عند النقر.
- تسجيل متصفحات Chromium المستنسخة يدوياً إن لزم، لأن كشف الفيديو قد يفشل إذا لم يُعرف مسار الـEXE.

### 2.2 كشف الفيديو

- الإضافة تمسح الروابط المارة باحثة عن ملفات أو تدفقات: **M3U8 / HLS، MPEG-DASH، Vimeo-stream** ونحوها.
- **Video Button:** إن حُدِّد التبويب والإطار يُوضع الزر فوق المشغّل يميناً. وإلا في زاوية التبويب. إن تعذّر التحديد يُخفى الزر ويُستخدم **Video Box** (قائمة التدفقات المكتشفة بدون الحاجة لربطها بإطار).
- استثناء مواقع («لا تلتقط من هذا الموقع»).
- خزّن الاكتشافات في ذاكرة الإضافة/الخدمة لا في `storage` الثقيل؛ عشرات التبويبات تنتج مئات التدفقات.
- روابط الفيديو غالباً **قصيرة العمر**. «تحميل لاحقاً» يجب أن يعيد استخراج الرابط من صفحة المصدر عند التنفيذ، لا أن يعتمد على URL المباشر بعد ساعات (وإلا 403/404).

### 2.3 تعدد الاتصالات وسلوك الشبكة

- تقسيم متعدد الخيوط، مع أمر صريح: **Download by one thread** للمواقع التي ترفض التعدد.
- **Download with reconnections:** إذا بدأت السرعة عالية ثم هبطت، إعادة توصيل دورية قد تستعيد السرعة. استخدمه بحذر؛ بعض الخوادم تعاقب إعادة التوصيل المتكرر. **لا يُطبَّق على بث الفيديو المقطّع (HLS)**.
- وضع سرعة منخفضة للإبقاء على التصفح أو البث أثناء التحميل.
- Ant أيضاً يتعلم أن بعض المواقع لا تسمح بأكثر من اتصال.

---

## 3) Ezoo Download: دروس من الهيكل والسلوك لا من الشيفرة

المستودع: [https://github.com/AhmedEzo686/ezoo-download](https://github.com/AhmedEzo686/ezoo-download) — دُرس محلياً للإصدار **1.0.11**. الرخصة مقيّدة كما ورد أعلاه. ما يلي وصف سلوكي/معماري لإرشاد التصميم، وليس مواصفات للنسخ.

### 3.1 الهيكل العام

```
crates/engine/   المحرك (تقسيم، HTTP، طابور، أدوات)
crates/app/      واجهة Slint + نوافذ وترجمات
crates/host/     برنامج Native Messaging صغير
extension/       إضافة MV3 (وmanifest خاص بفايرفوكس)
BUILD.bat        يستدعي build-windows.ps1
installer/       Inno Setup، PrivilegesRequired=lowest
```

أسماء الحزم الداخلية ما زالت تحمل أثر اسم قديم (swiftget). التثبيت للمستخدم الحالي تحت `%LOCALAPPDATA%\Programs\...` بدون صلاحيات مسؤول. البناء يجلب yt-dlp من إصدارات GitHub مع التحقق من `SHA2-256SUMS`، ويضع ffmpeg في `tools\`.

### 3.2 أفكار تستحق الإعجاب (ونعيد تصميمها بأنفسنا)

- **محرك مستقل عن الواجهة:** الـUI لا يعرف إلا مدير الطابور والأحداث. هذا يسمح بـCLI وواجهة رسومية وإضافة من نفس المحرك.
- **جدول أجزاء ديناميكي:** يبدأ بتقسيم أولي حسب عدد الاتصالات وحد أدنى للحجم، ثم عندما يفرغ اتصال يأخذ جزءاً خاملاً أو يقسم أكبر جزء متبقٍّ إلى نصفين. هذا قريب من IDM بعد انطلاق التحميل.
- **علامة مائية للقرص (`on_disk`):** ويندوز قد يُبقي الكتابة في الذاكرة. ما يُحفظ للاستئناف هو ما تُأكد وصوله للقرص (بعد `flush`/`sync`)، لا موضع المؤشر في الذاكرة. بعد انقطاع الكهرباء تُعاد ثوانٍ قليلة بدل ترك فجوات صامتة في الملف.
- **ملف واحد وكتابة موضعِيّة + ملف NTFS sparse:** التخصيص المسبق لملف ضخم على NTFS بدون `FSCTL_SET_SPARSE` يجعل ويندوز يملأ الأصفار ويتوقف التحميل. الملف المؤقت يمكن إخفاؤه أثناء التشغيل ثم إظهاره قبل أن يلمسه ffmpeg (البرامج قد ترفض الكتابة فوق ملف مخفي).
- **حفظ القائمة بشكل ذري:** كتابة إلى `.tmp` ثم `sync_all` ثم `rename`، مع نسخة احتياطية دورية، والاحتفاظ بنسخة `.broken` إن تلف JSON.
- **Token bucket مشترك** لكل الاتصالات مع سقف للدَّين حتى لا تنتظر 32 وصلة ثواني طويلة دفعة واحدة.
- **فحص حقيقي للـRange** بطلب `bytes=0-` (بعض الخوادم ترفض `bytes=0-0`) وقراءة `206` و`Content-Range`. إن جاء `200` يُعدّ غير قابل للتجزئة مهما قال `Accept-Ranges`.
- **If-Range مع ETag قوي**، وإن غاب الإصدار: إعادة قراءة عشرات البايتات السابقة من القرص ومقارنتها بما يرد من الخادم حتى لا يُخلط ملفان.
- **إبقاء استجابة الـprobe مفتوحة** وقتاً محدوداً وإكمال التحميل منها: الروابط لمرة واحدة تُستهلك إن أُغلق الـprobe ثم بدأ التحميل بطلب جديد (فيظهر 403).
- **تعلّم حد الاتصالات لكل موقع** من 429/403، مع انتهاء صلاحية (يوم مثلاً) حتى لا يُحبس الموقع على اتصال واحد للأبد بعد ازدحام عابر. احترم `Retry-After`.
- **إعادة المحاولة المتدرجة:** ميّز خطأ الشبكة (أعد بلا حدود مع backoff قصير) من رابط منتهٍ (401/403/404/410) ومن صفحة CAPTCHA.
- **ترويسات شبيهة بالمتصفح** وتمرير الكوكيز و`Referer` من الإضافة. بعض الحمايات (مثل تحديات Cloudflare) ترفض طلبات غير مرتبة الحقول أو بلا ALPN. جرّب IPv4 إن فشل IPv6 بـ403 (شائع مع روابط مربوطة بـIP).
- **أنواع التحميل:** HTTP مجزأ، HLS (مع رفض DRM)، DASH (فيديو+صوت ثم دمج ffmpeg)، لاحقاً ربما torrent.
- **yt-dlp للاستخراج لا للتنزيل:** `-J` / `--flat-playlist` لمعرفة الجودات والقائمة، ثم المحرك يحمّل الروابط المباشرة حتى تستفيد من تعدد الاتصالات. `--ignore-config` و`--` قبل الرابط. ملف كوكيز Netscape مؤقت يُحذف فوراً. حدّث yt-dlp يومياً مع SHA-256. لا تعتمد على نسخة المستخدم في PATH إن وُجدت إعدادات `--exec` خبيثة.
- **جسر الإضافة:** Native Messaging host صغير يقرأ بروتوكول stdin/stdout، ثم ينادي التطبيق المحلي. التطبيق يسمع على `127.0.0.1` (عندهم الافتراضي 9614) ويرفض Host/Origin الغريب ويوقّع الطلبات. المثبّت يسجّل مفاتيح HKCU لـ Chrome وChromium وEdge وBrave وFirefox، ويزيلها عند إلغاء التثبيت.
- **تصنيفات الامتدادات، جدولة، حد سرعة عام/لكل ملف، مراقبة الحافظة (قابلة للإيقاف)، تشغيل مع ويندوز اختياري.**

### 3.3 سلوك لا نكرره في Eid Download

- **التحديث الإلزامي الصامت** كل 6 ساعات بدون نافذة: خطر أمني وتجربة سيئة. التحديث عندنا اختياري، موقَّع، ويعرض للمستخدم ما سيتغيّر.
- **منفذ TCP محلي كقناة رئيسة:** حتى مع التوقيع، المنفذ عرضة لتعارض الأرقام وDNS rebinding إن أُسيء ضبط Host. الأفضل **Named Pipe** خاص بالمستخدم (`\\.\pipe\EidDownload-<sid>`) والجسر هو الـNative Host. إن بقي HTTP محلي فليكن 127.0.0.1 فقط، بمصادقة، ورفض أي Origin/Host خارج localhost.
- **تحديث إلزامي + برنامج غير موقَّع Authenticode:** README نفسه يعترف بتحذير SmartScreen. لا تبنِ سمعة Eid على هذا.
- حفظ بعض الأسرار كنص إن تُرك دون تشفير (DPIAP على ويندوز للكوكيز وكلمات السر).
- الإضافة في Chrome تضع `nativeMessaging` ضمن الصلاحيات؛ في Firefox قد تختلف (عندهم nativeMessaging أحياناً عبر ostensible companion). صمّم manifestين واضحين من اليوم الأول.

---

## 4) Rust وويندوز وManifest V3: أفضل الممارسات والمطبات

### 4.1 صناديق Rust المقترحة للمحرك

| الحاجة | التوصية | ملاحظات |
|---|---|---|
| زمن تشغيل غير متزامن | `tokio` (rt-multi-thread, fs, io-util, time, sync, process, net) | لا تستخدم runtime داخل runtime من الـUI بلا تهذيب. |
| HTTP | `reqwest` 0.12 مع `native-tls-alpn` (SChannel على ويندوز) | ALPN مهم لبعض الحماية. للتجزئة **أجبر HTTP/1.1** على عميل التحميل المجزأ. `hyper`/`hyper-util` إن احتجت تحكماً أدق بالاتصال. |
| TLS للشركات/مضادات الفيروس | native-tls / SChannel أفضل من rustls وحده | الـAV الذي يحقن شهادة جذر يعمل مع مخزن ويندوز. |
| URL والمسارات | `url`, `percent-encoding`, `encoding_rs` | لازم لـ`Content-Disposition` وRFC 5987 (`filename*`). |
| سلامة | `sha2`, `thiserror` + `anyhow` في الحدود | تحقق Digest/checksum إن وُجد. |
| حد السرعة | طبّل token bucket خاص بك، أو `governor` | حد عام + حد لكل مهمة، يُوزَّع على الاتصالات. |
| وقت وإعدادات | `chrono` أو `time`، `serde`/`serde_json`، `directories` | بيانات المستخدم تحت `%APPDATA%\EidDownload\`. |
| ويندوز | `windows-sys` | sparse files، خصائص الملف، DPAPI، سجل Native Messaging، مسار طويل. |
| سجل وتشخيص | `tracing` + `tracing-subscriber` | Native Host يكتب على **stderr فقط**. |

مراجع Range: [MDN HTTP range requests](https://developer.mozilla.org/en-US/docs/Web/HTTP/Guides/Range_requests).

### 4.2 منطق المحرك الذي يجب تثبيته مبكراً

1. **Probe ثم حمّل:** ابدأ باتصال واحد. إن ثبت `206` وحجم معروف، اقسم ديناميكياً. إن لم يوجد `Content-Length` ولا `Content-Range` الكلّي: تدفق مجهول الحجم باتصال واحد إلى ملف ينمو، بدون استئناف مضمون.
2. **لا تثق بـHEAD:** بعض الخوادم تجيب HEAD بخلاف GET. الـprobe يكون GET مع Range.
3. **لا تُهدر رابط المرة الواحدة:** إن فتحت الـprobe فإمّا أن تُكمل قراءة جسمه أو أن تضعه في ذاكرة قصيرة بمهلة (دقائق) للبدء منه.
4. **HTTP/2 لا يسرّع ملفاً واحداً كبيراً** إن تشاركت الطلبات نافذة ازدحام TCP واحدة. التسريع التاريخي لمديري التحميل جاء من **عدة اتصالات TCP منفصلة** (غالباً HTTP/1.1) تجاوزاً لحدود لكل اتصال. انظر [Parallel Downloading](https://textslashplain.com/2024/11/22/parallel-downloading/) ومناقشة [networking 2018](https://dl.ifip.org/db/conf/networking/networking2018/8B3-1570422313.pdf). استخدم HTTP/2 لطلبات الميتاداتا الصغيرة إن شئت، لا لشرائح الملف.
5. **اتبع التحويلات يدوياً أو بحذر:** حد أعلى لعددها، حدّث `Host` و`Referer` حسب السياسة، لا ترسل كوكيز المضيف الأول لمضيف التحويل الثالث دون قواعد.
6. **ملف مؤقت + بيانات استئناف:** مثلاً `name.eid.part` و`name.eid.meta.json` (أو نهاية الملف/ملف Sidecar). الميتاداتا تضم: URL النهائي، الحجم، ETag/Last-Modified، جدول الأجزاء مع مواضع `flushed`، المجموع الاختياري. بعد النجاح: `fsync` ثم `rename` ذري إلى الاسم النهائي. لا تكتب مباشرة فوق ملف مكتمل موجود؛ عالج التصادم (`(1)`, سؤال المستخدم).
7. **سلامة الاستئناف:** طابق URL/الحجم/ETag قبل إكمال `.part`. إن تغيّر شيء: ابدأ من صفر. تحقق من عدم وجود فجوات قبل الإعلان عن الاكتمال.
8. **السرعة:** token bucket؛ إعادة توصيل للوصلة الأبطأ بكثير من المتوسط (مثل فكرة Ant/IDM) مع حد حتى لا ندخل حلقة إعادة توصيل.
9. **أسماء الملفات:** نظّف `\ / : * ? " < > |` ونقطة النهاية والمسافات، ارفض `..`، ادعم العربية، قص الأسماء الطويلة مع الإبقاء على الامتداد.

### 4.3 خصوصيات ويندوز

- **المسار الطويل:** حد `MAX_PATH` (260) ما زال يفاجئ. أضف `longPathAware` في manifest التطبيق، واستخدم بادئة `\\?\` للمسارات المطلقة عند الحاجة. المرجع: [Maximum Path Length Limitation](https://learn.microsoft.com/en-us/windows/win32/fileio/maximum-file-path-limitation).
- **قفل الملف:** Defender/AV يقفل الملف أثناء الكتابة أو عند الإعادة التسمية. أعد المحاولة بـbackoff، ولا تحذف الـ`.part` ل dispensable error عابر.
- **Mark of the Web:** المتصفح يضع ADS `Zone.Identifier` بـ`ZoneId=3`. مدير التحميل الذي يحفظ بملفه يتخطى ذلك؛ المستحسن كتابة MotW للملفات التنفيذية والقادمة من الإنترنت حتى لا يتجاوز SmartScreen دون قصد. انظر [Downloads and the Mark-of-the-Web](https://textslashplain.com/2016/04/04/downloads-and-the-mark-of-the-web/).
- **SmartScreen:** البرامج الجديدة غير الموقَّعة تُخوَّف المستخدم. وقّع المثبّت والـEXE بشهادة Authenticode (انظر [Tauri Windows Code Signing](https://v2.tauri.app/distribute/sign/windows/)). السمعة تُبنى بالتحميلات الموقَّعة مع الوقت.
- **WebView2 (إن اخترنا Tauri):** على Windows 10 (أبريل 2018 / 1803) وWindows 11 الـruntime عادة جزء من النظام. المثبّت الافتراضي يمكنه `downloadBootstrapper` إن نقص. لا تفترض وجوده على كل نسخة Win10 القديمة جداً. التوثيق: [Tauri Windows Installer](https://v2.tauri.app/distribute/windows-installer/) و[المتطلبات](https://v2.tauri.app/start/prerequisites/).
- **مثبّت لكل مستخدم بدون إدمن:** NSIS في Tauri وضع `currentUser` يثبّت تحت `%LOCALAPPDATA%`. أو Inno Setup بـ`PrivilegesRequired=lowest`. لا تكتب في `Program Files` ولا في HKLM.
- **تحديث ملف قيد التشغيل:** ويندوز يسمح بإعادة تسمية exe شغال لا حذفه. انقل القديم إلى `.old` ثم ضع الجديد، واحذف القديم في التشغيل التالي.
- **سجل Native Messaging (لكل مستخدم):** القيمة الافتراضية للمفتاح هي المسار الكامل لملف JSON الخاص بالضيف. Chrome يبحث 32-بت ثم 64-بت. المراجع: [Chrome Native Messaging](https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging)، [Edge Native Messaging](https://learn.microsoft.com/en-us/microsoft-edge/extensions/developer-guide/native-messaging)، [Firefox Native manifests](https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/Native_manifests).

```
HKCU\Software\Google\Chrome\NativeMessagingHosts\com.eid.download
HKCU\Software\Microsoft\Edge\NativeMessagingHosts\com.eid.download
HKCU\Software\Chromium\NativeMessagingHosts\com.eid.download
HKCU\Software\BraveSoftware\Brave-Browser\NativeMessagingHosts\com.eid.download
HKCU\Software\Mozilla\NativeMessagingHosts\com.eid.download
```

- Chrome/Edge: الحقل `allowed_origins` = `chrome-extension://<id>/` (بدون حروف بديلة).
- Firefox: الحقل `allowed_extensions` = معرّف gecko الثابت مثل `eid-download@example.com`.
- Opera/Vivaldi غالباً يقرآن مفاتيح Chrome؛ اختبرهما.
- التطبيق يعيد كتابة الـmanifest عند التشغيل إن تغيّر المسار بعد التحديث.

### 4.4 بروتوكول Native Messaging (أخطاء تكسر التكامل بصمت)

- كل رسالة: 4 بايت طول (endian أصلي، على ويندوز little-endian) ثم JSON UTF-8.
- حد الرسالة من المضيف إلى المتصفح 1 MB، وعكسها حتى 64 MiB.
- على ويندوز اضبط stdin/stdout على **binary** (`O_BINARY`). الوضع النصي يفسد الطول لأن `\n` تصبح `\r\n`.
- **لا تطبع أي شيء على stdout** (حتى سطر تشخيص). التشخيص على stderr أو ملف سجل.
- `connectNative` يبقي العملية حتى يُغلق المنفذ؛ `sendNativeMessage` يبدأ عملية لكل رسالة. للإضافة الدائمة استخدم منفذاً طويلاً مع إعادة الاتصال عند `onDisconnect`.
- من Chrome 105: `connectNative` يُبقي service worker حيّاً. إن انهار المضيف أعد الاتصال. التوثيق: [دورة حياة service worker](https://developer.chrome.com/docs/extensions/develop/concepts/service-workers/lifecycle).
- المحتوى (content script) لا يستدعي Native Messaging مباشرة؛ يمر عبر service worker بعد التحقق من `sender.origin`/`sender.url`. [تحذير Chrome](https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging).

### 4.5 قيود Manifest V3 واعتراض التحميل

- لا توجد خلفية دائمة. صمّم الإضافة لتعمل بعد النوم: حالة في `chrome.storage`، مستمعون في أعلى الملف لا داخل `then`.
- اعتراض التحميل: `chrome.downloads` مع `onCreated` ويفضّل `onDeterminingFilename` لإلغاء التحميل قبل اكتمال حوار الحفظ. استدعِ `suggest` مرة واحدة إن استخدمت هذا الحدث. المراجع: [chrome.downloads](https://developer.chrome.com/docs/extensions/reference/api/downloads) ونقاشات الإلغاء المبكر ([Stack Overflow](https://stackoverflow.com/questions/42766407/how-to-cancel-all-downloads-in-chrome-extensions-and-avoid-displaying-save-as-di)).
- لا تعترض: صفحات HTML، عناوين `blob:`/`data:` إلا بوعي، ملفات صغيرة جداً (ضع حداً أدنى بالكيلوبايت)، الامتدادات خارج قائمة File Types، والمواقع في الاستثناءات.
- بعد `cancel` قد تحتاج `erase` لإزالة العنصر من قائمة المتصفح. عالج السباق: التحميل قد يكتمل في أجزاء من الثانية على شبكة سريعة.
- الفيديو: `webRequest.onResponseStarted` / `onSendHeaders` لأنواع MIME والتدفقات (`.m3u8`, `.mpd`, `video/`, `audio/`) + content script لرسم اللوحة فوق المشغّل. MV3 في Chrome قيّد `webRequestBlocking`؛ الكشف الرصدي يكفي غالباً، والاعتراض للملفات عبر `downloads` API.
- صلاحيات مقترحة بحذر: `downloads`, `cookies`, `contextMenus`, `storage`, `tabs`, `webRequest`, `nativeMessaging`, و`host_permissions` حسب الحاجة. اشرح للمستخدم لماذا كل صلاحية.
- قائمة سياق: «تحميل بواسطة Eid Download»، و«التقاط فيديو هذه الصفحة».
- وسيط bypass (Alt/Ctrl) كما في IDM/Ant.

---

## 5) تكامل yt-dlp وFFmpeg

مراجع: [README yt-dlp](https://github.com/yt-dlp/yt-dlp/blob/master/README.md)، [إعلان زمن تشغيل JS لليوتيوب](https://github.com/yt-dlp/yt-dlp/issues/15012)، [ويكي EJS](https://github.com/yt-dlp/yt-dlp/wiki/EJS)، إصدارات [2025.11.12](https://github.com/yt-dlp/yt-dlp/releases/tag/2025.11.12) وما بعدها.

### 5.1 كيف تستدعي الأداة؟

- ضَع النسخة الرسمية `yt-dlp.exe` في مجلد `tools` بجانب التطبيق، وكذلك `ffmpeg.exe` و`ffprobe.exe`، ومنذ 2025-11-12 أيضاً **Deno** (`deno.exe`، ليس `denort`). بدون زمن تشغيل JS يصبح دعم يوتيوب deprecated: جودات ناقصة اليوم، وقد يتوقف غداً. Deno هو الافتراضي والموصى به؛ Node/QuickJS يحتاجان تفعيلة صريحة.
- حدّث المسار عبر متغيرات البيئة للأدوات المساعدة فقط، مع `--ffmpeg-location`.
- **دائماً:** `--ignore-config` و`--no-cache-dir` و`--socket-timeout` و`--` ثم الرابط. لا تقرأ إعدادات المستخدم التي قد تحتوي `--exec`.
- استخراج فيديو واحد: `-J` (أو `--dump-single-json`) مع `--no-playlist --no-warnings --no-progress`.
- قائمة تشغيل/قناة: `-J --flat-playlist --yes-playlist` (سريع، بلا فحص كل حلقة). ثم استخرج كل عنصر عند دوره. لعناوين عربية: `--extractor-args youtube:lang=ar`.
- لا تستخدم `--dump-json` على قوائم ضخمة بلا `--flat-playlist` (بطيء وقد ينقطع).
- التقدم إن حمّلت عبر yt-dlp نفسه: `--newline --progress-template` بصيغة آلة. **التوصية:** لا تحمّل الوسيط عبر yt-dlp إلا كخطة بديلة؛ الخطة الرئيسة `-J` ثم محرك Eid على روابط الفيديو/الصوت (ومع HLS منطق الشرائح).
- الدمج: `ffmpeg -i video -i audio -c copy -movflags +faststart out.mp4`. لا تُعد الترميز إلا عند طلب المستخدم (MP3 مثلاً). اجعل الملف المخفي ظاهراً قبل استدعاء ffmpeg إن لزم.
- الكوكيز: ابنِ ملف Netscape مؤقتاً بصلاحيات ضيقة واحذفه في `finally`. لا تكتب كوكيز الجلسة في سجل أو في قائمة التحميل نصاً واضحاً.
- إن فشل الاستخراج بسبب تسجيل دخول أو تقييد عمر: اطلب من المستخدم فتح الصفحة وهو داخل على المتصفح مع الإضافة، ثم أعد المحاولة بالكوكيز.
- حدّث yt-dlp من `https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe` بعد مطابقة `SHA2-256SUMS`. لا تشغّل ملفاً فشل تحققه. حدّث Deno وffmpeg من مصادرهما الرسمية ببصمة معروفة وقت البناء.
- `--update` / `-U` يحدّث القناة نفسها فقط ولا يثبّت Deno؛ هذا مسؤولية التطبيق.

### 5.2 أعطال يوتيوب الشائعة التي يجب أن تظهر للمستخدم بوضوح

- تغيّر توقيع/نكهات المشغّل (`nsig` / signature) → حدّث yt-dlp وDeno فوراً.
- غياب JS runtime → جودات ناقصة أو تحذير EJS؛ ثبّت Deno المضمَّن.
- طلب تسجيل الدخول أو «confirm you are not a bot» / PO token: لا تَعِد بتجاوزه سحرياً؛ استخدم كوكيز المتصفح بعد تصفح حقيقي، واشرح القيد.
- روابط التحويل (`googlevideo.com`) قصيرة العمر: عند الاستئناف أو الطابور أعد `-J` ثم أكمل إن بقي النطاق متوافقاً، وإلا أعد الشريحة المتبقية برابط جديد بحذر (مع ETag/حجم).
- فصل الصوت عن الصورة (DASH): خيار الجودة يجب أن يختار زوج فيديو+صوت متوافق (معدل/ترميز)، ثم دمج.
- قوائم تشغيل كبيرة: واجهة تحديد الكل/بعض العناصر والجودة مسبقاً، وتنفيذ متسلسل أو بعدد محدود.
- DRM (Widevine/PlayReady): ارفض بوضوح. لا Circumvention.
- معدل طلبات 429 من يوتيوب: أبطئ الاستخراج، ولا تفتح عشرات `-J` معاً.

حقوق النشر: أظهر تنبيهاً أن المستخدم مسؤول عن الالتزام بشروط المواقع والقوانين المحلية. البرنامج أداة تحميل، لا خدمة كسر حماية.

---

## 6) المعماريّة الموصى بها لـ Eid Download

الصورة المرجعية في `docs/architecture-ref.png` (نموذج «زاجل»: واجهة Tauri+Svelte، سطر أوامر، محرك Rust، Native Messaging، وyt-dlp/FFmpeg بموافقة المستخدم) مناسبة جداً. **التوصية: Tauri 2 + Svelte للواجهة**، لا Slint، للأسباب التالية:

- مطابق لما طلبه المستخدم بصرياً (RTL عربي، ثيم فاتح/داكن، تصميم ويب أنيق أسهل في Svelte/CSS).
- WebView2 موجود على ويندوز 10 الحديث وويندوز 11؛ المثبّت يعالج النقص.
- IPC جاهز بين الواجهة والمحرك، وإمكانية نافذة تأكيد دائماً فوق النوافذ عند اعتراض المتصفح.
- Slint أخف ولا يحتاج WebView2، لكنه أصعب للـRTL المعقّد ولوحة الفيديو/الإعدادات الكثيفة. يمكن إبقاء الباب مفتوحاً لاحقاً لأن المحرك منفصل.

### 6.1 هيكل المستودع المقترح

```
EidDownload/
  crates/engine/          # المكتبة: تقسيم، HTTP، طابور، جدولة، yt-dlp wrapper
  crates/app/             # Tauri 2 + واجهة Svelte (EidDownload.exe)
  crates/cli/             # eid.exe — يكلم نفس المحرك عبر IPC
  crates/host/            # eid-host.exe — Native Messaging bridge
  extension/              # MV3: manifest.json + manifest.firefox.json
  tools/                  # yt-dlp.exe, ffmpeg.exe, ffprobe.exe, deno.exe (لا تُرفع للـgit)
  installer/              # تخصيص NSIS / خطافات سجل HKCU
  docs/                   # هذا التقرير ووثائق التصميم
```

### 6.2 تدفق التحكم

```
[Chrome/Edge/Firefox]
    downloads API / لوحة فيديو / قائمة سياق
            |  Native Messaging (stdio, طول 4 بايت)
            v
      eid-host.exe  ——  Named Pipe (مصادق)  ——  محرك داخل EidDownload.exe
                                                      |
                    CLI (eid.exe)  — نفس الـPipe ——   |
                                                      v
                                         crates/engine
                                         - probe + dynamic segments
                                         - resume metadata
                                         - queue / scheduler
                                         - ytdlp resolve + ffmpeg mux
```

- إن لم يكن التطبيق يعمل: الـhost يشغّله ثم ينتظر الجاهزية (مهلة قصيرة) ثم يرسل الطلب.
- أبقِ نسخة واحدة من التطبيق (mutex/named mutex). النسخة الثانية تمرّر الأمر للأولى وتخرج.
- الواجهة العربية افتراضياً حسب لغة ويندوز/`sys-locale`، مع مفتاح RTL في الجذر (`dir="rtl"`)، والإنجليزية LTR. الثيم: فاتح / داكن / تلقائي.

### 6.3 الإصدار الأول (MVP) ضد ما يؤجَّل

**في الإصدار الأول:** محرك HTTP ديناميكي + استئناف بعد الإغلاق والانهيار، طابور واحد مع عدد تحميلات متزامنة، تصنيفات ومجلدات، حد سرعة، إضافة MV3 تعترض التحميلات وتمرّر الكوكيز/Referer، لوحة فيديو أساسية، استخراج yt-dlp للجودة وقوائم التشغيل، دمج ffmpeg، CLI (`eid add|list|pause|resume|cancel`)، مثبّت per-user عربي/إنجليزي، ثيم فاتح/داكن.

**يؤجَّل إن لزم:** Grabber المواقع، مزامنة دورية للملفات، torrent، حصص الميغابايت/ساعة، تثبيت إضافة المتجر التلقائي لكل المستنسخات، التحديث التلقائي الكامل (يُكتفى بفحص إصدار موقَّع مع موافقة).

---

## 7) قرارات تصميم ملموسة (اعتمدوها ما لم يظهر مانع)

1. المحرك crate مستقل اسمه مثلاً `eid-engine`؛ التطبيق والـCLI يعتمدان عليه. لا منطق تحميل داخل ملفات الواجهة.
2. التقسيم: ابدأ بوصلة واحدة بعد الـprobe، ثم اقسم أكبر جزء متبقٍّ إلى نصفين كلما توفرت وصلة، مع حد أدنى للشطر (مثلاً 1–2 ميغابايت، قابل للضبط). العدد الافتراضي 8، والحد الأقصى 16 أو 32، واستثناءات لكل موقع.
3. ملف واحد مُسبق التخصيص، sparse على NTFS، كتابة موضعِيّة، بدون تجميع لصق في النهاية إلا لمسارات HLS/DASH التي تتطلبه.
4. احفظ الميتاداتا بعد `flush` عدة مرات في الدقيقة، وموضع `on_disk` لا موضع الذاكرة.
5. الـprobe بـGET `Range: bytes=0-`. `resumable` فقط بعد `206` صحيح. احتفظ بالجسم إن كان الرابط أحادي الاستخدام.
6. عميل التجزئة على HTTP/1.1 + keep-alive لإعادة استخدام الاتصال في طلب Range التالي (درس IDM في reuse بدون login جديد).
7. `If-Range` مع ETag قوي؛ وإلا مقارنة تداخل 64 بايت. تجاهل ETag الضعيف و`Last-Modified` المتقلّب كمصدر وحيد للحقيقة.
8. عميل `reqwest` بـSChannel وALPN، ترويسات شبيهة بالمتصفح، `User-Agent` الآتي من الإضافة إن وُجد.
9. عند 403/406/416/429: مسارات احتياطية (بدون Range، بدون بعض الترويسات، IPv4 فقط) ثم تعلّم حد الموقع مع `Retry-After`.
10. الكوكيز و`Referer` وAuthorization تُشفَّر في القرص بـDPAPI لمستخدم ويندوز الحالي، لا نصاً واضحاً.
11. حد سرعة عام + لكل مهمة بـtoken bucket مشترك بين الوصلات، مع سقف للدَّين.
12. IPC: Named Pipe مصادق (رمز عشوائي يُحفظ محلياً)، لا منفذ 9614 مكشوف كقناة رئيسة. الـNative Host هو الجسر الوحيد للمتصفح.
13. اسم المضيف: `com.eid.download`. سجّل مفاتيح HKCU لكل المتصفحات المستهدفة عند أول تشغيل وبعد التحديث، واحذفها عند إلغاء التثبيت.
14. الإضافة MV3: اعتراض حسب قائمة الامتدادات، حد أدنى للحجم، استثناءات مواقع، مفتاح Bypass، إعادة التحميل للمتصفح عند الإلغاء/المهلة، لوحة فيديو فوق المشغّل مع صندوق احتياطي.
15. «تحميل لاحقاً» للفيديو يخزّن **صفحة المصدر + الجودة المختارة** لا الرابط المباشر النافذ.
16. yt-dlp للاستخراج (`-J`)؛ المحرك ينزّل؛ ffmpeg يدمج بـ`-c copy`. Deno مضمَّن. `--ignore-config` و`--` إلزاميان.
17. تحديث الأدوات (yt-dlp/Deno/ffmpeg) اختياري أو يومي بعد التحقق من SHA-256. تحديث التطبيق اختياري موقَّع (SHA-256 + توقيع Ed25519 أو Authenticode)، لا صامت إلزامي.
18. المثبّت NSIS per-user، عربي وإنجليزي، بدون إدمن. وقّع المخرجات. اكتب MotW للملفات المنزَّلة من الإنترنت عند الحاجة.
19. manifest التطبيق: `longPathAware`. تعامل مع `\\?\`. تحقق من المساحة الحرة قبل البدء.
20. واجهة Svelte: عربي RTL افتراضي قابل للتبديل، i18n كامل، فاتح/داكن/نظام. لا نصوص ثابتة في المحرك للواجهة؛ المحرك يعيد رموز حالة.
21. أسماء الملفات تُستخرج من `Content-Disposition` (بما فيها `filename*`) ثم عنوان URL ثم اسم منطقي للفيديو من yt-dlp، مع التعقيم.
22. إن وُجد ملف الوجهة: لا تَكتب فوقه صامتاً.
23. رفض DRM صراحة في الإضافة والمحرك.
24. كل الشيفرة أصلية لـEid Download؛ راجع أي «استلهام» قبل الدمج. لا تنقل كود Ezoo حتى «بعد إعادة صياغة».
25. اختبارات وحدة إلزامية لجدول الأجزاء (تغطية بلا فراغ ولا تداخل)، لموزّع السرعة، ولمحلل Range/`Content-Range`، ولتعقيم الأسماء. اختبارات تكامل مع خادم محلي يحاكي 206/200/403/429 والتحويلات.

---

## 8) قائمة أخطاء شائعة يجب تجنبها

- نسخ أي جزء من شيفرة Ezoo أو أصوله أو اسمه أو شعاره أو مفاتيح توقيعه (مخالفة الرخصة).
- الاعتماد على حقل `MIT` في `Cargo.toml` الخاص بمشاريع أخرى دون قراءة ملف `LICENSE`.
- تقسيم ثابت فقط بلا work-stealing؛ آخر جزء بطيء يقتل الزمن الكلي.
- تقسيم إلى أجزاء أصغر من أن تستحق اتصال TLS جديداً.
- الثقة بـ`Accept-Ranges` أو بـHEAD دون تجربة Range حقيقية.
- تفسير `200 OK` على طلب Range وكأنه نجاح تجزئة.
- فتح عدة طلبات Range على HTTP/2 في اتصال واحد وانتظار تسريع «متعدد الاتصالات».
- حفظ موضع الكتابة قبل `FlushFileBuffers`/`sync` ثم الاستئناف على فجوات صفرية.
- لصق أجزاء بلا التحقق من أن `end` الحصري لكل جزء يلامس `start` التالي بلا تداخل.
- استهلاك رابط monouse في الـprobe ثم بدء التحميل من صفر فيفشل بـ403.
- خلط نسختين من الملف لأن ETag ضعيف أو تغيّر `Last-Modified` بلا `If-Range`/مقارنة بايتات.
- عدم معالجة `416` و`429` و`Retry-After`.
- إعادة المحاولة اللانهائية لـ404/410 وكأنها انقطاع شبكة.
- طباعة التشخيص على stdout في Native Host، أو نسيان `O_BINARY` على ويندوز.
- وضع منفذ HTTP محلي بلا فحص Host/Origin فيُفتح بهجوم DNS rebinding من صفحة ويب.
- إرسال كوكيز الجلسة من content script إلى المضيف دون تحقق `sender`.
- اعتراض كل التحميلات بما فيها HTML والصور الصغيرة، بلا قائمة امتدادات ولا Bypass.
- إلغاء التحميل متأخراً بعد ظهور «حفظ باسم» أو بعد اكتماله في المتصفح.
- الاعتماد على service worker حيّ دائماً دون إعادة `connectNative` عند القطع.
- تحميل فيديو «لاحقاً» برابط googlevideo منتهٍ.
- تشغيل yt-dlp بدون `--ignore-config` أو بدون `--` قبل الرابط (خطر خلط خيارات/تنفيذ).
- نسيان تضمين Deno بعد 2025-11-12 ثم اتهام المحرك بأن «يوتيوب لا يعمل».
- تحديث yt-dlp بدون مطابقة `SHA2-256SUMS`، أو تشغيل أداة من PATH غير موثوقة.
- إعادة ترميز ffmpeg افتراضياً بدل `-c copy` (بطء وضياع جودة).
- محاولة فك DRM.
- مثبّت يطلب Administrator بلا حاجة، أو يكتب مفاتيح Native Messaging في HKLM فقط.
- إغفال توقيع Authenticode ثم الاستغراب من SmartScreen.
- مسارات أطول من 260 حرفاً بدون `longPathAware`.
- الكتابة فوق ملف موجود، أو أسماء من `Content-Disposition` فيها `../`.
- تخزين كلمات السر والكوكيز نصاً واضحاً في `%APPDATA%`.
- تحديث تلقائي إلزامي صامت.
- إخفاء ملف الجزئية ثم استدعاء ffmpeg عليه فيفشل بدون تفسير.
- عدم وجود mutex فيسمح بنسختين من البرنامج تفسدان القائمة.
- تجاهل مسؤولية حقوق النشر في الواجهة.

---

## 9) مصادر مستشهد بها

### IDM
- https://www.internetdownloadmanager.com/support/segmentation.html
- https://www.internetdownloadmanager.com/features2.html
- https://www.internetdownloadmanager.com/support/idm-scheduler/idm_queues.html
- https://www.internetdownloadmanager.com/support/idm-scheduler/idm_scheduler.html
- https://secure.internetdownloadmanager.com/support/options.html
- https://www.internetdownloadmanager.com/register/new_faq/functions12.html
- https://www.internetdownloadmanager.com/welcome.html
- https://www.internetdownloadmanager.com/register/new_faq/video3.html
- https://stackoverflow.com/questions/17608498/how-does-internet-download-managers-chrome-extension-work
- https://superuser.com/questions/657720/how-does-idm-resume-a-paused-http-download

### Ant Download Manager
- https://antdownloadmanager.com/uguide/module_6_1.htm
- https://antdownloadmanager.com/uguide/module_15_1_1.htm
- https://antdownloadmanager.com/uguide/module_8_3.htm
- https://antdownloadmanager.com/uguide/module_15_3.htm
- https://antdownloadmanager.com/downloads/help/antDM_en.pdf

### Ezoo Download (دراسة أفكار فقط — الرخصة مقيّدة)
- https://github.com/AhmedEzo686/ezoo-download
- https://github.com/AhmedEzo686/ezoo-download/blob/main/LICENSE
- https://github.com/AhmedEzo686/ezoo-download/blob/main/README.md

### HTTP وRust والمتصفح وويندوز
- https://developer.mozilla.org/en-US/docs/Web/HTTP/Guides/Range_requests
- https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging
- https://developer.chrome.com/docs/extensions/reference/api/downloads
- https://developer.chrome.com/docs/extensions/develop/concepts/service-workers/lifecycle
- https://learn.microsoft.com/en-us/microsoft-edge/extensions/developer-guide/native-messaging
- https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/Native_manifests
- https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/Native_messaging
- https://v2.tauri.app/distribute/windows-installer/
- https://v2.tauri.app/distribute/sign/windows/
- https://v2.tauri.app/start/prerequisites/
- https://learn.microsoft.com/en-us/windows/win32/fileio/maximum-file-path-limitation
- https://textslashplain.com/2016/04/04/downloads-and-the-mark-of-the-web/
- https://textslashplain.com/2024/11/22/parallel-downloading/

### yt-dlp
- https://github.com/yt-dlp/yt-dlp/blob/master/README.md
- https://github.com/yt-dlp/yt-dlp/issues/15012
- https://github.com/yt-dlp/yt-dlp/wiki/EJS
- https://github.com/yt-dlp/yt-dlp/releases/tag/2025.11.12

---

*أُعدّ هذا التقرير لمشروع Eid Download فقط، ولا يعدّ ترخيصاً باستخدام شيفرة أي برنامج ثالث.*
