@echo off
REM KM5E's Base Camp v1.13.0 - Build Script for Windows
REM Requires Rust toolchain: https://rustup.rs/

echo ========================================
echo   Building KM5E's Base Camp v1.13.0
echo ========================================

cargo build --release

if %ERRORLEVEL% EQU 0 (
    echo.
    copy /Y "target\release\basecamp.exe" "target\release\basecamp-v1.13.0.exe" >nul
    echo Build successful!
    echo Binary: target\release\basecamp-v1.13.0.exe
    echo.
    echo Run with:  target\release\basecamp-v1.13.0.exe
) else (
    echo.
    echo Build FAILED. Check errors above.
    echo Make sure Rust is installed: https://rustup.rs/
)

pause
