<div dir="rtl">

# التقدم (2026-11-14)
- **P0–P2:** مُغلقة بموافقة المالك. بند المحرّك (أ) `state::load` للقراءة فقط نُفِّذ بأمر المالك (دُمج في main)؛ (ب)(ج) وADR-0012 مؤجَّلة.
- **P3 (IPC + Native Messaging): منفَّذة على الفرع `feat/p3-ipc-native` (7 commits)، تنتظر مراجعة منفصلة وموافقة المالك. لم أبدأ P4.**
- **ما يعمل (مختبر):** نقطة اتصال لكل مستخدم (Named Pipe بـDACL محمي / Unix socket) وتوكن 256-بت، إطارات ≤1MiB، `eid serve` بنسخة واحدة (الثانية تمرّر `--add` وتخرج)، `eid add/list/pause/resume/stop` عن بعد، `eid-native-host` (stdout بروتوكول فقط، ping/add فقط، معرّف مكرر = مهمة واحدة، يُطلق التطبيق مرة واحدة)، تسجيل المضيف (HKCU/ملف).
- **الدليل (Windows 10 محلياً):** `cargo test --workspace` = 268 ناجح/0 فاشل؛ 202 اسماً في `required_tests.txt` نجحت (فحص يدوي: لا Python لتشغيل `check_required.py`)؛ fmt وclippy وبناء Windows-gnu نظيفة؛ release لـ3 حزم ناجح.
- **اكتشفتُ وأصلحتُ:** ثغرة serde (حقول زائدة على variants بلا حقول)، تسريب أنابيب المضيف إلى التطبيق على Windows، وانحدار `eid resume` القديم.
- **لم يُختبر:** عزل الأنبوب بمستخدم ثانٍ (MANUAL-TESTS #9)؛ مسارات Unix (تُبنى فقط)؛ Chrome/Edge حقيقيان (#8, #16)؛ لا CI ولا طلب دمج.
- **غير منفَّذ:** DPAPI، إيقاف الجهاز الفعلي، DACL لملفات التحميل، استدعاء `register` من المثبّت (P7)، الإضافة (P6). التفاصيل: STATUS وDESIGN_NOTES (Round 8).
- **التالي:** مراجعة P3 في جلسة منفصلة (`docs/reviews/P3.md`) ثم قرار المالك؛ P4 لا تبدأ قبل ذلك.

</div>
