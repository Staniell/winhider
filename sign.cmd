echo.
echo.  Use --help/-h for help
@echo off
setlocal
REM Use --pass "<yourpass>" or -p "<yourpass>" for password (REQUIRED)
REM Use --signinstaller or -i to sign installer(s) ONLY (Misc\output\*.exe)
REM Use --signall or -a to sign installer(s) BOTH (Misc\output\*.exe) and (Build\bin\Release\*.exe)
REM For no arguments it signs (Build\bin\Release\*.exe)
REM Example : sign --signall --pass mypassword

REM Change to the directory where the script is located
cd /d "%~dp0"

REM Path to certificate (relative to script location)
set "SIGNTOOL=signtool.exe"
set "CERT=Misc\BitmutexCert.pfx"
set "PASSWORD="

REM Directories
set "TARGET_DIR=target\x86_64-pc-windows-msvc\release"
set "INSTALLER_DIR=Misc\output"

REM Flags
set "SIGN_INSTALLER_ONLY=0"
set "SIGN_ALL=0"

REM Parse command line arguments
:parse_args
if "%~1"=="" goto done_parse

if "%~1"=="--help" (
    goto show_help
)
if "%~1"=="-h" (
    goto show_help
)
if "%~1"=="--pass" (
    set "PASSWORD=%~2"
    shift
    shift
    goto parse_args
)
if "%~1"=="-p" (
    set "PASSWORD=%~2"
    shift
    shift
    goto parse_args
)
if "%~1"=="--signinstaller" (
    set "SIGN_INSTALLER_ONLY=1"
    shift
    goto parse_args
)
if "%~1"=="-i" (
    set "SIGN_INSTALLER_ONLY=1"
    shift
    goto parse_args
)
if "%~1"=="--signall" (
    set "SIGN_ALL=1"
    shift
    goto parse_args
)
if "%~1"=="-a" (
    set "SIGN_ALL=1"
    shift
    goto parse_args
)
shift
goto parse_args

:done_parse

REM Validate password is provided
if "%PASSWORD%"=="" (
    echo ERROR: Password is required. Use --pass "<yourpass>" or -p "<yourpass>"
    echo Run sign.cmd --help for usage information.
    EXIT /B 1
)

REM Timestamp server (HTTPS)
set "TIMESTAMP=https://timestamp.comodoca.com/authenticode"

if "%SIGN_ALL%"=="1" (
    echo Signing all files in both %TARGET_DIR% and %INSTALLER_DIR%...

    for %%F in ("%TARGET_DIR%\*.exe" "%TARGET_DIR%\*.dll") do (
        echo Signing %%~nxF
        "%SIGNTOOL%" sign /f "%CERT%" /p "%PASSWORD%" /t "%TIMESTAMP%" /fd sha256 "%%~F"
    )
    for %%F in ("%INSTALLER_DIR%\*.exe") do (
        echo Signing installer %%~nxF
        "%SIGNTOOL%" sign /f "%CERT%" /p "%PASSWORD%" /t "%TIMESTAMP%" /fd sha256 "%%~F"
    )

) else if "%SIGN_INSTALLER_ONLY%"=="1" (
    echo Signing installer .exe files in %INSTALLER_DIR%...
    for %%F in ("%INSTALLER_DIR%\*.exe") do (
        echo Signing %%~nxF
        "%SIGNTOOL%" sign /f "%CERT%" /p "%PASSWORD%" /t "%TIMESTAMP%" /fd sha256 "%%~F"
    )
) else (
    echo Signing all .exe and .dll files in %TARGET_DIR%...
    for %%F in ("%TARGET_DIR%\*.exe" "%TARGET_DIR%\winhider_payload.dll") do (
        echo Signing %%~nxF
        "%SIGNTOOL%" sign /f "%CERT%" /p "%PASSWORD%" /t "%TIMESTAMP%" /fd sha256 "%%~F"
    )
)

if "%errorlevel%"=="0" (
    echo All files signed successfully.
) else (
    echo ERROR: One or more files could not be signed.
    echo Exit code: %errorlevel%
)

PAUSE
EXIT /B %errorlevel%

:show_help
echo.
echo Usage:
echo   --pass "<yourpass>" or -p "<yourpass>"     Password for certificate (REQUIRED)
echo   --signinstaller or -i                      Sign installer(s) ONLY (Misc\output\*.exe)
echo   --signall or -a                            Sign both installers and release files
echo   --help or -h                               Show this help message and exit
echo.
echo   Password is REQUIRED for all signing operations.
echo.
exit /b 0
