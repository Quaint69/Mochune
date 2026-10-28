; Persistent portable app. Only the versioned runtime is replaced on update.
Unicode true
Name "Mochune Portable"
Icon "${__FILEDIR__}/../ui/assets/app.ico"
OutFile "${OUTPUT}"
RequestExecutionLevel user
SilentInstall silent
AutoCloseWindow true
SetCompressor /SOLID lzma
SetCompressorDictSize 8
!include "FileFunc.nsh"
Var lock
Var result
Var preparing
Var themesOnly
Function ShowPreparation
    Banner::show /NOUNLOAD "Preparing Mochune...$\r$\nExtracting app files. Please wait."
    Banner::getWindow /NOUNLOAD
    Pop $preparing
    System::Call 'user32::SetWindowTextW(p $preparing, w "Mochune")'
FunctionEnd
Function HidePreparation
    StrCmp $preparing 0 finished
    Banner::destroy
    StrCpy $preparing 0
finished:
FunctionEnd
Section
    StrCpy $preparing 0
    StrCpy $themesOnly 0
    StrCpy $INSTDIR "$EXEDIR\Mochune"
    ; Preserve an existing portable library on the first branded launch.
    IfFileExists "$INSTDIR\*.*" brand_ready
    IfFileExists "$EXEDIR\DuckTube\*.*" 0 legacy_ytlite
    ClearErrors
    Rename "$EXEDIR\DuckTube" "$INSTDIR"
    IfErrors folder_failed
    Goto brand_ready
legacy_ytlite:
    IfFileExists "$EXEDIR\YTLite\*.*" 0 brand_ready
    ClearErrors
    Rename "$EXEDIR\YTLite" "$INSTDIR"
    IfErrors folder_failed
brand_ready:
    ClearErrors
    CreateDirectory "$INSTDIR"
    IfErrors folder_failed
    ; Keep a per-folder lock until the app exits, preventing concurrent extraction.
    System::Call 'kernel32::CreateFileW(w "$INSTDIR\.launch.lock", i 0xC0000000, i 0, p 0, i 4, i 0x80, p 0) p .r0'
    StrCpy $lock $0
    StrCmp $lock -1 locked
    SetOutPath "$INSTDIR"
    StrCpy $0 ""
    ClearErrors
    FileOpen $1 "$INSTDIR\runtime\.bundle-id" r
    IfErrors extract
    FileRead $1 $0
    FileClose $1
    StrCmp $0 "${RUNTIME_ID}" 0 extract
    IfFileExists "$INSTDIR\runtime\mochune.exe" 0 extract
    IfFileExists "$INSTDIR\runtime\yt-dlp\yt-dlp.exe" 0 extract
    IfFileExists "$INSTDIR\runtime\yt-dlp\_internal\python310.dll" 0 extract
    IfFileExists "$INSTDIR\runtime\deno.exe" 0 extract
    IfFileExists "$INSTDIR\runtime\PORTABLE.txt" 0 extract
    ; Repair missing stock themes without replacing the already-unpacked runtime.
    IfFileExists "$INSTDIR\themes\pastelduck\skin.json" 0 repair_themes
    IfFileExists "$INSTDIR\themes\pixelduck\skin.json" 0 repair_themes
    IfFileExists "$INSTDIR\themes\crt-full\skin.json" 0 repair_themes
    IfFileExists "$INSTDIR\themes\pixelduck\fonts\DuckPixel.otf" 0 repair_themes
    IfFileExists "$INSTDIR\themes\pixelduck\fonts\DuckPixel-Bold.otf" 0 repair_themes
    IfFileExists "$INSTDIR\themes\pastelduck\fonts\PixelMplus10-Regular.ttf" 0 repair_themes
    IfFileExists "$INSTDIR\themes\pastelduck\fonts\PixelMplus10-Bold.ttf" 0 repair_themes
    Goto launch
repair_themes:
    StrCpy $themesOnly 1
extract:
    Call ShowPreparation
    ; Prepare missing themes on first launch, update or repair.
    ; Never replace the user's edited themes on an update.
    ClearErrors
    SetOutPath "$INSTDIR\themes"
    SetOverwrite off
    File /r "${PAYLOAD}/themes/*"
    SetOverwrite on
    IfErrors extract_failed
    StrCmp $themesOnly 1 launch
    ; User settings/caches live in the separate data folder and are preserved.
    IfFileExists "$INSTDIR\runtime\*.*" 0 runtime_absent
    ClearErrors
    RMDir /r "$INSTDIR\runtime"
    IfErrors extract_failed
runtime_absent:
    ClearErrors
    SetOutPath "$INSTDIR\runtime"
    IfErrors extract_failed
    SetOverwrite on
    File /r /x themes "${PAYLOAD}/*"
    IfErrors extract_failed
    ClearErrors
    FileOpen $1 "$INSTDIR\runtime\.bundle-id" w
    IfErrors extract_failed
    FileWrite $1 "${RUNTIME_ID}"
    FileClose $1
    IfErrors extract_failed
launch:
    Call HidePreparation
    SetOutPath "$INSTDIR\runtime"
    ${GetParameters} $1
    ClearErrors
    ExecWait '"$INSTDIR\runtime\mochune.exe" $1' $result
    IfErrors launch_failed
    Goto close_lock
extract_failed:
    Call HidePreparation
    MessageBox MB_OK|MB_ICONSTOP "Mochune could not prepare its runtime folder. Close any running copy and check that this location is writable. Your data folder has been kept."
    StrCpy $result 1
    Goto close_lock
launch_failed:
    MessageBox MB_OK|MB_ICONSTOP "Mochune could not start. Check that this Windows PC supports the app."
    StrCpy $result 1
close_lock:
    System::Call 'kernel32::CloseHandle(p $lock)'
    SetErrorLevel $result
    Goto done
locked:
    MessageBox MB_OK|MB_ICONSTOP "Mochune is already running from this folder, or the folder cannot be written to. Close it before launching again."
    SetErrorLevel 1
    Goto done
folder_failed:
    MessageBox MB_OK|MB_ICONSTOP "Mochune needs to create a folder beside this executable. Move it to a writable location and try again."
    SetErrorLevel 1
done:
    SetOutPath "$EXEDIR"
SectionEnd
