# Incident: overlay stops appearing while dictation keeps working

**Date reported:** 2026-08-28, ~10:50 local (UTC+7)
**Machine:** Windows 11 laptop (the one running the *installed* build, not the dev machine)
**Build in use:** `%LOCALAPPDATA%\Parle\echokey.exe`, built **2026-08-24 00:17**
**Repo state on this machine:** branch `windows-build` @ `1dd3f47`, working tree clean

> Written as a **diagnosis only**. No code was changed. The dev machine has unpushed work; the
> intent is to merge that first, then act on this.

> **Build provenance — read before trusting any line number.** The installed exe (2026-08-24
> 00:17) sits around commit `4bff2c2` and **predates the entire sync feature** (`a2401e9`
> onwards, from 00:58 the same night). All line references below are against HEAD `1dd3f47`,
> which is the code that will be *fixed* — not byte-for-byte the code that *failed*. The overlay
> and pipeline paths quoted here are unchanged between the two, but the sync-era additions
> (device identity, per-row `source_machine` stamping) were **not** in the failing binary. That
> also explains why the captured `settings.json` has no `sync` block — expected, not a bug.

---

## 1. Symptom (confirmed with the user)

The app had been left running for a day or two, across at least one lid-close / sleep cycle.
After that:

- Dictation worked **completely normally** — Copilot key started and stopped recording, and the
  transcribed text was pasted at the cursor every time. **Confirmed by the user.**
- **The overlay never appeared again.** Not during recording, not during transcription.
- Toggling the overlay style in Settings and back **did not fix it** — this was an initial
  impression the user later corrected.
- **Quitting and restarting the app fixed it.** That is the only thing that did.

## 2. The three constraints that any explanation must satisfy

This is the useful part. The corrected account narrows the field sharply:

1. **Everything except the overlay was healthy.** Recording, transcription and injection all ran.
   So the pipeline was emitting `StateChanged { Recording }` and `{ Transcribing }`, which means
   `sync_hud` (`state.rs:126-129`) *was being called*, repeatedly, for hours.
2. **It was sticky.** It survived dozens of dictation cycles. Every one of those called
   `hud.show()` again, and none of them helped.
3. **Only a process restart cleared it.** Nothing reachable from the running app's own code paths
   recovered it.

Constraint 2 is the sharp one: it rules out anything that a fresh `show()` would repair. Whatever
broke, it is **per-process state that `show()` does not touch**.

## 3. What the logs show — and what they can't

**The evidence for the incident is gone, and that is a bug in itself.** `parle.log` is opened with
`File::create` (`src-tauri/src/lib.rs:39`), i.e. **truncated on every launch** — so the user's own
fix (restarting the app) destroyed the log covering the failure. The captured `evidence/parle.log`
begins at that restart, 11:33:33 local.

Even intact it would have said little. At the default filter the app logs only startup lines and
one line per hotkey edge. There is **no logging at all** for pipeline state changes, HUD show/hide,
HUD window lookup failures, microphone-open failures, transcription completion, or history writes.
See §6.

### The one anomaly in the surviving log

```
04:46:52.026  hotkey Dictation Down -> StartRecording
04:46:57.911  hotkey Dictation Down -> StartRecording     <-- two Starts, no Stop between
04:48:19.272  hotkey Dictation Down -> StopRecording
```

The hotkey is in Toggle mode. `GestureMachine` (`hotkey_logic.rs:78-86`) strictly alternates
`Idle -> StartRecording -> ToggleRecording -> StopRecording`; two Starts in a row require the
machine to have been forced back to `Idle`. The only thing that does that is
`AppState::pipeline_start` (`state.rs:217-228`) when `pipeline.start()` returns `false` — which
happens **only** when the microphone fails to open (`pipeline.rs:98-113`).

So at 04:46:52 the mic failed to open and the app said nothing at all. That is a genuine defect
(§5, Finding D) but it happened **after** the restart and is **not** the cause of this incident —
if the mic had failed during the incident, no text would have been pasted, and text was pasted.

## 4. Structural facts about the HUD (all verified against HEAD)

- `create_hud` is called **exactly once**, at startup (`lib.rs:166`). Nothing ever recreates it.
- Position is computed **once**, from `primary_monitor()` (`hud.rs:76-82`). Never recomputed.
- `always_on_top(true)` is set **once**, at build time (`hud.rs:92`). Never re-asserted.
- `harden_overlay` (`platform/windows.rs:791-809`) runs once. It sets `WS_EX_TOPMOST` via
  `SetWindowLongPtrW`, which per Win32 docs does **not** actually change topmost state — the real
  topmost comes from Tauri's builder call. Relevant when reasoning about z-order.
- `sync_hud` (`hud.rs:122-125`) begins
  `let Some(hud) = app.get_webview_window(HUD_LABEL) else { return; };` — if the HUD window is ever
  gone, every subsequent show is a **silent no-op, forever, until restart**.
- **No handling anywhere** for `WM_POWERBROADCAST`, `WM_DISPLAYCHANGE`, DPI change, or session
  lock/unlock. Verified by grep across `src-tauri` and `crates`.
- `sync_hud` is driven **only** by `PipelineEvent::StateChanged` (`state.rs:126-129`).

## 5. Candidate root causes, re-ranked against §2

### A. HUD window gone — `sync_hud` silently no-ops forever

If the HUD webview/window was torn down (WebView2 renderer crash, or the window destroyed for any
reason), `get_webview_window(HUD_LABEL)` returns `None` and `sync_hud` returns immediately —
no log, no retry, no recreation. Every later dictation hits the same early return.

Fits all three constraints exactly, including "restart is the only cure", and it is the *only*
candidate where the code guarantees permanence by construction. Cheap to confirm with one log line
(§6.2) and cheap to fix regardless (§7.3).

### B. HUD stranded off-screen after a display-configuration change

Lid close, waking on a different display arrangement, or a DPI/resolution change moves what
"primary monitor" means. The HUD's coordinates were computed at startup and are never revisited.
Windows relocates *visible* windows when a monitor disappears; a **hidden** window — which the HUD
is whenever idle — keeps its stale coordinates. `hud.show()` then succeeds and returns `Ok`, and
nothing is visible.

Fits all three constraints, and correlates directly with the lid-close the user described.

### C. HUD lost topmost / z-order

Topmost can be dropped after a lock screen, a UAC prompt, or a fullscreen app taking foreground.
`always_on_top` is never re-asserted and `show()` (`ShowWindow`) does not restore z-order, so the
HUD would render *behind* whatever is being typed into — indistinguishable from "not appearing",
and permanent until restart.

Fits all three constraints.

### D. Microphone open failure — *a real defect, but NOT this incident*

Confirmed to occur (§3), and worth fixing. When `Pipeline::start` fails it emits
`PipelineEvent::Error`, which triggers `hud::hold_visible(4000)` (`state.rs:118`) — but `Error` is
**not** a `StateChanged`, so `sync_hud` is never called and **the HUD is never shown**. The
"Could not start microphone: …" message the code carefully builds is displayed nowhere and logged
nowhere.

Ruled out as the incident cause: text was pasted throughout, so the mic was opening fine.

### E. WebView2 render surface lost after resume — *weakest*

The HUD window is `transparent(true)`; a lost compositor surface would show the window painting
nothing, and a fully transparent window is invisible. Fits the constraints, but the HUD's DOM
mutates at ~20 Hz during recording (waveform bars), so repaints are being requested continuously
anyway. Keep as a fallback only if instrumentation clears A, B and C.

### F. Ruled out — HUD lost its event listeners

If the HUD webview stopped receiving `pipeline-event`, the DOM would stay on the idle pill. But
`.hud` has **no** `opacity: 0` idle rule in `src/hud.css` — the pill paints in the idle state too.
A listener failure would show a *stale visible* pill, not an invisible one. Not this.

### Settled: the style toggle was a red herring

The user has confirmed the toggle did not fix anything. This matches the code exactly, and no
longer needs explaining: `set_settings` (`commands.rs:29-55`) saves the file and calls
`apply_settings`, which (`state.rs:292-335`) touches the tray icon, gesture modes, hotkey bindings,
sync retention, the clipboard monitor and the engine — and **never touches the HUD window**.
`Hud.tsx` re-reads settings only on mount and on `state_changed -> recording`
(`src/Hud.tsx:48-51`), so the sole effect is a CSS class change on the next recording. Nothing in
that path could have restored a broken overlay. Do not build any fix around it.

## 6. The actual blocker: this bug is currently undiagnosable

No root cause can be confirmed, because the app destroyed its own evidence and never recorded the
relevant events. **Fix observability first** — before, or at least alongside, any overlay fix.
A, B, C and E are indistinguishable from the outside, and each needs a different fix.

1. **Stop truncating the log.** `lib.rs:39` uses `File::create`. Switch to append plus simple
   rotation (keep `parle.log` and `parle.log.1`, roll at a few MB). Without this, the user's
   natural response to any bug — restarting — deletes the only evidence, exactly as it did here.
2. **Log the HUD lifecycle.** In `sync_hud`, one line per transition: the state, **whether the HUD
   window was found**, the result of `show()`/`hide()`, the window's current outer position, and
   the bounds of the monitor that position lands on. That single line discriminates A from B from
   C from E — which is the whole ballgame.
3. **Log the silent failure paths**, all currently `let _ =` / `.unwrap_or(-1)`:
   - `Pipeline::start` returning `false` (`pipeline.rs:107`) — log the underlying device error.
   - `insert_transcription` failing (`pipeline.rs:438`, `pipeline.rs:395`, `pipeline.rs:623`).
   - `Settings::save` failing.
4. **Surface mic-open failure to the user.** Make `PipelineEvent::Error` show the HUD, not merely
   extend a hold on a HUD that is never shown (§5D).

## 7. Proposed fixes

Cheap, and correct regardless of which of A/B/C/E it turns out to be. Order matters.

1. **Self-heal a missing HUD window.** In `sync_hud`, when `get_webview_window(HUD_LABEL)` returns
   `None`, log it loudly and call `create_hud` instead of silently returning. Kills A. This is the
   smallest change on the list and covers the candidate that best fits the evidence.
2. **Re-derive HUD geometry on every show.** Before `hud.show()`, recompute the target position
   from the *current* monitor and call `set_position`. Prefer the monitor holding the foreground
   window (or the cursor) over `primary_monitor()` — on a multi-monitor desk the overlay should
   follow the user, not the primary display. Kills B.
3. **Re-assert always-on-top and the extended styles on every show.** Call
   `set_always_on_top(true)` and re-run `harden_overlay` before `show()`; on Windows use
   `SetWindowPos(HWND_TOPMOST, …, SWP_NOMOVE|SWP_NOSIZE|SWP_NOACTIVATE)` so topmost is genuinely
   re-applied rather than only written into the ex-style bits. Kills C.
4. **Handle resume and display change.** Subscribe to `WM_POWERBROADCAST`
   (`PBT_APMRESUMEAUTOMATIC` / `PBT_APMRESUMESUSPEND`), `WM_DISPLAYCHANGE` and
   `WM_WTSSESSION_CHANGE`. On each: reposition and re-harden the HUD, and re-probe the audio input
   device so a stale endpoint is found before the next dictation rather than during it. This is
   also the right home for the §5D mic fix.
5. **Recreate the HUD webview on resume** — only if instrumentation points at E. Heaviest option;
   do not do it speculatively.

Items 1–3 together mean the overlay repairs itself on the next dictation no matter which of the
three it was. That is worth shipping even before the cause is known.

## 8. Companion finding: history and settings writes have been silently failing

Independent of the overlay, and on the evidence possibly the bigger problem.

`%LOCALAPPDATA%\EchoKey`:

| file | last written |
|---|---|
| `parle.log` | today, live |
| `history.db-wal` | **2026-08-23 23:07:10** |
| `settings.json` | **2026-08-23 22:43:59** |

The history DB (read from a copy; the live DB was not touched) contains **9 rows, newest
2026-08-23 23:07:10**. Six dictations were performed today alone, and the user confirms text has
been pasting correctly throughout. The last successful history write is **~70 minutes before the
installed build was compiled** (2026-08-24 00:17): *since this build went on the machine, not one
transcription appears to have been persisted.*

It is invisible because injection happens **before** the history write, and the write's error is
discarded (`pipeline.rs:404-441`):

```rust
let injection = ...;                       // paste happens here, first
...
let item_id = self.store.lock()
    .insert_transcription(&tr, ...)
    .unwrap_or(-1);                        // <-- failure swallowed, nothing logged
```

`settings.json` stopping within the same hour is suspicious and may share a cause. Consistent with
that: `overlay.style` in the captured file is still `"cassette"` from 2026-08-23, so today's
overlay-style toggling never reached disk either.

**Fastest way to settle this — 5 seconds, no code:** open the History view in the running app. If
it shows nothing since 23 August, the write path is broken and §6.3 becomes urgent. If today's
dictations *are* listed, then the store is fine and this whole section dissolves into a
file-timestamp artefact, and only the `settings.json` half needs chasing.

## 9. Confirmed side bug found during this investigation

`hud::sync_hud`, Idle branch (`hud.rs:135-147`): when a hold is active it spawns a thread that
sleeps until the hold expires, then re-checks and hides. The re-check guards only against a
**newer hold** — not against a **new recording**:

```rust
std::thread::sleep(...);
if HOLD_UNTIL.load(Ordering::SeqCst) <= now_ms() {
    hud.hide();          // fires even if we are Recording again by now
}
```

Repro: dictation A ends in Empty/Error/manual-paste -> `hold_visible(2200..4000)` -> Idle spawns
the hide thread. Press the hotkey again inside that window -> Recording -> HUD shown. The sleep
expires, the hold is now in the past, and the HUD is hidden **mid-recording**.

Fix: have the delayed hide also check current pipeline state, or bump a generation counter on
every `sync_hud` call and bail if it changed.

## 10. Files to look at

| Concern | Location |
|---|---|
| HUD window creation, position, show/hide | `src-tauri/src/hud.rs` |
| Only caller of `create_hud` | `src-tauri/src/lib.rs:166` |
| Log file setup (the truncation) | `src-tauri/src/lib.rs:20-46` |
| Event sink -> `sync_hud` | `src-tauri/src/state.rs:105-131` |
| Mic-open failure -> gesture reset | `src-tauri/src/state.rs:217-228` |
| `Pipeline::start`, error path | `src-tauri/src/pipeline.rs:88-114` |
| Injection before history write | `src-tauri/src/pipeline.rs:404-441` |
| Windows overlay hardening | `src-tauri/src/platform/windows.rs:787-809` |
| Toggle gesture machine | `src-tauri/src/hotkey_logic.rs` |
| HUD frontend, settings refetch | `src/Hud.tsx:31-79` |
| Idle pill has no `opacity: 0` | `src/hud.css` |

## 11. Evidence captured

In `evidence/`, taken 2026-08-28 11:58 local:

- `parle.log` — post-restart only; the incident window was destroyed by the restart that fixed it
- `parle-hook.log` — last written 2026-08-23; note the
  `RegisterHotKey for Copilot chord failed: Hot key is already registered (0x80070581)` line
- `settings.json` — the live config, last written 2026-08-23 22:43
- `environment.txt` — process list, build timestamps, data-dir file times, full history-DB dump,
  repo state

## 12. Sequencing

1. Finish and push the dev-machine work; merge to `main`.
2. Pull here. This folder is untracked, so it survives the pull unchanged.
3. Answer the History-view question in §8 — it costs nothing and may reprioritise everything.
4. Land §6 (observability) first and reinstall. It is small, and it is what makes the next
   occurrence answerable instead of another dead end.
5. Land §7.1–7.4. They are cheap and cover A/B/C whether or not the cause is ever pinned down.
6. Chase §8 separately if the History view confirms it.
