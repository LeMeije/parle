# Parle sync, handover

LAN sync between a user's own devices. This file is the whole brief: what
exists, why it is shaped the way it is, what is unfinished, and how to carry on.

It replaces the round-6 handover written on Windows, which described eleven
failing tests, a quarantined hanging test and an uncommitted working tree. All
of that is resolved. **What has NOT changed is the one thing that matters most:
this feature has still never run between two physical machines.**

Read `docs/SYNC_DESIGN.md` first, it holds the binding requirements and, since
this round, an accurate description of the rules. Read `docs/SYNC_FIELD_TEST.md`
for what has been checked on real hardware and what has not.

---

## 1. State

Branch `windows-build`. Green, per package:

| Package | Result |
|---|---|
| `echokey-core --lib` | 104 pass, 0 fail, 0 ignored |
| `echokey-sync` (all targets) | 35 pass, 0 fail, 0 ignored |
| `echokey --lib` | 139 pass, 0 fail, 0 ignored |

Nothing is ignored and nothing is quarantined. Two `#[ignore]`d **diagnostics**
exist on purpose and are not part of that count; see section 5.

```bash
source ./env.sh && cargo test -p echokey-core --lib
source ./env.sh && cargo test -p echokey-sync
source ./env.sh && cargo test -p echokey --lib
```

**Do not run `cargo test --workspace` bare**, it exceeds a 10-minute timeout.
Run per package.

`env.sh` is gitignored and machine-specific; on macOS it needs only
`export PATH="$HOME/.cargo/bin:$PATH"`. A fresh checkout also needs the Tauri
sidecar staged once, or `echokey` fails to build with "resource path
`binaries/parle-hook-<triple>` doesn't exist":

```bash
node scripts/build-hook.mjs
```

### Where the code lives

| Path | What |
|---|---|
| `crates/echokey-core/src/history.rs` | SQLite store: schema, migrations v1→v6, conflict resolution, cursors, caps |
| `crates/echokey-core/src/settings.rs` | `SyncSettings`, `PairedDevice`, `ResendDebt` |
| `crates/echokey-sync/src/wire.rs` | Message shapes, size limits, `validate()` |
| `crates/echokey-sync/src/identity.rs` | Device ids, `validate_device_name`, `sanitise_device_name` |
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

## 2. The rules, and why each replaced one that looked right

`SYNC_DESIGN.md` now carries these properly. What follows is the short form plus
the ones added this round.

**Content is author-only.** `serve` offers items for `source == me`; `drain`
accepts items only from the device that wrote them. Pinning or correcting a
peer's row is LOCAL. Two blunter rules failed first: "any source in our paired
roster" let one compromised device author rows as another, and "an update to an
identity we already hold" survived two rounds and let a paired device rewrite our
history in place.

**Deletes are exempt.** Tombstones are served for every source, and accepted from
the author (even for a row we have never seen) or from anyone for an identity we
already hold. A relayed delete for an identity we do NOT hold is refused, that
gate is what stops a paired device tombstoning identities it invented.

**Whether a refused delete banks a receipt** turns on whether the row could ever
arrive. Content reaches us only from its author, so a source that is neither in
our paired roster nor one we already hold rows for can never reach us: the
refusal is permanent and the receipt is safe. Without that, the exchange never
went quiet. Getting it wrong in the other direction loses a delete that arrived
before its row, so `adversarial_r7_scale` pins it from both sides, one test
fails if you never bank, the other if you always do.

**Tombstones are absorbing**, and a tombstone knows whether WE made the delete
(`tombstones.local`, schema v6). The per-source cap evicts replicated entries
only. A replicated tombstone is safe to drop because the peer that sent it still
holds it; a local one is the only record anywhere that the user asked for the row
to go, and evicting it walked deleted rows back in.

**A local delete's clock** comes from `Store::delete_clock`: strictly above every
tombstone already held for that source, bounded at half the skew window. The old
`max(now, row.updated_at)` stamped a delete up to two minutes in the future
whenever the row came from a slightly fast peer, the peer's cursor went there, and
the NEXT delete fell below it and was never offered again.

**A local edit on a peer's row does not move `updated_at`**, because that clock
is what the author's own content is judged against, moving it made an ordinary
pin swallow the author's correction permanently. The row is flagged `local_edit`
(schema v6) instead, and that is what wins the tie against an unchanged echo. The
payload tiebreak stays total for everything else.

**Cursors (`source_marks`) are receipts, keyed by (peer, source)**, never derived
from the rows we hold, never banked for a refusal we might reverse, never banked
for an out-of-range clock, capped per peer with the peer's own cursor never
evicted.

**A device DOES advertise a mark for itself**, and `SYNC_DESIGN.md` used to say
the opposite. That sentence predates author-only content: a peer never sends us
items for our own source now, so a `(peer, our own id)` mark gates exactly one
stream: the deletes that peer relays back about our own rows, which is what a
receipt should gate. Removing it leaves that stream re-offered for ever.

**Widening retention resets receipts.** `drain` banks a receipt before the
retention check, and "retention only ever gets truer" is false: it is a user
setting. `retention_widened` reads 0 ("keep for ever") as the widest window, not
the narrowest.

**Device names are sanitised at the settings boundary.** The wire counts BYTES
and refuses `=`; the UI trimmed to 64 CHARACTERS. "Ben=Work" was stored happily
and then made every `Hello` unsendable and stopped discovery starting.

Paging, turn-taking, pairing admission and the wire's externally-tagged encoding
are unchanged from round 5; `SYNC_DESIGN.md` and the comments in `replicate.rs`
carry the reasoning.

---

## 3. Accepted residuals, do not "fix" these without reading the reasoning

| Residual | Why it stays |
|---|---|
| An attacker with 200+ LAN addresses can retire **one** pairing code. | You cannot both bound total guesses and always admit an unseen source when the attacker can mint addresses. The user shows another code; a fresh one is independently random. Win chance 2e-4 per code. |
| Any fixed inbound ceiling is reachable by minting addresses. | True of every fixed number. What matters is that outbound dialling is unaffected, which is why the dial-retry and no-evict-paired-peers rules exist. |
| An alternating-address attacker gets TWO dials per retry interval, not one. | One initial dial plus one address-change retry. A device that genuinely moved must be reached promptly; demanding one dial removes that. The property that matters is that the total does not grow with the announcement rate, which `r6_alternating_addresses_cannot_buy_unlimited_dials` asserts by comparing 10 announcements against 200. |
| Device ids are public in mDNS TXT records, so a paired-device enumeration oracle exists. | Fixing it needs authenticated discovery. |
| Non-author content edits do not propagate. | Deliberate. A signed per-device edit log is the only correct way to change this. |
| A local pin can be reverted by a one-shot re-offer after a kind or retention widening. | The re-offer serves from below the peer's cursor, so an equal-clock tie can flip `pinned` back once. A pin that occasionally needs redoing beats two machines silently holding different text for ever. |
| **A device still in the paired roster that never comes back** leaves its relayed tombstones re-offered once per exchange. | We cannot prove the row will not arrive tomorrow, and banking the receipt would lose the delete if it did. Bounded by what the peer holds for that source. Unpairing the dead device makes the refusal permanent and stops it. |
| Local tombstones are exempt from the per-source cap, so the table can exceed it. | Bounded by the rows this machine actually held, which `max_items` already bounds, and a peer cannot add to it. The alternative throws away a delete. |
| Prefix folding for IP budgets. | Rejected: puts the user's own device in the attacker's bucket. Recorded in `guard::network_of`, intentionally unused. |

---

## 4. Process rules, these are not optional

### 4.1 Triage before you fix

Roughly half of every round's reviewer findings have been test defects. For each
failure decide which it is: a real defect (fix the code, keep the test); a test
that inlines outdated logic (point it at the production function, extract one if
none exists, as `admit_inbound`, `make_room_for_peer`, `note_peer_record`,
`stop_claim` and `retention_widened` all were); or a test asserting something
deliberately rejected (rewrite it to the achievable contract and record why).
Never silently delete one.

### 4.2 Prove a test can fail

Round 7 wrote two tests that passed against the UNFIXED code, one flooded with
tombstones dated in the past so the eviction order could not discriminate, the
other picked texts where the echo lost the payload tiebreak on its own. Both
looked thorough and asserted nothing.

**Revert the fix, watch the test fail, restore the fix.** A guard that can find
nothing must assert that it found something: the tombstone-cap test now asserts
eviction actually ran before it asserts what survived.

Where a rule has two opposite failure modes, pin it from both sides. The two
drain-rule tests in `adversarial_r7_scale` fail on opposite reverts, so neither
extreme passes both.

### 4.3 Verify an edit landed

A production fix was silently lost once when a concurrent editor overwrote the
file from a stale read. After editing a file someone else may be touching:

```bash
git show HEAD:path/to/file.rs | grep -c "distinctive string from your change"
```

### 4.4 Never leave a test that can hang

Every socket needs read AND write timeouts, a write/write stall never reaches a
read. Every loop needs a hard bound. Better still, put a wall-clock budget on the
whole exchange and run both sides on their own thread, as
`adversarial_r7_scale::sync_bounded` does, so a stall fails with a message naming
the side that never returned instead of parking the suite.

### 4.5 Fixes have been regressions

Four times now a fix for round N introduced the defect found in round N+1. Re-run
the full per-package suite after every change and be most suspicious of the
newest code. Round 7 found two defects in round 6's own fixes.

---

## 5. The diagnostics

Two `#[ignore]`d tests that touch the real machine. They are not part of the
suite and a failure is information, not a broken build.

```bash
cargo test -p echokey-sync --test mdns_field_check -- --ignored --nocapture
cargo test -p echokey --test keychain_field_check -- --ignored --nocapture
```

Both pass on macOS 26.5.1, mDNS resolves on the real LAN in under a second, and
the keychain is silent at ~1ms per read. Run them first on any new machine;
between them they cover most of what makes a first pairing fail. Results and
caveats are in `docs/SYNC_FIELD_TEST.md`.

---

## 6. What is left

1. **The two-machine field test.** Nothing else comes close in value. Eleven
   steps in `docs/SYNC_FIELD_TEST.md`, in order, recording results as you go.
   Step 6, delete on the receiving device, confirm it disappears on the author , 
   is where most of the design effort went and it has never run for real. Expect
   the field results to generate a round of their own.
2. **Watch for the app's own local-network prompt on macOS.** The diagnostics run
   from a terminal that already holds that permission; the bundled app asks for
   its own. If discovery finds nothing, check System Settings > Privacy &
   Security > Local Network before suspecting the code.
3. **Watch for keychain ACLs across rebuilds.** Items are ACL'd to the identity
   that wrote them, so a re-signed bundle may be refused access to what an
   earlier build stored, and a refusal surfaces as "this device is not paired".
4. **Windows, unchanged by this session**: ASR benchmarks never run, Win+V
   exclusion unverified on hardware, Parakeet unverified, clean-account NSIS
   install untested. Linux not attempted.
5. **Fold this file into `SYNC_DESIGN.md` eventually.** The rules in section 2
   now live there properly; what is genuinely handover-only is sections 4 and 6.
