# Every remaining mention of "EchoKey"

Full scan, 29/08/2026, on the Windows machine. Repo at `main` @ `ca744cf` plus
this session's uncommitted work. **This is a report, not a change**: nothing
below has been renamed or deleted.

## Verdict first

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

## 1. Source code: 5 mentions, all required

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

## 2. Comments and prose: 4 mentions

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

## 8. Not checked

The **macOS machine**. Everything above is the Windows box and the repo on it. A
Mac may still hold `~/Library/Application Support/EchoKey`, an `EchoKey.app`, a
stale Login Item, and `EchoKey sync` keychain entries under the old service name
(the keychain service string is `Parle sync` now, so any pre-rename entries are
orphaned rather than migrated). Same scan is worth running there.
