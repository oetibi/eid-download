<div dir="rtl">

# مثبّت Windows (NSIS) — v0.1

حسب ADR-0005: مثبّت Tauri NSIS **لكل مستخدم** (`installMode: currentUser`، بلا صلاحيات مدير، في `%LOCALAPPDATA%\Eid Download`)، و`webviewInstallMode: embedBootstrapper` (يثبّت WebView2 إن نقص)، لغتان عربية وإنجليزية (يختار NSIS حسب لغة النظام)، اختصار في قائمة ابدأ (مجلد «Eid Download»)، ومزيل تثبيت. الـZIP باقٍ كما هو.

## كيف يُبنى (CI فقط)
`ci-windows.yml` بعد الاختبارات وفحص الواجهة وحزمة ZIP:
1. `scripts/windows/check_version.py` — رقم واحد في `Cargo.toml` و`Cargo.lock` و`tauri.conf.json` (والوسم في بناء الإصدار).
2. Tauri CLI مثبّت بنسخة محددة `tauri-cli@2.12.6` (= نسخة `tauri` في Cargo.lock) عبر `taiki-e/install-action`.
3. `scripts/windows/build_installer.ps1`: يجمع `eid.exe` و`eid-native-host.exe` وملفات الرخصة وملفات Native Messaging في `target\installer-stage`، ويكتب ملف إعداد إضافي `target\tauri.installer.conf.json` (`bundle.active=true` + `resources`)، ثم `cargo tauri bundle --bundles nsis --no-binary-patching`. لا يُعاد تجميع أي شيء، و`--no-binary-patching` يُبقي `EidDownload.exe` المثبَّت مطابقاً بايتاً ببايت للملف الذي فُحص (التحقق بـSHA-256 في الخطوة التالية). `bundle.active` يبقى `false` في `tauri.conf.json` فلا يتغير `cargo build`.
4. `scripts/windows/verify_installer.ps1`: تثبيت صامت `/S` بحساب الـrunner، فحص الملفات وتطابق SHA-256، مفاتيح HKCU، اختصار قائمة ابدأ وسطح المكتب، مدخل إزالة التثبيت بنفس رقم الإصدار، تشغيل `EidDownload.exe` المثبَّت (ملف تعريف مؤقت ونقطة IPC خاصة) وإيقافه بـ`eid.exe` المثبَّت، تشغيل `eid-native-host.exe` المثبَّت حتى EOF، ثم إزالة صامتة وفحص أن كل شيء حُذف وأن مفتاحاً يشير إلى نسخة أخرى لم يُحذف.
5. المثبّت `EidDownload-<version>-windows-x64-setup.exe` يدخل `SHA256SUMS.txt` والـartifact `eid-windows-x64` وأصول الإصدار.

## Native Messaging (إضافة المتصفح)
- **مصدر واحد للمعرّفات:** `extension/native-host-ids.json` — قوائم `chrome` و`edge` و`firefox` واسم المضيف `com.eid.download`.
- **القيم الحالية حقيقية (`"placeholder": false`):** Chrome/Edge `ambokeecedpgicighhcpdfjphdnphgaa` (مثبّت بالمفتاح العام في `scripts/ext/build.mjs`، للتثبيت اليدوي)، وFirefox `eid-download@eid-download`. معرّفات متاجر Chrome/Edge تُضاف عند النشر (`docs/OWNER-ACTIONS.md` §5). **بناء الوسم يرفض أي ملف وهمي** (`build_installer.ps1 -Release`). قائمة فارغة = لا يُسجَّل ذلك المتصفح.
- `scripts/windows/native_host_manifests.py` يكتب `com.eid.download.chrome.json` و`.edge.json` و`.firefox.json` بنفس قواعد التحقق في `crates/eid-platform/src/native_host.rs`. `path` فيها اسم الملف فقط `eid-native-host.exe`: على Windows يحلّ Chrome وEdge وFirefox المسار النسبي من مجلد ملف الـmanifest نفسه، فلا يعتمد الملف على مكان التثبيت. Edge يقبل معرّفات `edge` ومعرّفات `chrome` (لأن Edge يثبّت إضافات Chrome Web Store بمعرّفها).
- `installer/hooks.nsh`: بعد التثبيت يكتب `HKCU\Software\Google\Chrome\NativeMessagingHosts\com.eid.download` و`...\Microsoft\Edge\...` و`...\Mozilla\NativeMessagingHosts\...` (القيمة الافتراضية = مسار الـmanifest) لكل manifest موجود؛ عند الإزالة يحذف المفتاح **فقط إن كان يشير إلى هذا التثبيت**.

## ما لم يُختبر
- التثبيت على Windows 10/11 حقيقي بحساب مستخدم عادي (CI: Windows Server على GitHub).
- جهاز بلا WebView2 فعلاً (مسار `embedBootstrapper`).
- اتصال إضافة متصفح حقيقية بالمضيف **المثبَّت بالمثبّت على Windows** (مختبر على Linux في Chrome حقيقي: `scripts/ext/ext_e2e.mjs`).
- الترقية فوق نسخة سابقة مثبّتة (لا مثبّت قبل v0.1).
- SmartScreen: المثبّت غير موقّع (التوقيع يحتاج شهادة المالك).
</div>
