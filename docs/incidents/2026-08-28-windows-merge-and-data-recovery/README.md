# Windows: pulling the macOS work, and a history database that was nearly left behind

**Date:** 2026-08-28, evening (UTC+7)
**Machine:** ASUS Zephyrus G14 2025 (Ryzen AI 9 370HX, 64 GB, RTX 5070 Ti), Windows 11
**Repo:** `main` @ `ca744cf`, fast-forwarded from `windows-build` @ `1dd3f47` (55 commits)
**Build tested:** `npm run tauri build -- --features cuda`, 9m21s, exit 0

## 1. The merge itself is sound on Windows

`platform/windows.rs` gained 532 lines that were written on the Mac and had never
been near a Windows compiler. It built with **no errors and no signature drift**,
which repeats the result recorded in `WINDOWS_HANDOFF.md` for the original port.

| Check | Result |
|---|---|
| `cargo test -p parle-core` | 189 pass, both shared contract vectors green |
| `cargo test -p parle-sync` | 68 pass |
| `cargo test -p parle --lib` | 332 pass |
| Release build + NSIS + MSI | clean, 11 warnings, all dead-code |
| i18n key parity | 369 keys x 5 languages, identical sets AND order |
| i18n placeholders / no-dash rule | pass |
| Clipboard poll cadence | 150 ms, matches macOS |
| `EV_ABORT` chord-cancel | wired end to end, hook to app |
| `is_self()` on Windows | correct: exe name vs `current_exe` |
| Launch visibility (`4fa83af`) | VERIFIED on Windows, see section 4 |
| Device identity stability | stable across restarts |
| Shipped-exclusions union | 10 -> 20 entries, as designed |

The Mac claims 335 for `parle`; Windows runs 332 because six tests are
`cfg(target_os = "macos")`. **There are zero Windows-gated tests.** Every
Windows-specific behaviour in this repo is verified by hand or not at all.

## 2. The serious one: the app started on a stale history database

After installing and launching the new build, the running app was reading a
**9-row history database whose newest row was 2026-08-23T23:07**, while the
user's real database - **131 rows, newest 2026-08-28T19:22**, containing that
evening's dictations - sat in the old `%LOCALAPPDATA%\EchoKey` directory,
untouched and unreferenced.

Nothing was destroyed: the migration in `data_dir()` never deletes. But the app
presented an almost-empty history as if it were the whole history, with no
warning, and a user who then pruned or re-dictated could easily have made the
loss real.

### What made it possible

`crates/parle-core/src/settings.rs:716-727`, the branch that runs when the
destination directory already exists:

```rust
if new.exists() {
    if let Ok(entries) = std::fs::read_dir(&old) {
        for e in entries.flatten() {
            let to = new.join(e.file_name());
            if !to.exists() {
                let _ = std::fs::rename(e.path(), to);
            }
        }
    }
```

Two properties combine badly:

- **`if !to.exists()` makes the destination win.** A stale file already sitting
  in the destination silently shadows the live file in the source. The skip is
  deliberate and is the right instinct - it is what stops the migration
  overwriting anything - but its consequence is that the app can end up on the
  older of two copies and say nothing.
- **`let _ = std::fs::rename(...)`** discards every error, so a rename blocked
  by an open handle is indistinguishable from one that never needed to happen.

### Why Windows is where this bites

**On Windows the install directory and the data directory are the same path.**
`bundle.windows.nsis.installMode` is `currentUser`, so NSIS installs to
`$LOCALAPPDATA\<productName>` = `%LOCALAPPDATA%\Parle`; and `data_dir()` on
Windows is `dirs::data_local_dir()` joined with `Parle`, which is that same
folder. On macOS the two are nowhere near each other (`/Applications` vs
`~/Library/Application Support`), so the Mac cannot reproduce this.

That collision means the destination directory is one that installers, previous
builds and uninstallers all write to - exactly the place stale copies
accumulate, and therefore the worst possible place for a "destination wins"
merge rule.

### Honest limit on this account

I could not reconstruct the exact sequence that put a stale `history.db` where
one was found. A listing taken at 19:17 and one taken at 19:31 are mutually
inconsistent about which files were in which directory, and I have no mechanism
that explains both. What is certain, and is what matters: the live 131-row
database was not the one the app opened, and the two properties above are
sufficient for that outcome without anything else going wrong.

### What was done

1. Both directories backed up in full before anything was touched
   (`parle-backup-20260828-193500-FULL`, plus an earlier partial one).
2. The 131-row database checkpointed to a single clean file
   (`PRAGMA wal_checkpoint(TRUNCATE)`), `integrity_check` = ok.
3. That file installed as `%LOCALAPPDATA%\Parle\history.db`, and the live
   `settings.json` restored alongside it.
4. App relaunched: it opened the restored database, ran its schema migration
   (`user_version` 2 -> 6) and **all 131 rows survived**.
5. `%LOCALAPPDATA%\EchoKey` deliberately left in place as a safety copy.

### Recommended fixes, in order

1. **Separate the data directory from the install directory on Windows.** This
   is the root cause and the only fix that removes the class of problem. It is
   also the one that needs a decision, because it affects where existing users'
   data lives and implies another migration hop.
2. **Make the merge prefer the live file, or refuse to guess.** If both sides
   hold a `history.db`, the one with rows should win, or the app should stop and
   say so rather than silently choosing.
3. **Log the migration where it can be seen.** `data_dir()` is called by
   `log_path()` before the tracing subscriber exists, so its `tracing::info!`
   lines about migrating and merging go nowhere. The one event most worth having
   a record of is the one event that cannot produce one.

## 3. `parle.log` is still truncated on every launch

`src-tauri/src/lib.rs:48` still opens the log with `std::fs::File::create`, so
each launch destroys the previous run's log. This was the headline finding of
the 2026-08-28 overlay incident - restarting the app, the user's own workaround,
erased the evidence - and it is unchanged. Rotating one generation
(`parle.log` -> `parle.log.1` before create) would be a few lines and would make
the next occurrence diagnosable.

## 4. Launch visibility, verified

Commit `4fa83af` made "hide at startup only when the system asked for the
launch" one condition for both platforms. On Windows:

| Launch | `MainWindowTitle` | Reading |
|---|---|---|
| `parle.exe` | `Parle` | main window present |
| `parle.exe --hidden` | `com.novaire.parle-siw` | only the single-instance helper; main window hidden |

## 5. Still open on this machine

- The core dictation loop could not be exercised: it needs someone to speak.
- Anything requiring the GUI (language picker, waveform sensitivity, overlay
  style "None", custom model add, tray styles) needs a human at the screen.
- Win+V behaviour under the new rules.
- Two-machine sync: `docs/SYNC_FIELD_TEST.md` steps 1-12.
- The sync listener binds `0.0.0.0:0` (ephemeral) and advertises over mDNS, so
  Windows Defender Firewall will prompt on first use. That prompt has to be
  accepted by the user; a port-based rule will not work.

---

# Follow-up, same session: sync latency and history provenance

Two problems reported from the first real two-machine test, and what was done.

## 6. A dictation took five to seven minutes to reach the other machine

Not a slow exchange. An exchange is triggered **only** by an mDNS `PeerFound`
event (`manager.rs`, the discovery thread), gated on `DIAL_RETRY_AFTER` (60 s).
There was no periodic timer, and **nothing fires when a row is written**. So how
quickly a dictation travelled was decided entirely by how often `mdns-sd`
happened to re-announce a peer whose record had not changed. The exchange itself
is fast; all of that time was waiting.

Fixed two ways:

- **`SYNC_TICK`, 20 s.** A ticker thread that shares the listener's own
  generation stop flag, so a `stop()` kills it with everything else and a ticker
  from an earlier enable can never outlive its generation. Worst-case latency is
  now the tick, not mDNS's mood.
- **"Sync now"**, a user-visible button on the Sync screen.

Both call one new rule, `decide_manual_dials`, rather than adding a second dial
path. It ignores `DIAL_RETRY_AFTER` and **only** that: the backoff exists because
mDNS is unsigned, so a sighting is an attacker-controlled event and an ungated
dial per sighting is a free thread, keychain read and 20 s connect for anyone on
the LAN. A tick and a button press are local events that nobody on the network
can forge. The limits that are about local resources rather than abuse,
`MAX_DIALS` and never dialling a peer twice concurrently, are kept exactly as
`decide_dial` keeps them. Five tests pin that distinction.

It is deliberately neither a fetch nor a push: `replicate::exchange` serves our
rows and drains theirs in one pass, so one press moves everything both ways, and
the UI must never name it as one direction.

**Still not done:** nothing yet fires an exchange when a row is written. That is
the change that would take propagation from "up to 20 s" to "about a second",
and it needs the pipeline to reach the sync manager, which it currently cannot.

## 7. History could not say which machine a row came from

`items.source_machine` has been in the schema since v8 and drives the whole
replication authority model, but it was never exposed on `HistoryItem`, so a
list mixing three machines looked exactly like a list from one. An adversarial
test (`r12_flow`) had this recorded as an open finding; it is now a regression
test asserting the field is present AND correct.

The UI decision, and the reasoning, because the obvious answer is wrong:

The row footer already carries time, app, duration, model, language and trim
badges. Adding a device badge to every row makes the crowded case worse, and
**most rows are local**, so labelling all of them is noise. The signal worth
showing is "this one came from somewhere else". So:

- **A 3px coloured left edge, on peer rows only.** Pre-attentive, costs no
  horizontal space. A pseudo-element rather than a border or an inset shadow,
  because `.row` already spends its border on selection and its box-shadow on
  the selected ring, and a marker that vanishes when a row is selected is worse
  than no marker.
- **A named pill in the footer, on peer rows only.** Colour alone fails a
  colour-blind reader and cannot say WHICH Mac. Absent on local rows, so the
  common case stays clean and "marked = from elsewhere" is learnable in one
  glance.
- **A device filter**, shown only once a second device has actually written
  something. A one-machine user must never see a control that can only have one
  answer.
- **Colour is derived from a hash of the device id**, not from position in the
  paired roster, so a device keeps its colour when another is added or removed.
  A colour that moves is worse than none: the user learns "green is the Mac" and
  then green silently becomes the phone. Six hues, chosen clear of the hues
  already carrying meaning (amber unsure, violet trimmed, teal language).

A per-device colour override in settings was considered and not built. The hash
means it works with zero configuration, which is the case that matters; an
override is a preference, not a fix.

## 8. Sync moved out of Settings into its own tab

Settings is where you go to change something you already know exists. Sync has
to be found, and once found it is the only part of the app with live state to
watch: peers appearing, an exchange running, a code counting down. The panel
itself still lives in `SettingsView.tsx`, where its helpers are; only where it
is reached from changed. Moving the 500-line component and its four shared
helpers into the new file is a tidy-up worth doing separately.

The nav icon is `MonitorSmartphone`, NOT a cloud. Parle's whole promise is that
nothing leaves the machine, and a cloud glyph on the sync tab would contradict
the product in the one place a user looks to understand what sync does.
