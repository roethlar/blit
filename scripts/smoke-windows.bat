@echo off
REM Smoke Test Script for Windows
REM This script tests daemon push/pull operations on Windows using current CLI syntax.

setlocal EnableDelayedExpansion

@echo Starting Blit Smoke Test on Windows

REM Build the project in release mode
cargo build --release

REM Use random temp directory to avoid conflicts
set TMPDIR=%TEMP%\blit_test_%RANDOM%
set SRC=!TMPDIR!\src
set DST=!TMPDIR!\dst
set PULL=!TMPDIR!\pull

REM Create test directories
mkdir "!SRC!" "!DST!" "!PULL!"

REM Create test files
echo Test content > "!SRC!\a.txt"
mkdir "!SRC!\sub"
echo More test content > "!SRC!\sub\b.txt"

REM Start first daemon for push test
set PORT=9031
@echo Starting daemon on port %PORT% for push test...
start /B "" target\release\blit.exe daemon --root "!DST!" --port %PORT%
REM Wait for daemon to start
timeout /t 2 /nobreak >nul

@echo Pushing files to daemon...
target\release\blit.exe "!SRC!" "blit://127.0.0.1:%PORT%/" --mir -v

REM Start second daemon for pull test  
set PORT2=9032
@echo Starting daemon on port %PORT2% for pull test...
start /B "" target\release\blit.exe daemon --root "!SRC!" --port %PORT2%
timeout /t 2 /nobreak >nul

@echo Pulling files from daemon...
target\release\blit.exe "blit://127.0.0.1:%PORT2%/" "!PULL!" --mir -v

REM Verify transfer integrity - Push test
if exist "!DST!\a.txt" (
    if exist "!DST!\sub\b.txt" (
        @echo Push test passed: Files transferred correctly.
    ) else (
        @echo Push test failed: sub\b.txt missing.
        exit /b 1
    )
) else (
    @echo Push test failed: a.txt missing.
    exit /b 1
)

REM Verify transfer integrity - Pull test
if exist "!PULL!\a.txt" (
    if exist "!PULL!\sub\b.txt" (
        @echo Pull test passed: Files transferred correctly.
    ) else (
        @echo Pull test failed: sub\b.txt missing.
        exit /b 1
    )
) else (
    @echo Pull test failed: a.txt missing.
    exit /b 1
)

REM Cleanup
@echo Cleaning up...
taskkill /IM blit.exe /F >nul 2>&1
rmdir /S /Q "!TMPDIR!" >nul 2>&1

@echo Blit Smoke Test on Windows completed successfully.
endlocal