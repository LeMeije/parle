// Formatters shared between the main window's views and the dictation bar.

/// Audio-clock time as m:ss. Used for elapsed recording time and for the
/// timestamp an inserted mark is pinned to, which must read identically in
/// the bar and in Compose.
export function fmtTime(ms: number): string {
  const mm = Math.floor(ms / 60000);
  const ss = Math.floor((ms % 60000) / 1000);
  return `${mm}:${ss.toString().padStart(2, '0')}`;
}
