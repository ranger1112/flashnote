@echo off
rem Loads the Rust and MSVC environment for build.cmd / dev.cmd / cargo.cmd.
rem Called with `call` from those scripts, so do NOT setlocal here: the
rem variables have to survive in the caller's scope.
rem
rem Machine-specific setups can drop a scripts\_env.local.cmd (gitignored)
rem that sets VCVARS / RUSTUP_HOME / CARGO_HOME before the defaults below.
rem
rem Keep this file ASCII-only: cmd.exe reads batch files in the console code
rem page (GBK on zh-CN Windows), so UTF-8 comments break parsing.

if exist "%~dp0_env.local.cmd" call "%~dp0_env.local.cmd"

if not defined RUSTUP_HOME set "RUSTUP_HOME=%USERPROFILE%\.rustup"
if not defined CARGO_HOME set "CARGO_HOME=%USERPROFILE%\.cargo"
set "PATH=%CARGO_HOME%\bin;%PATH%"

if not defined VCVARS call :detect

if not defined VCVARS (
  echo [flashnote] vcvars64.bat not found. 1>&2
  echo [flashnote] Install "Visual Studio Build Tools 2022" with the "Desktop development 1>&2
  echo [flashnote] with C++" workload, then use the default install path or set VCVARS 1>&2
  echo [flashnote] to the full path of vcvars64.bat, or drop it in scripts\_env.local.cmd. 1>&2
  exit /b 1
)

call "%VCVARS%" >nul
if errorlevel 1 (
  echo [flashnote] failed to load MSVC environment from %VCVARS% 1>&2
  exit /b 1
)
exit /b 0

:detect
if defined VSINSTALLDIR set "VCVARS=%VSINSTALLDIR%VC\Auxiliary\Build\vcvars64.bat"
if defined VCVARS goto :eof

rem vswhere only reports installs it registered; the default paths are checked below.
set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
if exist "%VSWHERE%" call :detect_vswhere
if defined VCVARS goto :eof

for %%r in (
  "%ProgramFiles(x86)%\Microsoft Visual Studio\2022\BuildTools"
  "%ProgramFiles%\Microsoft Visual Studio\2022\BuildTools"
  "%ProgramFiles(x86)%\Microsoft Visual Studio\2022\Community"
  "%ProgramFiles%\Microsoft Visual Studio\2022\Community"
  "%ProgramFiles(x86)%\Microsoft Visual Studio\2022\Professional"
  "%ProgramFiles%\Microsoft Visual Studio\2022\Professional"
  "%ProgramFiles(x86)%\Microsoft Visual Studio\2022\Enterprise"
  "%ProgramFiles%\Microsoft Visual Studio\2022\Enterprise"
) do if not defined VCVARS if exist "%%~r\VC\Auxiliary\Build\vcvars64.bat" set "VCVARS=%%~r\VC\Auxiliary\Build\vcvars64.bat"
goto :eof

:detect_vswhere
for /f "usebackq delims=" %%i in (`"%VSWHERE%" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set "VCVARS=%%i\VC\Auxiliary\Build\vcvars64.bat"
goto :eof
