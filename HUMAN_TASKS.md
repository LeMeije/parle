# Parle: Human Tasks

Exact runbook of everything only you can do. Work top to bottom.

## 1. First-run verification on this Mac (~5 minutes)

The app is built at `target/debug/bundle/macos/Parle.app` (or run
`npm run tauri dev` from the repo). I verified the engine, pipeline, clipboard
capture and onboarding UI live, but the final hotkey-to-paste loop needs a
human at the keyboard (I stopped driving your mouse the moment I saw you were
using the machine):

1. Open the app. Walk through onboarding:
   - Microphone: click Grant, accept the system prompt.
   - Accessibility: System Settings opens -> Privacy & Security -> Accessibility
     -> add/enable Parle. Come back; the page updates itself.
     If the toggle looks on but nothing works: quit System Settings fully, then
     run: `tccutil reset Accessibility com.novaire.parle` and re-grant.
   - Model: click Download (it picked large-v3-turbo-q5_0 for this 24 GB M2).
   - Test dictation on the last page.
2. Click into any text field (Notes, Slack), hold the 🌐 Fn key, speak,
   release. Text should appear at the cursor within ~1s.
   - Tip: set System Settings -> Keyboard -> "Press 🌐 key to" = **Do Nothing**
     so macOS dictation doesn't fight for the key.
3. Tap Fn quickly instead of holding: recording latches; tap again to stop.
4. Press Escape mid-recording: cancels, nothing pastes.
5. Copy a few things in different apps -> open Parle -> History: entries
   appear with the source app. Copy something from 1Password: must NOT appear.

## 1a. Refine mode: verify on this Mac (~5 minutes)

Added 04/09/2026, reworked 10/09/2026, see `docs/REFINE.md`. The CLI
invocation, discovery and the whole pipeline are verified live by tests; the
keypress-to-rewrite loop needs a human at the keyboard.

1. Open Parle > Settings > Refine with AI. Switch **Refine mode** on. The row
   under "Path to the tool" should show a green tick and read
   "✓ Found it for you  /Users/you/Library/Application Support/Claude/claude-code/<version>/claude.app/Contents/MacOS/claude
   · version 2.1.x · signed in". That long path is correct and expected: it is
   the copy inside the Claude desktop app, which is where the CLI lives unless
   you installed it yourself.
   - If it is amber and says not found, press **Check again**, then **Choose…**
     and pick the tool. There is no text box any more; the picker opens where
     the tool is expected to be. A path set by hand in `settings.json` is still
     honoured.
2. **Model** should be a dropdown offering `fable, opus, sonnet, haiku`. Leave
   it on "The tool's own default" unless you want a specific one. If it shows
   "Something else…" with a value and an amber warning, the stored name is not
   a valid one: pick from the list. (`Sonnet 5` was the value that broke this
   silently for a week; names are lower case with no spaces.)
3. Click **Test it**. Expect a tidy meeting note within about 5 s on Claude, or
   nearer 20 s on Codex.
4. Optionally fill "Your rules" (the placeholder is a starting point) and pick
   your voice .md. **Both are sent, one after the other**: the file does not
   replace the rules.
5. Click into Notes. **Hold Shift and double-tap the Globe key** (the default
   trigger is "hold a modifier plus your normal dictation key", so your Fn
   double-tap still works on its own for ordinary dictation). Say a
   deliberately messy email
   ("um so basically tell Justin the deck is moved, no wait, it's Thursday,
   and uh attach the pacing sheet"), release. The overlay should be coral,
   say "Refine" under the waveform, then "Refining with Claude…" with a
   seconds counter, then the rewritten email lands at the cursor.
6. History: the row has a **refined** badge; "Restore raw" brings your words back.
7. Ordinary dictation must be unchanged: double-tap Globe with no Shift and
   check the overlay stays blue and the plain transcript is pasted.
8. On Windows the Copilot key needs **Ctrl**, not Shift: the Copilot key sends
   its own Shift, so Parle cannot tell yours from the keyboard's. Settings
   warns about that combination and about a Refine modifier that is also your
   dictation key.
9. If a modifier fights with something you use, change it, or switch the
   trigger to "Its own key" in Settings > Refine with AI.

## 1b. Duplicate Parle in Spotlight/Launchpad
FIXED automatically: `scripts/install-local.sh` now deletes the build-output
bundle after copying it to /Applications, so only one Parle is ever indexed.

If a duplicate ever reappears (e.g. after running `npm run tauri build`
directly instead of the script), just delete it:
`rm -rf target/debug/bundle/macos/Parle.app target/release/bundle/macos/Parle.app`
The canonical app is always /Applications/Parle.app, so launch that one.

## 2. Stable dev signing (5 minutes, once), needed before rebuilding often

TCC ties permission grants to the code signature; unsigned rebuilds lose the
Accessibility grant every time. Create one self-signed certificate:

1. Keychain Access -> Certificate Assistant -> Create a Certificate…
   - Name: `Parle Dev` · Identity Type: Self-Signed Root ·
     Certificate Type: **Code Signing**
2. In the repo, add to `src-tauri/tauri.conf.json` under `bundle.macOS`:
   `"signingIdentity": "Parle Dev"`.
3. Rebuild. Grants now survive rebuilds. (Never sign with `-` / ad-hoc.)

## 3. Windows build (when you're at the G14)

**04/09/2026: the Refine change touches the hook helper.** After pulling:
`node scripts/build-hook.mjs` (rebuilds `parle-hook.exe`, whose wire protocol
now carries the Refine key), then the usual `cargo test -p parle-core &&
cargo test -p parle-hook && cargo test -p parle --lib` and `npm run tauri
build`. Then check: Settings > Refine with AI finds `claude` (native installer
puts it in `%USERPROFILE%\.local\bin`; npm puts `claude.cmd` in
`%APPDATA%\npm`), Test it works, and the suggested `Ctrl+Shift+Space` chord
starts a coral take. None of this has run on Windows yet.


1. Sync/copy the repo to the Windows machine (git push to a private repo is
   simplest; no remote is configured yet, so create one if you want).
2. Follow `docs/WINDOWS_HANDOFF.md`: it has the toolchain list and a
   copy-paste pickup prompt for Claude Code at the bottom.

## 4. Distribution (only when you want to ship to others)

Installers are built by `.github/workflows/release.yml`: push a tag
(`git tag v0.1.0 && git push origin v0.1.0`) or press "Run workflow" on the
Actions tab, wait for both jobs, then open the DRAFT release on the Releases
page, check the two files and click Publish. `docs/INSTALL.md` is the guide
to send people.

Without the items below, the installers work but both operating systems
warn on first open (the install guide walks people past it).

- Apple Developer ID cert + notarisation for the .dmg. Needs the Apple
  Developer Program. Then add six repository secrets (Settings > Secrets and
  variables > Actions): `APPLE_CERTIFICATE` (base64 of the exported
  Developer ID Application .p12), `APPLE_CERTIFICATE_PASSWORD`,
  `APPLE_SIGNING_IDENTITY` ("Developer ID Application: Name (TEAMID)"),
  `APPLE_ID`, `APPLE_PASSWORD` (an app-specific password) and
  `APPLE_TEAM_ID`. The workflow switches over by itself; this path has not
  been run yet. It also stops Mac users having to re-grant Accessibility
  after every update, which ad-hoc builds need.
- Windows: an Authenticode cert if you want SmartScreen-clean installers.
- Updater: generate a minisign keypair (`npm run tauri signer generate`),
  add the pubkey to tauri.conf.json, host latest.json somewhere static.

## 5. Loose ends from the build session

- A macOS dialog "Claude is requesting to bypass the system private window
  picker…" may still be open. That was triggered by my screenshot checks
  during testing; Allow or dismiss as you prefer. Parle itself never
  records the screen.
- One Parle test instance wrote a few clipboard rows into history during
  testing (from your Notion copies at ~23:05). Delete them in History if you
  like: they're local only.
- Two research nice-to-haves worth a future session: Parakeet engine via
  sherpa-onnx (faster English, also the no-GPU Windows path) and the local-LLM
  cleanup tier (llama-cpp-2 is scaffolded in settings but not shipped in v1).
