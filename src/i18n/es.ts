// Spanish source strings. Neutral Spanish, written to read naturally in both
// Spain and Latin America, using the impersonal "usted" register Apple uses in
// its own interfaces.
//
// Key order and grouping deliberately mirror en.ts so the two files diff side
// by side. Placeholders in braces are copied verbatim from the English.
export const es: Record<string, string> = {
  // ---------- Shared ----------
  'common.cancel': 'Cancelar',
  'common.dismiss': 'Descartar',
  'common.grant': 'Conceder',
  'common.keepIt': 'Conservarlo',
  'common.openSystemSettings': 'Abrir Ajustes del Sistema',

  // ---------- Relative time ----------
  'time.justNow': 'ahora mismo',
  'time.minutesAgo': 'hace {n} min',
  'time.hoursAgo': 'hace {n} h',
  'time.secondsShort': '{n} s',

  // ---------- App shell ----------
  'app.nav.compose': 'Redactar',
  'app.nav.history': 'Historial',
  'app.nav.dictionary': 'Diccionario',
  'app.nav.models': 'Modelos',
  'app.nav.settings': 'Ajustes',
  'app.record.start': 'Iniciar dictado',
  'app.record.stop': 'Detener dictado',
  'app.toast.pasteInstruction': 'Copiado. Pulse {keys} para pegar',
  'app.toast.inserted': 'Insertado "{text}"',
  'app.toast.copied': 'Copiado "{text}"',

  // ---------- Recording overlay (HUD) ----------
  'hud.pasteInstruction': 'Copiado. Pulse {keys} para pegar',
  'hud.recordingClickToStop': 'Grabando. Haga clic para detener',
  'hud.transcribing': 'Transcribiendo…',
  'hud.stopAndPaste': 'Detener y pegar',
  'hud.working': 'Procesando…',
  'hud.cancel': 'Cancelar (Esc)',
  'hud.deck.rec': 'REC',
  'hud.deck.proc': 'PROC',

  // ---------- History ----------
  'history.searchPlaceholder':
    'Buscar en transcripciones y portapapeles…  (↑↓ · Enter pega · {copyKeys} copia)',
  'history.filter.all': 'Todo',
  'history.filter.dictations': 'Dictados',
  'history.filter.clipboard': 'Portapapeles',
  'history.gone':
    'Ese elemento ya no está aquí. Se eliminó en otro dispositivo, así que la lista se ha actualizado.',
  'history.empty.noMatches': 'Sin coincidencias.',
  'history.empty.nothingYet': 'Aquí todavía no hay nada. Mantenga pulsado su atajo y hable.',
  'history.deleteConfirm': '¿Eliminar este elemento? No se puede deshacer.',
  'history.mayAlsoDelete': 'Puede que también se elimine de sus dispositivos emparejados.',
  'history.alsoDeletesFrom': 'Esto también lo elimina de {devices}.',
  'history.localOnly.badge': 'solo en este dispositivo',
  'history.localOnly.title':
    'Parle no pudo descartar que esto fuera un campo de contraseña, así que se conserva en este dispositivo y nunca se envía a sus otros dispositivos',
  'history.action.paste': 'Pegar',
  'history.action.pasteTitle': 'Pegar en la app anterior (Enter)',
  'history.action.copy': 'Copiar',
  'history.action.copyTitle': 'Copiar ({copyKeys})',
  'history.action.editTitle': 'Editar (alimenta el aprendizaje automático)',
  'history.action.pin': 'Fijar',
  'history.action.unpin': 'Dejar de fijar',
  'history.action.delete': 'Eliminar',
  'history.trimmedCount': '{n} recortados',
  'history.unsureCount': '{n} dudosos',
  'history.restoreRaw': 'Restaurar el original',

  // ---------- Compose ----------
  'compose.title': 'Redactar',
  'compose.intro':
    'Dicte aquí y pegue enlaces o texto a mitad de frase: cada inserción queda fijada al momento exacto en que la añadió y se integra en el texto final, byte a byte.',
  'compose.start': 'Iniciar dictado',
  'compose.stop': 'Detener',
  'compose.transcribing': 'Transcribiendo…',
  'compose.recording': 'grabando',
  'compose.processing': 'procesando',
  'compose.markPlaceholder.recording': 'Pegue un enlace o escriba; Enter lo fija a este momento…',
  'compose.markPlaceholder.idle': 'Empiece a dictar para insertar enlaces y texto',
  'compose.insert': 'Insertar',
  'compose.noSpeech': 'No se ha detectado voz.',
  'compose.copyResult': 'Copiar resultado',
  'compose.alsoInserted': 'También se ha insertado en el cursor y guardado en el Historial.',

  // ---------- Models ----------
  'models.title': 'Modelos',
  'models.subtitle': 'Toda la transcripción se ejecuta en este dispositivo.',
  'models.warm': 'Cargado · {model}',
  'models.loadsOnFirstUse': 'El modelo se carga al usarlo por primera vez',
  'models.active': 'Activo',
  'models.backendTitle': 'El hardware en el que se ejecuta este modelo',
  'models.yourFile': 'Su archivo',
  'models.speedRating': 'velocidad {value}/5',
  'models.accuracyRating': 'precisión {value}/5',
  'models.languageCount': '{n} idiomas',
  'models.use': 'Usar',
  'models.deleteFile': 'Eliminar el archivo del modelo',
  'models.removeCustom': 'Quitar de esta lista (su archivo no se elimina)',
  'models.removeFromList': 'Quitar de esta lista',
  'models.fileMissing': 'Falta el archivo',
  'models.download': 'Descargar',
  'models.addLocal': 'Añadir un modelo local…',
  'models.addLocal.hint':
    'Un archivo GGML {ext} de whisper.cpp que ya tenga. Parle apunta a él donde esté y nunca lo copia, así que no ocupa disco adicional.',
  'models.picker.title': 'Elija un modelo de whisper.cpp',
  'models.picker.filter': 'Modelo de Whisper',
  'models.defaultLocalName': 'Modelo local',
  'models.fallbackHint':
    'Si el modelo activo no se puede cargar (por ejemplo, por falta de memoria), Parle recurre automáticamente al siguiente de la escala: su grabación nunca se pierde.',

  // ---------- Onboarding ----------
  'onboarding.welcome.title': 'Le damos la bienvenida a Parle',
  'onboarding.welcome.body':
    'Mantenga pulsada una tecla, hable y suéltela: sus palabras aparecen donde está el cursor. La transcripción se ejecuta por completo en este dispositivo. Nada de lo que diga sale nunca de él.',
  'onboarding.welcome.cta': 'Configurar',
  'onboarding.permissions.title': 'Permisos',
  'onboarding.permissions.introMac':
    'Parle necesita dos permisos para oírle y escribir por usted. Ambos se quedan en esta máquina.',
  'onboarding.permissions.introWin':
    'Parle necesita un permiso, para oírle. Se queda en esta máquina.',
  'onboarding.permissions.microphone': 'Micrófono',
  'onboarding.permissions.microphoneDesc': 'Para oír su dictado',
  'onboarding.permissions.accessibility': 'Accesibilidad',
  'onboarding.permissions.accessibilityDesc': 'Para vigilar su atajo y pegar en el cursor',
  'onboarding.permissions.macNote':
    'En Ajustes del Sistema, añada {app} en Privacidad y seguridad → Accesibilidad y vuelva aquí. Esta página se actualiza sola. Puede que haya que reiniciar Parle después de concederlo.',
  'onboarding.permissions.appName': 'Parle',
  'onboarding.permissions.openSettings': 'Abrir Ajustes',
  'onboarding.permissions.continue': 'Continuar',
  'onboarding.permissions.waiting': 'Esperando los permisos…',
  'onboarding.model.title': 'Su modelo',
  'onboarding.model.machine': '{ram} GB de RAM, {gpu}',
  'onboarding.model.recommendation':
    'Según esta máquina ({machine}), recomendamos {model}. Puede añadir modelos o cambiar de modelo cuando quiera en Ajustes → Modelos.',
  'onboarding.model.downloadFailed':
    'Error en la descarga: {error}. Compruebe su conexión y vuelva a intentarlo: se reanuda donde se quedó.',
  'onboarding.model.ready': 'Modelo listo',
  'onboarding.model.download': 'Descargar',
  'onboarding.hotkey.title': 'Su tecla',
  'onboarding.hotkey.macKey': 'tecla 🌐 Fn',
  'onboarding.hotkey.doNothing': 'No hacer nada',
  'onboarding.hotkey.mac':
    'Por defecto: la {key}. Manténgala pulsada y hable, suéltela para pegar, o dele un toque rápido para dejar la grabación activada. Consejo: en Ajustes del Sistema → Teclado, ponga “Al pulsar la tecla 🌐” en {doNothing} para que el dictado de macOS no se la dispute.',
  'onboarding.hotkey.winKey': 'Ctrl derecho',
  'onboarding.hotkey.win':
    'Por defecto: {key}. Mantenga la tecla pulsada y hable, suéltela para pegar, o dele un toque rápido para dejar la grabación activada. ¿Tiene una tecla Copilot? Asígnela en Ajustes → Atajos de teclado y Parle la controlará por completo.',
  'onboarding.hotkey.cta': 'Entendido',
  'onboarding.test.title': 'Pruébelo',
  'onboarding.test.body': 'Haga clic en el botón (o use su atajo), diga algo y luego deténgalo.',
  'onboarding.test.start': 'Iniciar dictado de prueba',
  'onboarding.test.stop': 'Detener',
  'onboarding.test.transcribing': 'Transcribiendo…',
  'onboarding.test.noSpeech': 'No se ha detectado voz. Inténtelo de nuevo un poco más alto.',
  'onboarding.test.finish': 'Finalizar configuración',

  // ---------- Settings: shell ----------
  'settings.title': 'Ajustes',
  'settings.subtitle': 'Solo local. Sin telemetría ni nube, nunca.',
  'settings.section.hotkeys': 'Atajos de teclado',
  'settings.section.language': 'Idioma',
  'settings.section.cleanup': 'Limpieza',
  'settings.section.dictionary': 'Diccionario',
  'settings.section.output': 'Salida',
  'settings.section.appearance': 'Apariencia',
  'settings.section.historyPrivacy': 'Historial y privacidad',
  'settings.section.audio': 'Audio',
  'settings.section.general': 'General',
  'settings.footer.tagline': 'dictado en el dispositivo',
  'settings.footer.note': 'nada sale nunca de esta máquina',

  // ---------- Settings: hotkeys ----------
  'settings.dictationKey.label': 'Tecla de dictado',
  'settings.dictationKey.hintMac': 'Fn necesita el permiso de Accesibilidad',
  'settings.dictationKey.hintWin':
    'Alt derecho es AltGr en muchas distribuciones, así que Ctrl derecho es más seguro',
  'settings.dictationKey.custom': 'Personalizada…',
  'settings.customBinding.label': 'Combinación personalizada',
  'settings.customBinding.hint':
    'Haga clic y luego pulse la tecla o la combinación que quiera. Esc cancela.',
  'settings.customBinding.listening': 'Pulse una combinación de teclas…',
  'settings.gesture.label': 'Gesto',
  'settings.gesture.hintDoubleTap':
    'El doble toque inicia y un solo toque detiene. La tecla nunca se intercepta, así que su comportamiento normal en el sistema sigue funcionando.',
  'settings.gesture.hint':
    'Híbrido: mantener pulsada para hablar; un toque rápido la deja fijada hasta el siguiente toque',
  'settings.gesture.hold': 'Mantener',
  'settings.gesture.toggle': 'Alternar',
  'settings.gesture.hybrid': 'Híbrido',
  'settings.gesture.doubleTap': 'Doble toque',
  'settings.latch.label': 'Margen de fijación',
  'settings.latch.hint':
    'Híbrido: los toques más cortos que esto quedan fijados en modo alternar. Doble toque: intervalo máximo entre toques',
  'settings.escCancel.label': 'Esc cancela la grabación',
  'settings.escCancel.hint':
    'Desactivado por defecto: Esc se pulsa por todo tipo de motivos ajenos, y descartar una toma que ya ha dicho es peor que detenerla con su atajo',
  'settings.historyPalette.label': 'Paleta del historial',
  'settings.historyPalette.hint': 'Atajo combinado para buscar',
  'settings.suppressCopilot.label': 'Impedir que se abra Copilot',
  'settings.suppressCopilot.hint':
    'Cuando la tecla Copilot está asignada (o esto está activado), la app de Copilot por defecto no se abre nunca',
  'settings.accessibilityMissing':
    'Falta el permiso de Accesibilidad. Las teclas especiales y el pegado en el cursor no funcionarán. Si ya lo concedió y este aviso sigue apareciendo, la entrada quedó obsoleta tras una recompilación: use Reparar el permiso.',
  'settings.repairPermission': 'Reparar el permiso',
  'settings.bindingWarning.leftCtrl':
    'Ctrl izquierdo gobierna la mayoría de los atajos de teclado, así que asignarlo hará que se dispare durante el uso normal.',
  'settings.bindingWarning.leftShift':
    'Shift izquierdo se pulsa constantemente al escribir, así que habrá activaciones falsas.',
  'settings.bindingWarning.rightAlt':
    'Alt derecho es AltGr en muchas distribuciones, así que escribe caracteres acentuados. Ctrl derecho es más seguro.',

  // ---------- Settings: key names ----------
  'keys.fn': '🌐 Fn / Globo',
  'keys.rightCommand': '⌘ derecho',
  'keys.leftCommand': '⌘ izquierdo',
  'keys.rightOption': '⌥ derecho',
  'keys.leftOption': '⌥ izquierdo',
  'keys.rightControl': '⌃ derecho',
  'keys.leftControl': '⌃ izquierdo',
  'keys.copilot': 'Tecla Copilot',
  'keys.rightCtrl': 'Ctrl derecho',
  'keys.leftCtrl': 'Ctrl izquierdo',
  'keys.rightShift': 'Shift derecho',
  'keys.leftShift': 'Shift izquierdo',
  'keys.leftAlt': 'Alt izquierdo',
  'keys.rightAlt': 'Alt derecho',
  'keys.rightWin': 'Win derecho',
  'keys.leftWin': 'Win izquierdo',

  // ---------- Settings: language ----------
  'settings.spokenLanguage.label': 'Idioma hablado',
  'settings.language.auto': 'Detección automática',
  'settings.language.en': 'Inglés',
  'settings.language.es': 'Español',
  'settings.language.fr': 'Francés',
  'settings.language.de': 'Alemán',
  'settings.language.it': 'Italiano',
  'settings.language.pt': 'Portugués',
  'settings.language.nl': 'Neerlandés',
  'settings.language.ja': 'Japonés',
  'settings.language.ko': 'Coreano',
  'settings.language.zh': 'Chino',
  'settings.language.hi': 'Hindi',
  'settings.language.ar': 'Árabe',
  'settings.language.ru': 'Ruso',
  'settings.language.pl': 'Polaco',
  'settings.language.sv': 'Sueco',
  'settings.localeSpelling.label': 'Ortografía regional',
  'settings.localeSpelling.hint': 'Afecta a la ortografía del resultado (colour frente a color)',
  'settings.locale.none': 'Sin preferencia',
  'settings.locale.enAU': 'Inglés (Australia)',
  'settings.locale.enGB': 'Inglés (Reino Unido)',
  'settings.locale.enUS': 'Inglés (EE. UU.)',
  'settings.applyLocaleSpelling.label': 'Aplicar la ortografía regional',
  'settings.applyLocaleSpelling.hint':
    'Convierte la ortografía estadounidense de la transcripción a su región',
  'settings.translate.label': 'Traducir al inglés',
  'settings.translate.hint': 'Hable en cualquier idioma, pegue en inglés',

  // ---------- Settings: cleanup ----------
  'settings.smartCleanup.label': 'Limpieza inteligente',
  'settings.smartCleanup.hint': 'Interruptor principal del nivel de limpieza determinista',
  'settings.removeFillers.label': 'Eliminar muletillas',
  'settings.removeFillers.hint': 'eh, em, esto…',
  'settings.removeHedges.label': 'Eliminar coletillas',
  'settings.removeHedges.hint': '¿sabes?, o sea, digamos (más agresivo)',
  'settings.trimSelfCorrections.label': 'Recortar rectificaciones',
  'settings.trimSelfCorrections.hint':
    '“el jueves, no, en realidad el miércoles” → “el miércoles”. Los fragmentos recortados se pueden seguir revisando en el Historial',
  'settings.dictatedPunctuation.label': 'Puntuación dictada',
  'settings.dictatedPunctuation.hint':
    '“coma”, “nueva línea”, “signo de interrogación”… (“literalmente coma” lo escapa)',
  'settings.capitalise.label': 'Mayúscula al inicio de frase',
  'settings.terminalPunctuation.label': 'Terminar con puntuación',
  'settings.paragraphPause.label': 'Párrafo tras una pausa larga',

  // ---------- Settings: dictionary ----------
  'settings.dictionary.enable': 'Activar el diccionario',
  'settings.dictionary.bias.label': 'Orientar el reconocimiento',
  'settings.dictionary.bias.hint': 'Sus términos se envían al motor como glosario',
  'settings.dictionary.fuzzy.label': 'Corregir grafías parecidas',
  'settings.dictionary.autoLearn.label': 'Aprender de mis ediciones',
  'settings.dictionary.autoLearn.hint':
    'Las ediciones de una sola palabra en el Historial se convierten en pares de corrección',

  // ---------- Settings: output ----------
  'settings.insertAtCursor.label': 'Insertar en el cursor',
  'settings.insertAtCursor.hint': 'Escribe el resultado en la app activa',
  'settings.copyToClipboard.label': 'Copiar al portapapeles',
  'settings.restoreClipboard.label': 'Restaurar el portapapeles anterior',
  'settings.restoreClipboard.hint':
    'Después de pegar, devuelve a su sitio el contenido anterior del portapapeles',
  'settings.restoreDelay.label': 'Retardo de restauración',
  'settings.restoreDelay.hint':
    'Las apps lentas (Office, escritorio remoto) leen el portapapeles con retraso',
  'settings.preferAxInsert.label': 'Preferir la inserción directa',
  'settings.preferAxInsert.hint':
    'Prueba la inserción de texto de Accesibilidad antes de pegar desde el portapapeles',
  'settings.pressEnter.label': 'Pulsar Enter después de insertar',
  'settings.pressEnter.hint':
    'Envía el mensaje justo después de pegar, práctico para apps de chat. Nunca se activa en campos seguros.',

  // ---------- Settings: appearance ----------
  'settings.theme.label': 'Tema',
  'settings.theme.system': 'Sistema',
  'settings.theme.light': 'Claro',
  'settings.theme.dark': 'Oscuro',
  'settings.palette.label': 'Paleta',
  'settings.palette.hint':
    'Pastel toma su tono del color de acento, así que pruébela con la rueda personalizada',
  'settings.palette.paper': 'Papel',
  'settings.palette.pastel': 'Pastel',
  'settings.palette.bold': 'Intensa',
  'settings.palette.retro': 'Retro',
  'settings.accent.label': 'Acento',
  'settings.accent.custom': 'Color personalizado',
  'settings.appIcon.label': 'Icono de la app',
  'settings.appIcon.hint':
    'Se aplica al instante en la app; el icono del Finder se actualiza al reiniciar',
  'settings.appIcon.default': 'Parle',
  'settings.appIcon.keycap': 'Tecla',
  'settings.appIcon.waveform': 'Forma de onda',
  'settings.appIcon.echoRings': 'Ondas de eco',
  'settings.appIcon.cassette': 'Casete',
  'settings.iconRestart':
    'Icono actualizado. Reinicie para actualizar el icono del Finder y del Dock.',
  'settings.restartParle': 'Reiniciar Parle',
  'settings.trayIcon.labelMac': 'Icono de la barra de menús',
  'settings.trayIcon.labelWin': 'Icono de la bandeja',
  'settings.trayIcon.hintMac':
    'Monocromo sigue a la barra de menús; el distintivo mantiene el color de Parle',
  'settings.trayIcon.hintWin':
    'Automático elige el contorno que se distingue en su barra de tareas',
  'settings.tray.template': 'Monocromo',
  'settings.tray.badge': 'Distintivo azul',
  'settings.tray.auto': 'Automático: según la barra de tareas',
  'settings.tray.light': 'Contorno claro',
  'settings.tray.dark': 'Contorno oscuro',
  'settings.tray.color': 'Contorno azul',
  'settings.overlayStyle.label': 'Estilo de la superposición',
  'settings.overlayStyle.hintHidden':
    'Ninguna superposición. Mientras Parle escucha, el icono de la barra de menús muestra un punto en la esquina, y esa es la única indicación.',
  'settings.overlayStyle.hint': 'Casete combina de maravilla con la paleta Retro',
  'settings.overlayStyle.pill': 'Cápsula',
  'settings.overlayStyle.cassette': 'Casete',
  'settings.overlayStyle.metal': 'Metal',
  'settings.overlayStyle.minimal': 'Mínima',
  'settings.overlayStyle.none': 'Ninguna',
  'settings.waveformSensitivity.label': 'Sensibilidad de la onda',
  'settings.waveformSensitivity.hint':
    'Súbala si las barras apenas se mueven al hablar, bájela si se quedan arriba del todo. Cambia lo que muestra el medidor, nunca lo que se graba ni lo que se transcribe.',
  'settings.showPartial.label': 'Mostrar transcripción en directo en la superposición',
  'settings.reduceMotion.label': 'Reducir movimiento',

  // ---------- Settings: history & privacy ----------
  'settings.clipboardCapture.label': 'Capturar el portapapeles',
  'settings.clipboardCapture.hint':
    'Todo lo que copie, con búsqueda. Los gestores de contraseñas quedan excluidos',
  'settings.retention.label': 'Conservar los elementos durante',
  'settings.retention.confirmNarrow':
    'Los elementos más antiguos que eso se eliminarán de este dispositivo y no se podrán recuperar, ni siquiera desde un dispositivo emparejado. ¿Continuar?',
  'settings.retention.forever': 'Siempre',
  'settings.retention.d90': '90 días',
  'settings.retention.d30': '30 días',
  'settings.retention.d7': '7 días',
  'settings.retention.d1': '1 día',
  'settings.excludedApps.label': 'Apps excluidas',
  'settings.excludedApps.hint':
    'Una por línea: el bundle id en Mac, el nombre del .exe en Windows. Esta lista es propia de cada dispositivo, así que añada la entrada en cada máquina. Desde el momento en que añade una entrada, Parle deja de enviar los registros de esa app a sus otros dispositivos. Lo que ya se haya sincronizado permanece en ellos.',
  'settings.dangerZone.label': 'Zona de peligro',
  'settings.clearHistory.button': 'Borrar todo el historial no fijado',
  'settings.clearHistory.confirmWithDevices':
    'Esto elimina todos los elementos no fijados de este dispositivo y de {devices}. Los elementos fijados se conservan. No se puede deshacer.',
  'settings.clearHistory.confirm':
    'Esto elimina todos los elementos no fijados de este dispositivo. Los elementos fijados se conservan. No se puede deshacer.',
  'settings.clearHistory.clearIt': 'Borrarlo',

  // ---------- Settings: audio ----------
  'settings.microphone.label': 'Micrófono',
  'settings.microphone.systemDefault': 'Predeterminado del sistema',
  'settings.minDuration.label': 'Ignorar las grabaciones más cortas de',
  'settings.microphoneDenied': 'El acceso al micrófono está denegado.',

  // ---------- Settings: general ----------
  'settings.launchAtLogin.label': 'Abrir al iniciar sesión',
  'settings.prewarm.label': 'Precargar el modelo al arrancar',
  'settings.prewarm.hint':
    'Consume memoria mientras está inactivo, hace que el primer dictado sea instantáneo',

  // ---------- Settings: sync ----------
  'sync.section': 'Sincronización',
  'sync.unavailable': 'La sincronización no está disponible ahora mismo. {error}',
  'sync.checking': 'Comprobando la sincronización…',
  'sync.genericError': 'Algo ha salido mal.',
  'sync.enable.label': 'Sincronizar con mis otros dispositivos',
  'sync.enable.hint':
    'Desactivado salvo que lo active. Su Mac y su PC se comunican directamente entre sí por la red local, con cifrado de extremo a extremo, sin cuenta y sin subir nada a ningún sitio. Mientras está activado, Parle anuncia el nombre de este dispositivo a las demás máquinas de la misma red para que puedan encontrarlo.',
  'sync.tryAgain': 'Reintentar',
  'sync.thisDevice.label': 'Este dispositivo',
  'sync.thisDevice.hint': 'El nombre que ve la otra máquina durante el emparejamiento.',
  'sync.thisDevice.placeholder': 'Ponga nombre a este dispositivo',
  'sync.nameSanitised':
    'Guardado como "{name}". El nombre de un dispositivo no puede contener "=" ni caracteres ocultos, y se recorta para que quepa.',
  'sync.deviceId.label': 'ID del dispositivo',
  'sync.deviceId.hint': 'La identidad de esta instalación. Nunca sale de su red.',
  'sync.paired.label': 'Dispositivos emparejados',
  'sync.paired.hint': 'Solo estas máquinas pueden ver su historial. El emparejamiento es mutuo.',
  'sync.paired.none': 'Aún no hay dispositivos emparejados. Empareje uno abajo para sincronizar.',
  'sync.syncedAgo': 'Sincronizado {when}',
  'sync.visibleNotSynced': 'Visible en la red, pero todavía no se ha sincronizado nada',
  'sync.neverConnected': 'Nunca se ha conectado',
  'sync.lastSeen': 'Visto por última vez {when}',
  'sync.unpairConfirm':
    '¿Desemparejar {name}? Dejará de sincronizarse y necesitará un código nuevo para volver. Lo que ya esté en {name} permanece allí.',
  'sync.unpair': 'Desemparejar',
  'sync.pairNew.label': 'Emparejar un dispositivo nuevo',
  'sync.pairNew.hint':
    'Puede empezar cualquiera de las dos máquinas. Lea los seis dígitos en voz alta o escríbalos en la otra.',
  'sync.direction.show': 'Mostrar un código',
  'sync.direction.enter': 'Introducir un código',
  'sync.code.typeOnOther': 'Escríbalo en el otro dispositivo. Caduca en {time}',
  'sync.code.expired': 'Este código ha caducado.',
  'sync.code.showNew': 'Mostrar un código nuevo',
  'sync.code.explain':
    'Parle muestra aquí seis dígitos; escríbalos en la otra máquina para confirmar que es realmente suya.',
  'sync.peers.notSearching':
    'Ahora mismo no se están buscando dispositivos. Abra Parle en la otra máquina, active también allí la sincronización y asegúrese de que ambas están en la misma red.',
  'sync.peers.stillNothing':
    'Sigue sin aparecer nada después de un rato. Compruebe que Parle está abierto en la otra máquina con la sincronización activada, y que ambas están en la misma red.',
  'sync.peers.macBlocked':
    'Si todo eso es correcto, puede que macOS esté impidiendo que Parle vea la red local, algo que se manifiesta exactamente así.',
  'sync.peers.openLocalNetwork': 'Abrir los ajustes de Red local',
  'sync.peers.winBlocked':
    'Si todo eso es correcto, puede que el Firewall de Windows esté bloqueando Parle.',
  'sync.peers.openFirewall': 'Abrir la configuración del firewall',
  'sync.peers.looking':
    'Buscando dispositivos en esta red… Abra Parle en la otra máquina y active también allí la sincronización.',
  'sync.pairing': 'Emparejando…',
  'sync.pair': 'Emparejar',
  'sync.dictations.label': 'Sincronizar los dictados',
  'sync.dictations.hintPaired':
    'Todo lo que dicte aparece en el Historial de ambas máquinas. Al volver a activarlo, su historial se reenvía a los dispositivos emparejados, lo que puede tardar un momento.',
  'sync.dictations.hint': 'Todo lo que dicte aparece en el Historial de ambas máquinas',
  'sync.clipboard.label': 'Sincronizar el portapapeles',
  'sync.clipboard.hintPaired':
    'Copie en una máquina y pegue en la otra. Al volver a activarlo, su historial se reenvía a los dispositivos emparejados, lo que puede tardar un momento.',
  'sync.clipboard.hint': 'Copie en una máquina y pegue en la otra',

  // ---------- Dictionary ----------
  'dictionary.title': 'Diccionario',
  'dictionary.subtitle':
    'Nombres, marcas y jerga que Parle debería acertar. Los términos orientan el reconocimiento y corrigen grafías parecidas, sin insertar nunca palabras que no haya dicho.',
  'dictionary.term.placeholder': 'Término (mayúsculas exactas, p. ej. “farsiight”)',
  'dictionary.corrections.placeholder':
    'Se oye como… (opcional, separado por comas, p. ej. “far sight, foresight”)',
  'dictionary.add': 'Añadir',
  'dictionary.empty': 'Todavía no hay términos. Añada los nombres y la jerga que usa a diario.',
  'dictionary.autoBadge': 'auto',
  'dictionary.autoBadgeTitle': 'Aprendido de sus correcciones',
  'dictionary.fuzzyMatch': 'coincidencia aproximada',

  // Onboarding: language
  'onboarding.language.title': 'Elija su idioma',
  'onboarding.language.sub': 'Parle hablará en este idioma y esperará que usted dicte en él.',
  'onboarding.language.note':
    'Puede cambiar cualquiera de los dos más adelante, y no tienen por qué coincidir: mucha gente usa la interfaz en un idioma y dicta en otro.',

  // ---------- Settings: interface language ----------
  'settings.uiLanguage.label': 'Idioma de la interfaz',
  'settings.uiLanguage.hint':
    'El idioma en el que está escrito el propio Parle. Es independiente del idioma en el que dicta, abajo.',
};
