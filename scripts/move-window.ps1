param(
  [int]$X = 300,
  [int]$Y = 200,
  [int]$Width = 480,
  [int]$Height = 360
)

Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class WindowMover {
  public delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr lParam);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowTextW(IntPtr hWnd, StringBuilder text, int max);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr hWnd, IntPtr after, int x, int y, int cx, int cy, uint flags);

  public static IntPtr Find(uint pid, string title) {
    IntPtr found = IntPtr.Zero;
    EnumWindows((h, l) => {
      uint windowPid;
      GetWindowThreadProcessId(h, out windowPid);
      if (windowPid == pid && IsWindowVisible(h)) {
        var text = new StringBuilder(512);
        GetWindowTextW(h, text, 512);
        if (text.ToString() == title) {
          found = h;
          return false;
        }
      }
      return true;
    }, IntPtr.Zero);
    return found;
  }

  public static string Move(uint pid, string title, int x, int y, int w, int h) {
    IntPtr handle = Find(pid, title);
    if (handle == IntPtr.Zero) return "not found";
    bool ok = SetWindowPos(handle, IntPtr.Zero, x, y, w, h, 0x0004 | 0x0010);
    return ok ? "moved" : "failed";
  }
}
"@

$process = Get-Process flashnote -ErrorAction SilentlyContinue
if (-not $process) {
  Write-Error "process not found"
  exit 1
}
$title = [string][char]0x95EA + [char]0x8BB0
Write-Output ([WindowMover]::Move([uint32]$process.Id, $title, $X, $Y, $Width, $Height))
