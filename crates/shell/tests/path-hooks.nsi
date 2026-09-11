; Exercises the PATH functions in ../installer-hooks.nsh.
;
; Editing someone's PATH is the most destructive thing this project does, and the bundle
; build that would otherwise exercise it takes an hour and a half - long enough that the
; logic would never get tested. This compiles the same file in a second.
;
; Run it with scripts/test-installer-hooks.ps1.

; Absolute, because NSIS resolves a relative OutFile against the script - which would
; drop a built executable into the source tree. scripts/test-installer-hooks.ps1 passes one.
!ifndef OUTPUT
  !error "compile this through scripts/test-installer-hooks.ps1, which sets OUTPUT"
!endif
OutFile "${OUTPUT}"
RequestExecutionLevel user
SilentInstall silent
InstallDir "$TEMP\mytimeoff-nsis-test"

!include "${__FILEDIR__}\..\installer-hooks.nsh"

Var LOG

!macro HAS name path dir expected
  StrCpy $R0 "${dir}"
  StrCpy $R1 "${path}"
  Call PathHasEntry
  ${If} $R2 == "${expected}"
    FileWrite $LOG "ok   has/${name}$\r$\n"
  ${Else}
    FileWrite $LOG "FAIL has/${name}: got [$R2] want [${expected}]$\r$\n"
  ${EndIf}
!macroend

!macro APPEND name path dir expected
  StrCpy $R0 "${dir}"
  StrCpy $R1 "${path}"
  Call PathAppendEntry
  ${If} $R2 == "${expected}"
    FileWrite $LOG "ok   append/${name}$\r$\n"
  ${Else}
    FileWrite $LOG "FAIL append/${name}: got [$R2] want [${expected}]$\r$\n"
  ${EndIf}
!macroend

!macro REMOVE name path dir expected
  StrCpy $R0 "${dir}"
  StrCpy $R1 "${path}"
  Call PathRemoveEntry
  ${If} $R2 == "${expected}"
    FileWrite $LOG "ok   remove/${name}$\r$\n"
  ${Else}
    FileWrite $LOG "FAIL remove/${name}: got [$R2] want [${expected}]$\r$\n"
  ${EndIf}
!macroend

Section
  FileOpen $LOG "$EXEDIR\result.txt" w

  !insertmacro HAS "plain"        "C:\a;C:\b"            "C:\b"      "1"
  !insertmacro HAS "case"         "C:\a;C:\b"            "C:\B"      "1"
  !insertmacro HAS "prefix-only"  "C:\Foobar"            "C:\Foo"    "0"
  !insertmacro HAS "suffix-only"  "C:\barFoo"            "C:\Foo"    "0"
  !insertmacro HAS "empty-path"   ""                     "C:\a"      "0"
  !insertmacro HAS "middle"       "C:\a;C:\app;C:\b"     "C:\app"    "1"
  !insertmacro HAS "first"        "C:\app;C:\b"          "C:\app"    "1"
  !insertmacro HAS "trailing-sep" "C:\a;C:\app;"         "C:\app"    "1"
  !insertmacro HAS "absent"       "C:\a;C:\b"            "C:\app"    "0"

  ; The one that matters: an empty PATH is either a PATH too long for NSIS to read or a
  ; PATH that is not there, and neither may be written over. Writing $INSTDIR on its own
  ; here is the bug that cost a real machine its PATH.
  !insertmacro APPEND "empty-path-refused"   ""              "C:\app"  ""
  !insertmacro APPEND "adds-to-a-real-path"  "C:\a;C:\b"     "C:\app"  "C:\a;C:\b;C:\app"
  !insertmacro APPEND "single-entry"         "C:\a"          "C:\app"  "C:\a;C:\app"
  !insertmacro APPEND "already-present"      "C:\a;C:\app"   "C:\app"  ""
  !insertmacro APPEND "already-present-case" "C:\a;C:\APP"   "C:\app"  ""
  !insertmacro APPEND "spaces"  "C:\Program Files\q"  "C:\app"  "C:\Program Files\q;C:\app"
  ; A trailing semicolon is left as an empty entry rather than tidied away. Windows ignores
  ; empty entries, and rebuilding the PATH to drop one would mean rewriting entries the
  ; user never asked this installer to touch.
  !insertmacro APPEND "trailing-sep"         "C:\a;"         "C:\app"  "C:\a;;C:\app"

  ; A PATH near NSIS_MAX_STRLEN must be left alone: StrCpy would truncate "$R1;$R0" and
  ; write back a PATH missing its tail. Built here rather than spelled out as a literal so
  ; the case stays pinned to PATH_LIMIT instead of to a count of characters.
  StrCpy $1 "C:\padding\directory\with\a\name\of\some\length\to\it"
  StrCpy $2 ""
  ${Do}
    StrLen $3 $2
    ${If} $3 >= 960
      ${Break}
    ${EndIf}
    ${If} $2 == ""
      StrCpy $2 "$1"
    ${Else}
      StrCpy $2 "$2;$1"
    ${EndIf}
  ${Loop}
  StrCpy $R0 "C:\Users\someone\AppData\Local\MyTimeOff"
  StrCpy $R1 "$2"
  Call PathAppendEntry
  ${If} $R2 == ""
    FileWrite $LOG "ok   append/too-long-to-build$\r$\n"
  ${Else}
    StrLen $4 $R2
    FileWrite $LOG "FAIL append/too-long-to-build: wrote a $4 character PATH$\r$\n"
  ${EndIf}

  ; ...but a PATH with room to spare is still appended to, so the guard above stays a limit
  ; rather than an excuse to never touch a long PATH.
  StrCpy $R1 "$2"
  StrCpy $R0 "C:\z"
  Call PathAppendEntry
  ${If} $R2 != ""
    FileWrite $LOG "ok   append/long-but-fits$\r$\n"
  ${Else}
    FileWrite $LOG "FAIL append/long-but-fits: refused a PATH that had room$\r$\n"
  ${EndIf}

  !insertmacro REMOVE "middle"      "C:\a;C:\x;C:\b"  "C:\x"  "C:\a;C:\b"
  !insertmacro REMOVE "only"        "C:\x"            "C:\x"  ""
  !insertmacro REMOVE "last"        "C:\a;C:\x"       "C:\x"  "C:\a"
  !insertmacro REMOVE "first"       "C:\x;C:\a"       "C:\x"  "C:\a"
  !insertmacro REMOVE "case"        "C:\a;C:\X"       "C:\x"  "C:\a"
  !insertmacro REMOVE "twice"       "C:\x;C:\a;C:\x"  "C:\x"  "C:\a"
  !insertmacro REMOVE "empties"     "C:\a;;C:\b;"     "C:\z"  "C:\a;C:\b"
  !insertmacro REMOVE "absent"      "C:\a;C:\b"       "C:\z"  "C:\a;C:\b"
  !insertmacro REMOVE "spaces"      "C:\a;C:\Program Files\q;C:\b"  "C:\Program Files\q"  "C:\a;C:\b"
  !insertmacro REMOVE "everything"  "C:\x"            "C:\X"  ""

  FileWrite $LOG "done$\r$\n"
  FileClose $LOG
SectionEnd

Section "Uninstall"
  ; Present only so the un. copies of the functions compile.
SectionEnd
