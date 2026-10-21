@echo off
setlocal
call "%~dp0_env.cmd" || exit /b 1
cargo %*
