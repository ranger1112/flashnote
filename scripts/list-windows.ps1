Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class WinList {
  public delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr lParam);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowTextW(IntPtr hWnd, StringBuilder text, int max);

  public static string ForPid(uint target) {
    var sb = new StringBuilder();
    EnumWindows((h, l) => {
      uint pid;
      GetWindowThreadProcessId(h, out pid);
      if (pid == target) {
        var text = new StringBuilder(512);
        GetWindowTextW(h, text, 512);
        sb.AppendLine("hwnd=" + h + " visible=" + IsWindowVisible(h) + " title='" + text + "'");
      }
      return true;
    }, IntPtr.Zero);
    return sb.ToString();
  }
}
"@

$process = Get-Process flashnote -ErrorAction SilentlyContinue
if (-not $process) {
  Write-Output "no process"
  exit 0
}
Write-Output ("pid=" + $process.Id)
Write-Output ([WinList]::ForPid([uint32]$process.Id))
