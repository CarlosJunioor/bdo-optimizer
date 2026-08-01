@echo off
:: Rebuild and relaunch BDO Optimizer. Double-click after any code change.
:: Self-elevates: the app runs as admin (PresentMon needs it), so killing and
:: relaunching it needs an elevated shell too.
net session >nul 2>&1 || (
  powershell -Command "Start-Process -FilePath '%~f0' -Verb RunAs"
  exit /b
)
cd /d "%~dp0"
taskkill /IM bdo-optimizer.exe /F >nul 2>&1
cargo build --release
if errorlevel 1 (
  echo.
  echo Build FAILED - fix the errors above.
  pause
  exit /b 1
)
copy /y "vendor\presentmon\PresentMon.exe" "target\release\PresentMon.exe" >nul
copy /y "vendor\nvidiaProfileInspector\nvidiaProfileInspector.exe" "target\release\nvidiaProfileInspector.exe" >nul
start "" "target\release\bdo-optimizer.exe"
