# Every remaining mention of the pre-rename name

Windows scan 29/08/2026. **macOS scan 30/08/2026, added in section 8.** The repo
half of this has since been ACTED ON (section 0); the machine-level findings
remain a report.

## 0. What changed on 30/08/2026

The source tree is now down to **one** mention, deliberately.

- `crates/parle-core/src/settings.rs`: the bare literal in `data_dir()` is now
  the named constant `OLD_DATA_DIR`, carrying the do-not-rename warning that
  used to sit as a loose comment beside a string. The three `tracing` messages
  and the doc comment no longer name the old product, since they describe a
  folder by its role and lose nothing by saying "the old data directory".
- `src-tauri/src/lib.rs`: the `log_path` incident comment keeps the lesson and
  drops the brand.
- `README.md`: the explanatory paragraph now points at `OLD_DATA_DIR` and at
  this file.
- This file was `ECHOKEY_AUDIT.md`.

**A future sweep must still skip `OLD_DATA_DIR`.** Making it a constant makes it
one clearly-labelled thing to skip rather than a bare string in a function body,
which is what a careless sweep destroyed the first time.

## 0b. How to retire `OLD_DATA_DIR`, when the time comes

The constant cannot just be deleted on a hunch, because the cost of being wrong
is a user's entire history and every downloaded model. As of 30/08/2026 there is
a procedure and a guard instead of a note-to-self.

**The check.** `parle_core::settings::legacy_data_dir_present()` reads the disk
and answers, for the machine it runs on, whether a legacy data folder is still
there. `resolve_data_dir` also returns a `LegacyOutcome`, and
`LegacyOutcome::still_needed()` is false only for `NoLegacy`. Note that
`Migrated` and `Merged` mean the constant did its job THIS run, which is the
opposite of safe to remove.

**The precondition.** Every machine that has ever run the app reports
`legacy_data_dir_present() == false`, having been launched at least once on a
build that includes the migration. Today that is two machines, the Mac and the
Windows box. The Mac satisfied it on 30/08/2026: `~/Library/Application
Support/EchoKey` is gone and `Parle` is live at 2.2 GB. The Windows box does
NOT: section 6 records `%LOCALAPPDATA%\EchoKey` still in place, deliberately, as
a safety net after the history recovery.

**Why there is no automatic answer.** Parle has no telemetry, on purpose, so the
app can never learn that every install everywhere has migrated. This stays a
judgement call about a known set of machines. The procedure exists to make that
judgement checkable rather than to remove it.

**The guard.** `settings.rs` has a test,
`the_legacy_folder_name_is_still_the_pre_rename_one`, that writes the
pre-rename name as a LITERAL rather than through the constant. This matters: the
other migration tests refer to the folder only via `OLD_DATA_DIR`, so a sweep
that rewrites the constant would move the seed and the assertion together and
they would all still pass while the real migration silently did nothing. Swept
on 30/08/2026 as a check, five tests failed, so the guard is real.

**When you do retire it**, delete in this order: the legacy branch in
`resolve_data_dir`, then `LegacyOutcome` down to whatever the callers still
need, then `legacy_data_dir_present`, then the constant, then the tests that
name it, and finally this section. Leaving the tests behind is the failure mode
to avoid: they would fail for the right reason at the wrong time and get deleted
in a hurry without anyone rereading them.

## Verdict first

*(As scanned 29/08. Section 0 records what has since changed.)*

The rename was done properly. **The source tree has 7 mentions and every one of
them is deliberate and load-bearing.** What was seen in `target/release` is
build output from before the rename, which cargo never cleans up on its own and
which is not shipped to anyone.

The one thing that genuinely deserves action is a set of 10 stale Windows
Firewall rules.

| Where | Count | Ships to a user? | Action |
|---|---|---|---|
| Source code | 5 | Yes, as strings in the exe | **Keep.** Required by the migration |
| Comments and docs | 4 | No | Keep, or reword |
| `target/` build artifacts | 6,289 files | No | `cargo clean` when convenient |
| Incident evidence files | 39 | No | **Keep.** Historical record |
| Windows Firewall rules | 10 | n/a | **Delete.** The only real litter |
| `%LOCALAPPDATA%\EchoKey` | 1 folder | n/a | Keep for now, delete once satisfied |
| Installed app, registry, shortcuts, crate names, mDNS, keychain | 0 | n/a | Already clean |

---

## 1. Source code: was 5 mentions, now 1

**Superseded by section 0 on 30/08/2026.** The line numbers and text below are
the state as scanned on 29/08; they no longer match the file. The five collapsed
into the single `OLD_DATA_DIR` constant. Kept for the record of what was there.

All five are in `crates/parle-core/src/settings.rs`, in `data_dir()`:

| Line | Text |
|---|---|
| 676 | doc comment: "The folder was called `EchoKey` before the rename" |
| 709 | `let old = base.join("EchoKey");` |
| 725 | log: "merged the EchoKey data directory into Parle" |
| 731 | log: "migrated the data directory from EchoKey to Parle" |
| 739 | log: "could not rename the data directory from EchoKey to Parle" |

**Line 709 must never be renamed.** It is the folder the app migrates FROM, and
the code says so in a comment that already records this being got wrong once:

> This one string must NOT be renamed with the rest: it is the folder we are
> migrating FROM, and a sweep that renames it turns this whole function into a
> no-op that silently loses the user's history and every downloaded model.
> (That is exactly what the first pass of the rename did.)

The four log lines are only reachable while a machine still has an unmigrated
`EchoKey` folder. They could be reworded to "the old data folder" without loss,
which would leave line 709 as the single mention in the whole codebase. That is
the only tidy-up available here, and it is cosmetic.

## 2. Comments and prose: was 4 mentions, now 0

**Superseded by section 0.** `lib.rs` and `README.md` were reworded on
30/08/2026; the two incident write-ups listed below are not on `main`.

| File | Line | What |
|---|---|---|
| `src-tauri/src/lib.rs` | 24 | comment explaining the migration bug `log_path()` caused |
| `README.md` | 59 | "The project was called EchoKey before it was called Parle." |
| `docs/incidents/2026-08-28-overlay-stops-appearing/README.md` | 2 | incident write-up |
| `docs/incidents/2026-08-28-windows-merge-and-data-recovery/README.md` | 2 | this session's write-up |

The README sentence is deliberate and worth keeping: it explains to a reader why
`data_dir()` looks the way it does.

## 3. The shipped binary: 4 occurrences of the string

`target/release/parle.exe` contains the literal `EchoKey` 4 times. These are the
string constant on line 709 and the three log messages, compiled in. **They are
not branding and are never displayed**: nothing renders them, they appear only
in `parle.log` and only on a machine mid-migration.

A note on method, because the obvious check misleads: grepping
`Parle_0.1.0_x64-setup.exe` finds nothing, but that is **not** evidence the
installer is clean. `parle.exe` is LZMA-compressed inside it, so the string is
simply not greppable there. The binary is the thing to check, and it was.

`parle-hook.exe` is genuinely clean: 0 occurrences.

## 4. `target/`: 6,289 files, and this is what was seen

```
target/release/echokey.exe            target/release/echokey_lib.dll
target/release/echokey_lib.rlib       target/release/echokey_lib.pdb
target/debug/deps/           5,641 files
target/debug/.fingerprint/     445 files
target/release/.fingerprint/    44 files
target/release/deps/            31 files
```

Every one is dated **22 to 27 August**, i.e. before the rename commit `0c596e7`
landed here on the 28th. Cargo keys artifacts by crate name and simply leaves
behind the ones whose crate no longer exists; it never garbage-collects them.

These are **gitignored, never shipped, and regenerated on demand**. One command
removes all of them:

```bash
cargo clean
```

The only cost is the next build being a cold one, which for this project with
CUDA is roughly 10 minutes. Nothing else is lost.

**Also in there:** `target/x86_64-apple-darwin/` holds macOS artifacts including
`echokey-hook` and `echokey_hook`. Worth knowing how a Darwin target directory
came to exist inside the Windows checkout, since nothing here builds for it.

## 5. Windows Firewall: 10 stale rules. The one thing worth fixing

Windows prompted during `cargo test` runs and kept a rule per test binary:

```
echokey_sync-0823063ccf6c05c4.exe      (x2, inbound allow)
echokey_sync-28de37c6229a6880.exe      (x2, inbound allow)
echokey_sync-9893ef79f778f41e.exe      (x2, inbound allow)
echokey_sync-a6b8d4c54c7ec370.exe      (x2, inbound allow)
echokey_sync-f87729ef5aae4c05.exe      (x2, inbound allow)
```

All point at `target\debug\deps\...` paths that no longer exist. There are 8
more of the same kind under the new names (`parle_sync-*`, `parle_lib-*`,
`r3_lifecycle-*`), so the pattern will keep growing with every test run that
opens a socket.

They are inert, since the executables are gone. They are still worth removing:
they carry the old name in a system-level list, and a rule pointing at a path
inside a build directory is a rule that could later match a DIFFERENT binary
built to the same path.

Deleting a firewall rule is a security setting, so this is left for you:

```powershell
Get-NetFirewallApplicationFilter |
  Where-Object { $_.Program -like "*\target\debug\deps\*" } |
  Get-NetFirewallRule | Remove-NetFirewallRule
```

Review the list before running it. The rule for
`C:\users\benjamin\appdata\local\parle\parle.exe` is the real one and must stay.

## 6. `%LOCALAPPDATA%\EchoKey`

Still present, holding `history.db` (217 KB), its WAL (4.2 MB), `settings.json`
and `parle-hook.log`. This is the copy left in place deliberately after the
history recovery earlier tonight.

`data_dir()` no longer looks at it: `%LOCALAPPDATA%\Parle` is now "occupied", so
the migration returns immediately without consulting the old folder. It is dead
weight, kept only as a safety net. Delete it once you are satisfied the 131 rows
in the live database are everything you expect, and not before.

## 7. Confirmed clean

Checked and found free of the old name:

- Installed app directory `%LOCALAPPDATA%\Parle`
- Registry: uninstall entry is `Parle`; no autostart Run key entry
- Start menu and Desktop shortcuts: `Parle.lnk`
- Crate names: `parle`, `parle_lib`, `parle-core`, `parle-asr`, `parle-audio`, `parle-sync`, `parle-hook`
- Bundle identifier: `com.novaire.parle`
- mDNS service type: `_parle._tcp.local.`
- Keychain / credential service: `Parle sync`
- Git branches and remotes: `main`, `windows-build`
- `parle-hook.exe`
- Frontend (`src/`), shared contract vectors, scripts

## 8. macOS, scanned 30/08/2026

The Mac is **clean in the places that matter and dirty in four that do not**,
with one exception that turned out to be a live bug rather than litter.

### 8a. The autostart LaunchAgent is broken, and has been since the rename

`~/Library/LaunchAgents/EchoKey.plist` is still present AND still loaded:

```
Label              EchoKey
ProgramArguments   /Applications/EchoKey.app/Contents/MacOS/echokey --hidden
RunAtLoad          true
last exit code     78: EX_CONFIG
```

That binary does not exist. There is **no `Parle.plist` beside it**, so nothing
launches Parle at login, while `settings.json` says `launch_at_login = true`.
The setting is showing a state the system is not in: the toggle reads as on, the
agent that would honour it is a dead reference to a deleted app, and it has
failed at every login since the rename.

The autostart plugin names the agent after the app, so the fix is to toggle
"launch at login" off and on in Settings, which writes a correct `Parle.plist`,
and then remove the stale one:

```bash
launchctl bootout "gui/$(id -u)/EchoKey" 2>/dev/null
rm ~/Library/LaunchAgents/EchoKey.plist
```

Verify with `launchctl list | grep -i parle` and confirm a `Parle.plist` exists.

### 8b. Orphaned state under the old bundle id, safe to delete

The new-id equivalent of each of these already exists and is the one in use, so
these are dead copies, not the live ones:

| Path | Size | Last touched |
|---|---|---|
| `~/Library/Caches/com.novaire.echokey` | 8 KB | 21/08/2026 |
| `~/Library/Preferences/com.novaire.echokey.plist` | 4 KB | 22/08/2026 |
| `~/Library/WebKit/com.novaire.echokey` | 228 KB | 21/08/2026 |
| `~/Library/Application Support/CrashReporter/echokey_59A668DD-*.plist` | small | pre-rename |

### 8c. A stale microphone permission under the old bundle id

`TCC.db` holds `kTCCServiceMicrophone` for BOTH `com.novaire.echokey` and
`com.novaire.parle`. The old row is orphaned: no app claims that identifier any
more. `TCC.db` is SIP-protected and must not be edited directly. If the entry
shows up under System Settings > Privacy & Security > Microphone, remove it
there; otherwise it is inert.

### 8d. An `EchoKey Dev` certificate in the SYSTEM keychain

`security dump-keychain` finds a self-signed code-signing certificate labelled
`EchoKey Dev` in `/Library/Keychains/System.keychain`.

**It is not in use.** `security find-identity -v` reports *0 valid identities*,
and the installed `/Applications/Parle.app` is **adhoc** signed
(`Signature=adhoc`, `TeamIdentifier=not set`). So nothing depends on it.

Two things follow, and they pull in opposite directions, so do not treat this as
simple litter:

1. Deleting a certificate is a security setting and is left to you. It lives in
   the System keychain, so it needs an admin authorisation in Keychain Access.
2. More importantly, HUMAN_TASKS.md §2 still wants a stable **`Parle Dev`**
   certificate to exist, because adhoc signing is exactly what makes macOS TCC
   forget the Accessibility grant on every rebuild. The old certificate is the
   dead remains of that mechanism. Removing it is fine; removing it and not
   creating `Parle Dev` leaves the underlying problem in place.

### 8e. Confirmed clean on macOS

- `~/Library/Application Support/EchoKey`: **gone.** The migration ran and
  `~/Library/Application Support/Parle` is live at 2.2 GB. `data_dir()` worked.
- No `EchoKey.app` in `/Applications` or `~/Applications`.
- Login Items: no EchoKey entry.
- Keychain: no `EchoKey sync` generic-password items. `Parle sync` is present
  and holds the device identity.
