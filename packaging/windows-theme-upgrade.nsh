; One-time migration of the shipped duck-era folders. Keep any edits outside
; the active catalog; fresh Mochune folders are installed at the same paths.
!macro ArchiveDuckTheme ROOT BACKUP FOLDER PREFIX
    IfFileExists "${ROOT}\${FOLDER}\walk.gif" ${PREFIX}_${FOLDER}_archive
    IfFileExists "${ROOT}\${FOLDER}\ducklife.gif" ${PREFIX}_${FOLDER}_archive ${PREFIX}_${FOLDER}_done
${PREFIX}_${FOLDER}_archive:
    ClearErrors
    CreateDirectory "${BACKUP}"
    IfErrors ${PREFIX}_${FOLDER}_failed
    Rename "${ROOT}\${FOLDER}" "${BACKUP}\${FOLDER}"
    IfErrors ${PREFIX}_${FOLDER}_failed ${PREFIX}_${FOLDER}_done
${PREFIX}_${FOLDER}_failed:
    ; Never overwrite an old theme if its backup could not be made.
    MessageBox MB_OK|MB_ICONSTOP "Mochune could not back up an older theme. Close the app and check that its themes folder is writable. Your existing theme has been kept."
    Abort
${PREFIX}_${FOLDER}_done:
!macroend

!macro ArchiveDuckThemes ROOT BACKUP PREFIX
    !insertmacro ArchiveDuckTheme "${ROOT}" "${BACKUP}" "pastelduck" "${PREFIX}"
    !insertmacro ArchiveDuckTheme "${ROOT}" "${BACKUP}" "pixelduck" "${PREFIX}"
    !insertmacro ArchiveDuckTheme "${ROOT}" "${BACKUP}" "crt-full" "${PREFIX}"
!macroend
