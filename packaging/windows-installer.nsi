; Per-user installation. The installed executable starts directly, without a
; self-extracting launcher or portable preparation banner.
Unicode true
!ifdef REVIEW_ONLY
Name "Mochune Installer Preview"
!else
Name "Mochune"
!endif
OutFile "${OUTPUT}"
Icon "${__FILEDIR__}/../ui/assets/app.ico"
UninstallIcon "${__FILEDIR__}/../ui/assets/app.ico"
RequestExecutionLevel user
!define APPKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Mochune"
InstallDir "$LOCALAPPDATA\Programs\Mochune"
InstallDirRegKey HKCU "${APPKEY}" "InstallLocation"
SetCompressor /SOLID lzma
SetCompressorDictSize 8
!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "nsDialogs.nsh"
!include "FileFunc.nsh"
!include "${__FILEDIR__}/windows-cleanup.nsh"
SetFont "Segoe UI" 10
BrandingText " "
!define MUI_ICON "${__FILEDIR__}/../ui/assets/app.ico"
!define MUI_UNICON "${__FILEDIR__}/../ui/assets/app.ico"
!define MUI_FORCECLASSICCONTROLS
!define MUI_BGCOLOR "131211"
!define MUI_TEXTCOLOR "F7F1E7"
!define MUI_INSTFILESPAGE_COLORS "F7F1E7 1C1A16"
!define MUI_LICENSEPAGE_BGCOLOR "FAF8F3"
!define MUI_WELCOMEFINISHPAGE_BITMAP "${__FILEDIR__}/installer-sidebar.bmp"
!define MUI_UNWELCOMEFINISHPAGE_BITMAP "${__FILEDIR__}/installer-sidebar.bmp"
!define MUI_HEADERIMAGE
!define MUI_HEADERIMAGE_RIGHT
!define MUI_HEADERIMAGE_BITMAP "${__FILEDIR__}/installer-header.bmp"
!define MUI_HEADERIMAGE_UNBITMAP "${__FILEDIR__}/installer-header.bmp"
!define MUI_WELCOMEPAGE_TITLE "Welcome to Mochune"
!define MUI_WELCOMEPAGE_TEXT "Your videos. Your music.$\r$\nA lighter way to play.$\r$\n$\r$\nInstall for your account, with no administrator access needed. Your library and themes stay yours."
!define MUI_FINISHPAGE_TITLE "Ready to take flight"
!define MUI_FINISHPAGE_TEXT "Mochune is installed and ready to open."
!define MUI_CUSTOMFUNCTION_GUIINIT ThemeWindow
!define MUI_CUSTOMFUNCTION_UNGUIINIT un.ThemeWindow
Var desktopShortcut
Var shortcutChoice
Var shortcutPage
Var startMenuShortcut
Var startMenuChoice
Var registeredLocation
!define MUI_ABORTWARNING
!insertmacro MUI_PAGE_WELCOME
!define MUI_PAGE_CUSTOMFUNCTION_SHOW LicenseColors
!insertmacro MUI_PAGE_LICENSE "${__FILEDIR__}/installer-license.rtf"
!define MUI_PAGE_CUSTOMFUNCTION_SHOW DirectoryColors
!define MUI_PAGE_CUSTOMFUNCTION_LEAVE VerifyInstallDirectory
!insertmacro MUI_PAGE_DIRECTORY
Page custom ShortcutPage ShortcutLeave
!define MUI_PAGE_CUSTOMFUNCTION_SHOW InstallationColors
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_RUN "$INSTDIR\mochune.exe"
!define MUI_FINISHPAGE_RUN_NOTCHECKED
!define MUI_FINISHPAGE_RUN_TEXT "Open Mochune"
!ifdef REVIEW_ONLY
!define MUI_FINISHPAGE_RUN_FUNCTION PreviewRunNoop
!endif
!insertmacro MUI_PAGE_FINISH
!define MUI_PAGE_CUSTOMFUNCTION_SHOW un.ConfirmColors
!insertmacro MUI_UNPAGE_CONFIRM
!define MUI_PAGE_CUSTOMFUNCTION_SHOW un.InstallationColors
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"


; Match the app's warm charcoal and duck-beak amber without another UI runtime.
!macro ThemeWindow prefix
Function ${prefix}ThemeWindow
    SetCtlColors $HWNDPARENT F7F1E7 131211
    GetDlgItem $0 $HWNDPARENT 1
    SetCtlColors $0 131211 F4A62A
    GetDlgItem $0 $HWNDPARENT 2
    SetCtlColors $0 F7F1E7 24211C
    GetDlgItem $0 $HWNDPARENT 3
    SetCtlColors $0 F7F1E7 24211C
    GetDlgItem $0 $HWNDPARENT 1028
    SetCtlColors $0 BCAE98 131211
    ; Hide the standard wizard separator on every page.
    GetDlgItem $0 $HWNDPARENT 1035
    ShowWindow $0 ${SW_HIDE}
    ; Ask modern Windows to use the dark title bar; older versions ignore it.
    System::Call 'dwmapi::DwmSetWindowAttribute(p $HWNDPARENT, i 20, *i 1, i 4)'
FunctionEnd
!macroend
!insertmacro ThemeWindow ""
!insertmacro ThemeWindow "un."
!macro InstallationColors prefix
Function ${prefix}InstallationColors
    SetCtlColors $mui.InstFilesPage F7F1E7 131211
    SetCtlColors $mui.InstFilesPage.Text F7F1E7 131211
    SetCtlColors $mui.InstFilesPage.Log F7F1E7 1C1A16
    System::Call 'uxtheme::SetWindowTheme(p $mui.InstFilesPage.ProgressBar, w "", w "")'
    ; Win32 COLORREF uses BGR, unlike app/NSIS hexadecimal color strings.
    SendMessage $mui.InstFilesPage.ProgressBar 0x409 0 0x002AA6F4
    SendMessage $mui.InstFilesPage.ProgressBar 0x2001 0 0x00161A1C
FunctionEnd
!macroend
!insertmacro InstallationColors ""
!insertmacro InstallationColors "un."
Function DirectoryColors
    SetCtlColors $mui.DirectoryPage F7F1E7 131211
    SetCtlColors $mui.DirectoryPage.Text F7F1E7 131211
    SetCtlColors $mui.DirectoryPage.Directory F7F1E7 24211C
    SetCtlColors $mui.DirectoryPage.DirectoryBox F7F1E7 131211
    SetCtlColors $mui.DirectoryPage.BrowseButton F7F1E7 24211C
    SetCtlColors $mui.DirectoryPage.SpaceRequired BCAE98 131211
    SetCtlColors $mui.DirectoryPage.SpaceAvailable BCAE98 131211
FunctionEnd
Function LicenseColors
    SetCtlColors $mui.LicensePage F7F1E7 131211
    SetCtlColors $mui.LicensePage.TopText F7F1E7 131211
    SetCtlColors $mui.LicensePage.Text F7F1E7 131211
    SetCtlColors $mui.LicensePage.LicenseText 2C2114 FAF8F3
FunctionEnd
Function un.ConfirmColors
    FindWindow $0 "#32770" "" $HWNDPARENT
    SetCtlColors $0 F7F1E7 131211
    GetDlgItem $1 $0 1006
    SetCtlColors $1 F7F1E7 131211
FunctionEnd
Function ShortcutPage
    !insertmacro MUI_HEADER_TEXT "Make yourself at home" "Choose where Mochune appears."
    nsDialogs::Create 1018
    Pop $shortcutPage
    StrCmp $shortcutPage error 0 +2
    Abort
    SetCtlColors $shortcutPage F7F1E7 131211
    ${NSD_CreateLabel} 0 4u 100% 24u "Choose the shortcuts you want to create."
    Pop $0
    SetCtlColors $0 BCAE98 131211
    ${NSD_CreateCheckbox} 0 34u 100% 18u "Create Start Menu shortcuts"
    Pop $startMenuChoice
    System::Call 'uxtheme::SetWindowTheme(p $startMenuChoice, w " ", w " ")'
    SetCtlColors $startMenuChoice F7F1E7 131211
    ${NSD_SetState} $startMenuChoice $startMenuShortcut
    ${NSD_CreateCheckbox} 0 62u 100% 18u "Create a desktop shortcut"
    Pop $shortcutChoice
    System::Call 'uxtheme::SetWindowTheme(p $shortcutChoice, w " ", w " ")'
    SetCtlColors $shortcutChoice F7F1E7 131211
    ${NSD_SetState} $shortcutChoice $desktopShortcut
    nsDialogs::Show
FunctionEnd
Function ShortcutLeave
    ${NSD_GetState} $startMenuChoice $startMenuShortcut
    ${NSD_GetState} $shortcutChoice $desktopShortcut
FunctionEnd

!ifdef REVIEW_ONLY
Function PreviewRunNoop
    ; The preview exercises real page controls without launching/installing anything.
FunctionEnd
!endif

; Normalize lexically: NSIS GetFullPathName may return empty for a folder
; that does not exist yet. Never let that turn validation into a drive-root scan.
!macro NormalizeInstallPath input output failure
    System::Call 'kernel32::GetFullPathNameW(w "${input}", i ${NSIS_MAX_STRLEN}, w .r0, p 0) i .r1'
    ${If} $1 == 0
    ${OrIf} $1 >= ${NSIS_MAX_STRLEN}
        Goto ${failure}
    ${EndIf}
    StrCpy ${output} $0
!macroend
; Each custom destination must be empty or an existing registered Mochune install.
Function VerifyInstallDirectory
!ifdef REVIEW_ONLY
    Return
!endif
    !insertmacro NormalizeInstallPath "$INSTDIR" $INSTDIR invalid_location
    StrCmp $INSTDIR "$LOCALAPPDATA\Mochune" invalid_location
    StrCmp $INSTDIR "$APPDATA\Mochune" invalid_location
    ${GetRoot} "$INSTDIR" $0
    StrCmp $INSTDIR "$0\" invalid_location
    StrCmp $INSTDIR "$0" invalid_location
    StrCmp $registeredLocation "" check_contents
    !insertmacro NormalizeInstallPath "$registeredLocation" $2 invalid_location
    StrCmp $INSTDIR $2 check_existing
    IfFileExists "$2\INSTALLED.txt" 0 check_contents
    MessageBox MB_OK|MB_ICONSTOP "Mochune is already installed elsewhere. Update in that folder, or uninstall it before choosing a different location."
    Abort
check_existing:
    IfFileExists "$INSTDIR\PORTABLE.txt" invalid_location
    IfFileExists "$INSTDIR\INSTALLED.txt" 0 check_contents
    IfFileExists "$INSTDIR\mochune.exe" verified_location invalid_location
check_contents:
    ; Never claim an unrelated directory: uninstall removes the owned folder.
    FindFirst $0 $1 "$INSTDIR\*"
    IfErrors verified_location
contents_loop:
    StrCmp $1 "" contents_empty
    StrCmp $1 "." contents_next
    StrCmp $1 ".." contents_next
    FindClose $0
    Goto invalid_location
contents_next:
    FindNext $0 $1
    IfErrors contents_empty
    Goto contents_loop
contents_empty:
    FindClose $0
    Goto verified_location
invalid_location:
    MessageBox MB_OK|MB_ICONSTOP "Choose an empty, dedicated Mochune folder, or the existing registered installation folder. Portable copies and folders containing other files cannot be used."
    Abort
verified_location:
FunctionEnd
Function .onInit
    StrCpy $desktopShortcut ${BST_CHECKED}
    StrCpy $startMenuShortcut ${BST_CHECKED}
    SetShellVarContext current
    SetRegView 64
!ifdef REVIEW_ONLY
    StrCpy $INSTDIR "$TEMP\Mochune-Installer-Preview"
    Return
!endif
    ReadRegStr $registeredLocation HKCU "${APPKEY}" "InstallLocation"
    StrCmp $registeredLocation "" +2
    StrCpy $INSTDIR $registeredLocation
FunctionEnd
Function un.onInit
    SetShellVarContext current
    SetRegView 64
    ReadRegStr $2 HKCU "${APPKEY}" "InstallLocation"
    StrCmp $2 "" invalid_uninstall
    !insertmacro NormalizeInstallPath "$2" $2 invalid_uninstall
    !insertmacro NormalizeInstallPath "$INSTDIR" $INSTDIR invalid_uninstall
    StrCmp $INSTDIR $2 0 invalid_uninstall
    ${GetRoot} "$INSTDIR" $0
    StrCmp $INSTDIR "$0\" invalid_uninstall
    StrCmp $INSTDIR "$0" invalid_uninstall
    IfFileExists "$INSTDIR\INSTALLED.txt" marker_ok
invalid_uninstall:
    MessageBox MB_OK|MB_ICONSTOP "The registered Mochune installation location or marker could not be verified. No files were removed."
    Abort
marker_ok:
FunctionEnd
!macro EnsureClosed prefix
    IfFileExists "$INSTDIR\mochune.exe" 0 ${prefix}_closed
    ; Opening for exclusive write detects an executable still mapped/running.
    ; No bytes are written and the handle is closed immediately.
    System::Call 'kernel32::CreateFileW(w "$INSTDIR\mochune.exe", i 0x40000000, i 0, p 0, i 3, i 0x80, p 0) p .r0'
    StrCmp $0 -1 0 ${prefix}_handle
    MessageBox MB_OK|MB_ICONSTOP "Close Mochune before installing or uninstalling it, then try again."
    Abort
${prefix}_handle:
    System::Call 'kernel32::CloseHandle(p r0)'
${prefix}_closed:
!macroend
Section "Mochune"
!ifdef REVIEW_ONLY
    ; Screenshot-only wizard: no application files, shortcuts or registry writes.
    DetailPrint "Installer preview: no changes will be made."
    Sleep 1800
!else
    Call VerifyInstallDirectory
    !insertmacro EnsureClosed install
    ClearErrors
    SetOutPath "$INSTDIR"
    SetOverwrite on
    File /r /x themes /x PORTABLE.txt "${PAYLOAD}/*"
    IfErrors install_failed
    ; Move the previous profile before bundling themes creates the destination.
    ; Never merge into or overwrite an existing Mochune profile.
    IfFileExists "$LOCALAPPDATA\Mochune\*.*" profile_ready
    IfFileExists "$LOCALAPPDATA\DuckTube\*.*" 0 profile_ready
    ClearErrors
    Rename "$LOCALAPPDATA\DuckTube" "$LOCALAPPDATA\Mochune"
    IfErrors install_failed
profile_ready:
    ; Editable themes are per-user data, preserved during updates.
    SetOutPath "$LOCALAPPDATA\Mochune\themes"
    SetOverwrite off
    File /r "${PAYLOAD}/themes/*"
    SetOverwrite on
    IfErrors install_failed
    FileOpen $0 "$INSTDIR\INSTALLED.txt" w
    IfErrors install_failed
    FileWrite $0 "Mochune per-user installation. User data: %LOCALAPPDATA%\Mochune."
    FileClose $0
    FileOpen $0 "$INSTDIR\.bundle-id" w
    IfErrors install_failed
    FileWrite $0 "${RUNTIME_ID}"
    FileClose $0
    IfErrors install_failed
    WriteUninstaller "$INSTDIR\Uninstall.exe"
    IfErrors install_failed
    ${If} $startMenuShortcut == ${BST_CHECKED}
        CreateDirectory "$SMPROGRAMS\Mochune"
        CreateShortcut "$SMPROGRAMS\Mochune\Mochune.lnk" "$INSTDIR\mochune.exe" "" "$INSTDIR\mochune.exe"
        CreateShortcut "$SMPROGRAMS\Mochune\Uninstall Mochune.lnk" "$INSTDIR\Uninstall.exe"
    ${Else}
        Delete "$SMPROGRAMS\Mochune\Mochune.lnk"
        Delete "$SMPROGRAMS\Mochune\Uninstall Mochune.lnk"
        RMDir "$SMPROGRAMS\Mochune"
    ${EndIf}
    ${If} $desktopShortcut == ${BST_CHECKED}
        CreateShortcut "$DESKTOP\Mochune.lnk" "$INSTDIR\mochune.exe" "" "$INSTDIR\mochune.exe"
        WriteINIStr "$INSTDIR\shortcuts.ini" "created" "desktop" "1"
    ${Else}
        ReadINIStr $0 "$INSTDIR\shortcuts.ini" "created" "desktop"
        ${If} $0 == "1"
            Delete "$DESKTOP\Mochune.lnk"
            WriteINIStr "$INSTDIR\shortcuts.ini" "created" "desktop" "0"
        ${EndIf}
    ${EndIf}
    WriteRegStr HKCU "${APPKEY}" "DisplayName" "Mochune"
    WriteRegStr HKCU "${APPKEY}" "DisplayVersion" "${VERSION}"
    WriteRegStr HKCU "${APPKEY}" "Publisher" "Quaint69"
    WriteRegStr HKCU "${APPKEY}" "URLInfoAbout" "https://github.com/Quaint69/Mochune"
    WriteRegStr HKCU "${APPKEY}" "DisplayIcon" "$INSTDIR\mochune.exe"
    WriteRegStr HKCU "${APPKEY}" "InstallLocation" "$INSTDIR"
    WriteRegStr HKCU "${APPKEY}" "UninstallString" '$\"$INSTDIR\Uninstall.exe$\"'
    WriteRegStr HKCU "${APPKEY}" "QuietUninstallString" '$\"$INSTDIR\Uninstall.exe$\" /S'
    WriteRegDWORD HKCU "${APPKEY}" "NoModify" 1
    WriteRegDWORD HKCU "${APPKEY}" "NoRepair" 1
    Goto install_done
install_failed:
    MessageBox MB_OK|MB_ICONSTOP "Mochune could not be installed. Close the app and check this account can write to the selected installation folder and LocalAppData. Existing user data has been kept."
    SetErrorLevel 1
    Abort
install_done:
!endif
SectionEnd
UninstallText "Remove Mochune and erase its settings, history, likes, playlists, caches, logs and custom themes? Exported media downloads and separate portable copies are left alone."
Section "Uninstall"
    !insertmacro EnsureClosed uninstall
    SetOutPath "$TEMP"
    ClearErrors
    Delete "$SMPROGRAMS\Mochune\Mochune.lnk"
    Delete "$SMPROGRAMS\Mochune\Uninstall Mochune.lnk"
    RMDir "$SMPROGRAMS\Mochune"
    ; Remove the desktop shortcut only if this installer created it.
    ClearErrors
    ReadINIStr $0 "$INSTDIR\shortcuts.ini" "created" "desktop"
    StrCmp $0 "1" 0 +2
    Delete "$DESKTOP\Mochune.lnk"
    ClearErrors
    StrCpy $cleanupFailed 0
    ; Fixed per-user app roots only. Never remove AppData or Downloads wholesale.
    Push "$LOCALAPPDATA\Mochune"
    Call un.RemoveOwnedTree
    Push "$APPDATA\Mochune"
    Call un.RemoveOwnedTree
    Push "$LOCALAPPDATA\DuckTube"
    Call un.RemoveOwnedTree
    Push "$APPDATA\DuckTube"
    Call un.RemoveOwnedTree
    Push "$LOCALAPPDATA\monoma\YTLite"
    Call un.RemoveOwnedTree
    Push "$APPDATA\monoma\YTLite"
    Call un.RemoveOwnedTree
    Push "$LOCALAPPDATA\YTLite"
    Call un.RemoveOwnedTree
    Push "$APPDATA\YTLite"
    Call un.RemoveOwnedTree
    StrCmp $cleanupFailed 0 +2
    Goto uninstall_failed
    ; Remove obsolete organization containers only when now completely empty.
    RMDir "$LOCALAPPDATA\monoma"
    RMDir "$APPDATA\monoma"
    Push "$INSTDIR"
    Call un.RemoveOwnedTree
    StrCmp $cleanupFailed 0 +2
    Goto uninstall_failed
    DeleteRegKey HKCU "${APPKEY}"
    Goto uninstall_done
uninstall_failed:
    MessageBox MB_OK|MB_ICONSTOP "Some Mochune files or saved data could not be removed. Close Mochune and retry the uninstaller to finish cleanup."
    SetErrorLevel 1
uninstall_done:
SectionEnd
