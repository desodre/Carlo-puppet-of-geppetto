# Carlo — plano inicial de implementação

Data do plano: 2026-10-31. Estado: P0 integrado; provas de lookup Binder e
registro/init/unregister em API 34 x86_64. Projeto Rust 2024, versão 0.1.0, CLI e cross-builds Android.
Hierarquia, captura e protocolo seguem pendentes.
[Evidência P1.1](docs/validation/p1-1-binder.md) e
[plano aplicado para sessão nativa](docs/plans/p1-accessibility-session.md).

## Objetivo e limites

Construir um agente Android nativo em Rust para fornecer hierarquia de interface e
streaming de vídeo ao Geppetto. Executar como usuário `shell`, iniciado pelo ADB,
sem APK, DEX, JVM/ART ou ponte Java. O processo usará Binder para integração com
serviços Android e NDK para mídia, onde houver API adequada.

ADB continua responsável por implantação, conexão, toque, texto, comandos de
sistema e encerramento forçado quando necessário. Carlo não substituirá adb-server.
Áudio, injeção de entrada, OCR, seletores, XPath e execução de testes ficam fora
do agente. Decodificação de vídeo e gravação em contêiner ficam no host.

O primeiro alvo comprovado será Android 14/API 34 em x86_64; em seguida arm64 em
dispositivo físico. API 29 será o piso técnico candidato para a rota libbinder_ndk,
não uma promessa de suporte. A matriz de versões só cresce após testes reais.

## Restrições que orientam a implementação

- Suporte Rust/NDK a Binder não estabiliza os contratos privados do framework.
  AOSP documenta restrições de plataforma e diferenças entre backends. Fixar fontes
  e ferramentas utilizadas, sem copiar transações de `main` para todas as versões.
  [AIDL backends](https://source.android.com/docs/core/architecture/aidl/aidl-backends).
- Não tratar AIDL do framework como geração automática garantida para Rust:
  parcelables implementados em Java exigem estudo do formato binário e codecs
  específicos. Ausência de parser compatível deve gerar erro explícito.
- A entrada candidata para hierarquia é
  `IAccessibilityManager.registerUiTestAutomationService`, com cliente de callbacks
  nativo. Não assumir que `IUiAutomationConnection` seja um serviço global pronto
  no ServiceManager. [Interface AOSP](https://android.googlesource.com/platform/frameworks/base/+/refs/heads/main/core/java/android/view/accessibility/IAccessibilityManager.aidl).
- MediaCodec NDK fornece encoder e Surface de entrada; isso, sozinho, não captura
  a tela. A API de input Surface está disponível desde API 26.
  [NDK Media](https://developer.android.com/ndk/reference/group/media).
- Espelhamento do display dependerá de integração privada. `screenrecord` utiliza
  interfaces gráficas de plataforma; suas dependências não equivalem a um SDK
  nativo público para captura.
  [Implementação AOSP](https://android.googlesource.com/platform/frameworks/av/+/refs/heads/main/cmds/screenrecord/screenrecord.cpp).
- Meta inicial: Rust com FFI para APIs C do sistema. Se um recurso depender de
  `libgui`/ABI C++ privada ou de um shim C++, registrar impedimento e decisão
  separada. Não introduzir Java, JNI, APK ou root como fallback silencioso.

## Organização proposta

Começar com um crate, `main.rs` pequeno e lógica testável em `lib.rs`. Criar módulos
conforme cada etapa entra, sem workspace com crates vazios:

| Módulo | Responsabilidade |
|---|---|
| `runtime` | Configuração, estado da sessão e encerramento |
| `protocol` | Framing, mensagens, erros e limites |
| `transport` | Sockets locais, deadlines e escrita independente |
| `binder` | FFI, referências, parcels, callbacks e morte de serviços |
| `compat` | Perfis por versão, origem AOSP e diagnóstico |
| `hierarchy` | Janelas, nós, consistência e XML |
| `display` | Identificação, geometria, orientação e captura |
| `video` | MediaCodec NDK, configuração e pacotes codificados |
| `diagnostics` | Capacidades, contadores e logs sem conteúdo sensível |

Um processo próprio do Carlo; serviços do Android continuam em outros processos.
Pool Binder atende callbacks; aquisição de hierarquia e encoder não bloqueiam o
leitor de comandos. Inicialmente usar threads explícitas e filas limitadas.
Escolher runtime async somente se simplificar uma necessidade observada.

FFI fica encapsulada por wrappers seguros, ownership explícito e RAII. Cada
`unsafe` terá justificativa `SAFETY`, regras de thread e contrato de validade.

## Etapas e critérios de conclusão

Estados, checklists e evidências ficam no [board do GitHub](https://github.com/users/desodre/projects/11/views/1).
[KANBAN.md](KANBAN.md) preserva a referência inicial das tarefas.
Este documento descreve o escopo e os critérios técnicos de cada etapa.

Cada etapa entrega comportamento utilizável, teste de falha e evidência reproduzível.
Concluir primeiro a funcionalidade; otimização virá após medir a implementação.

### P0 — Fundação e execução nativa

- Fixar Rust/MSRV, NDK e ferramentas auxiliares; registrar versões e licenças.
- Configurar builds `x86_64-linux-android` e `aarch64-linux-android`.
- Definir `CarloError`, códigos de saída, logs em stderr e mensagens tipadas.
- Implementar `--version`, `doctor` e estrutura de `serve`.
- Detectar API, ABI, UID, contexto SELinux e recursos disponíveis sem alterá-los.
- Preparar implantação em diretório temporário exclusivo e limpeza por sessão.

Concluído quando: binário inicia no emulador sob `shell`, informa ambiente e encerra
sem APK/DEX ou processo ART próprio. Build host executa testes dos módulos puros.

### P1 — Prova de Binder e hierarquia nativa

Esta etapa decide a viabilidade da parte mais incerta antes de construir todo o agente.

- Identificar símbolos exportados de libbinder_ndk e acesso ao ServiceManager
  no alvo API 34; decidir bindings Rust reutilizáveis versus FFI C mínima.
- Fixar revisão AOSP correspondente ao alvo; listar interfaces, transações,
  flags, assinaturas, permissões e formatos de parcel utilizados.
- Implementar pool Binder, referências locais/remotas e callbacks mínimos de
  `IAccessibilityServiceClient` necessários para registro e inicialização.
- Codificar configuração mínima de `AccessibilityServiceInfo`; registrar e
  receber `IAccessibilityServiceConnection` e identificador da conexão.
- Implementar consulta de um nó e callback de resultado, correlacionando IDs.
- Decodificar campos mínimos de `AccessibilityNodeInfo`, incluindo texto,
  bounds, identificadores e propriedades booleanas, com limites de leitura.
- Liberar registro ao encerrar; detectar sessão concorrente e recusar conflito.

Concluído quando: obter um nó real da fixture e observar mudança de texto sob
`shell`, sem Java, root ou alteração de SELinux. Salvar evidência e limitações.
Se falhar, documentar chamada, permissão ou formato responsável; infraestrutura
e encoder podem avançar, mas hierarquia permanece indisponível explicitamente.

### P2 — Prova de captura e encoder nativos

Pode avançar independentemente de P1, depois de P0.

- Criar encoder H.264 por AMediaCodec, configurar Surface e produzir primeiro
  vídeo sintético por EGL/GLES nativos. Drenar saída sem capturar tela ainda.
- Identificar display real e testar rota Binder de espelhamento permitida a
  `shell`: contratos de display/SurfaceFlinger e respectivos callbacks/tokens.
- Provar ligação da saída do display à Surface do encoder. Investigar a passagem
  do produtor gráfico/Surface pelo Binder; `ANativeWindow` é opaco, não fazer casts
  para estruturas C++ privadas nem presumir conversão pública para Binder.
- Implementar apenas a rota demonstrada para API 34 e registrar dependências.
- Tratar permissão negada, display ausente e formato incompatível separadamente.

Concluído quando: H.264 da tela real é recebido no host e decodificado, com conteúdo
da fixture reconhecível e recursos liberados. Encoder sintético funcionando não
conta como captura concluída. MediaProjection não é fallback deste plano nativo.

### P3 — Protocolo e transporte v1

- Dois canais independentes sobre ADB: controle/hierarquia e vídeo. Sem servidor
  HTTP, WebSocket ou acesso direto USB nesta primeira implementação.
- Preferir Unix socket em diretório de sessão com acesso restrito; validar
  exposição via ADB. Usar identificador de sessão e validar credenciais do peer.
- Definir handshake: versão do protocolo, versão do binário, API/ABI e capacidades.
- Cabeçalho binário com magic, versão, tipo, request ID e tamanho; ordem de bytes
  documentada. Controle JSON; XML e vídeo como payloads binários, sem Base64.
- Validar comprimento antes de alocar; leitura fragmentada, EOF, versões
  incompatíveis e mensagens desconhecidas devem ter respostas definidas.
- Incluir vinculação dos dois canais à mesma sessão e timeout para canal ausente.

Comandos iniciais: `hello`, `capabilities`, `ping`, `hierarchy.dump`, `display.info`,
`video.start`, `video.stop`, `stats` e `shutdown`. São propostas de wire protocol,
não APIs públicas já implementadas do Geppetto.

Concluído quando: cliente de teste negocia sessão, recebe respostas correlacionadas,
rejeita framing inválido e continua recebendo controle com vídeo ativo.

### P4 — Hierarquia completa e contrato XML

- Enumerar janelas e árvore ativa; preservar hierarquia e identificar display.
- Percorrer filhos e deduplicar por identidade de nó/janela dentro do snapshot.
- Entregar inicialmente os 16 atributos do benchmark: text, resource-id, class,
  package, content-desc, bounds, enabled, clickable, focusable, focused, selected,
  scrollable, checkable, checked, long-clickable e password.
- Preservar Unicode, escapar XML e manter nós estruturais sem texto.
- Informar snapshot ID, tempos monotônicos de início/fim, display ID, dimensões,
  rotação e geração da geometria. IDs de nós não são seletores persistentes.
- Evitar cache na primeira versão; receber invalidações e impedir resposta antiga
  de satisfazer requisição nova. Adicionar cache apenas com teste de invalidação.
- Tratar janela removida, callback tardio e árvore mudando durante aquisição.
  Fazer retry limitado ou marcar snapshot inconsistente; não prometer atomicidade.
- Declarar árvores vazias e truncamento; nunca devolver XML parcial como sucesso.

Concluído quando: paridade dos 51 alvos e atributos da fixture contra referência
Java, atualização após toque e cenários de teclado, diálogo e troca de janela.
Canvas/WebView/Compose dependem da árvore de acessibilidade exposta pelo app;
Carlo não inventará nós ausentes.

### P5 — Streaming H.264 utilizável

- `video.start` aceita display, tamanho máximo, fps, bitrate e preferência de
  encoder; resposta informa configuração realmente aceita e codec selecionado.
- Reportar hardware/software quando houver informação confiável; caso contrário,
  usar `unknown`. Degradação de qualidade ou troca de encoder deve ser explícita.
- Enviar configuração SPS/PPS, dimensões, stream generation, PTS, flags de codec
  e keyframe em formato documentado.
- Preservar buffers de saída até concluir cópia/escrita; liberar todos os índices
  do codec inclusive em erros. Evitar caminho por pixels na CPU quando a rota Surface
  direta estiver demonstrada; não prometer zero-copy integral.
- Consumidor lento deve atingir limite de fila/deadline e interromper somente
  vídeo. Não descartar NALs arbitrários: podem ser referência para frames seguintes.
- Nova sessão/reconfiguração envia configuração e keyframe para permitir decode.
- Parar vídeo mantém hierarquia disponível e libera display virtual, Surface e codec.

Concluído quando: host decodifica stream completo, inicia/paralisa repetidamente e
continua consultando hierarquia. Primeiro preset de comparação: 360×640, 15 fps,
1,5 Mbit/s; resultados não equivalem a capacidade máxima do dispositivo.

### P6 — Rotação e mudanças de display

- Começar pelo display principal; depois seleção de outro display existente.
- Detectar rotação, tamanho, tela desligada, display removido e mudança de modo.
- Reiniciar captura/encoder de forma ordenada quando necessário, emitindo evento
  com nova geração de geometria, dimensões e configuração.
- Associar snapshots e pacotes à geração correspondente; expor timestamps sem
  afirmar sincronização atômica entre árvore e imagem.
- Definir estado suspenso/erro para conteúdo indisponível ou protegido, sem
  tentar contornar restrições de captura.

Concluído quando: rotação durante streaming mantém vídeo decodificável, bounds
coerentes e sinalização suficiente para o host descartar geometria antiga.

### P7 — Recuperação e limites de recursos

- Estados explícitos para sessão e vídeo; start/stop/shutdown idempotentes.
- Tratar EOF, perda de ADB, cliente ausente, SIGTERM/SIGINT, morte de serviço
  Binder e falha/revogação de codec, com erro tipado e retry limitado.
- Ownership explícito de cada FD, referência Binder, listener, Surface e codec.
- Deadline monotônico para operações e encerramento; chamadas Binder síncronas
  não são necessariamente canceláveis. Não criar workers ilimitados para abandoná-las.
  Host pode encerrar o processo após período de graça se uma chamada ficar presa.
- Impedir sessões conflitantes por dispositivo e separar morte de vídeo de
  morte do canal de controle. Reconexão começa sessão nova na primeira versão.
- Estatísticas de bytes, frames, filas, timeouts e duração de aquisição; logs
  não incluem texto da tela, XML completo ou conteúdo de campos protegidos.

Limites iniciais propostos, a confirmar com testes: comando 64 KiB, XML 4 MiB,
10.000 nós, profundidade 128, pacote de vídeo 8 MiB, uma aquisição de hierarquia
em voo e fila de vídeo limitada por bytes. Tamanhos de imagem, contagem de nós e
aritmética de offsets também precisam de validação antes de alocar.

Concluído quando: ciclos de abrir/fechar e falhas injetadas não acumulam FDs,
threads ou registros de automação, e agente encerra sem deixar sessão ativa.
SIGKILL não executa Drop: validar também limpeza feita pelo sistema após morte.

### P8 — Perfis de compatibilidade e distribuição

- Adicionar API 35 e 36 após API 34; avaliar API 29–33 posteriormente.
- Separar adapters por contrato comprovado, com revisão AOSP, transações e
  layouts de parcel registrados. SDK_INT sozinho não prova compatibilidade OEM.
- `doctor` e handshake anunciam recursos disponíveis por módulo, com razão
  para indisponibilidade. Versão desconhecida não recebe adapter por tentativa cega.
- Validar arm64 e páginas de memória de 16 KiB; registrar dependências ELF e
  símbolos necessários, incluindo bibliotecas de plataforma não públicas.
- Distribuir binários por ABI/perfil com checksums e manifesto de build.

Concluído quando: cada combinação anunciada tem teste executado e demais
combinações falham com diagnóstico legível, sem crash nem mudança de arquitetura.

### P9 — Cliente host e integração opt-in com Geppetto

- Criar cliente mínimo para deploy, handshake, dump XML, recepção H.264 e teardown.
- Verificar checksum, versão, diretório exclusivo, quote de comandos e limpeza.
- Definir backend explicitamente selecionável; preservar uiautomator2 existente.
- Integrar dump com WindowDump e vídeo com consumidor/recorder no host.
- Mapear dependências atuais de `_u2`: construtor, window_size e esperas também
  usam uiautomator2. Um modo Carlo não deve instalar/iniciar esse agente sem necessidade.
- Resolver esperas no host usando snapshots de Carlo, preservando semântica
  pública; toque/texto/comandos permanecem no transporte ADB existente.
- Respeitar camadas Geppetto: execução de processos somente em `core/`, erros
  próprios, tipos estritos e fixtures unitárias sem dispositivo.
- Documentar instalação e uso; integrar inspetor/artefatos sem falha de captura
  alterar resultado do teste. Migração padrão é decisão posterior aos benchmarks.

Concluído quando: Geppetto obtém hierarquia e vídeo pelo backend escolhido, sem
regressão no backend atual; cliente fecha sessão ao terminar ou falhar.

### P10 — Testes, benchmarks e polimento incremental

- Unitários de framing, limites, XML, estado, erros e codecs de parcel com fixtures
  sintéticas ou de app controlado. Testes de propriedades/fuzzing nos parsers.
- Miri para wrappers/lógica executáveis no host com FFI simulada; testes Android
  são necessários para Binder/NDK reais e não são substituídos por Miri.
- Integração em emulador: árvore fresca, múltiplas janelas, rotação, desconexão,
  consumidor lento, perda de serviço e ciclos start/stop.
- Rodar fmt, Clippy, testes host e cross-build dos alvos suportados no CI.
- Comparar Carlo nativo, agente Java mínimo e uiautomator2 na mesma tela,
  dispositivo, carga e transporte, com ordem alternada e aquecimento explícito.
- Medir p50/p95/p99, CPU, PSS/RSS, FDs, threads, bytes, fps, configuração do codec
  e latência de vídeo com marcador visual. Reportar serviços compartilhados à parte.
- Repetir sob carga contínua e amostrada, com/sem vídeo; sessões de 1, 10 e
  60 minutos. Medir atraso de consumidor e estabilidade após aquecimento.
- Polir um gargalo por vez: transações por dump, prefetch, cópias, alocações,
  formato de saída, fila de vídeo e ajustes de encoder; medir antes/depois.

Concluído quando: funcionalidades e limites têm evidência reproduzível, sem
prometer memória menor ou velocidade maior apenas por usar Rust.

## Ordem de execução e primeiro marco

Sequência: P0 → P1/P2 → P3 → P4/P5 → P6/P7 → P8/P9 → P10 contínuo.
P7 fornece limites e cleanup desde P0; P10 acompanha cada mudança, não é uma fila
final de testes. P1 e P2 são trilhas independentes em termos de dependências,
sem exigir múltiplos agentes ou desenvolvimento simultâneo.

Primeiro marco entregável: binário nativo API 34 que inicia como shell, obtém um
nó real com mudança observável, gera H.264 sintético e prova captura de um frame
real, com teardown verificável. Somente depois consolidar protocolo e API host.

Cada etapa deve virar tarefa pequena com resultado, teste, limitações e evidência.
Estado deste documento não significa que algum recurso acima já está implementado.

## Referência experimental existente

O benchmark anterior mediu agente Java mínimo, não esta arquitetura nativa.
Em API 34: mínimo sem vídeo p50/p95 78,21/94,77 ms; mínimo com MediaProjection e
MediaCodec em APK separado 75,40/91,16 ms; uiautomator2 90,87/108,80 ms.
São controles experimentais, não metas portáveis nem prova da rota Binder nativa.

Relatório local: `../Geppetto/results/carlo-media-benchmark-2026-10-31/REPORT.md`.
Preservar dados originais e registrar novos resultados separadamente.
