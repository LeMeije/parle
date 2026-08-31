// German strings. Mirrors the key order and comment blocks of en.ts exactly so
// the two files diff side by side.
//
// Register: Sie throughout prose, bare infinitive for controls. "Einfügen" is
// used for paste on both platforms (rather than the macOS-only "Einsetzen"),
// because Parle ships on Windows too.
export const de: Record<string, string> = {
  // ---------- Shared ----------
  'common.cancel': 'Abbrechen',
  'common.dismiss': 'Schließen',
  'common.grant': 'Erlauben',
  'common.keepIt': 'Behalten',
  'common.openSystemSettings': 'Systemeinstellungen öffnen',

  // ---------- Relative time ----------
  'time.justNow': 'gerade eben',
  'time.minutesAgo': 'vor {n} Min.',
  'time.hoursAgo': 'vor {n} Std.',
  'time.secondsShort': '{n} s',

  // ---------- App shell ----------
  'app.nav.compose': 'Verfassen',
  'app.nav.history': 'Verlauf',
  'app.nav.dictionary': 'Wörterbuch',
  'app.nav.models': 'Modelle',
  'app.nav.sync': 'Synchronisierung',
  'app.nav.settings': 'Einstellungen',
  'app.record.start': 'Diktat starten',
  'app.record.stop': 'Diktat beenden',
  'app.toast.pasteInstruction': 'Kopiert. Zum Einfügen {keys} drücken',
  'app.toast.inserted': 'Eingefügt: „{text}“',
  'app.toast.copied': 'Kopiert: „{text}“',

  // ---------- Recording overlay (HUD) ----------
  'hud.pasteInstruction': 'Kopiert. Zum Einfügen {keys} drücken',
  'hud.recordingClickToStop': 'Aufnahme. Zum Beenden klicken',
  'hud.transcribing': 'Transkribieren…',
  'hud.stopAndPaste': 'Beenden und einfügen',
  'hud.working': 'Arbeitet…',
  'hud.cancel': 'Abbrechen (Esc)',
  'hud.deck.rec': 'REC',
  'hud.deck.proc': 'PROC',

  // ---------- History ----------
  'history.searchPlaceholder': 'Transkripte und Zwischenablage durchsuchen…  (↑↓ · Enter fügt ein · {copyKeys} kopiert)',
  'history.filter.all': 'Alle',
  'history.filter.dictations': 'Diktate',
  'history.filter.clipboard': 'Zwischenablage',
  'history.filter.allDevices': 'Alle Geräte',
  'history.filter.thisDevice': '{name} (dieses Gerät)',
  'history.gone': 'Dieser Eintrag ist nicht mehr vorhanden. Er wurde auf einem anderen Gerät gelöscht, deshalb wurde die Liste aktualisiert.',
  'history.empty.noMatches': 'Keine Treffer.',
  'history.empty.nothingYet': 'Noch nichts hier. Halten Sie Ihr Tastenkürzel gedrückt und sprechen Sie.',
  'history.deleteConfirm': 'Diesen Eintrag löschen? Das lässt sich nicht rückgängig machen.',
  'history.mayAlsoDelete': 'Möglicherweise wird er dadurch auch von Ihren gekoppelten Geräten gelöscht.',
  'history.alsoDeletesFrom': 'Dadurch wird er auch von {devices} gelöscht.',
  'history.localOnly.badge': 'nur dieses Gerät',
  'history.localOnly.title':
    'Parle konnte nicht ausschließen, dass dies ein Passwortfeld war. Der Eintrag bleibt deshalb auf diesem Gerät und wird nie an Ihre anderen Geräte gesendet',
  'history.fromDevice.title': 'Auf einem anderen Ihrer Geräte geschrieben und hierher synchronisiert',
  'history.action.paste': 'Einfügen',
  'history.action.pasteTitle': 'In die vorherige App einfügen (Enter)',
  'history.action.copy': 'Kopieren',
  'history.action.copyTitle': 'Kopieren ({copyKeys})',
  'history.action.editTitle': 'Bearbeiten (Parle lernt daraus)',
  'history.action.pin': 'Anheften',
  'history.action.unpin': 'Loslösen',
  'history.action.delete': 'Löschen',
  'history.trimmedCount': '{n} gekürzt',
  'history.unsureCount': '{n} unsicher',
  'history.restoreRaw': 'Original zurückholen',

  // ---------- Compose ----------
  'compose.title': 'Verfassen',
  'compose.intro':
    'Diktieren Sie hier und fügen Sie mitten im Satz Links oder Text ein: Jede Einfügung wird an genau den Moment geheftet, in dem Sie sie gemacht haben, und Byte-genau in den fertigen Text übernommen.',
  'compose.start': 'Diktat starten',
  'compose.stop': 'Stopp',
  'compose.transcribing': 'Transkribieren…',
  'compose.recording': 'Aufnahme',
  'compose.processing': 'Verarbeitung',
  'compose.markPlaceholder.recording': 'Link einfügen oder tippen, Enter heftet ihn an diesen Moment…',
  'compose.markPlaceholder.idle': 'Diktat starten, um Links und Text einzufügen',
  'compose.insert': 'Einfügen',
  'compose.noSpeech': 'Keine Sprache erkannt.',
  'compose.copyResult': 'Ergebnis kopieren',
  'compose.alsoInserted': 'Außerdem an der Einfügemarke eingefügt und im Verlauf gespeichert.',
  'compose.barActive':
    'Fügen Sie Text in der Diktierleiste am unteren Fensterrand ein oder tippen Sie dort, aus jedem Tab. Enter heftet ihn hier an.',

  // ---------- Diktierleiste ----------
  'bar.pinnedCount': 'Einfügungen: {n}',
  'bar.pinnedAt': 'Eingefügt bei {time}',
  'bar.openCompose': 'Verfassen öffnen, um Ihre Einfügungen zu sehen',
  'bar.insertHint': 'Enter fügt ein. Shift + Enter macht eine neue Zeile.',

  // ---------- Models ----------
  'models.title': 'Modelle',
  'models.subtitle': 'Die gesamte Transkription läuft auf diesem Gerät.',
  'models.warm': 'Geladen · {model}',
  'models.loadsOnFirstUse': 'Wird bei der ersten Nutzung geladen',
  'models.active': 'Aktiv',
  'models.backendTitle': 'Die Hardware, auf der dieses Modell läuft',
  'models.yourFile': 'Ihre Datei',
  'models.speedRating': 'Tempo {value}/5',
  'models.accuracyRating': 'Genauigkeit {value}/5',
  'models.languageCount': '{n} Sprachen',
  'models.use': 'Verwenden',
  'models.deleteFile': 'Modelldatei löschen',
  'models.removeCustom': 'Aus dieser Liste entfernen (Ihre Datei bleibt erhalten)',
  'models.removeFromList': 'Aus dieser Liste entfernen',
  'models.fileMissing': 'Datei fehlt',
  'models.download': 'Herunterladen',
  'models.addLocal': 'Lokales Modell hinzufügen…',
  'models.addLocal.hint':
    'Eine whisper.cpp-GGML-{ext}-Datei, die Sie bereits haben. Parle verweist auf sie an ihrem Ort und kopiert sie nie, belegt also keinen zusätzlichen Speicher.',
  'models.picker.title': 'whisper.cpp-Modell auswählen',
  'models.picker.filter': 'Whisper-Modell',
  'models.defaultLocalName': 'Lokales Modell',
  'models.fallbackHint':
    'Lässt sich das aktive Modell nicht laden, etwa bei knappem Arbeitsspeicher, wechselt Parle automatisch eine Stufe tiefer: Ihre Aufnahme geht nie verloren.',

  // ---------- Onboarding ----------
  'onboarding.welcome.title': 'Willkommen bei Parle',
  'onboarding.welcome.body':
    'Taste halten, sprechen, loslassen: Ihre Worte erscheinen dort, wo die Einfügemarke steht. Die Transkription läuft vollständig auf diesem Gerät. Nichts, was Sie sagen, verlässt es je.',
  'onboarding.welcome.cta': 'Einrichten',
  'onboarding.permissions.title': 'Berechtigungen',
  'onboarding.permissions.introMac':
    'Parle braucht zwei Berechtigungen, um Sie zu hören und für Sie zu tippen. Beide bleiben auf diesem Rechner.',
  'onboarding.permissions.introWin': 'Parle braucht eine Berechtigung, um Sie zu hören. Sie bleibt auf diesem Rechner.',
  'onboarding.permissions.microphone': 'Mikrofon',
  'onboarding.permissions.microphoneDesc': 'Um Ihr Diktat zu hören',
  'onboarding.permissions.accessibility': 'Bedienungshilfen',
  'onboarding.permissions.accessibilityDesc': 'Um Ihr Tastenkürzel zu erkennen und an der Einfügemarke einzufügen',
  'onboarding.permissions.macNote':
    'Fügen Sie {app} in den Systemeinstellungen unter „Datenschutz & Sicherheit“ → „Bedienungshilfen“ hinzu und kommen Sie dann zurück. Diese Seite aktualisiert sich von selbst. Nach dem Erteilen kann ein Neustart von Parle nötig sein.',
  'onboarding.permissions.appName': 'Parle',
  'onboarding.permissions.openSettings': 'Einstellungen öffnen',
  'onboarding.permissions.continue': 'Fortfahren',
  'onboarding.permissions.waiting': 'Warten auf Berechtigungen…',
  'onboarding.model.title': 'Ihr Modell',
  'onboarding.model.machine': '{ram} GB RAM, {gpu}',
  'onboarding.model.recommendation':
    'Für diesen Rechner ({machine}) empfehlen wir {model}. Modelle können Sie jederzeit unter „Einstellungen“ → „Modelle“ hinzufügen oder wechseln.',
  'onboarding.model.downloadFailed':
    'Download fehlgeschlagen: {error}. Prüfen Sie Ihre Verbindung und versuchen Sie es erneut: Er wird an der Abbruchstelle fortgesetzt.',
  'onboarding.model.ready': 'Modell bereit',
  'onboarding.model.download': 'Herunterladen',
  'onboarding.hotkey.title': 'Ihre Taste',
  'onboarding.hotkey.macKey': '🌐 Fn-Taste',
  'onboarding.hotkey.doNothing': 'Nichts tun',
  'onboarding.hotkey.mac':
    'Standard: die {key}. Halten Sie sie gedrückt und sprechen Sie, loslassen zum Einfügen, oder tippen Sie kurz darauf, um die Aufnahme festzustellen. Tipp: Stellen Sie „Systemeinstellungen“ → „Tastatur“ → „🌐-Taste drücken für“ auf {doNothing}, damit die macOS-Diktierfunktion nicht darum kämpft.',
  'onboarding.hotkey.winKey': 'Ctrl rechts',
  'onboarding.hotkey.win':
    'Standard: {key}. Halten Sie sie gedrückt und sprechen Sie, loslassen zum Einfügen, oder tippen Sie kurz darauf, um die Aufnahme festzustellen. Sie haben eine Copilot-Taste? Belegen Sie sie unter „Einstellungen“ → „Tastenkürzel“, dann übernimmt Parle sie vollständig.',
  'onboarding.hotkey.cta': 'Verstanden',
  'onboarding.test.title': 'Ausprobieren',
  'onboarding.test.body': 'Klicken Sie auf die Schaltfläche (oder nutzen Sie Ihr Tastenkürzel), sagen Sie etwas und stoppen Sie dann.',
  'onboarding.test.start': 'Testdiktat starten',
  'onboarding.test.stop': 'Stopp',
  'onboarding.test.transcribing': 'Transkribieren…',
  'onboarding.test.noSpeech': 'Keine Sprache erkannt. Versuchen Sie es etwas lauter.',
  'onboarding.test.finish': 'Einrichtung abschließen',

  // ---------- Settings: shell ----------
  'settings.title': 'Einstellungen',
  'settings.subtitle': 'Nur lokal. Keine Telemetrie, keine Cloud, niemals.',
  'settings.section.hotkeys': 'Tastenkürzel',
  'settings.section.language': 'Sprache',
  'settings.section.cleanup': 'Bereinigung',
  'settings.section.dictionary': 'Wörterbuch',
  'settings.section.output': 'Ausgabe',
  'settings.section.appearance': 'Darstellung',
  'settings.section.historyPrivacy': 'Verlauf & Datenschutz',
  'settings.section.audio': 'Audio',
  'settings.section.general': 'Allgemein',
  'settings.footer.tagline': 'Diktieren auf dem Gerät',
  'settings.footer.note': 'nichts verlässt je diesen Rechner',

  // ---------- Settings: hotkeys ----------
  'settings.dictationKey.label': 'Diktattaste',
  'settings.dictationKey.hintMac': 'Fn benötigt die Berechtigung „Bedienungshilfen“',
  'settings.dictationKey.hintWin': 'Alt rechts ist auf vielen Layouts AltGr, Ctrl rechts ist sicherer',
  'settings.dictationKey.custom': 'Eigene…',
  'settings.customBinding.label': 'Eigene Belegung',
  'settings.customBinding.hint': 'Klicken Sie, und drücken Sie dann die gewünschte Taste oder Kombination. Esc bricht ab.',
  'settings.customBinding.listening': 'Tastenkombination drücken…',
  'settings.gesture.label': 'Geste',
  'settings.gesture.hintDoubleTap':
    'Doppeltippen startet, einfaches Tippen stoppt. Die Taste wird nie abgefangen, ihre normale Systemfunktion bleibt also erhalten.',
  'settings.gesture.hint': 'Hybrid: halten zum Sprechen; kurzes Tippen stellt bis zum nächsten Tippen fest',
  'settings.gesture.hold': 'Halten',
  'settings.gesture.toggle': 'Umschalten',
  'settings.gesture.hybrid': 'Hybrid',
  'settings.gesture.doubleTap': 'Doppeltippen',
  'settings.latch.label': 'Feststellzeit',
  'settings.latch.hint':
    'Hybrid: Antippen unter dieser Dauer stellt fest. Doppeltippen: maximaler Abstand zwischen zwei Tipps',
  'settings.escCancel.label': 'Esc bricht die Aufnahme ab',
  'settings.escCancel.hint':
    'Standardmäßig aus: Esc wird aus allen möglichen Gründen gedrückt, und eine bereits gesprochene Aufnahme zu verwerfen ist schlimmer, als sie mit dem Tastenkürzel zu beenden',
  'settings.historyPalette.label': 'Verlaufspalette',
  'settings.historyPalette.hint': 'Tastenfolge für die Suche',
  'settings.suppressCopilot.label': 'Copilot-Start unterdrücken',
  'settings.suppressCopilot.hint':
    'Wenn die Copilot-Taste belegt ist (oder dies aktiv ist), öffnet sich die Copilot-App nie',
  'settings.accessibilityMissing':
    'Die Berechtigung „Bedienungshilfen“ fehlt. Sondertasten und das Einfügen an der Einfügemarke funktionieren nicht. Falls Sie sie bereits erteilt haben und diese Warnung bleibt, ist der Eintrag nach einer Neuinstallation veraltet: Nutzen Sie „Reparieren“.',
  'settings.repairPermission': 'Berechtigung reparieren',
  'settings.bindingWarning.leftCtrl':
    'Ctrl links steuert die meisten Tastenkürzel und löst deshalb im normalen Gebrauch aus.',
  'settings.bindingWarning.leftShift':
    'Shift links wird beim Tippen ständig gedrückt, rechnen Sie also mit Fehlauslösungen.',
  'settings.bindingWarning.rightAlt':
    'Alt rechts ist auf vielen Layouts AltGr und erzeugt damit Sonderzeichen. Ctrl rechts ist sicherer.',

  // ---------- Settings: key names ----------
  'keys.fn': '🌐 Fn / Globus',
  'keys.rightCommand': '⌘ rechts',
  'keys.leftCommand': '⌘ links',
  'keys.rightOption': '⌥ rechts',
  'keys.leftOption': '⌥ links',
  'keys.rightControl': '⌃ rechts',
  'keys.leftControl': '⌃ links',
  'keys.copilot': 'Copilot-Taste',
  'keys.rightCtrl': 'Ctrl rechts',
  'keys.leftCtrl': 'Ctrl links',
  'keys.rightShift': 'Shift rechts',
  'keys.leftShift': 'Shift links',
  'keys.leftAlt': 'Alt links',
  'keys.rightAlt': 'Alt rechts',
  'keys.rightWin': 'Win rechts',
  'keys.leftWin': 'Win links',

  // ---------- Settings: language ----------
  'settings.spokenLanguage.label': 'Gesprochene Sprache',
  'settings.language.auto': 'Automatisch erkennen',
  'settings.language.en': 'Englisch',
  'settings.language.es': 'Spanisch',
  'settings.language.fr': 'Französisch',
  'settings.language.de': 'Deutsch',
  'settings.language.it': 'Italienisch',
  'settings.language.pt': 'Portugiesisch',
  'settings.language.nl': 'Niederländisch',
  'settings.language.ja': 'Japanisch',
  'settings.language.ko': 'Koreanisch',
  'settings.language.zh': 'Chinesisch',
  'settings.language.hi': 'Hindi',
  'settings.language.ar': 'Arabisch',
  'settings.language.ru': 'Russisch',
  'settings.language.pl': 'Polnisch',
  'settings.language.sv': 'Schwedisch',
  'settings.localeSpelling.label': 'Regionale Schreibweise',
  'settings.localeSpelling.hint': 'Beeinflusst die Schreibweise der Ausgabe (colour statt color)',
  'settings.locale.none': 'Keine Vorgabe',
  'settings.locale.enAU': 'Englisch (Australien)',
  'settings.locale.enGB': 'Englisch (UK)',
  'settings.locale.enUS': 'Englisch (USA)',
  'settings.applyLocaleSpelling.label': 'Regionale Schreibweise anwenden',
  'settings.applyLocaleSpelling.hint': 'US-Schreibweisen im Transkript in Ihre Region übertragen',
  'settings.translate.label': 'Ins Englische übersetzen',
  'settings.translate.hint': 'Beliebige Sprache sprechen, Englisch einfügen',

  // ---------- Settings: cleanup ----------
  'settings.smartCleanup.label': 'Intelligente Bereinigung',
  'settings.smartCleanup.hint': 'Hauptschalter für die deterministische Bereinigungsstufe',
  'settings.removeFillers.label': 'Füllwörter entfernen',
  'settings.removeFillers.hint': 'äh, ähm, öh…',
  'settings.removeHedges.label': 'Floskeln entfernen',
  'settings.removeHedges.hint': 'sozusagen, wie gesagt (greift stärker ein)',
  'settings.trimSelfCorrections.label': 'Selbstkorrekturen kürzen',
  'settings.trimSelfCorrections.hint':
    '„Donnerstag, nein, eigentlich Mittwoch“ → „Mittwoch“. Gekürzte Stellen bleiben im Verlauf einsehbar',
  'settings.dictatedPunctuation.label': 'Diktierte Satzzeichen',
  'settings.dictatedPunctuation.hint':
    '„Komma“, „neue Zeile“, „Fragezeichen“… („wörtlich Komma“ hebt es auf)',
  'settings.capitalise.label': 'Sätze großschreiben',
  'settings.terminalPunctuation.label': 'Mit Satzzeichen abschließen',
  'settings.paragraphPause.label': 'Absatz bei langer Pause',

  // ---------- Settings: dictionary ----------
  'settings.dictionary.enable': 'Wörterbuch aktivieren',
  'settings.dictionary.bias.label': 'Erkennung beeinflussen',
  'settings.dictionary.bias.hint': 'Ihre Begriffe als Glossar an die Engine übergeben',
  'settings.dictionary.fuzzy.label': 'Ähnliche Schreibfehler korrigieren',
  'settings.dictionary.autoLearn.label': 'Aus meinen Korrekturen lernen',
  'settings.dictionary.autoLearn.hint': 'Ein-Wort-Korrekturen im Verlauf werden zu Korrekturpaaren',

  // ---------- Settings: output ----------
  'settings.insertAtCursor.label': 'An der Einfügemarke einfügen',
  'settings.insertAtCursor.hint': 'Tippt das Ergebnis in die aktive App',
  'settings.copyToClipboard.label': 'In die Zwischenablage kopieren',
  'settings.restoreClipboard.label': 'Vorherige Zwischenablage zurücklegen',
  'settings.restoreClipboard.hint': 'Nach dem Einfügen den alten Inhalt wiederherstellen',
  'settings.restoreDelay.label': 'Verzögerung',
  'settings.restoreDelay.hint': 'Langsame Apps (Office, Remotedesktop) lesen die Zwischenablage verspätet',
  'settings.preferAxInsert.label': 'Direktes Einfügen bevorzugen',
  'settings.preferAxInsert.hint': 'Erst das Einfügen über die Bedienungshilfen versuchen, dann die Zwischenablage',
  'settings.pressEnter.label': 'Nach dem Einfügen Enter drücken',
  'settings.pressEnter.hint':
    'Sendet die Nachricht direkt nach dem Einfügen, praktisch für Chat-Apps. Bei sicheren Feldern wird es nie ausgelöst.',

  // ---------- Settings: appearance ----------
  'settings.theme.label': 'Erscheinungsbild',
  'settings.theme.system': 'System',
  'settings.theme.light': 'Hell',
  'settings.theme.dark': 'Dunkel',
  'settings.palette.label': 'Farbpalette',
  'settings.palette.hint':
    'Pastell färbt sich nach Ihrer Akzentfarbe, probieren Sie es also mit dem eigenen Farbrad',
  'settings.palette.paper': 'Papier',
  'settings.palette.pastel': 'Pastell',
  'settings.palette.bold': 'Kräftig',
  'settings.palette.retro': 'Retro',
  'settings.accent.label': 'Akzent',
  'settings.accent.custom': 'Eigene Farbe',
  'settings.appIcon.label': 'App-Symbol',
  'settings.appIcon.hint': 'Wirkt sofort in der App; das Symbol im Finder folgt nach einem Neustart',
  'settings.appIcon.default': 'Parle',
  'settings.appIcon.keycap': 'Tastenkappe',
  'settings.appIcon.waveform': 'Wellenform',
  'settings.appIcon.echoRings': 'Echoringe',
  'settings.appIcon.cassette': 'Kassette',
  'settings.iconRestart': 'Symbol aktualisiert. Starten Sie neu, um das Symbol im Finder und im Dock zu aktualisieren.',
  'settings.restartParle': 'Parle neu starten',
  'settings.trayIcon.labelMac': 'Menüleistensymbol',
  'settings.trayIcon.labelWin': 'Taskleistensymbol',
  'settings.trayIcon.hintMac': 'Monochrom folgt der Menüleiste; das Badge behält Parles Farbe',
  'settings.trayIcon.hintWin': 'Automatisch wählt die Kontur, die sich von Ihrer Taskleiste abhebt',
  'settings.tray.template': 'Monochrom',
  'settings.tray.badge': 'Blaues Badge',
  'settings.tray.auto': 'Auto: an Taskleiste',
  'settings.tray.light': 'Kontur hell',
  'settings.tray.dark': 'Kontur dunkel',
  'settings.tray.color': 'Blaue Kontur',
  'settings.overlayStyle.label': 'Overlay-Stil',
  'settings.overlayStyle.hintHidden':
    'Gar kein Overlay. Während Parle zuhört, zeigt das Menüleistensymbol einen Punkt in der Ecke, und das ist der einzige Hinweis.',
  'settings.overlayStyle.hint': 'Kassette passt wunderbar zur Palette „Retro“',
  'settings.overlayStyle.pill': 'Kapsel',
  'settings.overlayStyle.cassette': 'Kassette',
  'settings.overlayStyle.metal': 'Metall',
  'settings.overlayStyle.minimal': 'Minimal',
  'settings.overlayStyle.none': 'Keines',
  'settings.waveformSensitivity.label': 'Wellenform-Empfindlichkeit',
  'settings.waveformSensitivity.hint':
    'Erhöhen Sie sie, wenn sich die Balken beim Sprechen kaum bewegen, senken Sie sie, wenn sie oben anstehen. Das ändert nur die Anzeige, nie das, was aufgenommen oder transkribiert wird.',
  'settings.showPartial.label': 'Live-Transkript im Overlay anzeigen',
  'settings.reduceMotion.label': 'Bewegung reduzieren',
  'settings.reduceMotion.hint':
    'Nichts wird animiert, was nicht animiert sein muss. Die Diktierleiste erscheint am unteren Fensterrand, statt aus der Aufnahmeschaltfläche herauszuwachsen, und die Kassettenspulen drehen sich nicht mehr. Sinnvoll, wenn Animationen störend sind, oder auf einem älteren Rechner.',

  // ---------- Settings: history & privacy ----------
  'settings.clipboardCapture.label': 'Zwischenablage erfassen',
  'settings.clipboardCapture.hint':
    'Alles, was Sie kopieren, durchsuchbar. Passwortmanager sind ausgenommen',
  'settings.retention.label': 'Aufbewahrungsdauer',
  'settings.retention.confirmNarrow':
    'Ältere Einträge werden von diesem Gerät gelöscht und lassen sich nicht zurückholen, auch nicht von einem gekoppelten Gerät. Fortfahren?',
  'settings.retention.forever': 'Unbegrenzt',
  'settings.retention.d90': '90 Tage',
  'settings.retention.d30': '30 Tage',
  'settings.retention.d7': '7 Tage',
  'settings.retention.d1': '1 Tag',
  'settings.excludedApps.label': 'Ausgeschlossene Apps',
  'settings.excludedApps.hint':
    'Eine pro Zeile: Bundle-ID auf dem Mac, EXE-Name unter Windows. Diese Liste gilt pro Gerät, tragen Sie den Eintrag also auf jedem Rechner ein. Ab dem Moment, in dem Sie einen Eintrag hinzufügen, sendet Parle keine Daten aus dieser App mehr an Ihre anderen Geräte. Bereits Synchronisiertes bleibt auf diesen Geräten erhalten.',
  'settings.dangerZone.label': 'Gefahrenzone',
  'settings.clearHistory.button': 'Nicht angehefteten Verlauf löschen',
  'settings.clearHistory.confirmWithDevices':
    'Dadurch wird jeder nicht angeheftete Eintrag auf diesem Gerät und auf {devices} gelöscht. Angeheftete Einträge bleiben. Das lässt sich nicht rückgängig machen.',
  'settings.clearHistory.confirm':
    'Dadurch wird jeder nicht angeheftete Eintrag auf diesem Gerät gelöscht. Angeheftete Einträge bleiben. Das lässt sich nicht rückgängig machen.',
  'settings.clearHistory.clearIt': 'Löschen',

  // ---------- Settings: audio ----------
  'settings.microphone.label': 'Mikrofon',
  'settings.microphone.systemDefault': 'Systemstandard',
  'settings.minDuration.label': 'Aufnahmen ignorieren unter',
  'settings.microphoneDenied': 'Der Mikrofonzugriff wurde verweigert.',

  // ---------- Settings: general ----------
  'settings.launchAtLogin.label': 'Bei der Anmeldung starten',
  'settings.prewarm.label': 'Modell beim Start vorladen',
  'settings.prewarm.hint': 'Belegt im Leerlauf Speicher, dafür startet das erste Diktat sofort',

  // ---------- Settings: sync ----------
  'sync.section': 'Sync',
  'sync.unavailable': 'Sync ist gerade nicht verfügbar. {error}',
  'sync.checking': 'Sync wird geprüft…',
  'sync.genericError': 'Etwas ist schiefgelaufen.',
  'sync.enable.label': 'Sync mit meinen anderen Geräten',
  'sync.enable.hint':
    'Aus, bis Sie es einschalten. Ihr Mac und Ihr PC sprechen über Ihr lokales Netzwerk direkt miteinander, Ende-zu-Ende-verschlüsselt, ohne Konto und ohne dass irgendetwas irgendwohin hochgeladen wird. Solange es an ist, gibt Parle den Namen dieses Geräts an andere Rechner im selben Netzwerk bekannt, damit sie es finden können.',
  'sync.tryAgain': 'Erneut versuchen',
  'sync.thisDevice.label': 'Dieses Gerät',
  'sync.thisDevice.hint': 'Der Name, den das andere Gerät beim Koppeln sieht.',
  'sync.thisDevice.placeholder': 'Dieses Gerät benennen',
  'sync.nameSanitised':
    'Gespeichert als „{name}“. Ein Gerätename darf kein „=“ und keine unsichtbaren Zeichen enthalten und wird passend gekürzt.',
  'sync.deviceId.label': 'Geräte-ID',
  'sync.deviceId.hint': 'Die Identität dieser Installation. Sie verlässt nie Ihr Netzwerk.',
  'sync.paired.label': 'Gekoppelte Geräte',
  'sync.paired.hint': 'Nur diese Rechner können Ihren Verlauf sehen. Die Kopplung gilt beidseitig.',
  'sync.paired.none': 'Noch keine Geräte gekoppelt. Koppeln Sie unten eines, um zu starten.',
  'sync.now.button': 'Jetzt synchronisieren',
  'sync.now.working': 'Synchronisiert',
  'sync.now.none': 'Derzeit ist kein gekoppeltes Gerät erreichbar.',
  'sync.now.ok': 'Austausch mit Ihren anderen Geräten läuft.',
  'sync.syncedAgo': 'Synchronisiert {when}',
  'sync.visibleNotSynced': 'Im Netzwerk sichtbar, aber noch nichts synchronisiert',
  'sync.neverConnected': 'Nie verbunden',
  'sync.lastSeen': 'Zuletzt gesehen {when}',
  'sync.unpairConfirm':
    '{name} entkoppeln? Die Synchronisierung endet und für eine Rückkehr ist ein neuer Code nötig. Was bereits auf {name} liegt, bleibt dort.',
  'sync.unpair': 'Entkoppeln',
  'sync.pairNew.label': 'Neues Gerät koppeln',
  'sync.pairNew.hint': 'Beide Rechner können anfangen. Lesen Sie die sechs Ziffern vor oder tippen Sie sie drüben ein.',
  'sync.direction.show': 'Code anzeigen',
  'sync.direction.enter': 'Code eingeben',
  'sync.code.typeOnOther': 'Geben Sie ihn auf dem anderen Gerät ein. Läuft ab in {time}',
  'sync.code.expired': 'Dieser Code ist abgelaufen.',
  'sync.code.showNew': 'Neuen Code anzeigen',
  'sync.code.explain':
    'Parle zeigt hier sechs Ziffern; geben Sie sie auf dem anderen Rechner ein, um zu bestätigen, dass er wirklich Ihnen gehört.',
  'sync.peers.notSearching':
    'Es wird gerade nicht nach Geräten gesucht. Öffnen Sie Parle auf dem anderen Rechner, schalten Sie Sync auch dort ein und achten Sie darauf, dass beide im selben Netzwerk sind.',
  'sync.peers.stillNothing':
    'Nach einer Weile immer noch nichts. Prüfen Sie, ob Parle auf dem anderen Rechner geöffnet und Sync dort eingeschaltet ist und ob beide im selben Netzwerk sind.',
  'sync.peers.macBlocked':
    'Wenn das alles stimmt, blockiert macOS möglicherweise Parles Zugriff auf das lokale Netzwerk, was genau so aussieht.',
  'sync.peers.openLocalNetwork': '„Lokales Netzwerk“ öffnen',
  'sync.peers.winBlocked': 'Wenn das alles stimmt, blockiert möglicherweise die Windows-Firewall Parle.',
  'sync.peers.openFirewall': 'Firewall-Einstellungen öffnen',
  'sync.peers.vpnHint':
    'Ein VPN ist die andere häufige Ursache: Viele blockieren den lokalen Netzwerkverkehr, auch wenn alles andere funktioniert. Schalten Sie es aus oder aktivieren Sie dessen Einstellung zur lokalen Netzwerkfreigabe.',
  'sync.peers.isolatedHint':
    'Gäste und Hotel WLANs verhindern oft, dass sich Geräte im selben Netzwerk überhaupt sehen. Ein Hotspot vom Telefon schließt das schnell aus.',
  'sync.peers.looking':
    'Suche nach Geräten in diesem Netzwerk… Öffnen Sie Parle auf dem anderen Rechner und schalten Sie Sync auch dort ein.',
  'sync.pairing': 'Wird gekoppelt…',
  'sync.pair': 'Koppeln',
  'sync.pair.needsDevice':
    'Wählen Sie zuerst oben das Gerät aus. Es wird nichts aufgeführt, solange sich beide Rechner im Netzwerk nicht sehen.',
  'sync.dictations.label': 'Diktate synchronisieren',
  'sync.dictations.hintPaired':
    'Alles, was Sie diktieren, erscheint im Verlauf beider Rechner. Schalten Sie dies wieder ein, wird Ihr Verlauf erneut an Ihre gekoppelten Geräte gesendet, was einen Moment dauern kann.',
  'sync.dictations.hint': 'Alles, was Sie diktieren, erscheint im Verlauf beider Rechner',
  'sync.clipboard.label': 'Zwischenablage synchronisieren',
  'sync.clipboard.hintPaired':
    'Auf einem Rechner kopieren, auf dem anderen einfügen. Schalten Sie dies wieder ein, wird Ihr Verlauf erneut an Ihre gekoppelten Geräte gesendet, was einen Moment dauern kann.',
  'sync.clipboard.hint': 'Auf einem Rechner kopieren, auf dem anderen einfügen',

  // ---------- Dictionary ----------
  'dictionary.title': 'Wörterbuch',
  'dictionary.subtitle':
    'Namen, Marken und Fachbegriffe, die Parle richtig treffen soll. Begriffe beeinflussen die Erkennung und korrigieren ähnliche Schreibfehler, fügen aber nie Wörter ein, die Sie nicht gesagt haben.',
  'dictionary.term.placeholder': 'Begriff (exakte Schreibweise, z. B. „farsiight“)',
  'dictionary.corrections.placeholder':
    'Verstanden als… (optional, mit Komma getrennt, z. B. „far sight, foresight“)',
  'dictionary.add': 'Hinzufügen',
  'dictionary.empty': 'Noch keine Begriffe. Fügen Sie die Namen und Fachbegriffe hinzu, die Sie täglich nutzen.',
  'dictionary.autoBadge': 'auto',
  'dictionary.autoBadgeTitle': 'Aus Ihren Korrekturen gelernt',
  'dictionary.fuzzyMatch': 'unscharfer Treffer',

  // Onboarding: language
  'onboarding.language.title': 'Wählen Sie Ihre Sprache',
  'onboarding.language.sub': 'Parle spricht diese Sprache und erwartet, dass Sie darin diktieren.',
  'onboarding.language.note': 'Sie können beides später ändern, und beides muss nicht übereinstimmen: Viele Menschen nutzen die Oberfläche in einer Sprache und diktieren in einer anderen.',

  // ---------- Settings: interface language ----------
  'settings.uiLanguage.label': 'Oberflächensprache',
  'settings.uiLanguage.hint': 'Die Sprache, in der Parle selbst geschrieben ist. Getrennt von der unten eingestellten Diktatsprache.',

  // ---------- Onboarding: what Parle is ----------
  'onboarding.hotkey.openKeyboard': 'Tastatur-Einstellungen öffnen',
  'onboarding.about.title': 'Was Parle macht',
  'onboarding.about.sub': 'Diktieren und ein Verlauf der Zwischenablage, beides bleibt auf diesem Rechner.',
  'onboarding.about.dictation.title': 'Sprechen statt tippen',
  'onboarding.about.dictation.body':
    'Halten Sie Ihre Diktattaste gedrückt, sagen Sie, was Sie wollen, und lassen Sie los. Der Text erscheint dort, wo die Einfügemarke steht. Nichts wird irgendwohin gesendet: Das Modell läuft auf diesem Rechner, und es funktioniert ganz ohne Internet.',
  'onboarding.about.clipboard.title': 'Alles, was Sie kopieren, aufgehoben',
  'onboarding.about.clipboard.body':
    'Parle merkt sich, was Sie kopieren, damit Sie es wiederfinden. Passwortmanager sind von Haus aus ausgenommen, und Sie können jede weitere App hinzufügen, die Parle lieber ignorieren soll.',
  'onboarding.about.sync.title': 'Ihre anderen Rechner, wenn Sie mögen',
  'onboarding.about.sync.body':
    'Parle kann sich über Ihr lokales Netzwerk mit Ihren eigenen Geräten synchronisieren, Ende-zu-Ende-verschlüsselt, ohne Konto und ohne Server dazwischen. Es ist aus, bis Sie es einschalten, und die Zwischenablage wird erst synchronisiert, wenn Sie das separat einschalten.',
  'onboarding.about.privacy.title': 'Es bleibt bei Ihnen',
  'onboarding.about.privacy.body':
    'Kein Konto, keine Telemetrie, keine Cloud. Ihr Verlauf ist eine Datei auf diesem Rechner, die Sie jederzeit löschen können.',
};
