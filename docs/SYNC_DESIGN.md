# Cross-machine sync — design (NOT BUILT; groundwork only)

Requested 22/08/2026 (also in Ben's Notion notes): install EchoKey on multiple
machines and share the clipboard/dictation history between them, with each
entry tagged by the machine it came from, so a dictation on the Mac is
instantly pasteable on the Windows box.

Status: DEFERRED until the Windows build runs (cannot be tested one-sided).
Shipped groundwork: history schema v2 adds `source_machine` to every row.

## Constraints (non-negotiable, from the product's trust wedge)
- No cloud, no relay servers, no accounts. LAN-local only.
- End-to-end encrypted; pairing must be explicit and human-verified.
- Password-manager exclusions and Concealed/Transient etiquette apply BEFORE
  anything leaves the machine — never sync what we wouldn't even store.

## Proposed architecture (v1)
- Discovery: mDNS/Bonjour service `_echokey._tcp` (crate: `mdns-sd`).
- Pairing: 6-digit code shown on machine A, typed on machine B; SPAKE2 over
  the code (crate: `spake2`) derives a long-term shared key; thereafter
  mutually-authenticated TLS-PSK or Noise (crate: `snow`) sessions.
- Transport: append-only replication of the `items` table keyed by
  (source_machine, id); tombstones for deletes; pins propagate; last-writer
  -wins on edits (row timestamps already exist).
- Machine identity: stable UUID per install + friendly name (settings:
  `sync.device_name`); every synced row keeps `source_machine` so the History
  UI can filter "from MacBook / from G14" (Ben's tag idea).
- UI: Settings -> Sync: enable toggle, device name, paired-devices list with
  unpair, "Pair new device" flow (code entry), per-kind sync toggles
  (dictations / clipboard).
- Windows parity: identical code path (all crates are cross-platform).

## Open questions for the build session
- Retention interplay: does a synced row obey the RECEIVER's retention? (Propose: yes.)
- Large clipboard payloads: cap synced text at ~256 KB, never images in v1.
- Sleep/wake reconnect cadence and battery etiquette on laptops.
