$src = @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class Y {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern IntPtr GetWindowLongPtr(IntPtr h, int i);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int m);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int m);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint p);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
  [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out int v, int s);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
}
"@
Add-Type -TypeDefinition $src -ErrorAction Stop
[void][Y]::SetProcessDPIAware()

$hud = [IntPtr][Convert]::ToInt64("1541136", 16)
[void][Y]::ShowWindow($hud, 4); Start-Sleep -Milliseconds 1000

# collect ALL visible windows in z-order, no filtering inside the callback
$all = New-Object System.Collections.ArrayList
$cb = [Y+EnumProc]{ param($h,$l); [void]$all.Add($h); return $true }
[void][Y]::EnumWindows($cb, [IntPtr]::Zero)
"enumerated {0} top-level windows" -f $all.Count

$HL=1016; $HT=1384; $HR=1864; $HB=1688
$idx = 0
"`n===== visible windows overlapping HUD rect, TOP of z-order first ====="
foreach ($h in $all) {
  $idx++
  if (-not [Y]::IsWindowVisible($h)) { continue }
  $r = New-Object Y+RECT
  if (-not [Y]::GetWindowRect($h, [ref]$r)) { continue }
  if ($r.R -le $HL -or $r.L -ge $HR -or $r.B -le $HT -or $r.T -ge $HB) { continue }
  $ex = [Y]::GetWindowLongPtr($h, -20).ToInt64()
  $tt = New-Object Text.StringBuilder 200; [void][Y]::GetWindowText($h, $tt, 200)
  $cn = New-Object Text.StringBuilder 200; [void][Y]::GetClassName($h, $cn, 200)
  $p = 0; [void][Y]::GetWindowThreadProcessId($h, [ref]$p)
  $pn = try { (Get-Process -Id $p -ErrorAction Stop).ProcessName } catch { "?" }
  $cloak = 0; [void][Y]::DwmGetWindowAttribute($h, 14, [ref]$cloak, 4)
  $mark = if ($h -eq $hud) { "   <<<<<< THE HUD" } else { "" }
  "z{0,3} 0x{1:X8} TOPMOST={2,-5} cloaked={3} {4,-14} {5,-24} rect=({6},{7})-({8},{9}) '{10}'{11}" -f `
    $idx, $h.ToInt64(), (($ex -band 0x8) -ne 0), $cloak, $pn, $cn.ToString(), $r.L,$r.T,$r.R,$r.B, $tt.ToString(), $mark
}
[void][Y]::ShowWindow($hud, 0)
