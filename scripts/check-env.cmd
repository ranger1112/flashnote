@echo off
setlocal
call "%~dp0_env.cmd" || exit /b 1

where link >nul 2>&1 || (
  >&2 echo [flashnote] link.exe not found: MSVC toolchain is not on PATH
  exit /b 1
)
where cargo >nul 2>&1 || (
  >&2 echo [flashnote] cargo not found: check CARGO_HOME or the rustup installation
  exit /b 1
)

echo INCLUDE=%INCLUDE%
echo LIB=%LIB%
echo [flashnote] environment ready
exit /b 0
