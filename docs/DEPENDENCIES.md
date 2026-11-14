# التبعيات ومصدرها وتراخيصها (دفتر مبكر — يُحدَّث مع كل إضافة)
## أدوات خارجية (عمليات منفصلة، تُنزَّل بعد موافقة المستخدم، **لا نعيد توزيعها**)
| الأداة | المصدر | الترخيص (حسب ADR/وثائقها) | ملاحظات |
|---|---|---|---|
| yt-dlp | إصدارات GitHub الرسمية | الكود Unlicense؛ الملف التنفيذي المستقل (PyInstaller) GPLv3+ بحسب README الخاص بـyt-dlp | تنزيل من المصدر الرسمي؛ SHA2-256SUMS |
| FFmpeg/ffprobe | بناء رسمي موثّق بإصداره | LGPL/GPL حسب البناء (يُسجَّل المختار قبل P4) | نسخة موثقة بالإصدار والبصمة |
| Deno | إصدارات رسمية | MIT | JS runtime ليوتيوب |
## مكتبات Rust (P1)
تُسجَّل كلها هنا قبل إضافتها مع ترخيصها: tokio (MIT)، reqwest (MIT/Apache-2.0)، serde/serde_json (MIT/Apache-2.0)، sha2 (MIT/Apache-2.0)، thiserror، url، percent-encoding، futures-util، tokio-util، fastrand، libc (MIT/Apache-2.0)، windows-sys (MIT/Apache-2.0). `cargo deny check` يُضاف إلى CI قبل إغلاق P1.
## مكتبات Rust (P3)
getrandom 0.2 (MIT/Apache-2.0، كانت في Cargo.lock أصلاً) لتوكن المصافحة 256-بت · tokio ميزتا `net` و`io-util` داخل eid-platform (MIT) · windows-sys ميزات `Win32_Security`/`Win32_Security_Authorization`/`Win32_System_Registry` (MIT/Apache-2.0).
