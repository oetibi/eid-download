<div dir="rtl">

# ADR-0006: طبقة المنصة `eid-platform`
**الحالة:** مقبول (رسمة المستخدم الثانية: «طبقة لكل نظام»).
**القرار:** كل ما يخص النظام (Named Pipe، DPAPI، سجل، sparse/ملفات، استبدال ذري، إيقاف الجهاز، إنهاء شجرة العمليات، إطلاق العمليات الفرعية بلا نافذة، الحافظة، الإشعارات) خلف traits في `eid-platform`، بتنفيذ Windows وUnix وstub. **ممنوع `cfg(windows)` خارجها** (استثناء موثق). المحرك وCLI يبنيان ويُختبران على Linux؛ تنفيذ Windows يُفحص بـ`cargo check --target x86_64-pc-windows-gnu` هنا وبالاختبار الكامل على windows-latest.
**الدليل:** فحص آلي (`tests/guard.rs`): لا `cfg(windows)` ولا `std::process::Command` خارج `eid-platform`.
</div>
