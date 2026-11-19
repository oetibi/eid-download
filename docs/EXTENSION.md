<div dir="rtl">

# إضافة المتصفح (Chrome / Edge / Firefox)

## ما تفعله
زر «تحميل هذا الفيديو» فوق أي مشغّل فيديو (`<video>` بحجم 200×120 أو أكبر)، داخل Shadow DOM مغلق
حتى لا تتأثر به الصفحة ولا يؤثر فيها. عند الضغط: قائمة بالجودات المتاحة فعلاً (حسب فحص yt-dlp في
البرنامج) حتى 2160p — **لا 8K** — و«صوت فقط». على صفحة فيديو داخل قائمة تشغيل: «تنزيل القائمة
كاملة» أو «اختيار عناصر…» مع جودة واحدة للكل. الملفات المباشرة (mp4 بلا موقع فيديو): «تنزيل الملف».

- **لا تجاوز لـDRM:** فيديو يعلن حماية (EME/`mediaKeys`) لا يظهر عليه زر، والمواقع المحمية تعطي رسالة واضحة.
- **الخصوصية:** لا يُرسَل شيء حتى يضغط المستخدم خياراً؛ يُرسَل عندها رابط الصفحة/الفيديو والجودة
  المختارة فقط، إلى البرنامج على نفس الجهاز (Native Messaging)، لا إلى الإنترنت. انظر `docs/PRIVACY.md`.
- **الإعدادات:** إظهار/إخفاء، الموضع، الحجم، اللون، الشكل، أيقونة أو أيقونة ونص، الشفافية، متى يظهر،
  المواقع المستثناة، واختبار الاتصال بالبرنامج. بالعربية والإنجليزية.

## البنية
`extension/src` مصدر واحد ← `node scripts/ext/build.mjs` ← `extension/dist/chrome` (MV3، service worker،
لـChrome وEdge) و`extension/dist/firefox` (MV3، background script، معرّف gecko).
الإضافة ← `eid-native-host.exe` (مضيف Native Messaging، `com.eid.download`) ← البرنامج عبر IPC محلي.
الأوامر المسموحة من المتصفح فقط: `ping` و`probe` و`add` و`add_video` و`add_playlist` (`allowed_from_browser` في `crates/eid-ipc`)؛
`shutdown/show/list/pause/resume` مرفوضة من المتصفح.

## المعرّفات
- Chrome/Edge (تثبيت يدوي): `ambokeecedpgicighhcpdfjphdnphgaa`، مثبّت بالمفتاح **العام** `CHROME_KEY` في
  `scripts/ext/build.mjs` (المفتاح الخاص لم يُكتب على أي قرص).
- Firefox: `eid-download@eid-download`.
- المثبّت يسجّل المضيف لهذه المعرّفات من `extension/native-host-ids.json`. معرّفات المتاجر تُضاف بعد
  النشر (`docs/OWNER-ACTIONS.md` §5).

## الحزم
`node scripts/ext/package.mjs` يكتب ملفات ZIP ثابتة البايتات: `eid-download-chrome-<v>.zip` (للمتاجر،
بلا `key`)، `eid-download-firefox-<v>.zip` (AMO)، `eid-download-chrome-sideload-<v>.zip` (للتثبيت
اليدوي، مع `key`)، و`SHA256SUMS.txt`. تُرفق بالإصدار تلقائياً من `ci-windows.yml`.

## الاختبارات
| الاختبار | ما يثبته | الأمر |
|---|---|---|
| وحدات (`extension/test`) | منطق الأزرار والجودات والرسائل، البناء، الحزم، ملفات المضيف | `cd extension && node --test` |
| متصفح حقيقي (Chrome بلا واجهة) | الإضافة تُحمَّل بالمعرّف الصحيح؛ الزر فوق `<video>` حقيقي؛ القائمة؛ رسالة «غير مثبّت»؛ لا زر للفيديو الصغير | `node scripts/ext/ext_e2e.mjs --chrome google-chrome` (في CI) |
| حقيقي كامل | Chrome ← المضيف الحقيقي ← `eid serve` ← yt-dlp حقيقي ← ملف فيديو حقيقي (15 ث)، واختيار عنصر من قائمة (13 ث) | `node scripts/ext/ext_e2e.mjs --real <رابط> --host target/release/eid-native-host --app target/release/eid --home DIR` |

في الوضع الحقيقي يحجب يوتيوب المتصفح الآلي («تأكد أنك لست روبوتاً») فيُسجَّل `SKIP` لصفحة يوتيوب نفسها
— **لا نتجاوز هذا الحجب**. لذلك يقدّم الاختبار صفحة بديلة على نفس رابط يوتيوب (عبر CDP) فيها `<video>`
حقيقي، والتحميل نفسه يتم من يوتيوب الحقيقي عبر yt-dlp. **الزر على صفحة يوتيوب الحقيقية في متصفحك
لم يُختبر آلياً** — هو بند في تجربة المالك (`docs/OWNER-ACTIONS.md` §2).

</div>
