$src = @"
using System;
using System.Runtime.InteropServices;
public class P {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
}
"@
Add-Type -TypeDefinition $src -ErrorAction Stop
[void][P]::SetProcessDPIAware()
Add-Type -AssemblyName System.Drawing
$dir = "C:\Users\Benjamin\AppData\Local\Temp\claude\C--Users-Benjamin-Documents-Programming\14570267-2687-4aa3-a1c9-6e72d912aa17\scratchpad"

function Snap($hex, $name, $show) {
  $h = [IntPtr][Convert]::ToInt64($hex, 16)
  if ($show) { [void][P]::ShowWindow($h, 4); Start-Sleep -Milliseconds 1200 }
  $r = New-Object P+RECT; [void][P]::GetWindowRect($h, [ref]$r)
  $w = $r.R - $r.L; $ht = $r.B - $r.T
  $bmp = New-Object System.Drawing.Bitmap $w, $ht
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $hdc = $g.GetHdc()
  $ok = [P]::PrintWindow($h, $hdc, 2)   # PW_RENDERFULLCONTENT
  $g.ReleaseHdc($hdc); $g.Dispose()

  # measure how much of the bitmap is non-blank
  $nonblank = 0; $total = 0; $colors = @{}
  for ($y = 0; $y -lt $ht; $y += 4) {
    for ($x = 0; $x -lt $w; $x += 4) {
      $c = $bmp.GetPixel($x, $y); $total++
      $k = "{0},{1},{2},{3}" -f $c.A,$c.R,$c.G,$c.B
      $colors[$k] = 1
      if ($c.A -ne 0 -and -not ($c.R -eq 0 -and $c.G -eq 0 -and $c.B -eq 0)) { $nonblank++ }
    }
  }
  "{0,-6} PrintWindow={1,-5} size={2}x{3} nonblank={4}/{5} ({6:P1}) distinctColors={7}" -f `
     $name, $ok, $w, $ht, $nonblank, $total, ($nonblank/[double]$total), $colors.Count
  $bmp.Save("$dir\pw_$name.png", [System.Drawing.Imaging.ImageFormat]::Png)
  $bmp.Dispose()
  if ($show) { [void][P]::ShowWindow($h, 0) }
}

Snap "1C6126A" "main" $false
Snap "1541136" "hud"  $true
