# Incident: the overlay goes invisible while dictation keeps working — SOLVED

**Date diagnosed:** 2026-09-05, ~08:00 local (UTC+7)
**Machine:** ASUS Zephyrus G14 2025, Windows 11, single 2880x1800 panel at 200% (DPI 192)
**Build in use:** `%LOCALAPPDATA%\Parle\parle.exe`, built 2026-08-31 17:29
**Process examined:** pid 86136, launched 2026-09-04 21:19:53, **still running and still broken
when the diagnosis was taken** — 10h35m uptime

> This is the follow-up to `2026-08-28-overlay-stops-appearing`, which could only rank candidates
> because the evidence had been destroyed by the restart that fixed it. This time the failing
> process was caught live and interrogated in place. **Candidate C was correct.**

---

## 1. Root cause

**The HUD window is evicted from the Windows topmost z-order *band*, while keeping its
`WS_EX_TOPMOST` style *bit*. The overlay is still shown, still positioned correctly and still
painting perfectly — it is just drawn underneath every window the user is typing into.**

The two things are not the same, and this is the whole bug:

- `WS_EX_TOPMOST` is a *style bit* readable with `GetWindowLongPtrW`.
- The *band* is the z-order partition Windows keeps topmost windows in. It is changed **only** by
  `SetWindowPos`.

`harden_overlay` (`platform/windows.rs`) sets the bit with `SetWindowLongPtrW`. Per Win32 that
writes the bit and does **not** move the window between bands. So the bit is permanently `1` and
permanently unable to tell you anything — including after Windows has demoted the window.

Nothing in Parle ever repairs it:

| Assertion | Where | How often |
|---|---|---|
| `always_on_top(true)` | `hud.rs` `create_hud`, builder | **once**, at startup |
| `WS_EX_TOPMOST` bit | `platform/windows.rs` `harden_overlay` | once, and ineffective for the band |
| `SetWindowPos(HWND_TOPMOST, …)` | **nowhere** — `grep` across `src-tauri` and `crates` | never |

`sync_hud`'s only action is `hud.show()`, which is `ShowWindow`, which does not touch z-order.
So once the band is lost it is lost for the life of the process, and **restarting is the only cure**
— exactly the behaviour reported both times.

Dictation is unaffected because the pipeline, transcription and injection never consult the HUD.
That is why the app looks healthy in every respect except the one the user can see.

## 2. How it was proven, on the live broken process

All measurements below come from pid 86136 *while it was failing*, before anything was restarted.

**a. The window is fine.** It exists, is the right size, and is exactly where `create_hud` computes:

```
hud rect = L1016 T1384 R1864 B1688   (848x304 physical = 424x152 at 2x)
expected  x = (1440-424)/2 = 508 -> 1016 physical   ✓
          y = 900-152-56  = 692 -> 1384 physical   ✓
MonitorFromWindow(MONITOR_DEFAULTTONULL) = non-null (on screen)   ✓
DWM cloaked = 0   ✓
```

That kills **candidate A** (window gone) and **candidate B** (stranded off-screen).

**b. The renderer is fine.** Both WebView2 renderer processes were alive from launch, and
`PrintWindow(PW_RENDERFULLCONTENT)` on the HUD returned a fully drawn pill — stop button,
waveform, the partial text `"Since you don't, you're not gonna ..."` and the `0:14` timer, left
over from the last dictation:

```
main   PrintWindow=True size=1986x1471 nonblank=97.8% distinctColors=1181
hud    PrintWindow=True size=848x304   nonblank=30.5% distinctColors=90
```

That kills **candidate E** (lost render surface). The webview never stopped working.

**c. The z-order is the fault.** Enumerating visible windows overlapping the HUD rect, top first:

```
z  9  ClickToDo   TOPMOST=True   'Click to Do'
z 23  claude      TOPMOST=False  'Claude'                <-- non-topmost, ABOVE the HUD
z 24  msedge      TOPMOST=False
z 27  SystemSettings TOPMOST=False
z 31  parle       TOPMOST=False  'Parle'   (its own main window, also above)
z 32  chrome      TOPMOST=False
z 35  explorer    TOPMOST=False
z 44  msedge      TOPMOST=False
z 47  PowerToys   TOPMOST=False
z 49  parle       TOPMOST=True   'Parle HUD'   <<<< the HUD, with the bit still set
```

A window in the topmost band can never sit below a non-topmost one. The HUD is at **z49 with
`TOPMOST=True`**, buried under eight non-topmost windows. The bit says topmost; the band says
otherwise. **This is the bug, stated as a single line of evidence.**

**d. The fix, applied live, repaired it without a restart.** Forcing the window visible reproduced
the symptom exactly — nothing on screen. One `SetWindowPos` then made it appear:

```
ShowWindow(SW_SHOWNOACTIVATE)            -> screen capture at HUD rect: nothing (bug reproduced)
SetWindowPos(HWND_TOPMOST,
             SWP_NOMOVE|NOSIZE|NOACTIVATE) = True
                                         -> screen capture: the pill, fully visible
re-enumerated z-order                    -> HUD moved from z49 to z9
```

Screenshots: `evidence/fix_a_show_only.png` (bug) and `evidence/fix_b_after_setwindowpos.png`
(repaired). `evidence/pw_hud.png` is the PrintWindow capture proving the renderer was always fine.

## 3. Secondary finding: the ex-style had been rewritten too

The live HUD's extended style was `0x00040118`. Decoded:

```
0x00040118 = WS_EX_APPWINDOW | WS_EX_WINDOWEDGE | WS_EX_ACCEPTFILES | WS_EX_TOPMOST
```

`harden_overlay` asks for `WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_TOOLWINDOW`. Two of those three
were **absent**, and `WS_EX_APPWINDOW` — the opposite of the `skip_taskbar(true)` the window was
built with — was present. The main window measured `0x00040110`: the HUD's style was, bit for bit,
a plain Tauri window plus the topmost bit.

`WS_EX_NOACTIVATE` is load-bearing: `hud.rs`'s own header comment calls it the detail that keeps
paste-at-cursor working. Losing it means the overlay can take focus.

**Not proven:** whether these bits were stripped later or never applied. `harden_overlay` runs
immediately after `.build()`, and TAO may apply its own window attributes afterwards; TAO also
implements `skip_taskbar` through `ITaskbarList`, not `WS_EX_TOOLWINDOW`, so the missing
`TOOLWINDOW` may never have been set at all. Distinguishing the two needs a measurement on a
freshly started process, which was deliberately not taken because it would have destroyed the live
failing state. The fix re-applies the bits on every show either way.

## 4. The trigger is still not identified — and the fix does not depend on it

The Windows System log for the whole 10h35m run contains **no sleep and no resume**
(no Kernel-Power 42 or 107). The overnight "sleep" theory carried over from the 2026-08-28 write-up
is therefore **wrong for this occurrence**. The only power events were three Kernel-Power **105**
(power-source change, AC<->battery) at 22:05, 07:20 and 07:55.

Plausible causes, none confirmed, all of which demote a topmost window on Windows:

- a full-screen exclusive app taking the display (DaVinci Resolve, the Screenbox video player and a
  YouTube tab were all open on this desktop),
- a GPU or display mode change — a power-source change on a hybrid-graphics laptop is a real
  candidate for this, and three of them occurred,
- another process forcing itself foreground, or a system overlay reordering the band. Note
  `ClickToDo` sits in the topmost band on this machine.

This does not need to be settled. The fix re-asserts the band on every show, so whichever of these
does it, the overlay repairs itself on the next dictation instead of staying broken until restart.

## 5. The fix

Two files, both surgical.

**`src-tauri/src/platform/windows.rs`** — new `reassert_overlay`, which does what
`harden_overlay` cannot:

```rust
pub fn reassert_overlay(window: &tauri::WebviewWindow) {
    harden_overlay(window);              // put the ex-style bits back
    // ... SetWindowPos(hwnd, HWND_TOPMOST, 0,0,0,0,
    //                  SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE)
}
```

`SWP_NOACTIVATE` matters: without it the re-assert would steal focus and break paste-at-cursor,
trading one bug for a worse one.

**`src-tauri/src/hud.rs`** — call it before every show, and log the show:

```rust
PipelineState::Recording | PipelineState::Transcribing => {
    #[cfg(target_os = "windows")]
    crate::platform::windows::reassert_overlay(&hud);
    let shown = hud.show();
    tracing::info!("hud: show for {:?} -> {:?}, pos {:?}", state, shown.is_ok(), hud.outer_position().ok());
}
```

The log line is the minimum version of §6.2 of the previous incident. It is what separates
"never asked to show" from "asked, and still not visible", which was unanswerable last time.

`cargo check -p parle --lib`: clean, 11 warnings, all the pre-existing dead-code ones.

## 5a. Does this affect macOS? No — and the asymmetry is the reason

Checked deliberately, because the structural weakness *is* shared: the macOS panel level is set in
`convert_to_panel`, which `create_hud` calls exactly once at startup, and nothing ever re-asserts
it. Same "assert once, never again" shape as Windows.

The mechanism does not transfer, and that is what decides it:

| | Windows | macOS |
|---|---|---|
| What marks the overlay | `WS_EX_TOPMOST`, a style **bit** | `NSWindow.level` = `PanelLevel::Status` (25), a **property** |
| What actually orders it | the topmost **band**, a separate thing | that same property — the window server reads it directly |
| Can the two disagree? | **Yes.** This bug. | **No.** There is no second representation to fall out of sync with |
| Restored by | `SetWindowPos` only | n/a — nothing to restore |

The Windows bug is specifically a *desynchronisation* between two representations of "topmost".
macOS has only one, so it cannot desynchronise. `set_level` is not something AppKit silently
revokes; a Status-level panel that ends up covered is covered because something legitimately sits
at an equal or higher level (a menu at `NSPopUpMenuWindowLevel`, the screen saver, Mission
Control), which is a different situation with a different signature — transient, and not cured by
a restart.

The genuine macOS ordering hazards are already handled in this codebase and have different
symptoms: `set_hides_on_deactivate(false)` (the documented NSPanel gotcha — an always-deactivated
overlay app would otherwise hide the HUD constantly), `can_join_all_spaces` +
`full_screen_auxiliary` (full-screen spaces), and the activation-policy note on
`set_regular_on_main`, whose failure mode is *every* window becoming unshowable, not the HUD alone
going invisible while dictation carries on.

**So no macOS change was made.** Adding a speculative `set_level` re-assert would be code for a
mechanism that does not exist on that platform, and it could not be exercised from the Windows
machine this was diagnosed on — untested code shipped to the platform that cannot test it. If the
Mac ever *does* show this symptom, that would be new information and the write-up above is the
method to re-run there; it would not be this root cause.

## 6. Deliberately NOT done

Listed so the next person does not assume they were considered and rejected silently:

- **Self-healing a missing HUD window** (previous doc §7.1). Measurement showed the window was
  present, so candidate A did not occur here. Still a real hole — `sync_hud` returns silently
  forever if the window is ever gone — but fixing it now would be speculative.
- **Re-deriving geometry on every show** (§7.2). The position was measured correct, and this
  machine has one display. Worth doing before any multi-monitor use.
- **`WM_POWERBROADCAST` / `WM_DISPLAYCHANGE` / session-change handling** (§7.4). Still entirely
  absent. Re-asserting on show makes it unnecessary for *this* bug; it is still the right home for
  the mic-endpoint-after-resume problem.
- **The delayed-hide race** (§9 of the previous doc) is still present in `sync_hud`'s Idle branch.
- **The mic-open failure that shows nothing** (§5D) is still unfixed.

## 7. Reproducing the measurement

The PowerShell P/Invoke probes are in `evidence/`: `probe.ps1` (window inventory), `probe5.ps1`
(PrintWindow + content measurement), `probe7.ps1` (z-order), `probe8.ps1` (the live fix). They take
the HUD's HWND as a hard-coded hex literal — re-read it from `probe.ps1`'s output before reusing
them. The sequence that matters, against a live pid:

1. `EnumWindows` filtered to the Parle pid; read `GetWindowRect`, `GetWindowLongPtrW(GWL_EXSTYLE)`,
   `MonitorFromWindow(..., MONITOR_DEFAULTTONULL)`, `DwmGetWindowAttribute(DWMWA_CLOAKED)`.
2. **Call `SetProcessDPIAware` first.** Without it every coordinate comes back in the virtualised
   1440x900 space while the screen is 2880x1800, and the first capture taken in this investigation
   photographed the wrong part of the screen and nearly produced a wrong conclusion.
3. `PrintWindow(PW_RENDERFULLCONTENT)` on the HUD *and* on the main window as an in-process
   control — that pairing is what cleanly separated "not painting" from "painting, not visible".
4. `EnumWindows` again for z-order (it returns top-first) and compare the HUD's index against the
   indices of `TOPMOST=False` windows overlapping its rect.
