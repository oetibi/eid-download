<div dir="rtl">

# دليل الاختبارات اليدوية لـP4 (‏#11 و#14 و#19) على Windows

هذا الدليل خطوة بخطوة، بلا خبرة سابقة. نفّذه على جهازك الحقيقي، لا داخل جهاز افتراضي أو جلسة بعيدة. كل أمر في سطر مستقل. انسخه والصقه في PowerShell واضغط Enter، ثم انتقل للخطوة التالية.

**مهم:** لا يُسجَّل أي اختبار «نجح» إلا بعد أن تنفّذه بنفسك وترسل النتيجة. انسخ جدول النتائج في آخر هذا الملف واملأه.

**ستحتاج:**
- نحو 30 دقيقة.
- اتصال إنترنت. تنزّل الأدوات نحو 230 MB، ثم فيديو قصير، ثم ملف كبير لاختبار #14.
- حسابك على GitHub، لأن المستودع خاص.

---

## الخطوة 0: افتح PowerShell
1. اضغط زر Windows واكتب `PowerShell`.
2. اختر **Windows PowerShell** (لا «Run as administrator»). يجب أن تعمل الاختبارات بصلاحيات مستخدم عادي.

## الخطوة 1: أنشئ مجلد الاختبار
```powershell
New-Item -ItemType Directory -Force "$HOME\eid-test" | Out-Null
```
```powershell
Set-Location "$HOME\eid-test"
```

## الخطوة 2: نزّل نسخة eid.exe من GitHub Actions
1. افتح في المتصفح، وأنت مسجّل الدخول في GitHub:
   `https://github.com/oetibi/eid-download/actions/workflows/ci-windows.yml?query=branch%3Amain`
2. اضغط **أحدث تشغيل ناجح** (علامة خضراء ✓) في أعلى القائمة. رقم الـcommit واسم آخر دمج يظهران بجانبه.
3. انزل إلى قسم **Artifacts** في صفحة التشغيل.
4. **قبل التنزيل:** انسخ قيمة **Digest** بجانب `eid-windows-x64`. تبدأ بـ`sha256:`. احفظها في ملف نصي.
5. اضغط `eid-windows-x64` لتنزيل الملف `eid-windows-x64.zip`، ويذهب عادةً إلى مجلد Downloads.
6. انقله إلى مجلد الاختبار:
```powershell
Move-Item "$HOME\Downloads\eid-windows-x64.zip" "$HOME\eid-test\"
```

## الخطوة 3: تحقق من بصمة SHA-256
**أ. بصمة الملف المضغوط:** قارنها بقيمة Digest التي نسختها من GitHub.
```powershell
(Get-FileHash "$HOME\eid-test\eid-windows-x64.zip" -Algorithm SHA256).Hash.ToLower()
```
يجب أن يطابق الناتج حرفاً بحرف ما بعد `sha256:` في صفحة GitHub. **إن اختلف: توقف ولا تكمل.**

**ب. فك الضغط:**
```powershell
Expand-Archive "$HOME\eid-test\eid-windows-x64.zip" -DestinationPath "$HOME\eid-test\eid" -Force
```

**ج. بصمة eid.exe:**
```powershell
Get-Content "$HOME\eid-test\eid\SHA256SUMS.txt"
```
```powershell
(Get-FileHash "$HOME\eid-test\eid\eid.exe" -Algorithm SHA256).Hash.ToLower()
```
يجب أن يطابق الرقم الثاني السطر الخاص بـ`eid.exe` في `SHA256SUMS.txt`. ويمكن مقارنته أيضاً بسطر الخطوة «SHA-256 checksums» في سجل التشغيل نفسه على GitHub.

**د. الانتقال إلى المجلد والتأكد أن البرنامج يعمل:**
```powershell
Set-Location "$HOME\eid-test\eid"
```
```powershell
.\eid.exe --help
```
إن ظهرت رسالة أمان من Windows عن ملف تم تنزيله، فسجّل نصّها في النتائج. `eid.exe` غير موقّع رقمياً بعد، والتوقيع من مهام P7.

---

## اختبار #19 (الجزء الأول): الموافقة قبل أي تنزيل
**الهدف:** بلا موافقة لا يُنزَّل ولا يُشغَّل شيء.

1. تأكد أن الأدوات غير موجودة. المتوقع: `False`.
```powershell
Test-Path "$env:LOCALAPPDATA\EidDownload\tools"
```
2. اعرض نص الموافقة وحالتها. المتوقع: النص، ثم أن الموافقة لم تُعطَ.
```powershell
.\eid.exe video consent
```
3. اطلب معلومات فيديو **بلا** `--yes-i-agree`. سيظهر النص ثم `Type yes to agree:`. اكتب `no` واضغط Enter.
```powershell
.\eid.exe video info "https://www.youtube.com/watch?v=aqz-KE-bpKQ"
```
المتوقع: رسالة «consent not given; nothing was downloaded or started».

4. تأكد مرة أخرى أن لا شيء نُزّل. المتوقع: `False`.
```powershell
Test-Path "$env:LOCALAPPDATA\EidDownload\tools"
```

## اختبار #11 و#19 (الجزء الثاني): تنزيل فيديو حقيقي
**الهدف (#11):** لا تظهر أي نافذة سوداء (console) لـyt-dlp أو FFmpeg أو Deno أثناء التنزيل.
**الهدف (#19):** تُنزَّل الأدوات من مصادرها الرسمية وتُفحص بصماتها، ويُنزَّل الفيديو.

1. أنشئ مجلداً للفيديو:
```powershell
New-Item -ItemType Directory -Force "$HOME\eid-test\videos" | Out-Null
```
2. شغّل التنزيل بالموافقة. الفيديو «Big Buck Bunny» برخصة CC BY 3.0 من Blender Foundation.
```powershell
.\eid.exe video get "https://www.youtube.com/watch?v=aqz-KE-bpKQ" -d "$HOME\eid-test\videos" --quality 720p --yes-i-agree
```
   - المرة الأولى تنزّل الأدوات (نحو 230 MB)، وقد تستغرق دقائق.
   - **راقب الشاشة طوال التنزيل:** هل ظهرت أو ومضت أي نافذة سوداء غير نافذة PowerShell نفسها؟ سجّل نعم أو لا (#11).
   - **راقب مضاد الفيروسات:** هل ظهر تنبيه أو حجر لملف؟ سجّل اسم الملف ونص التنبيه (#19).
   - المتوقع في النهاية: يطبع البرنامج مسار ملف `.mp4`.
3. تأكد أن الفيديو موجود وحجمه أكبر من صفر:
```powershell
Get-ChildItem "$HOME\eid-test\videos"
```
4. شغّل الفيديو بالضغط عليه مرتين في المستكشف. هل يعمل بصوت وصورة؟ سجّل ذلك.
5. شغّل الأمر نفسه **مرة ثانية**. هذه هي الحالة التي أصلحناها في PR #30: يجب أن يعمل التشغيل الثاني بلا رفض. المتوقع: لا يُعاد تنزيل الأدوات، ولا تظهر رسالة «multiple hard links».
```powershell
.\eid.exe video get "https://www.youtube.com/watch?v=aqz-KE-bpKQ" -d "$HOME\eid-test\videos" --quality 720p --yes-i-agree
```
6. سجل حماية Windows: افتح **Windows Security** ← **Virus & threat protection** ← **Protection history**، وسجّل أي حدث خلال الاختبار.

## اختبار #19 (الجزء الثالث): سحب الموافقة يحذف الأدوات
1. اسحب الموافقة:
```powershell
.\eid.exe video revoke
```
2. تأكد أن الأدوات وذاكرة Deno حُذفت. المتوقع: `False`.
```powershell
Test-Path "$env:LOCALAPPDATA\EidDownload\tools"
```
3. تأكد أن Deno لم يكتب خارج مجلداتنا. المتوقع: `False`. إن كانت النتيجة `True`، فسجّل ذلك، فقد يكون مجلداً قديماً من استعمال سابق لـDeno على جهازك.
```powershell
Test-Path "$env:LOCALAPPDATA\deno\node_compat_bin"
```

---

## اختبار #14: Ctrl-C الحقيقي ثم الاستكمال
**الهدف:** Ctrl-C في نافذة PowerShell حقيقية يوقف التنزيل بأمان، ويطبع أمر الاستكمال، وهذا الأمر يُكمل الملف.

1. اختر رابط ملف كبير (1 GB أو أكثر) من موقع رسمي، مثلاً صورة ISO لتوزيعة Linux من موقعها الرسمي. ويُفضَّل موقع ينشر بصمة SHA-256 للملف.
2. أنشئ مجلداً:
```powershell
New-Item -ItemType Directory -Force "$HOME\eid-test\files" | Out-Null
```
3. ابدأ التنزيل. ضع الرابط مكان `<الرابط>`، واترك علامتي التنصيص.
```powershell
.\eid.exe get "<الرابط>" -d "$HOME\eid-test\files"
```
4. بعد أن يتقدم التنزيل قليلاً، اضغط **Ctrl+C** مرة واحدة.
   - المتوقع: يتوقف ويطبع `To resume, run:`، وتحته سطر يبدأ بـ`PowerShell: eid resume` ومعه مسار ملف ينتهي بـ`.eid-state`.
   - سجّل نص السطر كما ظهر.
5. انسخ من ذلك السطر الجزء الذي يبدأ بـ`eid resume` حتى آخره، دون كلمة `PowerShell:`. ثم شغّله بإضافة `.\` قبل `eid`، أي: `.\eid resume '…المسار…'`.
   - المتوقع: يكمل من حيث توقف، لا من البداية، وينتهي بنجاح.
6. إن كان الموقع ينشر البصمة، فقارنها:
```powershell
(Get-FileHash "$HOME\eid-test\files\<اسم الملف>" -Algorithm SHA256).Hash.ToLower()
```

---

## التنظيف بعد الانتهاء (اختياري)
```powershell
Set-Location $HOME
```
```powershell
Remove-Item -Recurse -Force "$HOME\eid-test"
```

## جدول النتائج (انسخه وأرسله)
| الاختبار | ما حدث فعلاً |
|---|---|
| الخطوة 3: بصمة الـzip تطابق Digest؟ / بصمة eid.exe تطابق SHA256SUMS؟ | |
| رسالة أمان Windows عند تشغيل eid.exe (نصها إن وُجدت) | |
| #19-1: بلا موافقة، هل بقي `Test-Path` = False ورُفض الطلب؟ | |
| #11: هل ظهرت أي نافذة سوداء أثناء تنزيل الفيديو؟ | |
| #19-2: هل نُزّل الفيديو ويعمل بصوت وصورة؟ كم استغرق؟ | |
| #19-2: هل نجح التشغيل الثاني بلا رفض؟ | |
| #19-2: تنبيهات مضاد الفيروسات / Protection history | |
| #19-3: بعد `revoke`، هل `Test-Path` = False للأدوات؟ وماذا أعطى فحص `deno\node_compat_bin`؟ | |
| #14: نص سطر `eid resume` المطبوع | |
| #14: هل أكمل الاستكمال من مكانه وانتهى بنجاح؟ وهل طابقت البصمة إن فحصتها؟ | |

</div>
