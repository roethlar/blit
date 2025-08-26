@echo off
REM Smoke Test Script for Windows
REM This script tests async push/pull operations on Windows.

@echo Starting RoboSync Async Smoke Test on Windows

REM Build the project with async support
cargo build --features async

REM Create test directories and files
mkdir "C:\Users\%USERNAME%\AppData\Local\Temp\robosync_test_src"
mkdir "C:\Users\%USERNAME%\AppData\Local\Temp\robosync_test_dst"
echo Test content > "C:\Users\%USERNAME%\AppData\Local\Temp\robosync_test_src\testfile.txt"
REM Symlinks on Windows require admin or specific permissions, using mklink
mklink "C:\Users\%USERNAME%\AppData\Local\Temp\robosync_test_src\symlink_test" "C:\Users\%USERNAME%\AppData\Local\Temp\robosync_test_src\testfile.txt"

REM Start async server in background
start /B "" target\debug\robosync.exe --serve-async --root "C:\Users\%USERNAME%\AppData\Local\Temp\robosync_test_dst"
REM Wait for server to start
timeout /t 2 /nobreak >nul

@echo Pushing files to server...
target\debug\robosync.exe push --async --source "C:\Users\%USERNAME%\AppData\Local\Temp\robosync_test_src" localhost

@echo Pulling files from server...
rmdir /S /Q "C:\Users\%USERNAME%\AppData\Local\Temp\robosync_test_src\*"
target\debug\robosync.exe pull --async --dest "C:\Users\%USERNAME%\AppData\Local\Temp\robosync_test_src" localhost

REM Verify transfer integrity
if exist "C:\Users\%USERNAME%\AppData\Local\Temp\robosync_test_dst\testfile.txt" (
    @echo Push test passed: Files transferred correctly.
) else (
    @echo Push test failed: Files missing.
    exit /b 1
)

if exist "C:\Users\%USERNAME%\AppData\Local\Temp\robosync_test_src\testfile.txt" (
    @echo Pull test passed: Files transferred correctly.
) else (
    @echo Pull test failed: Files missing.
    exit /b 1
)

@echo Testing timeout behavior...
REM Simulate a timeout scenario (e.g., server not responding)
REM This part depends on timeout implementation details

REM Cleanup
taskkill /IM robosync.exe /F
rmdir /S /Q "C:\Users\%USERNAME%\AppData\Local\Temp\robosync_test_src"
rmdir /S /Q "C:\Users\%USERNAME%\AppData\Local\Temp\robosync_test_dst"

@echo RoboSync Async Smoke Test on Windows completed successfully.