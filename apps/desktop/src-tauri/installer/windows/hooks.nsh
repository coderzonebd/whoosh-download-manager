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

; Uninstalling (not updating: the in-app updater passes /UPDATE) removes the
; logon entries, so nothing starts the deleted programs at the next logon,
; and the browser host manifests, which point at them.
;
; With "Delete the application data" ticked it also removes everything WDM
; keeps for this user: the data directory (download list and history,
; settings, the resume journal, logs, the media tools fetched after install)
; and the WebView data. Downloaded files are never touched. This follows the
; box, not every uninstall, because installing a newer version by hand
; first runs the old uninstaller (Tauri's default "Uninstall before
; installing"), which must keep the user's downloads and settings.
!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $UpdateMode <> 1
    SetShellVarContext current
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "WDM daemon"
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run" "WDM daemon"
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Whoosh daemon"
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run" "Whoosh daemon"
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run" "${PRODUCTNAME}"
    RmDir /r "$LOCALAPPDATA\wdm\nmhost"
    ${If} $DeleteAppDataCheckboxState = 1
      RmDir /r "$LOCALAPPDATA\wdm"
      RmDir /r "$LOCALAPPDATA\${BUNDLEID}"
      RmDir /r "$APPDATA\${BUNDLEID}"
    ${EndIf}
  ${EndIf}
!macroend
