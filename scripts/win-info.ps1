$process = Get-Process flashnote -ErrorAction SilentlyContinue
if (-not $process) {
  Write-Output "no process"
  exit 0
}

Write-Output ("pid=" + $process.Id)
Write-Output ("handle=" + $process.MainWindowHandle)
Write-Output ("title=" + $process.MainWindowTitle)

Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class WinEnum {
  public delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr lParam);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
  [DllImport("user32.dll")] public static extern int GetWindowTextW(IntPtr hWnd, StringBuilder text, int max);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
}
"@

$script:targetPid = $process.Id
$callback = {
  param([IntPtr]$handle, [IntPtr]$lparam)
  [uint32]$windowPid = 0
  [void][WinEnum]::GetWindowThreadProcessId($handle, [ref]$windowPid)
  if ($windowPid -eq $script:targetPid) {
    $builder = New-Object System.Text.StringBuilder 512
    [void][WinEnum]::GetWindowTextW($handle, $builder, 512)
    Write-Output ("hwnd={0} visible={1} title='{2}'" -f $handle, [WinEnum]::IsWindowVisible($handle), $builder.ToString())
  }
  return $true
}

[void][WinEnum]::EnumWindows($callback, [IntPtr]::Zero)
