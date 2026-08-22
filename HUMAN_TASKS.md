# EchoKey — Human Tasks

Exact runbook of everything only you can do. Work top to bottom.

## 1. First-run verification on this Mac (~5 minutes)

The app is built at `target/debug/bundle/macos/EchoKey.app` (or run
`npm run tauri dev` from the repo). I verified the engine, pipeline, clipboard
capture and onboarding UI live, but the final hotkey-to-paste loop needs a
human at the keyboard (I stopped driving your mouse the moment I saw you were
using the machine):

1. Open the app. Walk through onboarding:
   - Microphone: click Grant, accept the system prompt.
   - Accessibility: System Settings opens -> Privacy & Security -> Accessibility
     -> add/enable EchoKey. Come back; the page updates itself.
     If the toggle looks on but nothing works: quit System Settings fully, then
     run: `tccutil reset Accessibility com.novaire.echokey` and re-grant.
   - Model: click Download (it picked large-v3-turbo-q5_0 for this 24 GB M2).
   - Test dictation on the last page.
2. Click into any text field (Notes, Slack), hold the 🌐 Fn key, speak,
   release. Text should appear at the cursor within ~1s.
   - Tip: set System Settings -> Keyboard -> "Press 🌐 key to" = **Do Nothing**
     so macOS dictation doesn't fight for the key.
3. Tap Fn quickly instead of holding: recording latches; tap again to stop.
4. Press Escape mid-recording: cancels, nothing pastes.
5. Copy a few things in different apps -> open EchoKey -> History: entries
   appear with the source app. Copy something from 1Password: must NOT appear.

## 1b. Spotlight shows two EchoKeys
The canonical app now lives at /Applications/EchoKey.app (launch that one).
The copy under the repo's target/ folder regenerates on every build; to hide
it from Spotlight: System Settings -> Siri & Spotlight -> Spotlight Privacy ->
add the repo's `target` folder. Refresh the /Applications copy any time with
`scripts/install-local.sh`.

## 2. Stable dev signing (5 minutes, once) — needed before rebuilding often

TCC ties permission grants to the code signature; unsigned rebuilds lose the
Accessibility grant every time. Create one self-signed certificate:

1. Keychain Access -> Certificate Assistant -> Create a Certificate…
   - Name: `EchoKey Dev` · Identity Type: Self-Signed Root ·
     Certificate Type: **Code Signing**
2. In the repo, add to `src-tauri/tauri.conf.json` under `bundle.macOS`:
   `"signingIdentity": "EchoKey Dev"`.
3. Rebuild. Grants now survive rebuilds. (Never sign with `-` / ad-hoc.)

## 3. Windows build (when you're at the G14)

1. Sync/copy the repo to the Windows machine (git push to a private repo is
   simplest — no remote is configured yet; create one if you want).
2. Follow `docs/WINDOWS_HANDOFF.md` — it has the toolchain list and a
   copy-paste pickup prompt for Claude Code at the bottom.

## 4. Distribution (only when you want to ship to others)

- Apple Developer ID cert + notarisation for the .dmg
  (`bundle.macOS.signingIdentity` + `tauri notarize` env vars; also swap the
  updater keys in — see tauri-plugin-updater docs). Until then the app runs
  fine locally; Gatekeeper will warn other people's machines.
- Windows: an Authenticode cert if you want SmartScreen-clean installers.
- Updater: generate a minisign keypair (`npm run tauri signer generate`),
  add the pubkey to tauri.conf.json, host latest.json somewhere static.

## 5. Loose ends from the build session

- A macOS dialog "Claude is requesting to bypass the system private window
  picker…" may still be open — that was triggered by my screenshot checks
  during testing; Allow or dismiss as you prefer. EchoKey itself never
  records the screen.
- One EchoKey test instance wrote a few clipboard rows into history during
  testing (from your Notion copies at ~23:05). Delete them in History if you
  like — they're local only.
- Two research nice-to-haves worth a future session: Parakeet engine via
  sherpa-onnx (faster English, also the no-GPU Windows path) and the local-LLM
  cleanup tier (llama-cpp-2 is scaffolded in settings but not shipped in v1).
