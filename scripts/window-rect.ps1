Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class WindowRectReader {
  public delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr lParam);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowTextW(IntPtr hWnd, StringBuilder text, int max);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }

  public static string Read(uint pid, string title) {
    string result = "not found";
    EnumWindows((h, l) => {
      uint windowPid;
      GetWindowThreadProcessId(h, out windowPid);
      if (windowPid == pid) {
        var text = new StringBuilder(512);
        GetWindowTextW(h, text, 512);
        if (text.ToString() == title) {
          RECT rect;
          GetWindowRect(h, out rect);
          result = "visible=" + IsWindowVisible(h) + " rect=" + rect.Left + "," + rect.Top + "," + (rect.Right - rect.Left) + "x" + (rect.Bottom - rect.Top);
          return false;
        }
      }
      return true;
    }, IntPtr.Zero);
    return result;
  }
}
"@

$process = Get-Process flashnote -ErrorAction SilentlyContinue
if (-not $process) {
  Write-Output "no process"
  exit 0
}
$title = [string][char]0x95EA + [char]0x8BB0
Write-Output ([WindowRectReader]::Read([uint32]$process.Id, $title))
