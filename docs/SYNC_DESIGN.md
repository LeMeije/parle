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
  `(source_machine, origin_id)`. Last-writer-wins on `updated_at`, with a total
  tiebreak on `(text, pinned)` so an exact tie cannot leave two machines
  disagreeing. Deletes are tombstones.

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
send them again. A device does not advertise a mark for itself at all — the peer
decides what to send us from the mark it keeps, so the two directions are
symmetric and neither side's deletions disturb the other's cursor.

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
  time and rewrote the row on every exchange. Rows beyond `now + 24h`, or at or
  below zero, are refused; the receipt still advances so they are not offered
  forever.
- **Our own tombstones are never pruned.** A tombstone for a row we authored is
  the only record that we deleted it and the only thing that will ever tell a
  peer so. Replicated tombstones are local bookkeeping and are pruned normally.

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
