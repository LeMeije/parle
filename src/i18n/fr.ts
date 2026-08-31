// French UI strings. Mirrors en.ts key for key, in the same order and with the
// same comment blocks, so the two files diff side by side. Several values
// describe security behaviour precisely and are translated to the letter.
export const fr: Record<string, string> = {
  // ---------- Shared ----------
  'common.cancel': 'Annuler',
  'common.dismiss': 'Ignorer',
  'common.grant': 'Autoriser',
  'common.keepIt': 'Conserver',
  'common.openSystemSettings': 'Ouvrir les réglages système',

  // ---------- Relative time ----------
  'time.justNow': 'à l’instant',
  'time.minutesAgo': 'il y a {n} min',
  'time.hoursAgo': 'il y a {n} h',
  'time.secondsShort': '{n} s',

  // ---------- App shell ----------
  'app.nav.compose': 'Rédiger',
  'app.nav.history': 'Historique',
  'app.nav.dictionary': 'Dictionnaire',
  'app.nav.models': 'Modèles',
  'app.nav.sync': 'Synchronisation',
  'app.nav.settings': 'Réglages',
  'app.record.start': 'Démarrer la dictée',
  'app.record.stop': 'Arrêter la dictée',
  'app.toast.pasteInstruction': 'Copié. Appuyez sur {keys} pour coller',
  'app.toast.inserted': '« {text} » inséré',
  'app.toast.copied': '« {text} » copié',

  // ---------- Recording overlay (HUD) ----------
  'hud.pasteInstruction': 'Copié. Appuyez sur {keys} pour coller',
  'hud.recordingClickToStop': 'Enregistrement. Cliquez pour arrêter',
  'hud.transcribing': 'Transcription…',
  'hud.stopAndPaste': 'Arrêter et coller',
  'hud.working': 'Traitement…',
  'hud.cancel': 'Annuler (Échap)',
  'hud.deck.rec': 'REC',
  'hud.deck.proc': 'PROC',

  // ---------- History ----------
  'history.searchPlaceholder': 'Rechercher dans les dictées et le presse-papiers…  (↑↓ · Entrée colle · {copyKeys} copie)',
  'history.filter.all': 'Tout',
  'history.filter.dictations': 'Dictées',
  'history.filter.clipboard': 'Presse-papiers',
  'history.filter.allDevices': 'Tous les appareils',
  'history.filter.thisDevice': '{name} (cet appareil)',
  'history.gone': 'Cet élément n’est plus là. Il a été supprimé sur un autre appareil, la liste a donc été actualisée.',
  'history.empty.noMatches': 'Aucun résultat.',
  'history.empty.nothingYet': 'Rien ici pour l’instant. Maintenez votre raccourci et parlez.',
  'history.deleteConfirm': 'Supprimer cet élément ? Cette action est irréversible.',
  'history.mayAlsoDelete': 'Cela peut aussi le supprimer de vos appareils associés.',
  'history.alsoDeletesFrom': 'Cela le supprime aussi de {devices}.',
  'history.localOnly.badge': 'cet appareil uniquement',
  'history.localOnly.title':
    'Parle n’a pas pu exclure qu’il s’agisse d’un champ de mot de passe : cet élément est donc conservé sur cet appareil et n’est jamais envoyé à vos autres appareils',
  'history.fromDevice.title': 'Écrit sur un autre de vos appareils et synchronisé ici',
  'history.action.paste': 'Coller',
  'history.action.pasteTitle': 'Coller dans l’app précédente (Entrée)',
  'history.action.copy': 'Copier',
  'history.action.copyTitle': 'Copier ({copyKeys})',
  'history.action.editTitle': 'Modifier (alimente l’apprentissage auto)',
  'history.action.pin': 'Épingler',
  'history.action.unpin': 'Désépingler',
  'history.action.delete': 'Supprimer',
  'history.trimmedCount': '{n} coupés',
  'history.unsureCount': '{n} incertains',
  'history.restoreRaw': 'Restaurer l’original',

  // ---------- Compose ----------
  'compose.title': 'Rédiger',
  'compose.intro':
    'Dictez ici et collez des liens ou du texte en cours de phrase : chaque insertion est épinglée à l’instant précis où vous l’avez ajoutée, puis intégrée au texte final, à l’octet près.',
  'compose.start': 'Démarrer la dictée',
  'compose.stop': 'Arrêter',
  'compose.transcribing': 'Transcription…',
  'compose.recording': 'enregistrement',
  'compose.processing': 'traitement',
  'compose.markPlaceholder.recording': 'Collez un lien ou saisissez du texte, Entrée l’épingle à cet instant…',
  'compose.markPlaceholder.idle': 'Lancez la dictée pour insérer liens et texte',
  'compose.insert': 'Insérer',
  'compose.noSpeech': 'Aucune parole détectée.',
  'compose.copyResult': 'Copier le résultat',
  'compose.alsoInserted': 'Également inséré au curseur et enregistré dans l’Historique.',
  'compose.barActive':
    'Collez ou saisissez du texte dans la barre de dictée en bas de la fenêtre, depuis n’importe quel onglet. Entrée l’épingle ici.',

  // ---------- Barre de dictée ----------
  'bar.pinnedCount': 'insertions : {n}',
  'bar.pinnedAt': 'Inséré à {time}',
  'bar.openCompose': 'Ouvrir Rédiger pour voir vos insertions',
  'bar.insertHint': 'Entrée insère. Maj + Entrée ajoute une ligne.',

  // ---------- Models ----------
  'models.title': 'Modèles',
  'models.subtitle': 'Toute la transcription se fait sur cet appareil.',
  'models.warm': 'Chargé · {model}',
  'models.loadsOnFirstUse': 'Chargé à la première utilisation',
  'models.active': 'Actif',
  'models.backendTitle': 'Le matériel sur lequel ce modèle s’exécute',
  'models.yourFile': 'Votre fichier',
  'models.speedRating': 'vitesse {value}/5',
  'models.accuracyRating': 'précision {value}/5',
  'models.languageCount': '{n} langues',
  'models.use': 'Utiliser',
  'models.deleteFile': 'Supprimer le fichier du modèle',
  'models.removeCustom': 'Retirer de cette liste (votre fichier n’est pas supprimé)',
  'models.removeFromList': 'Retirer de cette liste',
  'models.fileMissing': 'Fichier introuvable',
  'models.download': 'Télécharger',
  'models.addLocal': 'Ajouter un modèle local…',
  'models.addLocal.hint':
    'Un fichier GGML whisper.cpp {ext} que vous avez déjà. Parle le lit là où il se trouve et ne le copie jamais : il ne prend aucun espace disque supplémentaire.',
  'models.picker.title': 'Choisir un modèle whisper.cpp',
  'models.picker.filter': 'Modèle Whisper',
  'models.defaultLocalName': 'Modèle local',
  'models.fallbackHint':
    'Si le modèle actif ne se charge pas (sous pression mémoire par exemple), Parle redescend automatiquement d’un cran dans la liste : votre enregistrement n’est jamais perdu.',

  // ---------- Onboarding ----------
  'onboarding.welcome.title': 'Bienvenue dans Parle',
  'onboarding.welcome.body':
    'Maintenez une touche, parlez, relâchez : vos mots apparaissent là où se trouve votre curseur. La transcription se fait entièrement sur cet appareil. Rien de ce que vous dites n’en sort jamais.',
  'onboarding.welcome.cta': 'Configurer',
  'onboarding.permissions.title': 'Autorisations',
  'onboarding.permissions.introMac':
    'Parle a besoin de deux autorisations pour vous entendre et écrire à votre place. Les deux restent sur cette machine.',
  'onboarding.permissions.introWin': 'Parle a besoin d’une autorisation, pour vous entendre. Elle reste sur cette machine.',
  'onboarding.permissions.microphone': 'Micro',
  'onboarding.permissions.microphoneDesc': 'Pour entendre votre dictée',
  'onboarding.permissions.accessibility': 'Accessibilité',
  'onboarding.permissions.accessibilityDesc': 'Pour surveiller votre raccourci et coller au curseur',
  'onboarding.permissions.macNote':
    'Dans Réglages système, ajoutez {app} sous Confidentialité et sécurité → Accessibilité, puis revenez ici. Cette page se met à jour toute seule. Un redémarrage de Parle peut être nécessaire après l’autorisation.',
  'onboarding.permissions.appName': 'Parle',
  'onboarding.permissions.openSettings': 'Ouvrir les Réglages',
  'onboarding.permissions.continue': 'Continuer',
  'onboarding.permissions.waiting': 'En attente des autorisations…',
  'onboarding.model.title': 'Votre modèle',
  'onboarding.model.machine': '{ram} GB de RAM, {gpu}',
  'onboarding.model.recommendation':
    'D’après cette machine ({machine}), nous recommandons {model}. Vous pouvez ajouter ou changer de modèle à tout moment dans Réglages → Modèles.',
  'onboarding.model.downloadFailed':
    'Échec du téléchargement : {error}. Vérifiez votre connexion et réessayez : il reprend là où il s’est arrêté.',
  'onboarding.model.ready': 'Modèle prêt',
  'onboarding.model.download': 'Télécharger',
  'onboarding.hotkey.title': 'Votre touche',
  'onboarding.hotkey.macKey': '🌐 touche Fn',
  'onboarding.hotkey.doNothing': 'Ne rien faire',
  'onboarding.hotkey.mac':
    'Par défaut : la {key}. Maintenez-la et parlez, relâchez pour coller, ou appuyez brièvement pour verrouiller l’enregistrement. Astuce : réglez Réglages système → Clavier → « Appuyer sur la touche 🌐 pour » sur {doNothing}, pour que la dictée de macOS ne vous la dispute pas.',
  'onboarding.hotkey.winKey': 'Ctrl droite',
  'onboarding.hotkey.win':
    'Par défaut : {key}. Maintenez-la et parlez, relâchez pour coller, ou appuyez brièvement pour verrouiller l’enregistrement. Vous avez une touche Copilot ? Associez-la dans Réglages → Raccourcis et Parle la prendra entièrement en charge.',
  'onboarding.hotkey.cta': 'Compris',
  'onboarding.test.title': 'Essayez',
  'onboarding.test.body': 'Cliquez sur le bouton (ou utilisez votre raccourci), dites quelque chose, puis arrêtez.',
  'onboarding.test.start': 'Lancer une dictée d’essai',
  'onboarding.test.stop': 'Arrêter',
  'onboarding.test.transcribing': 'Transcription…',
  'onboarding.test.noSpeech': 'Aucune parole détectée. Réessayez en parlant un peu plus fort.',
  'onboarding.test.finish': 'Terminer la configuration',

  // ---------- Settings: shell ----------
  'settings.title': 'Réglages',
  'settings.subtitle': 'Tout reste en local. Aucune télémétrie, aucun cloud, jamais.',
  'settings.section.hotkeys': 'Raccourcis',
  'settings.section.language': 'Langue',
  'settings.section.cleanup': 'Nettoyage',
  'settings.section.dictionary': 'Dictionnaire',
  'settings.section.output': 'Sortie',
  'settings.section.appearance': 'Apparence',
  'settings.section.historyPrivacy': 'Historique et confidentialité',
  'settings.section.audio': 'Audio',
  'settings.section.general': 'Général',
  'settings.footer.tagline': 'dictée sur l’appareil',
  'settings.footer.note': 'rien ne quitte jamais cette machine',

  // ---------- Settings: hotkeys ----------
  'settings.dictationKey.label': 'Touche de dictée',
  'settings.dictationKey.hintMac': 'Fn requiert l’autorisation Accessibilité',
  'settings.dictationKey.hintWin': 'Alt droite fait office d’AltGr sur beaucoup de dispositions, Ctrl droite est plus sûre',
  'settings.dictationKey.custom': 'Personnalisée…',
  'settings.customBinding.label': 'Raccourci personnalisé',
  'settings.customBinding.hint': 'Cliquez, puis appuyez sur la touche ou la combinaison voulue. Échap annule.',
  'settings.customBinding.listening': 'Appuyez sur une combinaison…',
  'settings.gesture.label': 'Geste',
  'settings.gesture.hintDoubleTap':
    'Un double appui démarre, un simple appui arrête. La touche n’est jamais interceptée : son comportement système habituel continue de fonctionner.',
  'settings.gesture.hint': 'Hybride : maintenez pour parler ; un appui bref verrouille jusqu’au suivant',
  'settings.gesture.hold': 'Maintien',
  'settings.gesture.toggle': 'Bascule',
  'settings.gesture.hybrid': 'Hybride',
  'settings.gesture.doubleTap': 'Double appui',
  'settings.latch.label': 'Délai de verrouillage',
  'settings.latch.hint':
    'Hybride : un appui plus court que cela bascule en mode verrouillé. Double appui : écart maximal entre les appuis',
  'settings.escCancel.label': 'Échap annule l’enregistrement',
  'settings.escCancel.hint':
    'Désactivé par défaut : on appuie sur Échap pour toutes sortes de raisons sans rapport, et jeter une prise déjà dictée est pire que de l’arrêter avec votre raccourci',
  'settings.historyPalette.label': 'Palette d’historique',
  'settings.historyPalette.hint': 'Combinaison de touches pour la recherche',
  'settings.suppressCopilot.label': 'Bloquer le lancement de Copilot',
  'settings.suppressCopilot.hint':
    'Quand la touche Copilot est associée (ou que cette option est activée), l’app Copilot par défaut ne s’ouvre jamais',
  'settings.accessibilityMissing':
    'L’autorisation Accessibilité est absente. Les touches spéciales et le collage au curseur ne fonctionneront pas. Si vous l’avez déjà accordée et que cet avertissement persiste, l’entrée est devenue obsolète après une recompilation : utilisez Réparer.',
  'settings.repairPermission': 'Réparer l’autorisation',
  'settings.bindingWarning.leftCtrl':
    'Ctrl gauche pilote la plupart des raccourcis clavier : l’associer la déclenchera en usage normal.',
  'settings.bindingWarning.leftShift':
    'Maj gauche est sollicitée en permanence pendant la frappe : attendez-vous à des déclenchements intempestifs.',
  'settings.bindingWarning.rightAlt':
    'Alt droite fait office d’AltGr sur beaucoup de dispositions : elle sert à taper des caractères accentués. Ctrl droite est plus sûre.',

  // ---------- Settings: key names ----------
  'keys.fn': '🌐 Fn / Globe',
  'keys.rightCommand': '⌘ droite',
  'keys.leftCommand': '⌘ gauche',
  'keys.rightOption': '⌥ droite',
  'keys.leftOption': '⌥ gauche',
  'keys.rightControl': '⌃ droite',
  'keys.leftControl': '⌃ gauche',
  'keys.copilot': 'Touche Copilot',
  'keys.rightCtrl': 'Ctrl droite',
  'keys.leftCtrl': 'Ctrl gauche',
  'keys.rightShift': 'Maj droite',
  'keys.leftShift': 'Maj gauche',
  'keys.leftAlt': 'Alt gauche',
  'keys.rightAlt': 'Alt droite',
  'keys.rightWin': 'Win droite',
  'keys.leftWin': 'Win gauche',

  // ---------- Settings: language ----------
  'settings.spokenLanguage.label': 'Langue parlée',
  'settings.language.auto': 'Détection auto',
  'settings.language.en': 'Anglais',
  'settings.language.es': 'Espagnol',
  'settings.language.fr': 'Français',
  'settings.language.de': 'Allemand',
  'settings.language.it': 'Italien',
  'settings.language.pt': 'Portugais',
  'settings.language.nl': 'Néerlandais',
  'settings.language.ja': 'Japonais',
  'settings.language.ko': 'Coréen',
  'settings.language.zh': 'Chinois',
  'settings.language.hi': 'Hindi',
  'settings.language.ar': 'Arabe',
  'settings.language.ru': 'Russe',
  'settings.language.pl': 'Polonais',
  'settings.language.sv': 'Suédois',
  'settings.localeSpelling.label': 'Orthographe régionale',
  'settings.localeSpelling.hint': 'Influe sur l’orthographe du texte produit (colour ou color)',
  'settings.locale.none': 'Aucune préférence',
  'settings.locale.enAU': 'Anglais (Australie)',
  'settings.locale.enGB': 'Anglais (Royaume-Uni)',
  'settings.locale.enUS': 'Anglais (États-Unis)',
  'settings.applyLocaleSpelling.label': 'Appliquer l’orthographe régionale',
  'settings.applyLocaleSpelling.hint': 'Convertir les graphies américaines de la transcription vers votre région',
  'settings.translate.label': 'Traduire en anglais',
  'settings.translate.hint': 'Parlez n’importe quelle langue, collez de l’anglais',

  // ---------- Settings: cleanup ----------
  'settings.smartCleanup.label': 'Nettoyage intelligent',
  'settings.smartCleanup.hint': 'Interrupteur général du nettoyage déterministe',
  'settings.removeFillers.label': 'Supprimer les mots de remplissage',
  'settings.removeFillers.hint': 'euh, heu, hum…',
  'settings.removeHedges.label': 'Supprimer les tournures vagues',
  'settings.removeHedges.hint': 'vous voyez, tu vois (plus agressif)',
  'settings.trimSelfCorrections.label': 'Couper les autocorrections',
  'settings.trimSelfCorrections.hint':
    '« jeudi, non en fait mercredi » → « mercredi ». Les passages coupés restent consultables dans l’Historique',
  'settings.dictatedPunctuation.label': 'Ponctuation dictée',
  'settings.dictatedPunctuation.hint':
    '« virgule », « à la ligne », « point d’interrogation »… (« littéralement virgule » échappe)',
  'settings.capitalise.label': 'Majuscule en début de phrase',
  'settings.terminalPunctuation.label': 'Terminer par une ponctuation',
  'settings.paragraphPause.label': 'Paragraphe après une longue pause',

  // ---------- Settings: dictionary ----------
  'settings.dictionary.enable': 'Activer le dictionnaire',
  'settings.dictionary.bias.label': 'Orienter la reconnaissance',
  'settings.dictionary.bias.hint': 'Transmettre vos termes au moteur comme glossaire',
  'settings.dictionary.fuzzy.label': 'Corriger les graphies proches',
  'settings.dictionary.autoLearn.label': 'Apprendre de mes corrections',
  'settings.dictionary.autoLearn.hint': 'Les modifications d’un seul mot dans l’Historique deviennent des paires de correction',

  // ---------- Settings: output ----------
  'settings.insertAtCursor.label': 'Insérer au curseur',
  'settings.insertAtCursor.hint': 'Écrit le résultat dans l’app active',
  'settings.copyToClipboard.label': 'Copier dans le presse-papiers',
  'settings.restoreClipboard.label': 'Restaurer le presse-papiers précédent',
  'settings.restoreClipboard.hint': 'Après un collage, remet en place votre ancien presse-papiers',
  'settings.restoreDelay.label': 'Délai de restauration',
  'settings.restoreDelay.hint': 'Les apps lentes (Office, bureau à distance) lisent le presse-papiers tardivement',
  'settings.preferAxInsert.label': 'Privilégier l’insertion directe',
  'settings.preferAxInsert.hint': 'Essayer l’insertion de texte via Accessibilité avant le collage',
  'settings.pressEnter.label': 'Appuyer sur Entrée après l’insertion',
  'settings.pressEnter.hint':
    'Envoie le message juste après le collage, pratique pour les messageries. Ne se déclenche jamais dans un champ sécurisé.',

  // ---------- Settings: appearance ----------
  'settings.theme.label': 'Thème',
  'settings.theme.system': 'Système',
  'settings.theme.light': 'Clair',
  'settings.theme.dark': 'Sombre',
  'settings.palette.label': 'Palette',
  'settings.palette.hint':
    'Pastel se teinte de votre couleur d’accentuation : essayez-la avec la roue personnalisée',
  'settings.palette.paper': 'Papier',
  'settings.palette.pastel': 'Pastel',
  'settings.palette.bold': 'Vif',
  'settings.palette.retro': 'Rétro',
  'settings.accent.label': 'Accentuation',
  'settings.accent.custom': 'Couleur personnalisée',
  'settings.appIcon.label': 'Icône de l’app',
  'settings.appIcon.hint': 'S’applique aussitôt dans l’app ; l’icône du Finder se met à jour après un redémarrage',
  'settings.appIcon.default': 'Parle',
  'settings.appIcon.keycap': 'Touche',
  'settings.appIcon.waveform': 'Forme d’onde',
  'settings.appIcon.echoRings': 'Cercles d’écho',
  'settings.appIcon.cassette': 'Cassette',
  'settings.iconRestart': 'Icône mise à jour. Redémarrez pour actualiser l’icône du Finder et du Dock.',
  'settings.restartParle': 'Redémarrer Parle',
  'settings.trayIcon.labelMac': 'Icône de la barre des menus',
  'settings.trayIcon.labelWin': 'Icône de la zone de notification',
  'settings.trayIcon.hintMac': 'Monochrome suit la barre des menus ; le badge garde la couleur de Parle',
  'settings.trayIcon.hintWin': 'Auto choisit le contour qui ressort sur votre barre des tâches',
  'settings.tray.template': 'Monochrome',
  'settings.tray.badge': 'Badge bleu',
  'settings.tray.auto': 'Auto : selon la barre des tâches',
  'settings.tray.light': 'Contour clair',
  'settings.tray.dark': 'Contour sombre',
  'settings.tray.color': 'Contour bleu',
  'settings.overlayStyle.label': 'Style de l’incrustation',
  'settings.overlayStyle.hintHidden':
    'Aucune incrustation. Pendant que Parle écoute, l’icône de la barre des menus affiche un point dans son coin, et c’est la seule indication.',
  'settings.overlayStyle.hint': 'Cassette s’accorde à merveille avec la palette Rétro',
  'settings.overlayStyle.pill': 'Pastille',
  'settings.overlayStyle.cassette': 'Cassette',
  'settings.overlayStyle.metal': 'Métal',
  'settings.overlayStyle.minimal': 'Minimal',
  'settings.overlayStyle.none': 'Aucune',
  'settings.waveformSensitivity.label': 'Sensibilité de l’onde',
  'settings.waveformSensitivity.hint':
    'Augmentez-la si les barres bougent à peine quand vous parlez, baissez-la si elles restent en haut. Elle change ce qu’affiche l’indicateur, jamais ce qui est enregistré ni transcrit.',
  'settings.showPartial.label': 'Transcription en direct dans l’incrustation',
  'settings.reduceMotion.label': 'Réduire les animations',
  'settings.reduceMotion.hint':
    'Plus rien ne s’animera sans nécessité. La barre de dictée apparaît en bas de la fenêtre au lieu de se déployer depuis le bouton d’enregistrement, et les bobines de cassette cessent de tourner. Utile si les animations vous gênent, ou sur une machine plus ancienne.',

  // ---------- Settings: history & privacy ----------
  'settings.clipboardCapture.label': 'Capturer le presse-papiers',
  'settings.clipboardCapture.hint':
    'Tout ce que vous copiez, avec recherche. Les gestionnaires de mots de passe sont exclus',
  'settings.retention.label': 'Conserver pendant',
  'settings.retention.confirmNarrow':
    'Les éléments plus anciens que cette durée seront supprimés de cet appareil et ne pourront pas être récupérés, même depuis un appareil associé. Continuer ?',
  'settings.retention.forever': 'Toujours',
  'settings.retention.d90': '90 jours',
  'settings.retention.d30': '30 jours',
  'settings.retention.d7': '7 jours',
  'settings.retention.d1': '1 jour',
  'settings.excludedApps.label': 'Apps exclues',
  'settings.excludedApps.hint':
    'Une par ligne : identifiant de bundle sur Mac, nom du .exe sur Windows. Cette liste est propre à chaque appareil : ajoutez l’entrée sur chaque machine. Dès l’ajout d’une entrée, Parle cesse d’envoyer les éléments de cette app à vos autres appareils. Ce qui est déjà synchronisé y reste.',
  'settings.dangerZone.label': 'Zone de danger',
  'settings.clearHistory.button': 'Effacer tout l’historique non épinglé',
  'settings.clearHistory.confirmWithDevices':
    'Cela supprime tous les éléments non épinglés sur cet appareil et sur {devices}. Les éléments épinglés sont conservés. Cette action est irréversible.',
  'settings.clearHistory.confirm':
    'Cela supprime tous les éléments non épinglés sur cet appareil. Les éléments épinglés sont conservés. Cette action est irréversible.',
  'settings.clearHistory.clearIt': 'Tout effacer',

  // ---------- Settings: audio ----------
  'settings.microphone.label': 'Micro',
  'settings.microphone.systemDefault': 'Par défaut du système',
  'settings.minDuration.label': 'Ignorer les enregistrements de moins de',
  'settings.microphoneDenied': 'L’accès au micro est refusé.',

  // ---------- Settings: general ----------
  'settings.launchAtLogin.label': 'Lancer à l’ouverture de session',
  'settings.prewarm.label': 'Précharger le modèle au démarrage',
  'settings.prewarm.hint': 'Occupe de la mémoire au repos, rend la première dictée instantanée',

  // ---------- Settings: sync ----------
  'sync.section': 'Synchronisation',
  'sync.unavailable': 'La synchronisation n’est pas disponible pour le moment. {error}',
  'sync.checking': 'Vérification de la synchronisation…',
  'sync.genericError': 'Une erreur est survenue.',
  'sync.enable.label': 'Synchroniser avec mes autres appareils',
  'sync.enable.hint':
    'Désactivée tant que vous ne l’activez pas. Votre Mac et votre PC se parlent directement sur votre réseau local, avec un chiffrement de bout en bout, sans compte et sans que rien ne soit envoyé nulle part. Tant qu’elle est active, Parle annonce le nom de cet appareil aux autres machines du même réseau pour qu’elles puissent le trouver.',
  'sync.tryAgain': 'Réessayer',
  'sync.thisDevice.label': 'Cet appareil',
  'sync.thisDevice.hint': 'Le nom que voit l’autre machine pendant l’association.',
  'sync.thisDevice.placeholder': 'Nommez cet appareil',
  'sync.nameSanitised':
    'Enregistré sous « {name} ». Un nom d’appareil ne peut pas contenir « = » ni de caractères invisibles, et il est raccourci pour tenir.',
  'sync.deviceId.label': 'Identifiant de l’appareil',
  'sync.deviceId.hint': 'L’identité de cette installation. Elle ne quitte jamais votre réseau.',
  'sync.paired.label': 'Appareils associés',
  'sync.paired.hint': 'Seules ces machines peuvent voir votre historique. L’association est réciproque.',
  'sync.paired.none': 'Aucun appareil associé pour l’instant. Associez-en un ci-dessous pour lancer la synchronisation.',
  'sync.now.button': 'Synchroniser',
  'sync.now.working': 'Synchronisation',
  'sync.now.none': 'Aucun appareil associé n’est joignable pour le moment.',
  'sync.now.ok': 'Échange en cours avec vos autres appareils.',
  'sync.syncedAgo': 'Synchronisé {when}',
  'sync.visibleNotSynced': 'Visible sur le réseau, mais rien n’a encore été synchronisé',
  'sync.neverConnected': 'Jamais connecté',
  'sync.lastSeen': 'Vu {when}',
  'sync.unpairConfirm':
    'Dissocier {name} ? La synchronisation s’arrête et il faudra un nouveau code pour la rétablir. Ce qui se trouve déjà sur {name} y reste.',
  'sync.unpair': 'Dissocier',
  'sync.pairNew.label': 'Associer un nouvel appareil',
  'sync.pairNew.hint': 'L’une ou l’autre machine peut commencer. Lisez les six chiffres à voix haute ou saisissez-les.',
  'sync.direction.show': 'Afficher un code',
  'sync.direction.enter': 'Saisir un code',
  'sync.code.typeOnOther': 'Saisissez-le sur l’autre appareil. Expire dans {time}',
  'sync.code.expired': 'Ce code a expiré.',
  'sync.code.showNew': 'Afficher un nouveau code',
  'sync.code.explain':
    'Parle affiche six chiffres ici ; saisissez-les sur l’autre machine pour confirmer qu’elle est bien à vous.',
  'sync.peers.notSearching':
    'Aucune recherche d’appareils en cours. Ouvrez Parle sur l’autre machine, activez-y aussi la synchronisation et vérifiez que les deux sont sur le même réseau.',
  'sync.peers.stillNothing':
    'Toujours rien au bout d’un moment. Vérifiez que Parle est ouvert sur l’autre machine avec la synchronisation activée, et que les deux sont sur le même réseau.',
  'sync.peers.macBlocked':
    'Si tout cela semble correct, macOS empêche peut-être Parle de voir le réseau local, ce qui donne exactement ce résultat.',
  'sync.peers.openLocalNetwork': 'Ouvrir les réglages Réseau local',
  'sync.peers.winBlocked': 'Si tout cela semble correct, le pare-feu Windows bloque peut-être Parle.',
  'sync.peers.openFirewall': 'Ouvrir les réglages du pare-feu',
  'sync.peers.vpnHint':
    'Un VPN est l’autre cause fréquente : beaucoup bloquent le trafic du réseau local même quand tout le reste fonctionne. Désactivez-le, ou activez son option de partage du réseau local.',
  'sync.peers.isolatedHint':
    'Les Wi-Fi d’hôtel et d’invités empêchent souvent les appareils d’un même réseau de se voir. Un partage de connexion depuis un téléphone permet de le vérifier vite.',
  'sync.peers.looking':
    'Recherche d’appareils sur ce réseau… Ouvrez Parle sur l’autre machine et activez-y aussi la synchronisation.',
  'sync.pairing': 'Association…',
  'sync.pair': 'Associer',
  'sync.pair.needsDevice':
    'Sélectionnez d’abord l’appareil ci-dessus. Rien ne s’affiche tant que les deux machines ne se voient pas sur le réseau.',
  'sync.dictations.label': 'Synchroniser les dictées',
  'sync.dictations.hintPaired':
    'Tout ce que vous dictez apparaît dans l’Historique sur les deux machines. Réactiver cette option renvoie votre historique à vos appareils associés, ce qui peut prendre un moment.',
  'sync.dictations.hint': 'Tout ce que vous dictez apparaît dans l’Historique sur les deux machines',
  'sync.clipboard.label': 'Synchroniser le presse-papiers',
  'sync.clipboard.hintPaired':
    'Copiez sur une machine, collez sur l’autre. Réactiver cette option renvoie votre historique à vos appareils associés, ce qui peut prendre un moment.',
  'sync.clipboard.hint': 'Copiez sur une machine, collez sur l’autre',

  // ---------- Dictionary ----------
  'dictionary.title': 'Dictionnaire',
  'dictionary.subtitle':
    'Les noms, marques et termes de métier que Parle doit écrire correctement. Les termes orientent la reconnaissance et corrigent les graphies proches, sans jamais insérer de mots que vous n’avez pas dits.',
  'dictionary.term.placeholder': 'Terme (casse exacte, par ex. « farsiight »)',
  'dictionary.corrections.placeholder':
    'Entendu comme… (facultatif, séparés par des virgules, par ex. « far sight, foresight »)',
  'dictionary.add': 'Ajouter',
  'dictionary.empty': 'Aucun terme pour l’instant. Ajoutez les noms et le jargon que vous utilisez tous les jours.',
  'dictionary.autoBadge': 'auto',
  'dictionary.autoBadgeTitle': 'Appris à partir de vos corrections',
  'dictionary.fuzzyMatch': 'correspondance approchée',

  // Onboarding: language
  'onboarding.language.title': 'Choisissez votre langue',
  'onboarding.language.sub': 'Parle s’exprimera dans cette langue et s’attendra à ce que vous dictiez dans celle-ci.',
  'onboarding.language.note': 'Vous pourrez changer l’une comme l’autre plus tard, et elles n’ont pas à correspondre : beaucoup de gens utilisent l’interface dans une langue et dictent dans une autre.',

  // ---------- Settings: interface language ----------
  'settings.uiLanguage.label': 'Langue de l’interface',
  'settings.uiLanguage.hint': 'La langue dans laquelle Parle lui-même est écrit. Distincte de la langue dans laquelle vous dictez, ci-dessous.',

  // ---------- Onboarding: what Parle is ----------
  'onboarding.hotkey.openKeyboard': 'Ouvrir les réglages Clavier',
  'onboarding.about.title': 'Ce que fait Parle',
  'onboarding.about.sub': 'La dictée et un historique du presse-papiers, qui restent tous deux sur cette machine.',
  'onboarding.about.dictation.title': 'Parler plutôt que taper',
  'onboarding.about.dictation.body':
    'Maintenez votre touche de dictée, dites ce que vous voulez, relâchez. Le texte apparaît là où se trouve votre curseur. Rien n’est envoyé nulle part : le modèle tourne sur cette machine, et cela fonctionne sans aucune connexion internet.',
  'onboarding.about.clipboard.title': 'Tout ce que vous copiez, conservé',
  'onboarding.about.clipboard.body':
    'Parle retient ce que vous copiez pour que vous puissiez le retrouver. Les gestionnaires de mots de passe sont exclus d’office, et vous pouvez ajouter toute autre app que vous préférez qu’il ignore.',
  'onboarding.about.sync.title': 'Vos autres machines, si vous le souhaitez',
  'onboarding.about.sync.body':
    'Parle peut se synchroniser avec vos propres appareils sur votre réseau local, avec un chiffrement de bout en bout, sans compte et sans serveur intermédiaire. La synchronisation est désactivée tant que vous ne l’activez pas, et celle du presse-papiers reste désactivée tant que vous ne la demandez pas séparément.',
  'onboarding.about.privacy.title': 'Tout reste chez vous',
  'onboarding.about.privacy.body':
    'Aucun compte, aucune télémétrie, aucun cloud. Votre historique est un fichier sur cette machine, que vous pouvez effacer à tout moment.',
};

