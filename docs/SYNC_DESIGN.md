# Cross-machine sync — design and as-built

Requested 22/08/2026: install Parle on several machines and share the
clipboard/dictation history between them, each entry tagged by the machine it
came from, so a dictation on the Mac is immediately pasteable on the Windows box.

**Status: built.** Covered by unit tests and by end-to-end tests that run the
whole exchange between two stores over real sockets. It has **not** yet run
between two physical machines — there is one Windows box here — so treat the
first real pairing as the remaining acceptance test.

## Constraints (non-negotiable, from the product's trust wedge)

- No cloud, no relay servers, no accounts. LAN-local only.
- End-to-end encrypted; pairing must be explicit and human-verified.
- Password-manager exclusions and Concealed/Transient etiquette apply BEFORE
  anything leaves the machine — never sync what we wouldn't even store.
- Paired keys live in the OS keychain, never in `settings.json`.

## As built

- **Discovery**: mDNS `_echokey._tcp` via `mdns-sd`. Records are unsigned, so
  the address in one is only a hint: we refuse to dial anything that is not a
  private or link-local address, and the Noise handshake is what actually
  establishes trust.
- **Pairing**: a 6-digit code shown on A, typed on B. SPAKE2 over the code
  derives a long-term shared key. Rate-limited to a handful of guesses per code
  (`sync/guard.rs`); a lockout burns the code on screen but never prevents the
  user asking for a fresh one, because refusing that was a free denial of
  service for anyone on the LAN and bought no security.
- **Sessions**: Noise `NNpsk0_25519_ChaChaPoly_BLAKE2s` keyed by the paired
  secret. Device identity is exchanged **inside** that session — see below.
- **Replication**: rows are identified across machines by
  `(source_machine, origin_id)`, where `origin_id` for a new local row is a
  random UUID, never the rowid, which restarts at 1 if `history.db` is rebuilt
  and so hands new rows an identity a peer has already seen. Last-writer-wins on
  `updated_at`, with a total tiebreak on the whole payload
  `(text, pinned, created_at, kind)` so an exact tie cannot leave two machines
  disagreeing. Deletes are tombstones, and a tombstone is ABSORBING: once an
  identity is deleted no copy comes back, whatever its clock says.

  **Only the device that WROTE a row may change its content.** `serve` offers
  items for `source == me` alone, and `drain` accepts items only from the device
  that authored them. Pinning or correcting a peer's dictation is therefore a
  LOCAL change and does not travel; that cost was accepted knowingly, and making
  it travel needs a signed per-device edit log, not a loosening of this rule.

  A local edit on a peer's row does not move `updated_at` either, because that
  clock is what the author's own changes are judged against, bumping it made an
  ordinary pin swallow the author's correction permanently. The row is flagged
  `local_edit` instead, and that flag is what wins the tie against an unchanged
  echo. Deletes are exempt from all of this and travel for every source: a
  password cleared on the laptop has to vanish from the machine that recorded
  it, and a delete carries no attacker-chosen content.

### The two rules that everything else follows from

Most of the defects found in review came from violating one of these, so they
are stated plainly rather than left implicit.

**1. A device speaks only for itself.** `Attribution::accepts` takes rows only
from the handshake-proven peer, for that peer's own id. An earlier version also
accepted any device in our paired roster, on the reasoning that a peer could
usefully relay for a third machine. That was an authority escalation: any paired
device could author dictations attributed to another, delete another's rows with
forged tombstones, and silence a third device by sending one row for it stamped
far in the future. Relaying is also what "no relay servers" rules out. Paired
devices exchange directly; a full mesh is the price of explicit pairing.

**2. A watermark is a receipt, not an observation.** Each device records, in
`source_marks`, the newest clock it has actually RECEIVED from each peer, and
advertises exactly that. It is never derived from the rows currently held.
Deriving it was the root of a whole family of bugs: ordinary local housekeeping
(retention, count eviction, Clear History, a pruned tombstone) walked the mark
backwards, which makes a peer re-send the same rows on every exchange forever;
and a local edit with a fast clock walked it forwards, which makes a peer never
send them again.

Marks are keyed by `(peer, source)`. Keyed by source alone, any peer could move
another device's cursor by relaying one row for it.

This section used to end "a device does not advertise a mark for itself at all",
and that is **no longer the rule**. It was written when `serve` offered items for
every source it held; now that content is author-only, a peer never sends us
items attributed to our own device, so a `(peer, our own id)` mark gates exactly
one stream, the deletes that peer relays back to us about our own rows. That is
a stream a receipt SHOULD gate: without one, every peer re-offers every tombstone
it holds for our source on every exchange, for the life of the pairing, with
nothing able to say stop.

What made the self-mark dangerous was not its existence but the ordering it
relied on. A cursor is a promise never to ask below a clock again, which is only
sound if the stream is created in clock order, and tombstones were stamped
`max(now, row.updated_at)`, so deleting a row from a peer inside the accepted
skew produced a tombstone up to two minutes in the FUTURE, the mark went there,
and the next delete fell below it and was never offered again. `delete_clock`
fixes that at the source: a local delete is stamped strictly above every
tombstone already held for that source, bounded at half the skew window so the
fix cannot itself produce a delete the receiving side would refuse. The older
stamp has not been needed since tombstones became absorbing.

### Other things that are load-bearing

- **Turn-taking.** The dialler writes first, the acceptor reads first. A
  symmetric exchange, where both sides send everything before either reads,
  works only while the data fits in the socket and Noise buffers; a first sync
  of any real size fills them and both peers block in `write` forever.
- **Identity inside the session.** Sending it as plain frames after the key was
  confirmed still let an on-path attacker relay the SPAKE2 messages verbatim —
  never learning the key — and rewrite only the identity, so both machines
  paired on a shared key filed under an attacker-chosen id and name.
- **A wall-clock deadline, applied before the first byte.** A socket timeout
  bounds one syscall, and `read_exact` renews it on every byte that arrives, so
  a peer dribbling bytes can hold a handler thread for hours.
- **Clocks are refused, not clamped.** Clamping to `now + skew` is not
  deterministic, so re-applying the same message stored a different value each
  time and rewrote the row on every exchange. Rows beyond `now +
  MAX_CLOCK_SKEW_MS` (**two minutes**, not the 24 hours an earlier draft of this
  file recorded), or at or below zero, are refused, with a warning naming the
  device; the receipt still advances so they are not offered forever. Two
  minutes rather than a day because the cursor we keep for a peer IS that peer's
  clock: at 24 hours, a machine half a day fast wrote a cursor half a day ahead,
  and once its clock was corrected everything it produced fell below that cursor
  and was never requested again, silently.
- **Tombstones are never pruned by age at all**, ours or replicated. Dropping a
  replicated one was justified by "our receipt for that source outlives it", and
  that is not true across a `reset_source_marks()`, which every kind or retention
  widening performs.

  The per-source ceiling that bounds the table evicts **replicated tombstones
  only** (`tombstones.local = 0`). Evicting indiscriminately destroyed
  undelivered local deletes: a Clear History is written uncapped, so the table
  sits above the ceiling, and the next tombstone from a peer trimmed it by
  dropping the oldest, which straight after a Clear are the user's own deletes,
  none of them delivered. A replicated tombstone is safe to drop because the peer
  that sent it still holds it; a local one is the only record anywhere that the
  user asked for that row to go. What bounds the local ones instead is that they
  are created only by deleting rows this machine actually holds, which the user's
  own `max_items` already bounds, and a peer cannot add to them.

- **A relayed delete for an identity we do not hold** is refused, so a paired
  device cannot pre-emptively tombstone identities it invented. Whether that
  refusal banks a receipt depends on whether the row could ever arrive: content
  reaches us only from its author, so if the source is neither in our paired
  roster nor one we already hold rows for, the refusal is PERMANENT and the
  receipt is safe. Without that, the exchange never went quiet, A syncs with C,
  the user clears history on A, A pairs with B, and every A-B exchange from then
  on carries the same dead tombstones for ever.

## The answers to the original open questions

- **Does a synced row obey the RECEIVER's retention?** Yes. `Retention::keeps`
  refuses rows older than the receiving machine's window, and `prune` now runs
  after every exchange rather than only at startup.
- **Large clipboard payloads?** Capped on the wire. Text only; no images.
- **Sleep/wake cadence?** Dials happen on first sight of a peer, capped at
  `MAX_DIALS` concurrent, and a record refresh does not start a new exchange.

## Still open

- It has never run between two physical machines.
- No `Zeroize` on `PairedKey` or the SPAKE2 shared secret; key material can
  linger in freed heap. Nothing is logged or persisted, so this is
  defence-in-depth rather than a live leak.
- A device id is broadcast in cleartext in the mDNS TXT record, and
  `serve_session` closes immediately for an unpaired id but takes the handshake
  timeout for a paired one. Anyone on the LAN can therefore enumerate which
  devices this machine is paired with. The ids are already public in the TXT
  record, so the oracle adds little, but it is not nothing.
