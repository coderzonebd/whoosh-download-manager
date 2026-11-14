; Tauri NSIS installer hooks (bundle.windows.nsis.installerHooks, set by
; tauri.browsers.windows.conf.json). The NSIS installer is per user, so it connects
; the browsers for the installing user only: whoosh-nmhost writes the native
; messaging host manifests and HKCU keys for every supported browser, and
; asks the browsers to offer the extension from their store where a store ID
; exists (packaging/browsers/extension-ids.json). Uninstalling removes both.
; Failures are ignored: the app can register again from "Connect your browser".

!macro NSIS_HOOK_POSTINSTALL
  nsExec::Exec '"$INSTDIR\whoosh-nmhost.exe" --register --browser all --offer-extension'
  Pop $0
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::Exec '"$INSTDIR\whoosh-nmhost.exe" --unregister'
  Pop $0
!macroend
