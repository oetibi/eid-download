# التبعيات ومصدرها وتراخيصها (دفتر مبكر — يُحدَّث مع كل إضافة)
## أدوات خارجية (عمليات منفصلة، تُنزَّل بعد موافقة المستخدم، **لا نعيد توزيعها**)
| الأداة | المصدر | الترخيص (حسب ADR/وثائقها) | ملاحظات |
|---|---|---|---|
| yt-dlp | إصدارات GitHub الرسمية (`yt-dlp/yt-dlp`)، الملف `yt-dlp.exe` (onefile، D9) | الكود Unlicense؛ الملف التنفيذي المستقل (PyInstaller) GPLv3+ بحسب README الخاص بـyt-dlp | تنزيل من المصدر الرسمي؛ البصمة من `SHA2-256SUMS` للإصدار نفسه |
| FFmpeg/ffprobe | **BtbN/FFmpeg-Builds** على GitHub (D1): فرع n8.1، win64 ثابت (غير shared)، مثبَّت على وسم مؤرَّخ `autobuild-…` لا `latest` | **LGPL** (البناء `-lgpl-8.1`؛ libmp3lame مفعّلة) | البصمة من `checksums.sha256` لذلك الوسم؛ ffmpeg.org لا ينشر بناءً لـWindows، فهذه صفحة «المصدر الرسمي» المعتمدة |
| Deno | إصدارات GitHub الرسمية (`denoland/deno`)، `deno-x86_64-pc-windows-msvc.zip` | MIT | JS runtime ليوتيوب (EJS)، بوضع JIT-less (D15)؛ البصمة من `.sha256sum` المرافق |

**الإصدارات المبدئية (D16، تُعاد مراجعتها يوم بدء G2 وتُسجَّل البصمات من الملفات الرسمية لا من الذاكرة):** yt-dlp `2026.10.26` (بصمة `yt-dlp.exe` الرسمية `f5389d3698ecde4ef06f4e6b1ce0294fe9ac1396228c8990b8d3416d6d177a7d`)، Deno `v2.9.8`، FFmpeg `n8.1.3` (`autobuild-2026-11-14-12-37`، `ffmpeg-n8.1.3-35-gf1acb0a266-win64-lgpl-8.1.zip`). المرجع الملزم هو `tools/manifest.json` حين يُنشأ في G2.

**اتصال شبكي جديد معتمد (D14، AGENTS §8):** تنزيل الأدوات بعد الموافقة فقط، HTTPS فقط، من `github.com` ومضيفي أصول إصدارات GitHub فقط (القائمة الدقيقة في الـmanifest، واختبار `tool_manifest_rejects_non_https_and_unknown_hosts`). لا تحديث صامت: «التحديث» = تثبيت ما يثبّته manifest الإصدار الحالي من البرنامج.

## مكتبات Rust (P1)
تُسجَّل كلها هنا قبل إضافتها مع ترخيصها: tokio (MIT)، reqwest (MIT/Apache-2.0)، serde/serde_json (MIT/Apache-2.0)، sha2 (MIT/Apache-2.0)، thiserror، url، percent-encoding، futures-util، tokio-util، fastrand، libc (MIT/Apache-2.0)، windows-sys (MIT/Apache-2.0). `cargo deny check` يُضاف إلى CI قبل إغلاق P1.
## مكتبات Rust (P3)
getrandom 0.2 (MIT/Apache-2.0، كانت في Cargo.lock أصلاً) لتوكن المصافحة 256-بت · tokio ميزتا `net` و`io-util` داخل eid-platform (MIT) · (مراجعة P3) windows-sys ميزة `Win32_System_Pipes` (للتحقق من خادم الأنبوب) · windows-sys ميزات `Win32_Security`/`Win32_Security_Authorization`/`Win32_System_Registry` (MIT/Apache-2.0).
## مكتبات Rust (P4) — معتمدة، تُضاف في بوابتها
- **zip** (MIT) بـ`default-features = false` وميزة `deflate` فقط (D5)، لفك أرشيفات FFmpeg/Deno في G2 (مدخلات مسمّاة فقط، سقف حجم، SHA-256 لكل ملف مفكوك). تبعياتها المتعدية تُسجَّل هنا عند الإضافة بعد `cargo deny check`.
- **G1 (بلا مكتبة جديدة في Cargo.lock):** eid-platform: ميزات tokio `process`/`rt`/`sync` (MIT)، وميزتا windows-sys `Win32_System_JobObjects` و`Win32_System_Diagnostics_ToolHelp` (MIT/Apache-2.0). حزمة `eid-media` تستعمل ما في الشجرة أصلاً: serde/serde_json، thiserror، url، tokio. حزمة الاختبار `eid-fake-ytdlp` (لا تُشحن): serde_json، url، وsha2 للاختبار.

## فحص الرخص (`cargo deny`، D10)
`deny.toml` في جذر المستودع؛ CI (`ci-linux`) يشغّل `cargo deny check` **إلزامياً** (لا `continue-on-error`). الرخص المسموحة فيه هي الموجودة فعلاً في شجرة التبعيات (فُحصت من `cargo metadata` لهدفي Windows في 2026-11-15): MIT، Apache-2.0، ISC، BSD-3-Clause، Unicode-3.0، Zlib، BSL-1.0، Unlicense، CDLA-Permissive-2.0 (بيانات `webpki-roots`). حزمنا `publish = false` وبلا رخصة بعد (ADR-0008) فتُتجاهل كحزم خاصة. أي رخصة جديدة = تعديل `deny.toml` + سطر هنا.
