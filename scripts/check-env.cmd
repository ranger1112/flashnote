@echo off
setlocal
call "%~dp0_env.cmd" || exit /b 1
where link
where cargo
echo INCLUDE=%INCLUDE%
echo LIB=%LIB%
