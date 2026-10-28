; Recursive deletion that removes reparse points themselves, never their targets.
; The caller supplies fixed Mochune data directories or the verified registered
; installation directory, and resets cleanupFailed once.
Var cleanupFailed
Function un.RemoveOwnedTree
    Exch $0
    Push $1
    Push $2
    Push $3
    Push $4
    System::Call 'kernel32::GetFileAttributesW(w r0) i .r1 ?e'
    Pop $4
    IntCmp $1 -1 missing
    IntOp $2 $1 & 0x400
    IntCmp $2 0 ordinary
    ; Directory junction/symlink: remove the link without recursion.
    IntOp $2 $1 & 0x10
    ClearErrors
    IntCmp $2 0 link_file
    RMDir "$0"
    Goto check
link_file:
    Delete "$0"
    Goto check
ordinary:
    IntOp $2 $1 & 0x10
    IntCmp $2 0 link_file
    FindFirst $2 $3 "$0\*"
    StrCmp $2 "" remove_directory
loop:
    StrCmp $3 "" close
    StrCmp $3 "." next
    StrCmp $3 ".." next
    StrCpy $4 "$0\$3"
    Push $4
    Call un.RemoveOwnedTree
next:
    FindNext $2 $3
    Goto loop
close:
    FindClose $2
remove_directory:
    ClearErrors
    RMDir "$0"
check:
    IfErrors 0 done
    StrCpy $cleanupFailed 1
    Goto done
missing:
    StrCpy $1 $4
    IntCmp $1 2 done
    IntCmp $1 3 done
    StrCpy $cleanupFailed 1
 done:
    Pop $4
    Pop $3
    Pop $2
    Pop $1
    Pop $0
FunctionEnd
