Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class WinState {
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern int GetWindowTextW(IntPtr hWnd, StringBuilder text, int max);
}
"@

$process = Get-Process flashnote -ErrorAction SilentlyContinue
if (-not $process) {
  Write-Output "no process"
  exit 0
}

$handle = $process.MainWindowHandle
Write-Output ("window_visible=" + [WinState]::IsWindowVisible($handle))

$foreground = [WinState]::GetForegroundWindow()
$builder = New-Object System.Text.StringBuilder 512
[void][WinState]::GetWindowTextW($foreground, $builder, 512)
Write-Output ("foreground_title=" + $builder.ToString())
