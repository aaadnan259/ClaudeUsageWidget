@echo off
rem Sets up Claude Usage Widget as an app. Safe to run again (repairs the install).
cd /d "%~dp0"
title Installing Claude Usage Widget

if not exist ".venv\Scripts\python.exe" (
  echo Creating the widget's private Python environment...
  py -3 -m venv .venv 2>nul || python -m venv .venv
  if errorlevel 1 goto :fail
)

echo Installing packages...
".venv\Scripts\python.exe" -m pip install --disable-pip-version-check -q -r requirements.txt
if errorlevel 1 goto :fail

".venv\Scripts\python.exe" install.py
if errorlevel 1 goto :fail

echo.
echo Starting the widget...
start "" ".venv\Scripts\pythonw.exe" "%~dp0claude_usage_widget.pyw"
timeout /t 4 >nul
exit /b 0

:fail
echo.
echo Setup failed. Make sure Python 3.11+ is installed from python.org, then run this again.
pause
exit /b 1
