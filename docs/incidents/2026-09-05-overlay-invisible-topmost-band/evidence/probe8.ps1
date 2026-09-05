$src = @"
using System;
using System.Runtime.InteropServices;
public class T {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint f);
  [DllImport("user32.dll")] public static extern IntPtr GetWindowLongPtr(IntPtr h, int i);
}
"@
Add-Type -TypeDefinition $src -ErrorAction Stop
[void][T]::SetProcessDPIAware()
Add-Type -AssemblyName System.Drawing
$dir = "C:\Users\Benjamin\AppData\Local\Temp\claude\C--Users-Benjamin-Documents-Programming\14570267-2687-4aa3-a1c9-6e72d912aa17\scratchpad"
$hud = [IntPtr][Convert]::ToInt64("1541136", 16)

function Grab($p) {
  $b = New-Object System.Drawing.Bitmap 848, 304
  $g = [System.Drawing.Graphics]::FromImage($b)
  $g.CopyFromScreen(1016, 1384, 0, 0, (New-Object System.Drawing.Size 848, 304))
  $g.Dispose(); $b.Save($p, [System.Drawing.Imaging.ImageFormat]::Png); $b.Dispose()
}

"ex-style now: 0x{0:X8}  (TOPMOST bit set = {1})" -f `
  [T]::GetWindowLongPtr($hud,-20).ToInt64(), (([T]::GetWindowLongPtr($hud,-20).ToInt64() -band 0x8) -ne 0)

# Step 1: just show it, as the app's own hud.show() does
[void][T]::ShowWindow($hud, 4); Start-Sleep -Milliseconds 1000
Grab "$dir\fix_a_show_only.png"

# Step 2: re-assert the topmost BAND via SetWindowPos (the proposed fix)
$HWND_TOPMOST = [IntPtr](-1)
$SWP = 0x0001 -bor 0x0002 -bor 0x0010   # NOSIZE|NOMOVE|NOACTIVATE
$ok = [T]::SetWindowPos($hud, $HWND_TOPMOST, 0,0,0,0, $SWP)
"SetWindowPos(HWND_TOPMOST) = {0}" -f $ok
Start-Sleep -Milliseconds 1000
Grab "$dir\fix_b_after_setwindowpos.png"

# restore resting state
[void][T]::ShowWindow($hud, 0)
"restored hidden"
