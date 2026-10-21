@echo off
setlocal
call "%~dp0_env.cmd" || exit /b 1
cd /d "%~dp0.."
npm run tauri dev
