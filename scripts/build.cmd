@echo off
setlocal
set "RUSTUP_HOME=D:\dev\rust\rustup"
set "CARGO_HOME=D:\dev\rust\cargo"
set "PATH=C:\Program Files (x86)\Microsoft Visual Studio\Installer;%CARGO_HOME%\bin;%PATH%"
call "D:\BuildTools\VC\Auxiliary\Build\vcvars64.bat" >nul
cd /d D:\Code\flashnote
npm run tauri build
