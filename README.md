<div dir="rtl">

# Eid Download — عيد للتحميل

مدير تحميل لويندوز 10 و11 (64-bit) مكتوب بلغة **Rust**، واجهته **عربية أولاً** (من اليمين إلى اليسار) مع الإنجليزية، فاتح وداكن.

> **الحالة: قيد البناء — لا يوجد إصدار بعد.** هذا الملف لا يعد بشيء لم يثبته اختبار. ما اختُبر وما لم يُختبر مكتوب في [docs/STATUS.md](docs/STATUS.md).

## الفكرة
- محرّك تحميل بعدة اتصالات (حتى 32) بتقسيم ديناميكي، واستكمال حتى بعد إغلاق البرنامج أو انقطاع الكهرباء.
- قوائم انتظار وجدولة وحد سرعة، وتصنيف تلقائي.
- إضافة لمتصفحات Chrome وEdge (وFirefox لاحقاً) تلتقط التحميلات عبر Native Messaging.
- فيديو وقوائم تشغيل عبر yt-dlp وFFmpeg — **فقط بعد موافقتك الصريحة** على تنزيل هذه الأدوات.
- سطر أوامر `eid` يستخدم نفس المحرّك.
- مثبّت لكل مستخدم بدون صلاحيات مدير.

## ما لا يفعله
انظر [docs/REQUIREMENTS.md](docs/REQUIREMENTS.md) (لا تجاوز DRM، لا تتبع، لا منفذ localhost، لا تحديث صامت…).

## بنية المشروع
المحرّك (Rust) ← سطر الأوامر ← واجهة Tauri + Svelte ← إضافة المتصفح (Native Messaging) ← yt-dlp وFFmpeg. التفاصيل: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). القرارات: [docs/adr/](docs/adr/). الخطة والمراحل: [docs/PLAN.md](docs/PLAN.md). المقارنة مع IDM وغيره: [docs/IDM-COMPARISON.md](docs/IDM-COMPARISON.md).

## للمطوّرين ووكلاء الذكاء الاصطناعي
اقرأ [AGENTS.md](AGENTS.md) أولاً. طريقة العمل: مرحلة واحدة لكل جلسة Codex جديدة، ومراجعة في جلسة منفصلة.

## الرخصة
**لم تُحسم بعد — تُحسم قبل أي إصدار علني** (لا يوجد ملف LICENSE عمداً الآن؛ المقترح في ADR-0008: Apache-2.0 أو MIT). لم يُنسخ أي كود من IDM أو Ant أو Ezoo أو زاجل ([docs/adr/0008-license.md](docs/adr/0008-license.md)).

## البناء (للمطوّرين)
```
cargo test --workspace          # على Linux أو Windows
cargo run -p eid-cli -- get <url> -c 8
```
**مخطط (غير موجود بعد):** المثبّت يُبنى في P7 على `windows-latest`؛ الـCI الحالي يبني ويختبر ويحسب بصمة `eid.exe` فقط ([.github/workflows/ci.yml](.github/workflows/ci.yml)). كل ما في «الفكرة» أعلاه **مخطط** حتى يظهر اختبار له في [STATUS.md](docs/STATUS.md).
</div>
