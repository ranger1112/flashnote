@echo off
setlocal
call "%~dp0_env.cmd" || exit /b 1
rem 允许从仓库任意位置调用：统一切到 src-tauri 再执行
cd /d "%~dp0..\src-tauri"
cargo %*
