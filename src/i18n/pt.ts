// Brazilian Portuguese (pt-BR) strings. Mirrors the key order and the comment
// grouping of en.ts so the two files diff side by side.
//
// Trigger words that the engine matches literally (filler words, hedges,
// dictated punctuation commands, self-correction cues) are English-only in
// crates/parle-core/src/formatter.rs, so the examples in those hints stay in
// English on purpose: translating them would promise behaviour that does not
// exist.
export const pt: Record<string, string> = {
  // ---------- Shared ----------
  'common.cancel': 'Cancelar',
  'common.dismiss': 'Dispensar',
  'common.grant': 'Permitir',
  'common.keepIt': 'Manter',
  'common.openSystemSettings': 'Abrir configurações do sistema',

  // ---------- Relative time ----------
  'time.justNow': 'agora mesmo',
  'time.minutesAgo': 'há {n} min',
  'time.hoursAgo': 'há {n} h',
  'time.secondsShort': '{n}s',

  // ---------- App shell ----------
  'app.nav.compose': 'Redigir',
  'app.nav.history': 'Histórico',
  'app.nav.dictionary': 'Dicionário',
  'app.nav.models': 'Modelos',
  'app.nav.sync': 'Sincronização',
  'app.nav.settings': 'Ajustes',
  'app.record.start': 'Iniciar ditado',
  'app.record.stop': 'Parar ditado',
  'app.toast.pasteInstruction': 'Copiado. Pressione {keys} para colar',
  'app.toast.inserted': 'Inserido "{text}"',
  'app.toast.copied': 'Copiado "{text}"',

  // ---------- Recording overlay (HUD) ----------
  'hud.pasteInstruction': 'Copiado. Pressione {keys} para colar',
  'hud.recordingClickToStop': 'Gravando. Clique para parar',
  'hud.transcribing': 'Transcrevendo…',
  'hud.stopAndPaste': 'Parar e colar',
  'hud.working': 'Processando…',
  'hud.cancel': 'Cancelar (Esc)',
  'hud.deck.rec': 'REC',
  'hud.deck.proc': 'PROC',

  // ---------- History ----------
  'history.searchPlaceholder': 'Buscar transcrições e área de transferência…  (↑↓ · Enter cola · {copyKeys} copia)',
  'history.filter.all': 'Tudo',
  'history.filter.dictations': 'Ditados',
  'history.filter.clipboard': 'Área de transferência',
  'history.filter.allDevices': 'Todos os dispositivos',
  'history.filter.thisDevice': '{name} (este dispositivo)',
  'history.gone': 'Esse item não está mais aqui. Ele foi apagado em outro dispositivo, por isso a lista foi atualizada.',
  'history.empty.noMatches': 'Nenhum resultado.',
  'history.empty.nothingYet': 'Nada aqui ainda. Segure sua tecla de atalho e fale.',
  'history.deleteConfirm': 'Apagar este item? Não é possível desfazer.',
  'history.mayAlsoDelete': 'Isso também pode apagar o item dos seus dispositivos pareados.',
  'history.alsoDeletesFrom': 'Isso também apaga o item de {devices}.',
  'history.localOnly.badge': 'somente neste dispositivo',
  'history.localOnly.title':
    'O Parle não pôde descartar que este era um campo de senha, por isso o item fica somente neste dispositivo e nunca é enviado aos seus outros dispositivos',
  'history.fromDevice.title': 'Escrito noutro dos seus dispositivos e sincronizado aqui',
  'history.action.paste': 'Colar',
  'history.action.pasteTitle': 'Colar no app anterior (Enter)',
  'history.action.copy': 'Copiar',
  'history.action.copyTitle': 'Copiar ({copyKeys})',
  'history.action.editTitle': 'Editar (alimenta o aprendizado automático)',
  'history.action.pin': 'Fixar',
  'history.action.unpin': 'Desafixar',
  'history.action.delete': 'Apagar',
  'history.trimmedCount': '{n} cortes',
  'history.unsureCount': '{n} incertos',
  'history.restoreRaw': 'Restaurar original',

  // ---------- Compose ----------
  'compose.title': 'Redigir',
  'compose.intro':
    'Dite aqui e cole links ou textos no meio da frase: cada inserção fica presa ao momento exato em que você a adicionou e entra no texto final sem alterar um byte.',
  'compose.start': 'Iniciar ditado',
  'compose.stop': 'Parar',
  'compose.transcribing': 'Transcrevendo…',
  'compose.recording': 'gravando',
  'compose.processing': 'processando',
  'compose.markPlaceholder.recording': 'Cole um link ou digite, Enter fixa neste momento…',
  'compose.markPlaceholder.idle': 'Comece a ditar para inserir links e textos',
  'compose.insert': 'Inserir',
  'compose.noSpeech': 'Nenhuma fala detectada.',
  'compose.copyResult': 'Copiar resultado',
  'compose.alsoInserted': 'Também inserido no seu cursor e salvo no Histórico.',
  'compose.barActive':
    'Cole ou digite na barra de ditado na parte inferior da janela, de qualquer aba. Enter fixa aqui.',

  // ---------- Barra de ditado ----------
  'bar.pinnedCount': 'inserções: {n}',
  'bar.pinnedAt': 'Inserido em {time}',
  'bar.openCompose': 'Abrir Redigir para ver o que inseriu',
  'bar.insertHint': 'Enter insere. Shift + Enter adiciona uma linha.',

  // ---------- Models ----------
  'models.title': 'Modelos',
  'models.subtitle': 'Toda a transcrição roda neste dispositivo.',
  'models.warm': 'Carregado · {model}',
  'models.loadsOnFirstUse': 'O modelo carrega no primeiro uso',
  'models.active': 'Ativo',
  'models.backendTitle': 'O hardware em que este modelo roda',
  'models.yourFile': 'Seu arquivo',
  'models.speedRating': 'velocidade {value}/5',
  'models.accuracyRating': 'precisão {value}/5',
  'models.languageCount': '{n} idiomas',
  'models.use': 'Usar',
  'models.deleteFile': 'Apagar arquivo do modelo',
  'models.removeCustom': 'Remover desta lista (seu arquivo não é apagado)',
  'models.removeFromList': 'Remover desta lista',
  'models.fileMissing': 'Arquivo ausente',
  'models.download': 'Baixar',
  'models.addLocal': 'Adicionar um modelo local…',
  'models.addLocal.hint':
    'Um arquivo GGML {ext} do whisper.cpp que você já tem. O Parle aponta para ele onde ele está e nunca o copia, então não ocupa disco a mais.',
  'models.picker.title': 'Escolha um modelo do whisper.cpp',
  'models.picker.filter': 'Modelo Whisper',
  'models.defaultLocalName': 'Modelo local',
  'models.fallbackHint':
    'Se o modelo ativo não carregar (por exemplo, sob pressão de memória), o Parle recua automaticamente para o modelo seguinte da lista: sua gravação nunca é perdida.',

  // ---------- Onboarding ----------
  'onboarding.welcome.title': 'Boas-vindas ao Parle',
  'onboarding.welcome.body':
    'Segure uma tecla, fale e solte: suas palavras aparecem onde o cursor está. A transcrição roda inteiramente neste dispositivo. Nada do que você diz sai dele.',
  'onboarding.welcome.cta': 'Configurar',
  'onboarding.permissions.title': 'Permissões',
  'onboarding.permissions.introMac':
    'O Parle precisa de duas permissões para ouvir você e digitar por você. As duas ficam nesta máquina.',
  'onboarding.permissions.introWin': 'O Parle precisa de uma permissão, para ouvir você. Ela fica nesta máquina.',
  'onboarding.permissions.microphone': 'Microfone',
  'onboarding.permissions.microphoneDesc': 'Para ouvir seu ditado',
  'onboarding.permissions.accessibility': 'Acessibilidade',
  'onboarding.permissions.accessibilityDesc': 'Para monitorar sua tecla de atalho e colar no cursor',
  'onboarding.permissions.macNote':
    'Nos Ajustes do Sistema, adicione o {app} em Privacidade e Segurança → Acessibilidade e volte aqui. Esta página se atualiza sozinha. Pode ser preciso reiniciar o Parle depois de conceder.',
  'onboarding.permissions.appName': 'Parle',
  'onboarding.permissions.openSettings': 'Abrir configurações',
  'onboarding.permissions.continue': 'Continuar',
  'onboarding.permissions.waiting': 'Aguardando as permissões…',
  'onboarding.model.title': 'Seu modelo',
  'onboarding.model.machine': '{ram} GB de RAM, {gpu}',
  'onboarding.model.recommendation':
    'Com base nesta máquina ({machine}), recomendamos o {model}. Você pode adicionar ou trocar de modelo quando quiser em Ajustes → Modelos.',
  'onboarding.model.downloadFailed':
    'Falha no download: {error}. Verifique sua conexão e tente de novo: ele continua de onde parou.',
  'onboarding.model.ready': 'Modelo pronto',
  'onboarding.model.download': 'Baixar',
  'onboarding.hotkey.title': 'Sua tecla',
  'onboarding.hotkey.macKey': '🌐 tecla Fn',
  'onboarding.hotkey.doNothing': 'Não Fazer Nada',
  'onboarding.hotkey.mac':
    'Padrão: a {key}. Segure e fale, solte para colar, ou dê um toque rápido para travar a gravação. Dica: em Ajustes do Sistema → Teclado, defina “Pressionar a tecla 🌐 para” como {doNothing}, assim o ditado do macOS não disputa a tecla.',
  'onboarding.hotkey.winKey': 'Ctrl direito',
  'onboarding.hotkey.win':
    'Padrão: {key}. Segure e fale, solte para colar, ou dê um toque rápido para travar a gravação. Tem uma tecla Copilot? Vincule-a em Ajustes → Atalhos e o Parle assume o controle dela por completo.',
  'onboarding.hotkey.cta': 'Entendi',
  'onboarding.test.title': 'Experimente',
  'onboarding.test.body': 'Clique no botão (ou use sua tecla de atalho), diga alguma coisa e pare.',
  'onboarding.test.start': 'Iniciar ditado de teste',
  'onboarding.test.stop': 'Parar',
  'onboarding.test.transcribing': 'Transcrevendo…',
  'onboarding.test.noSpeech': 'Nenhuma fala detectada. Tente de novo, um pouco mais alto.',
  'onboarding.test.finish': 'Concluir configuração',

  // ---------- Settings: shell ----------
  'settings.title': 'Ajustes',
  'settings.subtitle': 'Só local. Sem telemetria, sem nuvem, nunca.',
  'settings.section.hotkeys': 'Atalhos',
  'settings.section.language': 'Idioma',
  'settings.section.cleanup': 'Limpeza',
  'settings.section.dictionary': 'Dicionário',
  'settings.section.output': 'Saída',
  'settings.section.appearance': 'Aparência',
  'settings.section.historyPrivacy': 'Histórico e privacidade',
  'settings.section.audio': 'Áudio',
  'settings.section.general': 'Geral',
  'settings.footer.tagline': 'ditado no dispositivo',
  'settings.footer.note': 'nada nunca sai desta máquina',

  // ---------- Settings: hotkeys ----------
  'settings.dictationKey.label': 'Tecla de ditado',
  'settings.dictationKey.hintMac': 'A tecla Fn precisa da permissão de Acessibilidade',
  'settings.dictationKey.hintWin': 'Alt direito é AltGr em muitos layouts, então Ctrl direito é mais seguro',
  'settings.dictationKey.custom': 'Personalizada…',
  'settings.customBinding.label': 'Atalho personalizado',
  'settings.customBinding.hint': 'Clique e depois pressione a tecla ou combinação que quiser. Esc cancela.',
  'settings.customBinding.listening': 'Pressione uma combinação de teclas…',
  'settings.gesture.label': 'Gesto',
  'settings.gesture.hintDoubleTap':
    'Dois toques iniciam, um toque para. A tecla nunca é interceptada, então o comportamento normal dela no sistema continua funcionando.',
  'settings.gesture.hint': 'Híbrido: segure para falar; um toque rápido trava até o toque seguinte',
  'settings.gesture.hold': 'Segurar',
  'settings.gesture.toggle': 'Alternar',
  'settings.gesture.hybrid': 'Híbrido',
  'settings.gesture.doubleTap': 'Dois toques',
  'settings.latch.label': 'Janela de travamento',
  'settings.latch.hint':
    'Híbrido: toques mais curtos que isso travam no modo alternar. Dois toques: intervalo máximo entre os toques',
  'settings.escCancel.label': 'Esc cancela a gravação',
  'settings.escCancel.hint':
    'Desativado por padrão: o Esc é pressionado por todo tipo de motivo sem relação, e descartar uma gravação que você já falou é pior do que pará-la com sua tecla de atalho',
  'settings.historyPalette.label': 'Paleta do histórico',
  'settings.historyPalette.hint': 'Combinação de teclas para a busca',
  'settings.suppressCopilot.label': 'Impedir a abertura do Copilot',
  'settings.suppressCopilot.hint':
    'Quando a tecla Copilot está vinculada (ou isto está ativado), o app padrão do Copilot nunca abre',
  'settings.accessibilityMissing':
    'A permissão de Acessibilidade está faltando. Teclas especiais e a colagem no cursor não vão funcionar. Se você já concedeu e este aviso continua, o registro ficou desatualizado depois de uma recompilação: use Reparar.',
  'settings.repairPermission': 'Reparar permissão',
  'settings.bindingWarning.leftCtrl':
    'O Ctrl esquerdo comanda a maioria dos atalhos de teclado, então vinculá-lo vai disparar durante o uso normal.',
  'settings.bindingWarning.leftShift':
    'O Shift esquerdo é pressionado o tempo todo enquanto você digita, então espere disparos acidentais.',
  'settings.bindingWarning.rightAlt':
    'O Alt direito é AltGr em muitos layouts, então ele digita caracteres acentuados. O Ctrl direito é mais seguro.',

  // ---------- Settings: key names ----------
  'keys.fn': '🌐 Fn / Globo',
  'keys.rightCommand': '⌘ direito',
  'keys.leftCommand': '⌘ esquerdo',
  'keys.rightOption': '⌥ direito',
  'keys.leftOption': '⌥ esquerdo',
  'keys.rightControl': '⌃ direito',
  'keys.leftControl': '⌃ esquerdo',
  'keys.copilot': 'Tecla Copilot',
  'keys.rightCtrl': 'Ctrl direito',
  'keys.leftCtrl': 'Ctrl esquerdo',
  'keys.rightShift': 'Shift direito',
  'keys.leftShift': 'Shift esquerdo',
  'keys.leftAlt': 'Alt esquerdo',
  'keys.rightAlt': 'Alt direito',
  'keys.rightWin': 'Win direito',
  'keys.leftWin': 'Win esquerdo',

  // ---------- Settings: language ----------
  'settings.spokenLanguage.label': 'Idioma falado',
  'settings.language.auto': 'Detectar automaticamente',
  'settings.language.en': 'Inglês',
  'settings.language.es': 'Espanhol',
  'settings.language.fr': 'Francês',
  'settings.language.de': 'Alemão',
  'settings.language.it': 'Italiano',
  'settings.language.pt': 'Português',
  'settings.language.nl': 'Holandês',
  'settings.language.ja': 'Japonês',
  'settings.language.ko': 'Coreano',
  'settings.language.zh': 'Chinês',
  'settings.language.hi': 'Hindi',
  'settings.language.ar': 'Árabe',
  'settings.language.ru': 'Russo',
  'settings.language.pl': 'Polonês',
  'settings.language.sv': 'Sueco',
  'settings.localeSpelling.label': 'Ortografia regional',
  'settings.localeSpelling.hint': 'Afeta a ortografia do resultado (colour vs color)',
  'settings.locale.none': 'Sem preferência',
  'settings.locale.enAU': 'Inglês (Austrália)',
  'settings.locale.enGB': 'Inglês (Reino Unido)',
  'settings.locale.enUS': 'Inglês (EUA)',
  'settings.applyLocaleSpelling.label': 'Aplicar ortografia regional',
  'settings.applyLocaleSpelling.hint': 'Converte grafias dos EUA na transcrição para a sua região',
  'settings.translate.label': 'Traduzir para o inglês',
  'settings.translate.hint': 'Fale em qualquer idioma, cole em inglês',

  // ---------- Settings: cleanup ----------
  'settings.smartCleanup.label': 'Limpeza inteligente',
  'settings.smartCleanup.hint': 'Chave geral do nível de limpeza determinística',
  'settings.removeFillers.label': 'Remover hesitações',
  'settings.removeFillers.hint': 'hum, ahn, hmm…',
  'settings.removeHedges.label': 'Remover expressões vagas',
  'settings.removeHedges.hint': 'ou seja, por assim dizer (mais agressivo)',
  'settings.trimSelfCorrections.label': 'Cortar autocorreções',
  'settings.trimSelfCorrections.hint':
    '“Thursday, no actually Wednesday” → “Wednesday”. Os trechos cortados continuam revisáveis no Histórico',
  'settings.dictatedPunctuation.label': 'Pontuação ditada',
  'settings.dictatedPunctuation.hint':
    '“comma”, “new line”, “question mark”… (“literally comma” escapa)',
  'settings.capitalise.label': 'Iniciar frases com maiúscula',
  'settings.terminalPunctuation.label': 'Terminar com pontuação',
  'settings.paragraphPause.label': 'Novo parágrafo em pausa longa',

  // ---------- Settings: dictionary ----------
  'settings.dictionary.enable': 'Ativar dicionário',
  'settings.dictionary.bias.label': 'Direcionar o reconhecimento',
  'settings.dictionary.bias.hint': 'Envia seus termos ao motor como um glossário',
  'settings.dictionary.fuzzy.label': 'Corrigir grafias parecidas',
  'settings.dictionary.autoLearn.label': 'Aprender com minhas edições',
  'settings.dictionary.autoLearn.hint': 'Edições de uma palavra no Histórico viram pares de correção',

  // ---------- Settings: output ----------
  'settings.insertAtCursor.label': 'Inserir no cursor',
  'settings.insertAtCursor.hint': 'Digita o resultado no app em foco',
  'settings.copyToClipboard.label': 'Copiar para a área de transferência',
  'settings.restoreClipboard.label': 'Restaurar a área de transferência anterior',
  'settings.restoreClipboard.hint': 'Depois da colagem, devolve o conteúdo antigo da área de transferência',
  'settings.restoreDelay.label': 'Atraso da restauração',
  'settings.restoreDelay.hint': 'Apps lentos (Office, área de trabalho remota) leem a área de transferência com atraso',
  'settings.preferAxInsert.label': 'Preferir inserção direta',
  'settings.preferAxInsert.hint': 'Tenta a inserção de texto por Acessibilidade antes de colar pela área de transferência',
  'settings.pressEnter.label': 'Pressionar Enter depois de inserir',
  'settings.pressEnter.hint':
    'Envia a mensagem logo depois de colar, útil em apps de mensagem. Nunca dispara em campos seguros.',

  // ---------- Settings: appearance ----------
  'settings.theme.label': 'Tema',
  'settings.theme.system': 'Sistema',
  'settings.theme.light': 'Claro',
  'settings.theme.dark': 'Escuro',
  'settings.palette.label': 'Paleta',
  'settings.palette.hint':
    'A Pastel se tinge com a sua cor de destaque, então experimente com a roda de cores personalizada',
  'settings.palette.paper': 'Papel',
  'settings.palette.pastel': 'Pastel',
  'settings.palette.bold': 'Vibrante',
  'settings.palette.retro': 'Retrô',
  'settings.accent.label': 'Destaque',
  'settings.accent.custom': 'Cor personalizada',
  'settings.appIcon.label': 'Ícone do app',
  'settings.appIcon.hint': 'Vale na hora dentro do app; o ícone no Finder muda depois de reiniciar',
  'settings.appIcon.default': 'Parle',
  'settings.appIcon.keycap': 'Tecla',
  'settings.appIcon.waveform': 'Onda',
  'settings.appIcon.echoRings': 'Anéis de eco',
  'settings.appIcon.cassette': 'Cassete',
  'settings.iconRestart': 'Ícone atualizado. Reinicie para atualizar o ícone no Finder e no Dock.',
  'settings.restartParle': 'Reiniciar o Parle',
  'settings.trayIcon.labelMac': 'Ícone na barra de menus',
  'settings.trayIcon.labelWin': 'Ícone na bandeja',
  'settings.trayIcon.hintMac': 'O monocromático acompanha a barra de menus; o selo mantém a cor do Parle',
  'settings.trayIcon.hintWin': 'O automático escolhe o contorno que se destaca na sua barra de tarefas',
  'settings.tray.template': 'Monocromático',
  'settings.tray.badge': 'Selo azul',
  'settings.tray.auto': 'Automático: acompanhar a barra de tarefas',
  'settings.tray.light': 'Contorno claro',
  'settings.tray.dark': 'Contorno escuro',
  'settings.tray.color': 'Contorno azul',
  'settings.overlayStyle.label': 'Estilo da sobreposição',
  'settings.overlayStyle.hintHidden':
    'Nenhuma sobreposição. Enquanto o Parle está ouvindo, o ícone na barra de menus mostra um ponto no canto, e essa é a única indicação.',
  'settings.overlayStyle.hint': 'A Cassete combina lindamente com a paleta Retrô',
  'settings.overlayStyle.pill': 'Pílula',
  'settings.overlayStyle.cassette': 'Cassete',
  'settings.overlayStyle.metal': 'Metal',
  'settings.overlayStyle.minimal': 'Mínima',
  'settings.overlayStyle.none': 'Nenhuma',
  'settings.waveformSensitivity.label': 'Sensibilidade da onda',
  'settings.waveformSensitivity.hint':
    'Aumente se as barras mal se mexem quando você fala, diminua se elas ficam no topo. Isso muda o que o medidor mostra, nunca o que é gravado ou transcrito.',
  'settings.showPartial.label': 'Mostrar transcrição ao vivo na sobreposição',
  'settings.reduceMotion.label': 'Reduzir movimento',
  'settings.reduceMotion.hint':
    'Nada é animado sem necessidade. A barra de ditado aparece na parte inferior da janela em vez de crescer a partir do botão de gravação, e os rolos da cassete param de girar. Útil se as animações incomodam, ou em uma máquina mais antiga.',

  // ---------- Settings: history & privacy ----------
  'settings.clipboardCapture.label': 'Capturar a área de transferência',
  'settings.clipboardCapture.hint':
    'Tudo o que você copia, pesquisável. Gerenciadores de senhas ficam de fora',
  'settings.retention.label': 'Manter itens por',
  'settings.retention.confirmNarrow':
    'Itens mais antigos que isso serão apagados deste dispositivo e não poderão ser recuperados, nem a partir de um dispositivo pareado. Continuar?',
  'settings.retention.forever': 'Para sempre',
  'settings.retention.d90': '90 dias',
  'settings.retention.d30': '30 dias',
  'settings.retention.d7': '7 dias',
  'settings.retention.d1': '1 dia',
  'settings.excludedApps.label': 'Apps ignorados',
  'settings.excludedApps.hint':
    'Um por linha: bundle id no Mac, nome do exe no Windows. Esta lista vale por dispositivo, então adicione a entrada em cada máquina. A partir do momento em que você adiciona uma entrada, o Parle para de enviar registros desse app aos seus outros dispositivos. O que já foi sincronizado continua neles.',
  'settings.dangerZone.label': 'Zona de perigo',
  'settings.clearHistory.button': 'Limpar todo o histórico não fixado',
  'settings.clearHistory.confirmWithDevices':
    'Isso apaga todos os itens não fixados neste dispositivo e em {devices}. Os itens fixados permanecem. Não é possível desfazer.',
  'settings.clearHistory.confirm':
    'Isso apaga todos os itens não fixados neste dispositivo. Os itens fixados permanecem. Não é possível desfazer.',
  'settings.clearHistory.clearIt': 'Limpar',

  // ---------- Settings: audio ----------
  'settings.microphone.label': 'Microfone',
  'settings.microphone.systemDefault': 'Padrão do sistema',
  'settings.minDuration.label': 'Ignorar gravações menores que',
  'settings.microphoneDenied': 'O acesso ao microfone foi negado.',

  // ---------- Settings: general ----------
  'settings.launchAtLogin.label': 'Abrir ao fazer login',
  'settings.prewarm.label': 'Pré-carregar o modelo ao iniciar',
  'settings.prewarm.hint': 'Usa memória enquanto está ocioso, deixa o primeiro ditado instantâneo',

  // ---------- Settings: sync ----------
  'sync.section': 'Sincronização',
  'sync.unavailable': 'A sincronização não está disponível agora. {error}',
  'sync.checking': 'Verificando a sincronização…',
  'sync.genericError': 'Algo deu errado.',
  'sync.enable.label': 'Sincronizar com meus outros dispositivos',
  'sync.enable.hint':
    'Desativado a menos que você ative. Seu Mac e seu PC conversam direto entre si pela sua rede local, com criptografia de ponta a ponta, sem conta e sem nada enviado para lugar nenhum. Enquanto está ativado, o Parle anuncia o nome deste dispositivo às outras máquinas da mesma rede para que elas possam encontrá-lo.',
  'sync.tryAgain': 'Tentar de novo',
  'sync.thisDevice.label': 'Este dispositivo',
  'sync.thisDevice.hint': 'O nome que a outra máquina vê durante o pareamento.',
  'sync.thisDevice.placeholder': 'Dê um nome a este dispositivo',
  'sync.nameSanitised':
    'Salvo como "{name}". Um nome de dispositivo não pode conter "=" nem caracteres ocultos, e é encurtado para caber.',
  'sync.deviceId.label': 'ID do dispositivo',
  'sync.deviceId.hint': 'A identidade desta instalação. Ela nunca sai da sua rede.',
  'sync.paired.label': 'Dispositivos pareados',
  'sync.paired.hint': 'Só estas máquinas podem ver seu histórico. O pareamento é mútuo.',
  'sync.paired.none': 'Nenhum dispositivo pareado ainda. Pareie um abaixo para começar a sincronizar.',
  'sync.now.button': 'Sincronizar agora',
  'sync.now.working': 'A sincronizar',
  'sync.now.none': 'Nenhum dispositivo emparelhado está acessível neste momento.',
  'sync.now.ok': 'A trocar dados com os seus outros dispositivos.',
  'sync.syncedAgo': 'Sincronizado {when}',
  'sync.visibleNotSynced': 'Visível na rede, mas nada foi sincronizado ainda',
  'sync.neverConnected': 'Nunca conectado',
  'sync.lastSeen': 'Visto {when}',
  'sync.unpairConfirm':
    'Desparear {name}? Ele para de sincronizar e vai precisar de um novo código para voltar. O que já está em {name} continua lá.',
  'sync.unpair': 'Desparear',
  'sync.pairNew.label': 'Parear um novo dispositivo',
  'sync.pairNew.hint': 'Qualquer uma das máquinas pode começar. Leia os seis dígitos em voz alta ou digite-os na outra.',
  'sync.direction.show': 'Mostrar um código',
  'sync.direction.enter': 'Digitar um código',
  'sync.code.typeOnOther': 'Digite no outro dispositivo. Expira em {time}',
  'sync.code.expired': 'Este código expirou.',
  'sync.code.showNew': 'Mostrar um novo código',
  'sync.code.explain':
    'O Parle mostra seis dígitos aqui; digite-os na outra máquina para confirmar que ela é mesmo sua.',
  'sync.peers.notSearching':
    'Nenhuma busca por dispositivos no momento. Abra o Parle na outra máquina, ative a Sincronização lá também e confirme que as duas estão na mesma rede.',
  'sync.peers.stillNothing':
    'Ainda nada depois de um tempo. Verifique se o Parle está aberto na outra máquina com a Sincronização ativada, e se as duas estão na mesma rede.',
  'sync.peers.macBlocked':
    'Se estiver tudo certo, o macOS pode estar impedindo o Parle de ver a rede local, o que parece exatamente com isto.',
  'sync.peers.openLocalNetwork': 'Abrir os ajustes de Rede Local',
  'sync.peers.winBlocked': 'Se estiver tudo certo, o Firewall do Windows pode estar bloqueando o Parle.',
  'sync.peers.openFirewall': 'Abrir as configurações do firewall',
  'sync.peers.vpnHint':
    'Uma VPN é a outra causa comum: muitas bloqueiam o tráfego da rede local mesmo quando tudo o resto funciona. Desligue-a ou active a definição de partilha da rede local.',
  'sync.peers.isolatedHint':
    'As redes Wi-Fi de hotéis e de convidados costumam impedir que dispositivos na mesma rede se vejam. Uma partilha de ligação do telemóvel é a forma rápida de o despistar.',
  'sync.peers.looking':
    'Procurando dispositivos nesta rede… Abra o Parle na outra máquina e ative a Sincronização lá também.',
  'sync.pairing': 'Pareando…',
  'sync.pair': 'Parear',
  'sync.pair.needsDevice':
    'Seleccione primeiro o dispositivo acima. Nada é listado enquanto as duas máquinas não se virem na rede.',
  'sync.dictations.label': 'Sincronizar ditados',
  'sync.dictations.hintPaired':
    'Tudo o que você dita aparece no Histórico nas duas máquinas. Reativar isto reenvia seu histórico aos dispositivos pareados, o que pode levar um instante.',
  'sync.dictations.hint': 'Tudo o que você dita aparece no Histórico nas duas máquinas',
  'sync.clipboard.label': 'Sincronizar a área de transferência',
  'sync.clipboard.hintPaired':
    'Copie em uma máquina, cole na outra. Reativar isto reenvia seu histórico aos dispositivos pareados, o que pode levar um instante.',
  'sync.clipboard.hint': 'Copie em uma máquina, cole na outra',

  // ---------- Dictionary ----------
  'dictionary.title': 'Dicionário',
  'dictionary.subtitle':
    'Nomes, marcas e jargões que o Parle deve acertar. Os termos direcionam o reconhecimento e corrigem grafias parecidas, sem nunca inserir palavras que você não disse.',
  'dictionary.term.placeholder': 'Termo (com as maiúsculas exatas, ex.: “farsiight”)',
  'dictionary.corrections.placeholder':
    'Ouvido como… (opcional, separado por vírgulas, ex.: “far sight, foresight”)',
  'dictionary.add': 'Adicionar',
  'dictionary.empty': 'Nenhum termo ainda. Adicione os nomes e jargões que você usa todo dia.',
  'dictionary.autoBadge': 'auto',
  'dictionary.autoBadgeTitle': 'Aprendido com suas correções',
  'dictionary.fuzzyMatch': 'correspondência aproximada',

  // Onboarding: language
  'onboarding.language.title': 'Escolha seu idioma',
  'onboarding.language.sub': 'O Parle vai falar este idioma e vai esperar que você dite nele.',
  'onboarding.language.note': 'Você pode mudar os dois depois, e eles não precisam ser iguais: muita gente usa a interface em um idioma e dita em outro.',

  // ---------- Settings: interface language ----------
  'settings.uiLanguage.label': 'Idioma da interface',
  'settings.uiLanguage.hint': 'O idioma em que o próprio Parle é escrito. Separado do idioma em que você dita, abaixo.',

  // ---------- Onboarding: what Parle is ----------
  'onboarding.hotkey.openKeyboard': 'Abrir os ajustes de Teclado',
  'onboarding.about.title': 'O que o Parle faz',
  'onboarding.about.sub': 'Ditado e um histórico da área de transferência, e os dois ficam nesta máquina.',
  'onboarding.about.dictation.title': 'Falar em vez de digitar',
  'onboarding.about.dictation.body':
    'Segure sua tecla de ditado, diga o que quiser e solte. O texto aparece onde o cursor está. Nada é enviado para lugar nenhum: o modelo roda nesta máquina e funciona sem internet nenhuma.',
  'onboarding.about.clipboard.title': 'Tudo o que você copia, guardado',
  'onboarding.about.clipboard.body':
    'O Parle lembra o que você copia para você achar de novo depois. Gerenciadores de senhas ficam de fora desde o começo, e você pode adicionar qualquer outro app que preferir que ele ignore.',
  'onboarding.about.sync.title': 'Suas outras máquinas, se você quiser',
  'onboarding.about.sync.body':
    'O Parle pode sincronizar com seus próprios dispositivos pela sua rede local, com criptografia de ponta a ponta, sem conta e sem servidor no meio. Fica desativado até você ativar, e a sincronização da área de transferência continua desativada até você pedir separadamente.',
  'onboarding.about.privacy.title': 'Continua sendo seu',
  'onboarding.about.privacy.body':
    'Sem conta, sem telemetria, sem nuvem. Seu histórico é um arquivo nesta máquina que você pode limpar quando quiser.',
};
