$src = @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class W {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumChildWindows(IntPtr p, EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int m);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int m);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern IntPtr GetWindowLongPtr(IntPtr h, int i);
  [DllImport("user32.dll")] public static extern IntPtr MonitorFromWindow(IntPtr h, uint f);
  [DllImport("user32.dll")] public static extern bool GetLayeredWindowAttributes(IntPtr h, out uint key, out byte alpha, out uint flags);
  [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out int v, int s);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
}
"@
Add-Type -TypeDefinition $src -ErrorAction Stop

$targets = @{}
Get-Process | Where-Object { $_.ProcessName -match 'parle' } | ForEach-Object { $targets[[uint32]$_.Id] = $_.ProcessName }

$rows = New-Object System.Collections.ArrayList
$cb = [W+EnumProc]{
  param($h, $l)
  $pid2 = 0
  [void][W]::GetWindowThreadProcessId($h, [ref]$pid2)
  if ($targets.ContainsKey([uint32]$pid2)) {
    $cn = New-Object Text.StringBuilder 256; [void][W]::GetClassName($h, $cn, 256)
    $tt = New-Object Text.StringBuilder 256; [void][W]::GetWindowText($h, $tt, 256)
    $r = New-Object W+RECT; [void][W]::GetWindowRect($h, [ref]$r)
    $ex = [W]::GetWindowLongPtr($h, -20).ToInt64()
    $st = [W]::GetWindowLongPtr($h, -16).ToInt64()
    $mon = [W]::MonitorFromWindow($h, 0)   # MONITOR_DEFAULTTONULL
    $alpha = [byte]255; $key = [uint32]0; $flags = [uint32]0
    $hasLayered = [W]::GetLayeredWindowAttributes($h, [ref]$key, [ref]$alpha, [ref]$flags)
    $cloak = 0; [void][W]::DwmGetWindowAttribute($h, 14, [ref]$cloak, 4)
    [void]$rows.Add([pscustomobject]@{
      Proc=$targets[[uint32]$pid2]; PID=$pid2; HWND=("0x{0:X}" -f $h.ToInt64())
      Class=$cn.ToString(); Title=$tt.ToString()
      Visible=[W]::IsWindowVisible($h); Minimized=[W]::IsIconic($h)
      Rect=("L{0} T{1} R{2} B{3}" -f $r.L,$r.T,$r.R,$r.B)
      W=($r.R-$r.L); H=($r.B-$r.T)
      OnMonitor=($mon -ne [IntPtr]::Zero)
      ExStyle=("0x{0:X8}" -f $ex)
      TOPMOST=(($ex -band 0x8) -ne 0); NOACTIVATE=(($ex -band 0x8000000) -ne 0)
      TOOLWIN=(($ex -band 0x80) -ne 0); LAYERED=(($ex -band 0x80000) -ne 0)
      Alpha=$(if($hasLayered){$alpha}else{"n/a"})
      WS_VISIBLE=(($st -band 0x10000000) -ne 0)
      Cloaked=$cloak
    })
  }
  return $true
}
[void][W]::EnumWindows($cb, [IntPtr]::Zero)

"===== TOP-LEVEL WINDOWS OWNED BY PARLE ====="
$rows | Format-List

"===== MONITORS ====="
Add-Type -AssemblyName System.Windows.Forms
[System.Windows.Forms.Screen]::AllScreens | ForEach-Object {
  "{0} primary={1} bounds={2} working={3}" -f $_.DeviceName, $_.Primary, $_.Bounds, $_.WorkingArea
}
