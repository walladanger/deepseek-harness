@echo off
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0Manage.ps1" -Action RepairShortcut %*
if errorlevel 1 pause
