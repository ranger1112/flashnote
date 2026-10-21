@echo off
set "PATH=C:\Program Files (x86)\Microsoft Visual Studio\Installer;%PATH%"
call "D:\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
where link
echo INCLUDE=%INCLUDE%
echo LIB=%LIB%
