@echo off
REM KM5E's Base Camp v1.21.0 - Build Script for Windows
REM Requires Rust toolchain: https://rustup.rs/

echo ========================================
echo   Building KM5E's Base Camp v1.21.0
echo ========================================

cargo build --release

if %ERRORLEVEL% EQU 0 (
    echo.
    copy /Y "target\release\basecamp.exe" "target\release\basecamp-v1.21.0.exe" >nul
    copy /Y "target\release\basecamp.exe" "basecamp-v1.21.0.exe" >nul
    echo Build successful!
    echo Binaries:
    echo   target\release\basecamp-v1.21.0.exe
    echo   basecamp-v1.21.0.exe  (same directory as build.bat)
    echo.
    echo Run with:  basecamp-v1.21.0.exe
) else (
    echo.
    echo Build FAILED. Check errors above.
    echo Make sure Rust is installed: https://rustup.rs/
)

pause
