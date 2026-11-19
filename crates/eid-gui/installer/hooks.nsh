; NSIS hooks for the Tauri installer (tauri.conf.json bundle.windows.nsis.installerHooks).
; Native Messaging registration for the browser extension, per user (HKCU, never admin).
;
; The manifests (com.eid.download.<browser>.json) are written at build time by
; scripts/windows/native_host_manifests.py from extension/native-host-ids.json and installed
; next to eid-native-host.exe. A browser whose ID list is empty has no manifest, so it is not
; registered. Key layout: crates/eid-platform/src/native_host.rs (imp::key_path).
; Verified on the Windows CI runner by scripts/windows/verify_installer.ps1.

!define EID_NM_HOST "com.eid.download"
!define EID_NM_CHROME "Software\Google\Chrome\NativeMessagingHosts\${EID_NM_HOST}"
!define EID_NM_EDGE "Software\Microsoft\Edge\NativeMessagingHosts\${EID_NM_HOST}"
!define EID_NM_FIREFOX "Software\Mozilla\NativeMessagingHosts\${EID_NM_HOST}"

!macro EID_NM_REGISTER KEY TAG
  ${If} ${FileExists} "$INSTDIR\${EID_NM_HOST}.${TAG}.json"
    WriteRegStr HKCU "${KEY}" "" "$INSTDIR\${EID_NM_HOST}.${TAG}.json"
  ${EndIf}
!macroend

; Remove the key only when it points at this installation's manifest: another copy of the app
; installed elsewhere keeps its registration.
!macro EID_NM_UNREGISTER KEY TAG
  ReadRegStr $R9 HKCU "${KEY}" ""
  ${If} $R9 == "$INSTDIR\${EID_NM_HOST}.${TAG}.json"
    DeleteRegKey HKCU "${KEY}"
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  !insertmacro EID_NM_REGISTER "${EID_NM_CHROME}" "chrome"
  !insertmacro EID_NM_REGISTER "${EID_NM_EDGE}" "edge"
  !insertmacro EID_NM_REGISTER "${EID_NM_FIREFOX}" "firefox"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro EID_NM_UNREGISTER "${EID_NM_CHROME}" "chrome"
  !insertmacro EID_NM_UNREGISTER "${EID_NM_EDGE}" "edge"
  !insertmacro EID_NM_UNREGISTER "${EID_NM_FIREFOX}" "firefox"
!macroend
