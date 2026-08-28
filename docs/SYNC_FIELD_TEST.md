# Parle sync, field test

The sync feature has never run between two physical machines. Everything in the
suite is single-machine, over loopback sockets. This file records what has been
checked on real hardware, what has not, and exactly how to do the rest.

Keep it honest: record what happened, including the boring result. A line saying
"tried, worked, took 2s" is worth more than an untested assumption.

---

## Environment

| | |
|---|---|
| Mac | macOS 26.5.1, Apple silicon |
| Windows | not present in this session |
| Date of the macOS checks below | 27/08/2026 |

`env.sh` is gitignored and machine-specific. On macOS it needs almost nothing:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
```

The Tauri sidecar is not built by `cargo test`, so a fresh checkout fails to
build `parle` with "resource path `binaries/parle-hook-<triple>` doesn't
exist" until you stage it once:

```bash
node scripts/build-hook.mjs
```

---

## Done: the two macOS unknowns

Both were listed in `SYNC_HANDOVER.md` as the things most likely to bite, and
both now have an answer on real hardware rather than a guess. Each is a
`#[ignore]`d diagnostic so it stays out of the ordinary suite and can be re-run
on any new machine.

### 1. mDNS discovery works, VERIFIED 27/08/2026

```bash
cargo test -p parle-sync --test mdns_field_check -- --ignored --nocapture
```

Two independent `Discovery` instances found each other on the real LAN
interface (172.16.0.65) in under a second. The `mdns-sd` crate's own stack does
not conflict with macOS's mDNSResponder on this version.

**A real blocker was found and fixed while checking this.** `Info.plist`
declared neither `NSBonjourServices` nor `NSLocalNetworkUsageDescription`. On
macOS 14 and later the system filters an app's mDNS traffic outright without the
first, and has no explanation to show in the permission prompt without the
second. The failure is silent, discovery reports no error and simply never
finds anyone, so the first two-machine attempt would have looked like a
mysterious "they cannot see each other". Both keys are now in `Info.plist`.

Caveat that is NOT settled: the diagnostic runs from the terminal, which already
holds local-network permission. The bundled, signed app asks for its own. Expect
a prompt on first launch, and check **System Settings > Privacy & Security >
Local Network** if discovery finds nothing.

### 2. The keychain is silent, VERIFIED 27/08/2026

```bash
cargo test -p parle --test keychain_field_check -- --ignored --nocapture
```

Store, read, re-read and delete against the real login keychain: first read
1.0ms, second read 0.7ms, no prompt. A missing entry reports `NoEntry`, which is
what `keystore::load` relies on to mean "not paired" rather than "the credential
store is broken". There is no prompt storm here.

Caveat that is NOT settled: keychain items are ACL'd to the identity that wrote
them. This ran as a plain `cargo test` binary. A rebuilt or re-signed app bundle
may be refused access to items an earlier build stored, and a refusal surfaces in
the UI as "this device is not paired". If pairing mysteriously evaporates after a
rebuild, delete the `Parle sync` items from Keychain Access and pair again.

---

## Before the field test: two things that are still true

Neither is a code defect, and both change what step 5 will show you.

**Accessibility permission decides whether a password-field dictation is kept.**
The secure-field gate asks the accessibility tree what the user is typing into.
With that permission denied it cannot tell, and a dictation into a password field
IS stored. Parle needs the permission for paste-at-cursor anyway, so grant it on
both machines before testing, or you will be testing the degraded path without
knowing.

**On Windows the gate sees classic Win32 password fields only.** A WinUI
PasswordBox or a browser password input is not recognised, so a dictation into
one is stored and replicated. That gap is written into
`platform::windows::focused_field_is_secure` and needs UI Automation to close.
Do not read step 5 passing as proof that secure fields are covered on Windows.

## Not done: the milestone

**Two machines have still never synced.** Nothing below has been run. It needs a
second physical device, which this session did not have.

Work through it in order and record the result of each step in this file. Steps
1 to 3 are prerequisites; step 6 is the one most of the design effort went into
and the one most likely to surprise.

1. **Build and run on each machine.** Confirm the app starts and sync can be
   switched on. On macOS, accept the local-network prompt.
2. **Discovery.** Confirm each machine lists the other. If not, work down the
   remediation list in the failure message of `mdns_field_check`.
3. **Pair.** One machine shows a 6 digit code, the other types it. That is the
   only asymmetry: it decides who dials, not who sends. Pairing is mutual and
   history flows BOTH ways from the first exchange, which is what step 4 and
   step 5 are checking. The earlier wording here said the showing machine "only
   receives", which is true of the connection and false of the data, and a
   tester who believed it would read the next two steps as failures.
4. **A dictation on one appears on the other.**
5. **A copy on one appears on the other**, with the clipboard kind switched on.
6. **Delete on the RECEIVING device and confirm it disappears on the author.**
   This is the path the authority rules are built around, deletes travel for
   every source while content travels only from its author, and it has never
   run for real.
7. **Toggle a sync kind off and on.** Confirm the one-shot re-offer backfills
   both directions rather than leaving a hole.
8. **Widen the retention window.** Same question: `set_retention_days` resets
   receipts on a widening, and that repair has only ever been tested in-process.
9. **Kill one app mid-exchange.** Confirm the other recovers and no row is lost.
10. **Set one machine's clock 5 minutes fast.** Rows must be REFUSED with a
    warning naming the device, not silently accepted; correcting the clock must
    restore sync. Five minutes is chosen to sit outside `MAX_CLOCK_SKEW_MS`,
    which is two minutes.
11. **Delete two synced rows in quick succession on the receiving machine.**
    This is the sequence that used to lose the second delete, before
    `delete_clock` made a local delete's clock strictly exceed every tombstone
    already held for that source. Worth doing by hand because the failure was
    invisible: the delete simply never arrived.

12. **Set one machine's clock a day fast, write a few rows, then correct it.**
    This is the sequence three consecutive rounds got wrong. Everything written
    while the clock was wrong may never reach the other machine, which is
    accepted. What must be true is that the machine RECOVERS: rows written after
    the correction must arrive, and Clear History must still propagate. If they
    do not, the ceiling clamp in `Store::next_clock_impl` has regressed and
    section 3b of the handover explains why it is shaped the way it is.

## Also still open on Windows

From `docs/WINDOWS_HANDOFF.md`, unchanged by this session:

- Windows ASR benchmarks have never been run; `docs/BENCHMARKS.md` has M2 Metal
  numbers only and its Windows section is a prediction.
- Win+V clipboard-history exclusion is implemented but unverified on hardware.
- Parakeet on Windows unverified; a clean-account NSIS install is untested.
- Linux has not been attempted.
