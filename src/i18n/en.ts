// English source strings. This is the fallback dictionary: every other
// language falls back to a key here, so a key must never be removed from this
// file while it is still referenced from the UI.
//
// Keys are dot-namespaced by area and stable. Values are the exact English
// text as written, several of which describe security behaviour precisely.
export const en: Record<string, string> = {
  // ---------- Shared ----------
  'common.cancel': 'Cancel',
  'common.dismiss': 'Dismiss',
  'common.grant': 'Grant',
  'common.keepIt': 'Keep it',
  'common.openSystemSettings': 'Open System Settings',

  // ---------- Relative time ----------
  'time.justNow': 'just now',
  'time.minutesAgo': '{n}m ago',
  'time.hoursAgo': '{n}h ago',
  'time.secondsShort': '{n}s',

  // ---------- App shell ----------
  'app.nav.compose': 'Compose',
  'app.nav.history': 'History',
  'app.nav.dictionary': 'Dictionary',
  'app.nav.models': 'Models',
  'app.nav.sync': 'Sync',
  'app.nav.settings': 'Settings',
  'app.record.start': 'Start dictation',
  'app.record.stop': 'Stop dictation',
  'app.toast.pasteInstruction': 'Copied. Press {keys} to paste',
  'app.toast.inserted': 'Inserted "{text}"',
  'app.toast.copied': 'Copied "{text}"',
  'app.toast.refinedInserted': 'Refined and inserted "{text}"',
  'app.toast.refinedCopied': 'Refined and copied "{text}"',

  // ---------- Recording overlay (HUD) ----------
  'hud.pasteInstruction': 'Copied. Press {keys} to paste',
  'hud.recordingClickToStop': 'Recording. Click to stop',
  'hud.transcribing': 'Transcribing…',
  'hud.stopAndPaste': 'Stop and paste',
  'hud.working': 'Working…',
  'hud.cancel': 'Cancel (Esc)',
  'hud.stopAndRefine': 'Stop and refine',
  'hud.refining': 'Refining with {provider}… {secs}s',
  'hud.refineTag': 'Refine',
  'hud.deck.rec': 'REC',
  'hud.deck.proc': 'PROC',
  'hud.deck.ai': 'AI',
  'hud.deck.aiTag': '· AI',

  // ---------- History ----------
  'history.searchPlaceholder': 'Search transcriptions and clipboard…  (↑↓ · Enter pastes · {copyKeys} copies)',
  'history.filter.all': 'All',
  'history.filter.dictations': 'Dictations',
  'history.filter.clipboard': 'Clipboard',
  'history.filter.allDevices': 'All devices',
  'history.filter.thisDevice': '{name} (this device)',
  'history.gone': 'That item is no longer here. It was deleted on another device, so the list has been refreshed.',
  'history.empty.noMatches': 'No matches.',
  'history.empty.nothingYet': 'Nothing here yet. Hold your hotkey and speak.',
  'history.deleteConfirm': 'Delete this item? It cannot be undone.',
  'history.mayAlsoDelete': 'This may also delete it from your paired devices.',
  'history.alsoDeletesFrom': 'This also deletes it from {devices}.',
  'history.localOnly.badge': 'this device only',
  'history.localOnly.title':
    'Parle could not rule out that this was a password field, so it is kept on this device and never sent to your other devices',
  'history.fromDevice.title': 'Written on another one of your devices and synced here',
  'history.refined.badge': 'refined',
  'history.refined.title': 'Rewritten by {by} in {secs}s. Restore raw brings back your own words.',
  'history.action.paste': 'Paste',
  'history.action.pasteTitle': 'Paste into the previous app (Enter)',
  'history.action.copy': 'Copy',
  'history.action.copyTitle': 'Copy ({copyKeys})',
  'history.action.editTitle': 'Edit (feeds auto-learn)',
  'history.action.pin': 'Pin',
  'history.action.unpin': 'Unpin',
  'history.action.delete': 'Delete',
  'history.trimmedCount': '{n} trimmed',
  'history.unsureCount': '{n} unsure',
  'history.restoreRaw': 'Restore raw',

  // ---------- Compose ----------
  'compose.title': 'Compose',
  'compose.intro':
    'Dictate here and paste links or text mid-sentence: each insert is pinned to the exact moment you added it and spliced into the final text, byte-exact.',
  'compose.start': 'Start dictation',
  'compose.stop': 'Stop',
  'compose.transcribing': 'Transcribing…',
  'compose.recording': 'recording',
  'compose.recordingRefine': 'recording · refine',
  'compose.processing': 'processing',
  'compose.refining': 'Refining…',
  'compose.refiningPill': 'refining',
  'compose.startRefine': 'Refine',
  'compose.startRefine.title': 'Dictate, then have the AI rewrite it before it is pasted',
  'compose.refinedNote': 'Rewritten by the AI, inserted at your cursor and saved to History. Your own words are the raw text of that row.',
  'compose.markPlaceholder.recording': 'Paste a link or type, Enter pins it to this moment…',
  'compose.markPlaceholder.idle': 'Start dictating to insert links and text',
  'compose.insert': 'Insert',
  'compose.noSpeech': 'No speech detected.',
  'compose.copyResult': 'Copy result',
  'compose.alsoInserted': 'Also inserted at your cursor and saved to History.',
  'compose.barActive':
    'Paste or type into the dictation bar at the bottom of the window, from any tab. Enter pins it here.',

  // ---------- Dictation bar ----------
  'bar.pinnedCount': '{n} inserted',
  'bar.pinnedAt': 'Inserted at {time}',
  'bar.openCompose': 'Open Compose to see what you have inserted',
  'bar.insertHint': 'Enter inserts. Shift + Enter adds a line.',

  // ---------- Models ----------
  'models.title': 'Models',
  'models.subtitle': 'All transcription runs on this device.',
  'models.warm': 'Warm · {model}',
  'models.loadsOnFirstUse': 'Model loads on first use',
  'models.active': 'Active',
  'models.backendTitle': 'The hardware this model runs on',
  'models.yourFile': 'Your file',
  'models.speedRating': 'speed {value}/5',
  'models.accuracyRating': 'accuracy {value}/5',
  'models.languageCount': '{n} languages',
  'models.use': 'Use',
  'models.deleteFile': 'Delete model file',
  'models.removeCustom': 'Remove from this list (your file is not deleted)',
  'models.removeFromList': 'Remove from this list',
  'models.fileMissing': 'File missing',
  'models.download': 'Download',
  'models.addLocal': 'Add a local model…',
  'models.addLocal.hint':
    'A whisper.cpp GGML {ext} file you already have. Parle points at it where it is and never copies it, so it costs no extra disk.',
  'models.picker.title': 'Choose a whisper.cpp model',
  'models.picker.filter': 'Whisper model',
  'models.defaultLocalName': 'Local model',
  'models.fallbackHint':
    'If the active model fails to load (for example under memory pressure), Parle automatically falls back down the ladder: your recording is never lost.',

  // ---------- Onboarding ----------
  'onboarding.welcome.title': 'Welcome to Parle',
  'onboarding.welcome.body':
    'Hold a key, speak, release: your words appear where your cursor is. Transcription runs entirely on this device. Nothing you say ever leaves it.',
  'onboarding.welcome.cta': 'Set up',
  'onboarding.permissions.title': 'Permissions',
  'onboarding.permissions.introMac':
    'Parle needs two grants to hear you and type for you. Both stay on this machine.',
  'onboarding.permissions.introWin': 'Parle needs one grant, to hear you. It stays on this machine.',
  'onboarding.permissions.microphone': 'Microphone',
  'onboarding.permissions.microphoneDesc': 'To hear your dictation',
  'onboarding.permissions.accessibility': 'Accessibility',
  'onboarding.permissions.accessibilityDesc': 'To watch your hotkey and paste at the cursor',
  'onboarding.permissions.macNote':
    'In System Settings, add {app} under Privacy & Security → Accessibility, then come back. This page updates by itself. A restart of Parle may be needed after granting.',
  'onboarding.permissions.appName': 'Parle',
  'onboarding.permissions.openSettings': 'Open Settings',
  'onboarding.permissions.continue': 'Continue',
  'onboarding.permissions.waiting': 'Waiting for permissions…',
  'onboarding.model.title': 'Your model',
  'onboarding.model.machine': '{ram} GB RAM, {gpu}',
  'onboarding.model.recommendation':
    'Based on this machine ({machine}), we recommend {model}. You can add or switch models any time in Settings → Models.',
  'onboarding.model.downloadFailed':
    'Download failed: {error}. Check your connection and retry: it resumes where it stopped.',
  'onboarding.model.ready': 'Model ready',
  'onboarding.model.download': 'Download',
  'onboarding.hotkey.title': 'Your key',
  'onboarding.hotkey.macKey': '🌐 Fn key',
  'onboarding.hotkey.doNothing': 'Do Nothing',
  'onboarding.hotkey.mac':
    "Default: the {key}. Hold it and talk, release to paste, or tap it quickly to latch recording on. Tip: set System Settings → Keyboard → “Press 🌐 key to” to {doNothing} so macOS dictation doesn't fight for it.",
  'onboarding.hotkey.winKey': 'Left Ctrl',
  'onboarding.hotkey.win':
    'Default: {key}. Hold it and talk, release to paste, or tap it quickly to latch recording on. Have a Copilot key? Bind it in Settings → Hotkeys and Parle will take it over completely.',
  'onboarding.hotkey.cta': 'Got it',
  'onboarding.test.title': 'Try it',
  'onboarding.test.body': 'Click the button (or use your hotkey), say something, then stop.',
  'onboarding.test.start': 'Start test dictation',
  'onboarding.test.stop': 'Stop',
  'onboarding.test.transcribing': 'Transcribing…',
  'onboarding.test.noSpeech': 'No speech detected. Try again a little louder.',
  'onboarding.test.finish': 'Finish setup',

  // ---------- Settings: shell ----------
  'settings.title': 'Settings',
  'settings.subtitle': 'Local-only. No telemetry, no cloud, ever.',
  'settings.section.hotkeys': 'Hotkeys',
  'settings.section.language': 'Language',
  'settings.section.cleanup': 'Cleanup',
  'settings.section.dictionary': 'Dictionary',
  'settings.section.output': 'Output',
  'settings.section.refine': 'Refine with AI',
  'settings.section.appearance': 'Appearance',
  'settings.section.historyPrivacy': 'History & privacy',
  'settings.section.audio': 'Audio',
  'settings.section.general': 'General',
  'settings.footer.tagline': 'on-device dictation',
  'settings.footer.note': 'nothing ever leaves this machine',

  // ---------- Settings: hotkeys ----------
  'settings.dictationKey.label': 'Dictation key',
  'settings.dictationKey.hintMac': 'Fn needs Accessibility permission',
  'settings.dictationKey.hintWin': 'Left Ctrl sits where Globe does on a Mac. Double tap leaves it working normally; Right Alt is AltGr on many layouts',
  'settings.dictationKey.custom': 'Custom…',
  'settings.customBinding.label': 'Custom binding',
  'settings.customBinding.hint': 'Click, then press the key or combination you want. Esc cancels.',
  'settings.customBinding.listening': 'Press a key combination…',
  'settings.gesture.label': 'Gesture',
  'settings.gesture.hintDoubleTap':
    'Double-tap starts, single tap stops. The key is never intercepted, so its normal system behaviour keeps working.',
  'settings.gesture.hint': 'Hybrid: hold to talk; a quick tap latches until the next tap',
  'settings.gesture.hold': 'Hold',
  'settings.gesture.toggle': 'Toggle',
  'settings.gesture.hybrid': 'Hybrid',
  'settings.gesture.doubleTap': 'Double tap',
  'settings.latch.label': 'Latch window',
  'settings.latch.hint':
    'Hybrid: taps shorter than this latch into toggle. Double tap: max gap between taps',
  'settings.escCancel.label': 'Esc cancels recording',
  'settings.escCancel.hint':
    'Off by default: Esc gets pressed for all sorts of unrelated reasons, and discarding a take you already spoke is worse than stopping it with your hotkey',
  'settings.historyPalette.label': 'History palette',
  'settings.historyPalette.hint': 'Chord shortcut for search',
  'settings.suppressCopilot.label': 'Suppress Copilot launch',
  'settings.suppressCopilot.hint':
    'When the Copilot key is bound (or this is on), the default Copilot app never opens',
  'settings.accessibilityMissing':
    "Accessibility permission is missing. Special keys and paste-at-cursor won't work. If you already granted it and this warning stays, the entry went stale after a rebuild: use Repair.",
  'settings.repairPermission': 'Repair permission',
  'settings.bindingWarning.leftCtrl':
    'Left Ctrl drives most keyboard shortcuts, so binding it will fire during normal use.',
  'settings.bindingWarning.leftShift':
    'Left Shift is pressed constantly while typing, so expect false triggers.',
  'settings.bindingWarning.rightAlt':
    'Right Alt is AltGr on many layouts, so it types accented characters. Left Ctrl, double tapped, is safer.',

  // ---------- Settings: key names ----------
  'keys.fn': '🌐 Fn / Globe',
  'keys.rightCommand': 'Right ⌘',
  'keys.leftCommand': 'Left ⌘',
  'keys.rightOption': 'Right ⌥',
  'keys.leftOption': 'Left ⌥',
  'keys.rightControl': 'Right ⌃',
  'keys.leftControl': 'Left ⌃',
  'keys.copilot': 'Copilot key',
  'keys.rightCtrl': 'Right Ctrl',
  'keys.leftCtrl': 'Left Ctrl',
  'keys.rightShift': 'Right Shift',
  'keys.leftShift': 'Left Shift',
  'keys.leftAlt': 'Left Alt',
  'keys.rightAlt': 'Right Alt',
  'keys.rightWin': 'Right Win',
  'keys.leftWin': 'Left Win',

  // ---------- Settings: language ----------
  'settings.spokenLanguage.label': 'Spoken language',
  'settings.language.auto': 'Auto-detect',
  'settings.language.en': 'English',
  'settings.language.es': 'Spanish',
  'settings.language.fr': 'French',
  'settings.language.de': 'German',
  'settings.language.it': 'Italian',
  'settings.language.pt': 'Portuguese',
  'settings.language.nl': 'Dutch',
  'settings.language.ja': 'Japanese',
  'settings.language.ko': 'Korean',
  'settings.language.zh': 'Chinese',
  'settings.language.hi': 'Hindi',
  'settings.language.ar': 'Arabic',
  'settings.language.ru': 'Russian',
  'settings.language.pl': 'Polish',
  'settings.language.sv': 'Swedish',
  'settings.localeSpelling.label': 'Locale spelling',
  'settings.localeSpelling.hint': 'Affects spelling of the output (colour vs color)',
  'settings.locale.none': 'No preference',
  'settings.locale.enAU': 'English (Australia)',
  'settings.locale.enGB': 'English (UK)',
  'settings.locale.enUS': 'English (US)',
  'settings.applyLocaleSpelling.label': 'Apply locale spelling',
  'settings.applyLocaleSpelling.hint': 'Convert US spellings in the transcript to your locale',
  'settings.translate.label': 'Translate to English',
  'settings.translate.hint': 'Speak any language, paste English',

  // ---------- Settings: cleanup ----------
  'settings.smartCleanup.label': 'Smart cleanup',
  'settings.smartCleanup.hint': 'Master switch for the deterministic cleanup tier',
  'settings.removeFillers.label': 'Remove filler words',
  'settings.removeFillers.hint': 'um, uh, er…',
  'settings.removeHedges.label': 'Remove hedges',
  'settings.removeHedges.hint': 'you know, sort of, I mean (more aggressive)',
  'settings.trimSelfCorrections.label': 'Trim self-corrections',
  'settings.trimSelfCorrections.hint':
    '“Thursday, no actually Wednesday” → “Wednesday”. Trimmed spans stay reviewable in History',
  'settings.dictatedPunctuation.label': 'Dictated punctuation',
  'settings.dictatedPunctuation.hint':
    '“comma”, “new line”, “question mark”… (“literally comma” escapes)',
  'settings.capitalise.label': 'Capitalise sentences',
  'settings.terminalPunctuation.label': 'End with punctuation',
  'settings.paragraphPause.label': 'Paragraph on long pause',

  // ---------- Settings: dictionary ----------
  'settings.dictionary.enable': 'Enable dictionary',
  'settings.dictionary.bias.label': 'Bias recognition',
  'settings.dictionary.bias.hint': 'Feed your terms to the engine as a glossary',
  'settings.dictionary.fuzzy.label': 'Fix close misspellings',
  'settings.dictionary.autoLearn.label': 'Learn from my edits',
  'settings.dictionary.autoLearn.hint': 'Single-word edits in History become correction pairs',

  // ---------- Settings: output ----------
  'settings.insertAtCursor.label': 'Insert at cursor',
  'settings.insertAtCursor.hint': 'Types the result into the focused app',
  'settings.copyToClipboard.label': 'Copy to clipboard',
  'settings.restoreClipboard.label': 'Restore previous clipboard',
  'settings.restoreClipboard.hint': 'After paste-injection, put your old clipboard back',
  'settings.restoreDelay.label': 'Restore delay',
  'settings.restoreDelay.hint': 'Slow apps (Office, remote desktop) read the clipboard late',
  'settings.preferAxInsert.label': 'Prefer direct insertion',
  'settings.preferAxInsert.hint': 'Try Accessibility text insertion before clipboard-paste',
  'settings.pressEnter.label': 'Press Enter after inserting',
  'settings.pressEnter.hint':
    'Sends the message right after pasting, handy for chat apps. Never fires on secure fields.',

  // ---------- Settings: refine ----------
  'settings.refine.enable.label': 'Refine mode',
  'settings.refine.enable.hint':
    'A second way to dictate. Speak a brain dump, and instead of pasting the transcript Parle hands it to an AI on this machine and pastes the rewrite: organised, punctuated, ready to send. The transcript is still copied and kept in History. This is the one thing in Parle that sends your words off this device, and it only happens on the gesture you choose below.',
  'settings.refine.provider.label': 'AI',
  'settings.refine.provider.hint':
    'A command-line tool you have already installed and signed in to. Parle never holds an API key.',
  'settings.refine.provider.claude': 'Claude Code (claude)',
  'settings.refine.provider.codex': 'OpenAI Codex CLI (codex)',
  'settings.refine.provider.gemini': 'Google Gemini CLI (gemini)',
  'settings.refine.provider.custom': 'Custom command',
  'settings.refine.programPath.label': 'Path to the tool',
  'settings.refine.programPath.hint': 'Leave empty to find it automatically. Set it if the check below cannot.',
  'settings.refine.programPath.placeholder': 'auto',
  'settings.refine.customCommand.label': 'Command',
  'settings.refine.customCommand.hint':
    'The whole command line. It receives the prompt on standard input and must print the rewrite on standard output. No shell: pipes and variables are passed as plain text.',
  'settings.refine.model.label': 'Model',
  'settings.refine.model.hint': "Passed to the tool as its model option. Empty uses the tool's default.",
  'settings.refine.model.placeholder': 'default',
  'settings.refine.status.checking': 'Checking…',
  'settings.refine.status.notChecked': 'Not checked yet. Turn Refine on, or press Check again.',
  'settings.refine.status.found': 'Found {program}',
  'settings.refine.status.version': 'version {version}',
  'settings.refine.status.signedIn': 'signed in',
  'settings.refine.status.notSignedIn': 'not signed in',
  'settings.refine.status.recheck': 'Check again',
  'settings.refine.status.voiceMissing': 'The voice file cannot be read; dictations will be refined without it.',
  'settings.refine.key.label': 'Refine key',
  'settings.refine.key.hintMac': 'Right Option is suggested. Option chords like ⌥E still work: they cancel the take and pass through.',
  'settings.refine.key.hintWin':
    'A chord is suggested. Binding a bare modifier here would stop it working as a modifier while bound.',
  'settings.refine.key.clash': 'This is also the dictation key. The dictation key wins, so Refine would never start. Pick a different key.',
  'settings.refine.gesture.label': 'Refine gesture',
  'settings.refine.accent.label': 'Refine colour',
  'settings.refine.accent.hint': 'The overlay and the dictation bar take this colour while a Refine take runs, so you always know which mode you are in.',
  'settings.refine.rules.label': 'Your rules',
  'settings.refine.rules.hint':
    'Standing instructions baked into every rewrite: spelling, tone, characters to avoid, how to sign off.',
  'settings.refine.rules.placeholder':
    'American English spelling. Never use em dashes. Keep my first person. Short paragraphs. If it reads like an email, end with just my first name, [your name].',
  'settings.refine.voice.label': 'Voice file',
  'settings.refine.voice.hint':
    'Optional Markdown file about how you write. Read fresh each time, so keep editing the one you already have. Capped at 64 KB.',
  'settings.refine.voice.choose': 'Choose…',
  'settings.refine.voice.clear': 'Clear',
  'settings.refine.voice.picker': 'Choose a voice file',
  'settings.refine.voice.filter': 'Markdown or text',
  'settings.refine.timeout.label': 'Give up after',
  'settings.refine.timeout.hint': 'The tool is stopped at this point and the fallback below applies.',
  'settings.refine.fallback.label': 'If the AI fails',
  'settings.refine.fallback.clipboard': 'Copy the plain transcript, do not insert it',
  'settings.refine.fallback.insert': 'Insert the plain transcript as usual',
  'settings.refine.fallback.hint':
    'You asked for Refine because the raw dictation was not ready to send, so by default it stays off your document and goes to the clipboard and History instead.',
  'settings.refine.test.button': 'Test it',
  'settings.refine.test.running': 'Refining a sample…',
  'settings.refine.test.result': 'Rewritten in {secs}s{model}:',
  'settings.refine.test.hint': 'Runs a fixed sample dictation through the whole path, exactly as a real take would.',
  'settings.refine.privacy':
    'What is sent: the transcript, your rules and your voice file. What is not: audio, History, the clipboard, or anything about the app you are typing into. The tool runs with its own tools, plugins and project files switched off, so a dictation can never become an action.',

  // ---------- Settings: refine trigger ----------
  'settings.refine.trigger.label': 'How you start it',
  'settings.refine.trigger.hint':
    'Either hold a modifier while using the dictation key you already have, or give Refine a key of its own.',
  'settings.refine.trigger.modifier': 'Hold a modifier',
  'settings.refine.trigger.ownKey': 'Its own key',
  'settings.refine.modifier.label': 'Modifier',
  'settings.refine.trigger.gestureHint':
    'Hold {mod}, then {gesture} {key} exactly as you do now. Hold the modifier first: it is read at the moment the recording starts. Either the left or the right one works.',
  'settings.refine.trigger.verb.hold': 'hold',
  'settings.refine.trigger.verb.tap': 'tap',
  'settings.refine.trigger.verb.doubleTap': 'double tap',
  'settings.refine.trigger.verb.hybrid': 'hold or tap',
  'settings.refine.trigger.clashModifier':
    'Your dictation key is {key}, which is the same modifier as {mod}, so holding it cannot mean anything extra. Pick another modifier, or give Refine its own key.',
  'settings.refine.trigger.chordKey':
    'Your dictation key is the combination {key}. A modifier trigger only works with a single key, such as the Globe key, the Copilot key, or a bare modifier. Give Refine its own key instead.',
  'settings.refine.trigger.copilotModifier':
    'The Copilot key sends its own Shift and Windows key, so Parle cannot tell {mod} held by you from the one the keyboard sent. Use Ctrl or Alt with the Copilot key.',

  // ---------- Settings: appearance ----------
  'settings.theme.label': 'Theme',
  'settings.theme.system': 'System',
  'settings.theme.light': 'Light',
  'settings.theme.dark': 'Dark',
  'settings.palette.label': 'Palette',
  'settings.palette.hint':
    'Pastel tints itself from your accent colour, so try it with the custom wheel',
  'settings.palette.paper': 'Paper',
  'settings.palette.pastel': 'Pastel',
  'settings.palette.bold': 'Bold',
  'settings.palette.retro': 'Retro',
  'settings.accent.label': 'Accent',
  'settings.accent.custom': 'Custom colour',
  'settings.appIcon.label': 'App icon',
  'settings.appIcon.hint': 'Applies immediately in-app; the Finder icon updates after a restart',
  'settings.appIcon.default': 'Parle',
  'settings.appIcon.keycap': 'Keycap',
  'settings.appIcon.waveform': 'Waveform',
  'settings.appIcon.echoRings': 'Echo rings',
  'settings.appIcon.cassette': 'Cassette',
  'settings.iconRestart': 'Icon updated. Restart to refresh the Finder and Dock icon.',
  'settings.restartParle': 'Restart Parle',
  'settings.trayIcon.labelMac': 'Menu bar icon',
  'settings.trayIcon.labelWin': 'Tray icon',
  'settings.trayIcon.hintMac': 'Monochrome follows the menu bar; the badge keeps Parle’s colour',
  'settings.trayIcon.hintWin': 'Auto picks the outline that reads against your taskbar',
  'settings.tray.template': 'Monochrome',
  'settings.tray.badge': 'Blue badge',
  'settings.tray.auto': 'Auto: match taskbar',
  'settings.tray.light': 'Outline light',
  'settings.tray.dark': 'Outline dark',
  'settings.tray.color': 'Blue outline',
  'settings.overlayStyle.label': 'Overlay style',
  'settings.overlayStyle.hintHidden':
    'No overlay at all. While Parle is listening, the menu bar icon shows a dot in its corner, and that is the only indication.',
  'settings.overlayStyle.hint': 'Cassette pairs beautifully with the Retro palette',
  'settings.overlayStyle.pill': 'Pill',
  'settings.overlayStyle.cassette': 'Cassette',
  'settings.overlayStyle.metal': 'Metal',
  'settings.overlayStyle.minimal': 'Minimal',
  'settings.overlayStyle.none': 'None',
  'settings.waveformSensitivity.label': 'Waveform sensitivity',
  'settings.waveformSensitivity.hint':
    'Raise it if the bars barely move when you speak, lower it if they sit at the top. It changes what the meter shows, never what is recorded or transcribed.',
  'settings.showPartial.label': 'Show live transcript in overlay',
  'settings.reduceMotion.label': 'Reduce motion',
  'settings.reduceMotion.hint':
    'Nothing animates that does not have to. The dictation bar appears at the bottom of the window instead of growing out of the record button, and the cassette reels stop spinning. Useful if animation bothers you, or on an older machine.',

  // ---------- Settings: history & privacy ----------
  'settings.clipboardCapture.label': 'Capture clipboard',
  'settings.clipboardCapture.hint':
    'Everything you copy, searchable. Password managers are excluded',
  'settings.retention.label': 'Keep items for',
  'settings.retention.confirmNarrow':
    'Items older than that will be deleted from this device and cannot be brought back, even from a paired device. Continue?',
  'settings.retention.forever': 'Forever',
  'settings.retention.d90': '90 days',
  'settings.retention.d30': '30 days',
  'settings.retention.d7': '7 days',
  'settings.retention.d1': '1 day',
  'settings.excludedApps.label': 'Excluded apps',
  'settings.excludedApps.hint':
    'One per line: bundle id on Mac, exe name on Windows. This list is per device, so add the entry on each machine. From the moment you add an entry, Parle stops sending rows from that app to your other devices. Anything already synced stays on them.',
  'settings.dangerZone.label': 'Danger zone',
  'settings.clearHistory.button': 'Clear all unpinned history',
  'settings.clearHistory.confirmWithDevices':
    'This deletes every unpinned item on this device and on {devices}. Pinned items stay. It cannot be undone.',
  'settings.clearHistory.confirm':
    'This deletes every unpinned item on this device. Pinned items stay. It cannot be undone.',
  'settings.clearHistory.clearIt': 'Clear it',

  // ---------- Settings: audio ----------
  'settings.microphone.label': 'Microphone',
  'settings.microphone.systemDefault': 'System default',
  'settings.minDuration.label': 'Ignore recordings shorter than',
  'settings.microphoneDenied': 'Microphone access is denied.',

  // ---------- Settings: general ----------
  'settings.launchAtLogin.label': 'Launch at login',
  'settings.prewarm.label': 'Pre-warm model at startup',
  'settings.prewarm.hint': 'Uses memory while idle, makes the first dictation instant',

  // ---------- Settings: sync ----------
  'sync.section': 'Sync',
  'sync.unavailable': 'Sync isn’t available right now. {error}',
  'sync.checking': 'Checking sync…',
  'sync.genericError': 'Something went wrong.',
  'sync.enable.label': 'Sync with my other devices',
  'sync.enable.hint':
    "Off unless you turn it on. Your Mac and PC talk straight to each other over your local network, end-to-end encrypted, with no account and nothing uploaded anywhere. While it is on, Parle announces this device's name to other machines on the same network so they can find it.",
  'sync.tryAgain': 'Try again',
  'sync.thisDevice.label': 'This device',
  'sync.thisDevice.hint': 'The name the other machine sees while pairing.',
  'sync.thisDevice.placeholder': 'Name this device',
  'sync.nameSanitised':
    'Saved as "{name}". A device name cannot contain "=" or hidden characters, and is trimmed to fit.',
  'sync.deviceId.label': 'Device ID',
  'sync.deviceId.hint': 'This install’s identity. It never leaves your network.',
  'sync.paired.label': 'Paired devices',
  'sync.paired.hint': 'Only these machines can see your history. Pairing is mutual.',
  'sync.paired.none': 'No devices paired yet. Pair one below to start syncing.',
  'sync.now.button': 'Sync now',
  'sync.now.working': 'Syncing',
  'sync.now.none': 'No paired device is reachable right now.',
  'sync.now.ok': 'Exchanging with your other devices.',
  'sync.syncedAgo': 'Synced {when}',
  'sync.visibleNotSynced': 'Visible on the network, but nothing has synced yet',
  'sync.neverConnected': 'Never connected',
  'sync.lastSeen': 'Last seen {when}',
  'sync.unpairConfirm':
    'Unpair {name}? It stops syncing and needs a new code to come back. Anything already on {name} stays there.',
  'sync.unpair': 'Unpair',
  'sync.pairNew.label': 'Pair a new device',
  'sync.pairNew.hint': 'Either machine can start. Read the six digits aloud or type them across.',
  'sync.direction.show': 'Show a code',
  'sync.direction.enter': 'Enter a code',
  'sync.code.typeOnOther': 'Type it on the other device. Expires in {time}',
  'sync.code.expired': 'This code has expired.',
  'sync.code.showNew': 'Show a new code',
  'sync.code.explain':
    'Parle shows six digits here; type them into the other machine to confirm it’s really yours.',
  'sync.peers.notSearching':
    'Not searching for devices right now. Open Parle on the other machine, turn Sync on there too and make sure both are on the same network.',
  'sync.peers.stillNothing':
    'Still nothing after a while. Check that Parle is open on the other machine with Sync turned on, and that both are on the same network.',
  'sync.peers.macBlocked':
    'If that all looks right, macOS may be blocking Parle from seeing the local network, which looks exactly like this.',
  'sync.peers.openLocalNetwork': 'Open Local Network settings',
  'sync.peers.winBlocked': 'If that all looks right, Windows Firewall may be blocking Parle.',
  'sync.peers.openFirewall': 'Open firewall settings',
  'sync.peers.vpnHint':
    'A VPN is the other common cause: many block local network traffic even while everything else works. Turn it off, or switch on its local network sharing setting.',
  'sync.peers.isolatedHint':
    'Guest and hotel Wi-Fi often stop devices on the same network from seeing each other at all. A phone hotspot is a quick way to rule that out.',
  'sync.peers.looking':
    'Looking for devices on this network… Open Parle on the other machine and turn Sync on there too.',
  'sync.pairing': 'Pairing…',
  'sync.pair': 'Pair',
  'sync.pair.needsDevice':
    'Select the device above first. Nothing is listed until both machines can see each other on the network.',
  'sync.dictations.label': 'Sync dictations',
  'sync.dictations.hintPaired':
    'Everything you dictate shows up in History on both machines. Turning this back on re-sends your history to your paired devices, which can take a moment.',
  'sync.dictations.hint': 'Everything you dictate shows up in History on both machines',
  'sync.clipboard.label': 'Sync clipboard',
  'sync.clipboard.hintPaired':
    'Copy on one machine, paste on the other. Turning this back on re-sends your history to your paired devices, which can take a moment.',
  'sync.clipboard.hint': 'Copy on one machine, paste on the other',

  // ---------- Dictionary ----------
  'dictionary.title': 'Dictionary',
  'dictionary.subtitle':
    "Names, brands and jargon Parle should get right. Terms bias recognition and fix close misspellings, never inserting words you didn't say.",
  'dictionary.term.placeholder': 'Term (exact casing, e.g. “farsiight”)',
  'dictionary.corrections.placeholder':
    'Heard as… (optional, comma-separated, e.g. “far sight, foresight”)',
  'dictionary.add': 'Add',
  'dictionary.empty': 'No terms yet. Add the names and jargon you use every day.',
  'dictionary.autoBadge': 'auto',
  'dictionary.autoBadgeTitle': 'Learned from your corrections',
  'dictionary.fuzzyMatch': 'fuzzy match',

  // Onboarding: language
  'onboarding.language.title': 'Choose your language',
  'onboarding.language.sub': 'Parle will speak this language, and will expect you to dictate in it.',
  'onboarding.language.note': 'You can change either of these later, and they do not have to match: plenty of people run the interface in one language and dictate in another.',

  // ---------- Settings: interface language ----------
  'settings.uiLanguage.label': 'Interface language',
  'settings.uiLanguage.hint': 'The language Parle itself is written in. Separate from the language you dictate in, below.',

  // ---------- Onboarding: what Parle is ----------
  'onboarding.hotkey.openKeyboard': 'Open Keyboard settings',
  'onboarding.about.title': 'What Parle does',
  'onboarding.about.sub': 'Dictation and a clipboard history, both of which stay on this machine.',
  'onboarding.about.dictation.title': 'Talk instead of typing',
  'onboarding.about.dictation.body':
    'Hold your dictation key, say what you want, let go. The text appears where your cursor is. Nothing is sent anywhere: the model runs on this computer, and it works with no internet at all.',
  'onboarding.about.clipboard.title': 'Everything you copy, kept',
  'onboarding.about.clipboard.body':
    'Parle remembers what you copy so you can find it again. Password managers are excluded out of the box, and you can add any other app you would rather it ignored.',
  'onboarding.about.sync.title': 'Your other machines, if you want',
  'onboarding.about.sync.body':
    'Parle can sync to your own devices over your local network, encrypted end to end, with no account and no server in between. It is off until you turn it on, and clipboard sync stays off until you ask for it separately.',
  'onboarding.about.privacy.title': 'It stays yours',
  'onboarding.about.privacy.body':
    'No account, no telemetry, no cloud. Your history is a file on this machine that you can clear at any time.',
};
