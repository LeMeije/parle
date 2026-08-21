# Platform Integration Research (verified 21/08/2026)

Full findings from the platform research pass. Crate versions checked on crates.io same day.

## Windows Copilot key
- Emits (firmware-dependent): most common = chord `LShift down, LWin down, F23 down` (+ mirrored ups).
  VKs: VK_LSHIFT (0xA0), VK_LWIN (0x5B), VK_F23 (0x86). Some OEMs: single VK_LAUNCH_APP1 (0xB6).
  Some newer firmware exposes a discrete VK_C7 "CopilotKey" (less common).
- Intercept: SetWindowsHookEx(WH_KEYBOARD_LL) on a dedicated thread WITH a message pump (GetMessage loop).
  Track LShift/LWin state from the hook stream; when VK_F23 down arrives with both held -> Copilot key.
  Swallow by returning 1 (skip CallNextHookEx) for F23 down AND up.
- START-MENU TRAP: if only F23 is swallowed, the OS sees a bare Win press+release and opens Start.
  Fix (PowerToys trick): while LWin is still down, SendInput a dummy VK 0xFF event -> Win counts as
  "used as modifier". Injected events carry LLKHF_INJECTED — skip them in our own hook (loop guard).
- Hold-to-talk: F23 auto-repeats; first down = press-start, F23 up = release. (CopilotRemap reference:
  tap vs double-tap 350ms vs hold 500ms.)
- Settings-based remap requires MSIX packaging + Copilot hardware key provider registration — not viable
  for NSIS/MSI; LL hook needs no packaging changes.
- Pitfalls: hook proc must be allocation-free/lock-free (LowLevelHooksTimeout ~300ms silently REMOVES the
  hook — add a watchdog that re-installs); UIPI = no events while an elevated app has focus (known gap);
  games/anti-cheat can see raw input anyway — offer "disable in fullscreen games"; secure desktop never delivers.

## Global hotkeys
- tauri-plugin-global-shortcut v2.3.2 (wraps global-hotkey v0.8.0): HAS Pressed/Released states ->
  hold-to-talk works for normal chords. NO modifier-only hotkeys, NO left/right discrimination.
  Windows backend = RegisterHotKey (chord becomes exclusive system-wide). SPIKE: how Released is
  synthesised on Windows + its latency.
- rdev v0.5.3 is dead (2023). rdevin v0.1.0 = published RustDesk fork. device_query = polling only.
- Architecture: plugin for ordinary user chords; hand-rolled native listeners for flagship keys:
  - Windows: same WH_KEYBOARD_LL hook (KBDLLHOOKSTRUCT.vkCode gives VK_LSHIFT/VK_RSHIFT/VK_LMENU/... directly).
  - macOS: CGEventTap — kCGSessionEventTap, kCGHeadInsertEventTap, kCGEventTapOptionDefault (active tap;
    listen-only CANNOT swallow and is gated on Input Monitoring instead of Accessibility).
    Mask keyDown|keyUp|flagsChanged. Dedicated CFRunLoop thread. On kCGEventTapDisabledByTimeout /
    ...ByUserInput -> CGEventTapEnable(tap, true) and return event unmodified. Keep callback FAST
    (a slow default tap delays every keystroke system-wide). Re-check AXIsProcessTrusted on repeated disables.
  - Fn/Globe: flagsChanged with keyCode 63 (kVK_Function), maskSecondaryFn flag; keyDown/keyUp never fire.
    Excluded from RegisterEventHotKey — tap is the only route. Swallowing in an active tap prevents the
    system double-Fn dictation. Also advise users: System Settings > Keyboard > "Press 🌐 key to" = Do Nothing.
  - L/R modifiers (flagsChanged keyCodes): Shift 56/60, Cmd 55/54, Opt 58/61, Ctrl 59/62.
- Hold-to-latch: app-side state machine (tap under ~450ms latches into toggle; longer hold = stop on release).

## Non-focus-stealing overlay
- macOS: tauri-nspanel (git, branch v2.1, objc2 rewrite, PanelBuilder; NOT on crates.io).
  Recipe: NonactivatingPanel style mask, canBecomeKeyWindow=false, level Status (25),
  collectionBehavior canJoinAllSpaces|fullScreenAuxiliary|stationary, and set_hides_on_deactivate(false)
  (THE gotcha: NSPanel auto-hides on app deactivate and we are always deactivated).
  Non-activating panel cannot take keyboard input — HUD is mouse-only. Transparency needs macos-private-api.
- Windows: Tauri window (decorations:false, transparent:true, alwaysOnTop:true, skipTaskbar:true,
  focus:false, shadow:false) then raw-style hardening on the HWND:
  GWL_EXSTYLE |= WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_TOOLWINDOW (focus:false alone historically
  insufficient — tauri#7519/#11897). Re-apply styles if window recreated. Optional WS_EX_TRANSPARENT|LAYERED
  for click-through.

## Paste at cursor
- macOS ranked: (1) primary = clipboard-write + CGEventPost Cmd+V (kVK_ANSI_V=9, kCGEventFlagMaskCommand,
  post to kCGHIDEventTap) — uses the app's real paste path; TCC kTCCServicePostEvent surfaces as
  Accessibility. (2) opportunistic AX fast path: AXUIElementSetAttributeValue(focused el,
  kAXSelectedTextAttribute, text); fails in Chromium/Electron/VS Code/terminals/Java/secure fields;
  verify by reading back kAXSelectedTextRange, fall through on any error. (3) clipboard-only.
- Clipboard restore discipline (macOS): snapshot changeCount + round-trippable items; write transcript
  WITH org.nspasteboard.TransientType (+ ConcealedType if sensitive) so managers skip it; target apps read
  the pasteboard ASYNC after the keystroke — restore delay default ~700ms, configurable; check changeCount
  before restoring (never clobber a user copy); do NOT restore previous content that was marked Concealed.
- Secure input (macOS): IsSecureEventInputEnabled() (Carbon, cheap — poll at hotkey time). Degrade to
  clipboard-only (Concealed+Transient) + "press Cmd+V" HUD hint. Often stuck system-wide by password
  managers — report as state, not error.
- Windows: primary = clipboard write + SendInput Ctrl+V (release/await our own held modifiers first).
  UIA insertion NOT viable (TextPattern read-only; ValuePattern replaces entire single-line fields only).
  Write with ExcludeClipboardContentFromMonitorProcessing + CF_CLIPBOARD_VIEWER_IGNORE +
  CanIncludeInClipboardHistory=0 (keeps transcript out of Win+V history/cloud sync).
  Restore after ~500ms; verify GetClipboardSequenceNumber unchanged; OpenClipboard needs a retry loop.
  UIPI: cannot paste into elevated windows (known gap).

## Clipboard monitoring
- macOS: poll NSPasteboard.general.changeCount every 250-500ms. Skip items typed
  org.nspasteboard.TransientType / ConcealedType / AutoGeneratedType; skip while secure input active.
  Source app heuristic = NSWorkspace frontmostApplication at tick time.
- Windows: hidden message-only window (HWND_MESSAGE) + AddClipboardFormatListener -> WM_CLIPBOARDUPDATE.
  Honour the exclusion formats above. Source = GetClipboardOwner -> GetWindowThreadProcessId ->
  QueryFullProcessImageNameW (best-effort; owner may be NULL).
- Read/write via arboard v3.6.1. (clipboard-rs watcher unvetted.)

## Tauri 2 specifics
- Core 2.11.x. Tray built-in (feature "tray-icon", TrayIconBuilder, template icons on macOS).
- Plugins: single-instance v2.4.3 (register FIRST), autostart v2.5.1, updater v2.10.1 (minisign-signed
  artifacts, createUpdaterArtifacts:true), global-shortcut v2.3.2.
- Pipeline: own threads started in setup(); cpal callback must never block; crossbeam/mpsc into ASR thread.
  To webview: tauri::ipc::Channel<T> for high-rate ordered streams (partials, mic levels) — the event
  system is NOT for high throughput; app.emit only for low-frequency state.
- HUD look: window-vibrancy v0.8.0 (NSVisualEffectMaterial::HudWindow / acrylic / mica).
- Footprint ballpark: installer 3-10MB vs Electron 80-120MB; idle RAM ~60-150MB (WebView2 helpers) on
  Windows, less on macOS; the model dwarfs this. Pitch: "no bundled Chromium, no Node runtime".

## macOS permissions / TCC
- Mic: NSMicrophoneUsageDescription mandatory (hardened runtime kills without it) +
  com.apple.security.device.audio-input entitlement. Explicit request via objc2-av-foundation
  AVCaptureDevice::requestAccessForMediaType (block2::RcBlock), or let first cpal open trigger it.
- Accessibility: AXIsProcessTrustedWithOptions(kAXTrustedCheckOptionPrompt=true). Needed for active tap +
  CGEventPost (also CGRequestPostEventAccess).
- Listen-only tap = Input Monitoring; active tap = Accessibility. We need active -> Accessibility is the
  headline permission. Test the actual prompt set on current macOS.
- TCC SIGNING TRAP: ad-hoc signing (-s -) makes a new CDHash per build -> grants silently orphaned every
  rebuild (AXIsProcessTrusted false while toggle looks enabled). Dev fix: ONE stable self-signed or Apple
  Development cert, sign every dev build with it. Recovery: tccutil reset Accessibility com.novaire.echokey
  (never a bare reset) and fully quit System Settings (the pane caches). Ship = Developer ID + hardened
  runtime + notarisation. Never run the .app from an iCloud-synced folder (sync corrupts signatures).

## Open spikes
1. global-hotkey Windows Released mechanism + latency.  2. MSIX Copilot-provider contract (skip for v1).
3. Which TCC pane(s) the tap prompts under on current macOS.  4. Per-app clipboard restore delays (make configurable).
