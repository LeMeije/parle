# Parle sync — handover

You are picking up a LAN sync feature mid-flight. This file is the whole brief:
what exists, why it is shaped the way it is, exactly what is unfinished, and how
to carry on. Read it end to end before touching code. It is long on purpose —
the reasoning behind several decisions is not recoverable from the diff, and
re-deriving it wrongly has already cost this project two regressions.

Written on Windows. **The next session is expected to be on macOS**, which
matters more than usual — see [Working on the Mac](#working-on-the-mac).

---

## 1. What the feature is

Parle is a Tauri 2 + Rust on-device dictation and clipboard-history app.
Cross-machine sync lets a user's own devices share that history over the LAN.

`docs/SYNC_DESIGN.md` holds the binding requirements. The ones that constrain
every decision below:

- **No cloud, no relay servers, no accounts. LAN-local only.**
- **End-to-end encrypted.** Pairing is explicit and human-verified.
- **Paired keys live in the OS keychain, never in `settings.json`.**
- Password-manager exclusions and Concealed/Transient etiquette apply *before*
  anything leaves the machine — never sync what we would not even store.

That last point is why so much of the work below is about deletes. If a user
clears a password from their history, it has to be gone everywhere. A sync
feature that loses deletes is worse than no sync feature.

### Where the code lives

| Path | What |
|---|---|
| `crates/echokey-core/src/history.rs` | SQLite store: schema, migrations (v1→v5), conflict resolution, cursors, caps |
| `crates/echokey-core/src/settings.rs` | `SyncSettings`, `PairedDevice`, `ResendDebt` |
| `crates/echokey-sync/src/wire.rs` | Message shapes, size limits, `validate()` |
| `crates/echokey-sync/src/pairing.rs` | SPAKE2 pairing primitives |
| `crates/echokey-sync/src/session.rs` | Noise `NNpsk0` session |
| `crates/echokey-sync/src/discovery.rs` | mDNS (`_echokey._tcp`) |
| `src-tauri/src/sync/replicate.rs` | The exchange: serve, drain, authority, paging |
| `src-tauri/src/sync/manager.rs` | Lifecycle: listener, dialling, pairing, persistence |
| `src-tauri/src/sync/guard.rs` | Pairing rate limit |
| `src-tauri/src/sync/pair_flow.rs` | Pairing over TCP |
| `src-tauri/src/sync/deadline.rs` | Wall-clock-free deadline wrapper |
| `src-tauri/src/sync/wire_tcp.rs` | Pre-session framing |

---

## 2. Build and test

`env.sh` at the repo root is **gitignored** and machine-specific. On Windows it
sets the cargo path, CMake, `LIBCLANG_PATH` (bindgen needs it), and
`CUDA_PATH_V13_3` (MSBuild resolves the toolkit through the versioned variable,
not `CUDA_PATH`). **You must `source ./env.sh` before every cargo command** on
Windows.

**On macOS you will need to write your own `env.sh`** — probably near-empty,
since the Mac build needs no CUDA and no LLVM path. Start with:

```bash
# env.sh on macOS — adjust if your toolchain differs
export PATH="$HOME/.cargo/bin:$PATH"
```

Test commands used throughout:

```bash
source ./env.sh && cargo test -p echokey-core --lib
```

```bash
source ./env.sh && cargo test -p echokey --lib sync::
```

```bash
source ./env.sh && cargo test -p echokey-sync
```

**Do not run `cargo test --workspace` bare.** With the current test count it
exceeds a 10-minute timeout on this machine. Run per package.

---

## 3. Exact state as of this handover

Branch `windows-build`, HEAD `187336e`. Ten sync commits, oldest first:

```
d3ddd47  receipts replace derived watermarks; turn-taking ends the first-sync deadlock
d7f666c  remove relaying, and stop deriving watermarks from local state
60f415b  close the pre-session slow-loris, the pairing MITM, six lifecycle defects
5dcda41  LAN-only dialling, prune after every exchange, unpair stops live sessions
6675ecb  receipts per peer, relayed edits and deletes restored, absorbing tombstones
9a1bcb0  pairing survives a LAN attacker, and pre-auth work is bounded
45e3765  lifecycle — re-pairing works again, revocation takes effect mid-exchange
5cff8af  round 3 — authority on the kind we hold, deletes that survive, bounded decode
337e49c  round 4 — keyset paging, uuid identities, authority over our own rows
fcd0712  a stop can no longer be undone by the start it interrupted
187336e  content only from its author; deletes still travel everywhere
```

### Test results

| Package | Result |
|---|---|
| `echokey-core --lib` | **97 pass, 1 fail** |
| `echokey-sync` (all targets) | **35 pass, 0 fail** |
| `echokey --lib` | **121 pass, 10 fail, 1 ignored** |

Every failure is a round-6 adversarial-review test. **No failure is a
regression of previously-passing behaviour** — the suite was fully green at
`187336e`, and these tests were added afterwards by reviewers.

### Uncommitted working tree

```
 M crates/echokey-core/src/lib.rs        (registers the reviewer test module)
 M src-tauri/src/sync/manager.rs         (MY round-6 fixes + reviewer tests)
 M src-tauri/src/sync/mod.rs             (registers reviewer test modules)
?? crates/echokey-core/src/adversarial_r6_data.rs
?? src-tauri/src/sync/adversarial_r6.rs
?? src-tauri/src/sync/adversarial_r6_sec.rs
```

`manager.rs` contains **unreviewed production fixes of mine** (section 6) mixed
with reviewer test modules. Read the diff before committing.

---

## 4. The design, and why it is this shape

This is the part that is not recoverable from the code. Several of these rules
replaced an earlier rule that looked more obviously correct and was not.

### 4.1 Identity

A row is identified across machines by `(source_machine, origin_id)`.

`source_machine` is the device's stable UUID from `settings.json`.
`origin_id` for **new local rows is a random UUID** (`stamp_origin`).

> **Why not the rowid?** It was, and that was a permanent-kill bug. A rowid is
> unique within one database file, while the device id it pairs with lives in a
> *different* file with a different lifetime. Lose or rebuild `history.db` and
> rowids restart at 1, so every new dictation is born wearing an identity a peer
> has already seen — overwriting unrelated content there, or swallowed outright
> where the peer holds a tombstone, since tombstones are absorbing. Sync dies
> permanently with nothing to show the user why.
>
> Rows **migrated** from an older schema keep their rowid identity, deliberately:
> a peer may already know them under it.

### 4.2 Content authority — the single most important rule

**Only the device that WROTE a row may change its content.**

- `serve` offers only rows where `source == me`.
- `drain` accepts items only when `source == peer_id`.
- Editing a peer's row (pin, text correction) is **local-only** and does not
  travel back.

> **Why.** Two earlier rules failed. "Any source in our paired roster" let one
> compromised device author rows as another. "An update to an identity we
> already hold" survived two rounds and is also wrong: we hold every row we ever
> wrote, and we hand a peer the identity of every row we sync to it, so a paired
> device could rewrite our history in place — and a third device's *through* us,
> because `serve` offered it every source we held and the edit came back through
> the same door, to be relayed onward.
>
> **The cost is real and was accepted knowingly:** pinning or correcting a
> dictation on a machine that did not record it stays local. If a future
> requirement needs that to sync, it needs a different mechanism (a signed
> per-device edit log), not a loosening of this rule.

### 4.3 Deletes are exempt, and must be

Tombstones are served for **every** source, and accepted when:

- the sender is the **authoring device** (may delete its own row even one we
  have never seen — a delete legitimately overtakes the row it deletes), **or**
- we **already hold** the identity (a relayed delete for a row we have).

Anything else is a **temporary refusal that banks no receipt** (see 4.4).

An author's delete **bypasses the kind gate**; a relayed delete does not.

> **Why.** A password cleared on the laptop has to vanish from the machine that
> recorded it, and a delete carries no attacker-chosen content, so it is safe to
> accept where an edit is not. The kind-gate exemption exists because switching a
> sync kind off must not turn a machine into a place deletes go to die.

**Tombstones are ABSORBING**: once an identity is deleted, no copy comes back,
whatever its clock says. Last-writer-wins used to let an edit stamped after a
delete resurrect the row — defensible for a shared document, wrong here. There
is no undelete anywhere in the product, so "I deleted it" has to mean it.

### 4.4 Cursors (`source_marks`) — keyed by (peer, source)

A "receipt" records **what a given peer has handed us**, per source device.
Schema v5: `source_marks(peer_machine, source_machine, received_clock)`.

Rules, each of which replaced a bug:

- **Never derived from the rows we hold.** Every version that computed a mark
  from live rows could be walked backwards by ordinary housekeeping (retention,
  eviction, Clear History, a pruned tombstone), which makes a peer re-send the
  same rows forever.
- **Keyed by peer, not by source alone.** Keyed by source, any peer could move
  another device's cursor by relaying one row for it, hiding whatever that
  device wrote below the mark.
- **No receipt for a refusal we might reverse.** A cursor is a promise never to
  ask for anything at or below it again. Banking one for a tombstone we refused
  because we do not hold the identity made that delete unreachable *for ever*
  once the row arrived from its author.
- **No receipt for an out-of-range clock.** `note_received` refuses clocks `<= 0`
  or `> now + MAX_CLOCK_SKEW_MS` internally, so no caller can poison a cursor.
- **Capped per peer** (`MAX_SOURCES_PER_PEER`), oldest evicted — but **never the
  peer's own cursor**, which is the one it serves against and the one a spray of
  invented source ids would aim at.

### 4.5 Clock skew — 2 minutes, refuse beyond it

`MAX_CLOCK_SKEW_MS = 2 * 60 * 1000`.

> **Why so tight?** The cursor we keep for a peer *is* that peer's clock, so
> anything we accept into it we are stuck with. At 24 hours, a machine half a
> day fast wrote a cursor half a day ahead, and once its clock was corrected
> every row it produced fell below that cursor and was never requested again —
> silently. Beyond the window, rows are **refused with a warning naming the
> device**: a complaint the user can act on, instead of silent loss after the fix.

### 4.6 Paging — a `(clock, origin_id)` keyset cursor

`items_from` / `tombstones_from`. `items_since` is exclusive via an
`ORIGIN_CEILING` sentinel that sorts above every origin id.

- **Every full page is trimmed back to a millisecond boundary**, not just the
  truncated one. The peer records the highest clock it sees and the next
  exchange asks strictly above it, so a run that stops *between* pages —
  unpair mid-exchange, sync switched off, network drop — would otherwise park
  the cursor inside a millisecond, and nothing lowers a cursor.
- A truncated **item** pass caps the **tombstone** pass at where it stopped;
  they share one cursor on the wire, so a later tombstone would otherwise carry
  the cursor over items that were never sent.

> Paging on the clock alone could not express "half of this millisecond". The
> old code only noticed when an *entire* page shared a clock, then re-fetched up
> to 20,000 rows in one statement — missing the common case *and* letting a peer
> freeze the history UI for most of a second, since `updated_at` is stored
> verbatim from the peer.

### 4.7 Turn-taking

`Turn::First` (the dialler) serves then drains; `Turn::Second` drains then
serves. Exactly one side writes at any point.

> Both peers used to send their entire history before either started reading.
> That works only while everything fits in the socket and Noise buffers; a first
> sync of any real size filled them and both sides blocked in `write`. **Verified
> by setting both peers to `Turn::First` — the test hangs past ten minutes.**
> Read timeouts do not save you here, which is why the test sockets set write
> timeouts too.

### 4.8 Conflict resolution

1. A tombstone is absorbing (4.3).
2. Otherwise last-writer-wins on `updated_at`, strictly greater.
3. On an exact tie, the tiebreak is over the **whole payload**:
   `(text, pinned, created_at, kind)`. It must be total, or two devices settle
   on different rows for the same identity and neither ever updates.

Local edits (`set_pinned`, `update_text`, `delete_item_local`) stamp
`max(now, row_clock + 1)`, so a user's edit always wins the conflict it is about
to enter. A bare `now_ms()` meant an edit of a row from a slightly-fast peer was
born losing and was silently reverted by the next echo.

### 4.9 The one-shot re-offer

Widening a sync kind has to backfill **both** directions:

- **Inbound**: `reset_source_marks()` clears our receipts (durable, in SQLite).
- **Outbound**: every paired device is owed one full re-offer (`resend_owed`,
  persisted in `settings.json` as `ResendDebt`), because the rows our outbound
  filter suppressed sit below the mark the *peer* keeps for us and we cannot
  reach into its receipts.

A truncated re-offer records `resend_progress` and keeps the debt; only
**truncated** sources constrain the resume point (including every source pinned
the cursor at the start, so it never advanced).

### 4.10 Wire

Externally tagged JSON (`{"items": {...}}`) with `deny_unknown_fields` on the
enum **and** on `SyncItem` / `Tombstone` / `Watermark`.

> This is a memory-safety decision, not style. An internally tagged enum forces
> serde to buffer the whole message into its `Content` tree before it can read
> the tag, so the batch limit never saw an oversized batch until after it had
> been materialised — 17 MB from a refused message, ~16x amplification from an
> ignored unknown field. There is a counting-allocator test at
> `crates/echokey-sync/tests/adv3_decode_allocation.rs` that guards this.

`PROTOCOL_VERSION = 3`; `NOISE_PROLOGUE = b"echokey-sync/noise/3"`.
`serve` and `drain` share `MAX_EXCHANGE_MESSAGES`.

### 4.11 Pairing and admission

- SPAKE2 pairing; identity is exchanged **inside** the Noise session (as
  `SyncMessage::Hello`), never on the raw socket.
  > An on-path attacker used to relay the SPAKE2 messages and confirmation tags
  > verbatim — never learning the key, both sides verifying — and rewrite only
  > the cleartext identity frame, so both machines paired on a shared key filed
  > under an attacker-chosen id and name.
- `PairingGuard`: `MAX_PER_SOURCE = 3` with 1s/2s/4s backoff,
  `MAX_PER_CODE = 12` for sources that have already guessed, and a **reserved
  first guess for any source not yet seen**, bounded by
  `HARD_MAX_PER_CODE = 200`.
  > Keyed by **exact address, deliberately**. Folding to a network prefix (/24,
  > /64) looks like obvious hardening and destroys the carve-out: on a home LAN
  > the user's own second device sits in the *same* prefix as the attacker.
- `admit_inbound(in_flight, already_here)`: the global `MAX_INBOUND` pool is
  consulted only for an address already being served; an unseen address is
  admitted up to `MAX_INBOUND_HARD`.
- `PreauthGuard`: per-address cap on concurrent pre-auth connections;
  `PREAUTH_TIMEOUT` 3s; the socket is wrapped in `Timed` **before the first byte**.
- Dialling is restricted to private/LAN ranges. Paired peers are re-dialled on
  `DIAL_RETRY_AFTER`; a paired device is never evicted from the peer map by
  unknown mDNS records.

### 4.12 Accepted residuals — do not "fix" these without reading the reasoning

These are deliberate. Each was argued, and in two cases a reviewer's proposed
fix was **rejected** as unachievable or actively harmful.

| Residual | Why it stays |
|---|---|
| An attacker with 200+ LAN addresses can retire **one** pairing code. | You cannot both bound total guesses and always admit an unseen source, when the attacker can mint addresses. A budget that always admits an unseen source is not a budget. The user shows another code; a fresh code is independently random. Win chance is 2e-4 per code. |
| Any fixed inbound ceiling is reachable by minting addresses. | True of every fixed number. What matters is that **outbound dialling is unaffected**, so sync still completes — which is why the dial-retry and no-evict-paired-peers rules exist. |
| Device ids are public in mDNS TXT records, so a paired-device enumeration oracle exists. | Already documented in `SYNC_DESIGN.md`. Fixing it needs authenticated discovery. |
| Non-author content edits do not propagate. | See 4.2. Deliberate. |
| Tombstone cap eviction can, in principle, lose a delete for a device offline across the whole overflow. | The alternative — an unbounded table a paired peer fully controls — is certain harm. Newest deletes are kept. **But see finding D1 in section 5: a reviewer thinks this is reachable more easily than believed.** |
| Prefix folding for IP budgets. | Rejected: puts the user's own device in the attacker's bucket. Recorded in `guard::network_of`, which is intentionally unused. |

---

## 5. Open findings — what is actually broken right now

Ten failing tests plus one ignored. **Each needs to be triaged before it is
fixed**, because past experience is that roughly half of reviewer tests encode a
premise that is wrong, or inline an old copy of the logic they claim to test.

### Triage rule

For each failing test, decide which of these it is:

1. **A real defect** → fix production code, keep the test.
2. **A test that inlines outdated logic** → rewrite it to call the production
   function. (`admit_inbound`, `make_room_for_peer`, `note_peer_record` were
   extracted into pure functions precisely so tests can exercise the real rule.)
3. **A test asserting something we deliberately rejected** → rewrite it to
   assert the achievable contract, and record the reasoning in the test body.
   Do **not** silently delete it.

### `echokey-core --lib` — 1 failure

**D1. `adversarial_r6_data::r6_the_tombstone_ceiling_drops_undelivered_local_deletes_and_the_row_returns`**

Claims the tombstone ceiling (4.6) drops local deletes permanently and the row
returns. **I traced the logic and believe the deletes are delivered on a later
exchange rather than dropped, but I did not verify it and I am not asserting
it.** The ceiling only applies when the item pass truncated; on the next
exchange the item cursor has advanced so the ceiling rises. Confirm or refute
with a bounded test before changing anything.

### `echokey --lib` — 10 failures

Round-6 data reviewer (`sync::adversarial_r6`), all unverified by me:

- `r6_a_local_pin_on_the_peer_permanently_swallows_the_authors_correction`
  — plausible and important: interacts directly with 4.2 and 4.8. A local pin
  stamps `row_clock + 1`, which may then beat the author's later correction.
- `r6_a_tombstone_for_an_unreachable_source_never_stops_being_re_sent`
  — the flip side of "no receipt for a temporary refusal" (4.4). If real, it is
  an unbounded-transfer defect (criterion C). **Expect a genuine tension here:
  banking the receipt resurrects deletes; not banking it may loop.** The answer
  is probably a bounded retry rather than either extreme.
- `r6_the_dead_tombstone_loop_is_not_an_artefact_of_clear_history`
- `r6_widening_retention_never_refetches_what_the_narrow_window_refused`
  — retention widening has no counterpart to the kind-widening re-offer (4.9).
  Likely real.

Round-6 security reviewer (`sync::adversarial_r6_sec`), all unverified:

- `r6sec_a_device_name_the_ui_accepts_can_disable_sync_entirely`
  — a device name the settings layer accepts but `validate_device_name` rejects
  makes every `Hello` unsendable. Likely real and easy to fix (validate at the
  settings boundary).
- `r6sec_a_peer_can_make_us_advertise_a_watermark_for_our_own_device`
- `r6sec_an_ordinary_relayed_delete_makes_us_advertise_a_mark_for_ourselves`
  — these two may be **expected behaviour**: a `(peer, our_own_id)` entry is
  meaningful under 4.4 and there is a passing test asserting exactly that
  (`a_peer_never_offers_us_rows_we_authored`). Read both before changing code.
- `r6sec_tombstones_a_peer_can_never_accept_are_not_re_offered_forever`
  — same family as the data reviewer's second finding above.

Round-6 concurrency reviewer (`sync::manager::adversarial_r6_conc`):

- `r6_a_toggle_inside_stop_leaves_sync_reading_on_with_nothing_running`
  — **I fixed the underlying defect** (section 6) but the test still fails.
  Determine whether the test inlines the old two-critical-section shape.
- `r6_alternating_addresses_cannot_buy_unlimited_dials`
  — **I fixed this too**; the test now yields **2 dials where it asserts 1**.
  My reading: 2 is correct — one legitimate initial dial plus one legitimate
  address-change retry — and the test does not account for the move retry being
  a feature. Either tighten the rule or rewrite the test, but decide explicitly.

### Ignored

- `sync::adversarial_r6::r6_clear_history_loses_deletes_when_a_peer_tombstone_arrives_first`
  — **HANGS.** Written by a reviewer that a session limit cut off before it
  bounded its loop; it blocked the entire suite. I marked it `#[ignore]` with the
  reason inline. The claim is plausible and unproven. **Re-derive it from scratch
  with a hard iteration bound and socket read+write timeouts.**

---

## 6. My own uncommitted changes — review these

In `src-tauri/src/sync/manager.rs`, not yet committed, fixing round-6
concurrency findings:

1. **`stop()` takes all its handles in ONE critical section, before the wait.**
   Previously `listen_stop` stayed `Some` through a 5-second wait, so a
   `set_enabled(true)` landing in that window hit "already running" at the top of
   `start()`, returned `Ok`, and did nothing — for a listener `stop()` was about
   to destroy. The `stop_epoch` check could not catch it because it is only read
   by a `start()` that got *past* that entry test.
2. **`note_peer_record` rate-limits its own retry reset** (`last_move` map).
   My round-5 fix cleared `last_dial` on *every* address change, so an attacker
   alternating two addresses made every announcement read as "due" — unlimited
   dials, which removed the very mitigation that makes inbound saturation
   survivable.
3. **`persist()` takes one snapshot under one lock.** It was reaching into
   `inner` from under the `settings` guard — the exact order inversion its own
   comment warned against — and locking `inner` three separate times, so the
   snapshot it wrote could be torn.
4. **`Builder::spawn` instead of `std::thread::spawn`** for the accept and dial
   threads (the latter panics on OS thread-creation failure, which would unwind
   the loop with `listen_stop` still installed), and `DialGuard::new` is
   constructed **before** the spawn so a failed spawn still releases the slot.

---

## 7. Process rules — these are not optional

Learned the hard way in this session.

### 7.1 Verify that an edit actually landed

**A production fix was silently lost.** A reviewer editing `replicate.rs`
concurrently overwrote it from a stale read; my patch script printed "ok" and I
moved on. The hole was reported as still-open two rounds later.

After any edit to a file a reviewer may also be touching:

```bash
git show HEAD:path/to/file.rs | grep -c "distinctive string from your change"
```

and confirm the working tree matches what you believe you wrote.

### 7.2 Never leave a test that can hang

Every test that opens a socket must set **read *and* write** timeouts — a
write/write stall never reaches a read. Every loop needs a hard iteration bound.
One unbounded reviewer test blocked the entire suite for ten minutes.

### 7.3 Reviewer tests are evidence, not instructions

Roughly half the reviewer tests in this session either inlined a stale copy of
the logic they claimed to test, or asserted a property that cannot be built.
Always ask: *does this test call the production function?* If it reimplements
the gate inline, extract the gate into a pure function and point the test at it.

### 7.4 Fixes have been regressions

Three separate times, a fix for round N introduced the defect found in round
N+1 (relay removal broke delete propagation; `floor_for` lost deletes; the
address-change retry became a dial amplifier). **Re-run the full per-package
suite after every change**, and be most suspicious of the newest code.

---

## 8. How to run the adversarial review

The review process is the deliverable as much as the code. Three reviewers per
round, run as background agents, each with a distinct scope: **data integrity**,
**security**, **concurrency/lifecycle**. Each brief must:

- Demand **running Rust tests**, not opinions. "Anything you cannot demonstrate
  must be labelled SPECULATIVE with what stopped you."
- State the **accepted residuals** (4.12) explicitly, so the reviewer does not
  re-report them: *"do not report these unless you can show something worse."*
- Require **new files or uniquely-prefixed modules** (`mod adversarial_r7_data`),
  never rewriting a whole existing file — see 7.1.
- Require read+write socket timeouts and bounded loops — see 7.2.
- Note that `SyncManager` holds `tauri::AppHandle<Wry>` and **cannot be built
  with `MockRuntime`**, so manager-level integration tests do not compile.
  Reviewers should extract logic into pure functions or mark findings read-only.
- End with exactly one line: `VERDICT: PASS` or `VERDICT: FAIL`.

Pass criteria that have been used (keep them):

- **A.** No 2-or-3-device sequence produces divergence, a lost row, or a
  resurrected delete.
- **B.** Migrations v1..v4 → v5 preserve every row and yield a schema identical
  to a fresh v5, including when interrupted.
- **C.** No sequence causes unbounded repeated transfer; every exchange goes quiet.
- **D.** Hostile input cannot corrupt or crash the store; no panic or overflow
  from peer-controlled values.
- **E.** Every size limit bounds **allocation**, on decode as well as encode.
- **F.** Every network read path is bounded by a deadline a peer cannot extend.
- **G.** Pairing identity cannot be forged or rewritten by an on-path attacker.
- Plus, for concurrency: no lock held across network I/O, no lock-order
  inversion, no re-entrant `parking_lot` lock, no leaked thread/socket/slot on
  panic, manager state and `settings.json` cannot diverge, keys never in
  `settings.json`, and the history UI cannot be blocked unboundedly.

**The bar the user set:** keep going until an impartial reviewer cannot find a
flaw. Do not stop at "tests pass".

---

## 9. Working on the Mac

This matters more than a normal platform switch, because **the entire point of
this feature is cross-platform sync and it has never run between two physical
machines.** Everything so far is single-machine tests over loopback sockets.

### What should just work

The sync stack is platform-independent: `echokey-core`, `echokey-sync`, and
`src-tauri/src/sync/*` contain no `cfg(windows)` code. The macOS build of the
app is the older, more-tested one. Expect `cargo test -p echokey-core` and
`-p echokey-sync` to pass unchanged.

### What to check first on macOS

1. **Write `env.sh`** (section 2) — it is gitignored, so it does not exist in
   your checkout.
2. **Keychain.** `keystore.rs` uses the `keyring` crate. On macOS this is the
   login keychain and **will prompt for permission**, possibly per-access. On
   Windows it is Credential Manager and silent. Watch for: a prompt storm during
   sync, a denied prompt surfacing as "unpaired", and the fact that macOS
   keychain items are ACL'd to the signing identity — a rebuilt unsigned binary
   may be refused access to items written by a previous build.
3. **mDNS.** macOS has its own mDNSResponder. The `mdns-sd` crate runs its own
   stack, which can conflict or be filtered. Also, **macOS 14+ prompts for local
   network access** — if the user declines, discovery silently finds nothing.
   That prompt does not exist on Windows, so it is a failure mode you have not
   seen yet. Surface it in `SyncStatus.error` if you can detect it.
4. **Firewall.** macOS application firewall may block the inbound listener for an
   unsigned binary.

### The real milestone: two machines

Nothing in this feature has been proven end to end. The first genuinely new
information will come from pairing the Mac and the Windows box. Suggested order:

1. Build and run on the Mac; confirm the app starts and sync can be enabled.
2. Confirm the two machines **discover** each other (mDNS across platforms).
3. **Pair** them — the code-showing side is the one that only receives.
4. Dictate on one, confirm it appears on the other.
5. Copy on one, confirm the clipboard row appears (with the kind toggles on).
6. **Delete on the receiving device and confirm it disappears on the author.**
   This is the path most of the design effort went into and it has never run for
   real.
7. Toggle a sync kind off and on; confirm the re-offer backfills.
8. Kill one app mid-exchange; confirm the other recovers and no rows are lost.
9. Set one machine's clock 5 minutes fast; confirm rows are refused with a
   warning naming the device, and that correcting the clock restores sync.

Record results in `docs/BENCHMARKS.md` or a new `docs/SYNC_FIELD_TEST.md`.

### Also still open on Windows (from `docs/WINDOWS_HANDOFF.md`)

- Windows ASR benchmarks have never been run (`docs/BENCHMARKS.md` has only M2
  Metal numbers; its Windows section is a prediction).
- Win+V clipboard-history exclusion is implemented but unverified on hardware.
- Parakeet on Windows unverified; clean-account NSIS install untested.
- Linux not attempted.

---

## 10. Suggested order of work

1. **Commit my uncommitted manager fixes** (section 6) after reading the diff.
2. **Triage the 10 + 1 failing tests** (section 5) using the triage rule. Expect
   about half to be test defects.
3. Fix the real ones. Prioritise, in order:
   - the tombstone re-offer loop (criterion C, two reviewers found it),
   - retention widening with no re-offer (criterion A, silent data hole),
   - the local-pin-swallows-correction interaction (criterion A),
   - the device-name validation gap (easy, disables sync entirely).
4. **Re-run all three reviewers** (section 8) until they return `PASS`.
5. **Then, and only then, take it to two machines** (section 9). Field results
   will probably generate a round of their own.
6. Update `docs/SYNC_DESIGN.md` — it still describes an earlier design in
   places, and this file should eventually be folded into it.
