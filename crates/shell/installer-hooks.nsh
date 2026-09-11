; Puts the install directory's bin folder on the user's PATH, so `mytimeoff` works
; in a terminal.
;
; The window is the front door and nothing here is required to use it. This exists so that
; `mytimeoff check` - the one command a person runs when the quiz is behaving oddly - can be
; typed rather than hunted for in %LOCALAPPDATA%.
;
; The command line lives in bin rather than beside the window because Windows filenames
; are case-insensitive: MyTimeOff.exe and mytimeoff.exe in one directory are one file, and
; the installer would write the command line over the window. VS Code solves it the same
; way, which is why so many PATHs carry a "...\Microsoft VS Code\bin".
;
; Only HKCU\Environment is touched, which is the per-user PATH and matches a per-user
; install; the machine PATH is none of an installer's business when it never asked for
; administrator rights.
;
; Everything below is written twice, once for the installer and once for the uninstaller,
; from one macro - NSIS uninstaller functions live in a separate namespace and must carry
; the `un.` prefix. Compiling the same source twice is the standard way to avoid keeping
; two copies of the logic honest by hand.

!include "LogicLib.nsh"
!include "WinMessages.nsh"

; THE RULE, learned the hard way: never write a PATH that was not read in full.
;
; A standard NSIS build holds strings in NSIS_MAX_STRLEN (1024) byte buffers. Measured
; against a real 2914-character user PATH, ReadRegStr does not truncate it - it yields an
; empty string and sets the error flag, which is character for character what it does for a
; value that does not exist at all. The two cases cannot be told apart. An earlier version
; of this file read that empty string as "this user has no PATH yet" and wrote $INSTDIR as
; the entire value, which is how a working machine loses its PATH.
;
; So an empty or failed read means: leave it alone. That costs a user with a very long PATH
; one convenience - typing `mytimeoff` instead of its full path - and it costs a user with
; no PATH value the same. Writing instead costs them every other program on their PATH.
; The trade is not close.
;
; The second way to write a string nobody read is to build one: StrCpy truncates silently
; at the same limit, so "$R1;$R0" quietly loses the tail when the two together are too long.
; PathAppendEntry checks the combined length before it concatenates, not after.
!define PATH_LIMIT 1000

!macro MyTimeOffPathFunctions UN

; Reads the user PATH into $0 and sets $1 to "ok", or leaves $0 empty and $1 "unreadable".
;
; "unreadable" covers both an absent value and one too long for NSIS to hold - see THE RULE
; above for why this function refuses to distinguish them.
Function ${UN}ReadUserPath
  ClearErrors
  ReadRegStr $0 HKCU "Environment" "Path"
  ${If} ${Errors}
  ${OrIf} $0 == ""
    StrCpy $0 ""
    StrCpy $1 "unreadable"
  ${Else}
    StrCpy $1 "ok"
  ${EndIf}
FunctionEnd

; Is $R0 (a directory) already one of the entries in $R1 (a PATH)? Leaves "1" or "0" in $R2.
;
; Both sides are wrapped in semicolons before the search, so `C:\Foo` never matches inside
; `C:\Foobar`. StrCmp is case-insensitive, which is what Windows paths want.
Function ${UN}PathHasEntry
  StrCpy $R3 ";$R1;"   ; haystack
  StrCpy $R4 ";$R0;"   ; needle
  StrLen $R5 $R4
  StrLen $R6 $R3
  StrCpy $R7 0
  StrCpy $R2 "0"
  ${Do}
    IntOp $R8 $R7 + $R5
    ${If} $R8 > $R6
      ${Break}
    ${EndIf}
    StrCpy $R9 $R3 $R5 $R7
    ${If} $R9 == $R4
      StrCpy $R2 "1"
      ${Break}
    ${EndIf}
    IntOp $R7 $R7 + 1
  ${Loop}
FunctionEnd

; Works out the PATH to write when adding $R0 (a directory) to $R1 (a PATH), leaving it in
; $R2 - or leaves $R2 empty to mean "write nothing".
;
; Every reason to not write collapses to that one empty answer, because the caller has only
; one decision to make. The reasons are: the entry is already there, the result would be
; long enough for StrCpy to truncate it, or the PATH came back empty and so may never be
; written over (THE RULE). That last case is why an empty $R1 yields an empty $R2 rather
; than $R0 on its own: $R0 on its own is precisely the shape of the bug this guards.
Function ${UN}PathAppendEntry
  ${If} $R1 == ""
    StrCpy $R2 ""
    Return
  ${EndIf}
  Call ${UN}PathHasEntry
  ${If} $R2 == "1"
    StrCpy $R2 ""
    Return
  ${EndIf}
  StrLen $R3 $R1
  StrLen $R4 $R0
  IntOp $R5 $R3 + $R4
  IntOp $R5 $R5 + 1          ; the separator
  ${If} $R5 >= ${PATH_LIMIT}
    StrCpy $R2 ""
    Return
  ${EndIf}
  StrCpy $R2 "$R1;$R0"
FunctionEnd

; Removes every entry equal to $R0 from the PATH in $R1, leaving the result in $R2.
;
; Rebuilt from the entries rather than cut out of the string: splicing around a match is
; where stray semicolons and half-deleted entries come from. Empty entries are dropped,
; which also tidies the trailing semicolon a lot of PATHs carry. The result is never longer
; than the input, so it needs no length guard of its own.
Function ${UN}PathRemoveEntry
  StrCpy $R2 ""       ; what has been kept so far
  StrCpy $R3 "$R1;"   ; what is left to read, always semicolon-terminated
  ${Do}
    ${If} $R3 == ""
      ${Break}
    ${EndIf}
    ; $R4 takes everything up to the next semicolon; $R3 keeps the rest.
    StrCpy $R4 ""
    StrLen $R5 $R3
    StrCpy $R6 0
    ${Do}
      ${If} $R6 >= $R5
        ${Break}
      ${EndIf}
      StrCpy $R7 $R3 1 $R6
      ${If} $R7 == ";"
        ${Break}
      ${EndIf}
      StrCpy $R4 "$R4$R7"
      IntOp $R6 $R6 + 1
    ${Loop}
    IntOp $R6 $R6 + 1
    StrCpy $R3 $R3 "" $R6
    ${If} $R4 != ""
    ${AndIf} $R4 != $R0
      ${If} $R2 == ""
        StrCpy $R2 "$R4"
      ${Else}
        StrCpy $R2 "$R2;$R4"
      ${EndIf}
    ${EndIf}
  ${Loop}
FunctionEnd

; Tells every running program that the environment changed. Without this the new PATH only
; reaches terminals opened after the next sign-in.
Function ${UN}BroadcastEnvironmentChange
  SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000
FunctionEnd

!macroend

!insertmacro MyTimeOffPathFunctions ""
!insertmacro MyTimeOffPathFunctions "un."

!macro NSIS_HOOK_POSTINSTALL
  Call ReadUserPath
  ${If} $1 == "unreadable"
    DetailPrint "Left your PATH alone - run mytimeoff from $INSTDIR\bin."
  ${Else}
    StrCpy $R0 "$INSTDIR\bin"
    StrCpy $R1 "$0"
    Call PathAppendEntry
    ${If} $R2 != ""
      DetailPrint "Adding $INSTDIR\bin to your PATH so mytimeoff works in a terminal"
      ; Expand, not plain: a PATH that contains %USERPROFILE% must keep meaning it.
      WriteRegExpandStr HKCU "Environment" "Path" "$R2"
      Call BroadcastEnvironmentChange
    ${EndIf}
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  Call un.ReadUserPath
  ${If} $1 != "unreadable"
    StrCpy $R0 "$INSTDIR\bin"
    StrCpy $R1 "$0"
    Call un.PathHasEntry
    ${If} $R2 == "1"
      DetailPrint "Removing $INSTDIR\bin from your PATH"
      StrCpy $R0 "$INSTDIR\bin"
      StrCpy $R1 "$0"
      Call un.PathRemoveEntry
      WriteRegExpandStr HKCU "Environment" "Path" "$R2"
      Call un.BroadcastEnvironmentChange
    ${EndIf}
  ${EndIf}
!macroend
