<div dir="rtl">

# ADR-0005: NSIS لكل مستخدم + WebView2 bootstrapper
**الحالة:** مقبول.
**القرار:** Tauri NSIS: `installMode: currentUser` (لا admin، يثبّت في `%LOCALAPPDATA%`)، `webviewInstallMode: embedBootstrapper` (يحمّل WebView2 على ويندوز 10 الذي ينقصه)، `minimumWebview2Version`، لغة عربية+إنجليزية، خطافات NSIS تكتب/تحذف مفاتيح HKCU لـNative Messaging. بدون Inno Setup (سلسلة أدوات واحدة). التوقيع Authenticode يحتاج شهادة المالك؛ بدونها SmartScreen سيحذّر (موثّق).
**الدليل:** P7: CI على windows-latest ينتج المثبّت + SHA-256؛ تثبيت صامت بحساب غير مدير وفحص مفاتيح HKCU **على Windows حقيقي فقط**.
</div>
