; Tauri NSIS installer hooks (bundle.windows.nsis.installerHooks, set by
; tauri.browsers.windows.conf.json). The NSIS installer is per user, so it connects
; the browsers for the installing user only: wdm-nmhost writes the native
; messaging host manifests and HKCU keys for every supported browser, and
; asks the browsers to offer the extension from their store where a store ID
; exists (packaging/browsers/extension-ids.json). Uninstalling removes both.
; Failures are ignored: the app can register again from "Connect your browser".
;
; Before installing over an earlier version, and before uninstalling, the
; daemon and the browser host are stopped: they keep running after the app's
; window is closed (the daemon finishes downloads; the host serves an open
; browser), and Windows does not let the installer replace or delete a running
; .exe ("Error opening file for writing: ...\wdm-daemon.exe"). Stopping them
; is safe: a download resumes from its journal, and the browser starts the
; host again when the extension next connects. taskkill only reaches this
; user's processes, which is what a per-user install needs.

!macro WDM_STOP_BACKGROUND
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /IM wdm-daemon.exe'
  Pop $0
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /IM wdm-nmhost.exe'
  Pop $0
  ; Give Windows a moment to release the files.
  Sleep 500
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro WDM_STOP_BACKGROUND
!macroend

!macro NSIS_HOOK_POSTINSTALL
  nsExec::Exec '"$INSTDIR\wdm-nmhost.exe" --register --browser all --offer-extension'
  Pop $0
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::Exec '"$INSTDIR\wdm-nmhost.exe" --unregister'
  Pop $0
  !insertmacro WDM_STOP_BACKGROUND
!macroend
