# Carlo — kanban

[README e roadmap](README.md) · [Plano técnico](PLANO.md)

Acompanhamento ativo: [board no GitHub](https://github.com/users/desodre/projects/11/views/1).

As etapas são [11 issues pai com 71 subissues nativas](docs/ISSUES.md).
O board é a fonte de estados, subtarefas e evidências. Este arquivo preserva
a referência inicial dos 11 cartões e 71 tarefas; não sincroniza automaticamente
com o GitHub. O plano técnico mantém requisitos e justificativas. Na publicação
inicial, P0 está em Pronto e as demais etapas em Backlog.

## Quadro inicial

| Backlog | Pronto | Em andamento | Em revisão | Concluído |
|---|---|---|---|---|
| [P1 — Binder e nó real](#p1) | [P0 — Fundação nativa](#p0) | — | — | — |
| [P2 — Captura e encoder](#p2) | — | — | — | — |
| [P3 — Protocolo e transporte](#p3) | — | — | — | — |
| [P4 — Hierarquia XML](#p4) | — | — | — | — |
| [P5 — Streaming H.264](#p5) | — | — | — | — |
| [P6 — Rotação e display](#p6) | — | — | — | — |
| [P7 — Recuperação e recursos](#p7) | — | — | — | — |
| [P8 — Compatibilidade e distribuição](#p8) | — | — | — | — |
| [P9 — Integração Geppetto](#p9) | — | — | — | — |
| [P10 — Testes e benchmarks](#p10) | — | — | — | — |

## Como atualizar

1. No board do GitHub, mover cartão para **Pronto** quando houver dependências e critério de conclusão claros.
2. Ao começar, mover para **Em andamento** e registrar o trabalho no cartão do GitHub.
3. Marcar cada tarefa pelo identificador estável, por exemplo `P1.3`, com evidência
   verificável: arquivo, comando, resultado de teste, relatório ou commit.
4. Usar **Em revisão** após implementação e validação; mover para **Concluído**
   quando todas as tarefas e o critério de conclusão estiverem satisfeitos.
5. Se houver impedimento, registrar causa e condição de desbloqueio no cartão.
   Dependência futura, por si, mantém o cartão no backlog; não indica falha técnica.
6. Mudança de escopo atualiza plano, cartão no GitHub, referência local e roadmap quando afetado.

P7 e P10 acompanham todas as etapas: suas tarefas podem avançar antes da conclusão
integral dos cartões. Um cartão grande pode ser dividido em cartões menores,
preservando os IDs das tarefas e a ligação com sua etapa original.

## Cartões

<a id="p0"></a>

### P0 — Fundação e execução nativa

Issue pai: [#1](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/1). Subissues: [P0.1 · #12](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/12), [P0.2 · #13](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/13), [P0.3 · #14](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/14), [P0.4 · #15](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/15), [P0.5 · #16](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/16), [P0.6 · #17](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/17).

**Estado:** Pronto  
**Dependências:** Nenhuma; primeiro cartão pronto para iniciar.

- [ ] **P0.1** — Fixar Rust/MSRV, NDK e ferramentas auxiliares; registrar versões e licenças.
- [ ] **P0.2** — Configurar builds `x86_64-linux-android` e `aarch64-linux-android`.
- [ ] **P0.3** — Definir `CarloError`, códigos de saída, logs em stderr e mensagens tipadas.
- [ ] **P0.4** — Implementar `--version`, `doctor` e estrutura de `serve`.
- [ ] **P0.5** — Detectar API, ABI, UID, contexto SELinux e recursos disponíveis sem alterá-los.
- [ ] **P0.6** — Preparar implantação em diretório temporário exclusivo e limpeza por sessão.

Concluído quando: binário inicia no emulador sob `shell`, informa ambiente e encerra
sem APK/DEX ou processo ART próprio. Build host executa testes dos módulos puros.

**Evidências:** pendentes.

**Impedimentos técnicos:** nenhum registrado; validar as hipóteses do plano.


<a id="p1"></a>

### P1 — Prova de Binder e hierarquia nativa

Issue pai: [#2](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/2). Subissues: [P1.1 · #18](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/18), [P1.2 · #19](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/19), [P1.3 · #20](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/20), [P1.4 · #21](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/21), [P1.5 · #22](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/22), [P1.6 · #23](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/23), [P1.7 · #24](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/24).

**Estado:** Backlog  
**Dependências:** P0. Prova independente de P2.

Esta etapa decide a viabilidade da parte mais incerta antes de construir todo o agente.

- [ ] **P1.1** — Identificar símbolos exportados de libbinder_ndk e acesso ao ServiceManager
  no alvo API 34; decidir bindings Rust reutilizáveis versus FFI C mínima.
- [ ] **P1.2** — Fixar revisão AOSP correspondente ao alvo; listar interfaces, transações,
  flags, assinaturas, permissões e formatos de parcel utilizados.
- [ ] **P1.3** — Implementar pool Binder, referências locais/remotas e callbacks mínimos de
  `IAccessibilityServiceClient` necessários para registro e inicialização.
- [ ] **P1.4** — Codificar configuração mínima de `AccessibilityServiceInfo`; registrar e
  receber `IAccessibilityServiceConnection` e identificador da conexão.
- [ ] **P1.5** — Implementar consulta de um nó e callback de resultado, correlacionando IDs.
- [ ] **P1.6** — Decodificar campos mínimos de `AccessibilityNodeInfo`, incluindo texto,
  bounds, identificadores e propriedades booleanas, com limites de leitura.
- [ ] **P1.7** — Liberar registro ao encerrar; detectar sessão concorrente e recusar conflito.

Concluído quando: obter um nó real da fixture e observar mudança de texto sob
`shell`, sem Java, root ou alteração de SELinux. Salvar evidência e limitações.
Se falhar, documentar chamada, permissão ou formato responsável; infraestrutura
e encoder podem avançar, mas hierarquia permanece indisponível explicitamente.

**Evidências:** pendentes.

**Impedimentos técnicos:** nenhum registrado; validar as hipóteses do plano.


<a id="p2"></a>

### P2 — Prova de captura e encoder nativos

Issue pai: [#3](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/3). Subissues: [P2.1 · #25](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/25), [P2.2 · #26](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/26), [P2.3 · #27](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/27), [P2.4 · #28](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/28), [P2.5 · #29](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/29).

**Estado:** Backlog  
**Dependências:** P0. Prova independente de P1.

Pode avançar independentemente de P1, depois de P0.

- [ ] **P2.1** — Criar encoder H.264 por AMediaCodec, configurar Surface e produzir primeiro
  vídeo sintético por EGL/GLES nativos. Drenar saída sem capturar tela ainda.
- [ ] **P2.2** — Identificar display real e testar rota Binder de espelhamento permitida a
  `shell`: contratos de display/SurfaceFlinger e respectivos callbacks/tokens.
- [ ] **P2.3** — Provar ligação da saída do display à Surface do encoder. Investigar a passagem
  do produtor gráfico/Surface pelo Binder; `ANativeWindow` é opaco, não fazer casts
  para estruturas C++ privadas nem presumir conversão pública para Binder.
- [ ] **P2.4** — Implementar apenas a rota demonstrada para API 34 e registrar dependências.
- [ ] **P2.5** — Tratar permissão negada, display ausente e formato incompatível separadamente.

Concluído quando: H.264 da tela real é recebido no host e decodificado, com conteúdo
da fixture reconhecível e recursos liberados. Encoder sintético funcionando não
conta como captura concluída. MediaProjection não é fallback deste plano nativo.

**Evidências:** pendentes.

**Impedimentos técnicos:** nenhum registrado; validar as hipóteses do plano.


<a id="p3"></a>

### P3 — Protocolo e transporte v1

Issue pai: [#4](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/4). Subissues: [P3.1 · #30](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/30), [P3.2 · #31](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/31), [P3.3 · #32](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/32), [P3.4 · #33](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/33), [P3.5 · #34](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/34), [P3.6 · #35](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/35).

**Estado:** Backlog  
**Dependências:** P0; consolidar contrato após evidências P1/P2.

- [ ] **P3.1** — Dois canais independentes sobre ADB: controle/hierarquia e vídeo. Sem servidor
  HTTP, WebSocket ou acesso direto USB nesta primeira implementação.
- [ ] **P3.2** — Preferir Unix socket em diretório de sessão com acesso restrito; validar
  exposição via ADB. Usar identificador de sessão e validar credenciais do peer.
- [ ] **P3.3** — Definir handshake: versão do protocolo, versão do binário, API/ABI e capacidades.
- [ ] **P3.4** — Cabeçalho binário com magic, versão, tipo, request ID e tamanho; ordem de bytes
  documentada. Controle JSON; XML e vídeo como payloads binários, sem Base64.
- [ ] **P3.5** — Validar comprimento antes de alocar; leitura fragmentada, EOF, versões
  incompatíveis e mensagens desconhecidas devem ter respostas definidas.
- [ ] **P3.6** — Incluir vinculação dos dois canais à mesma sessão e timeout para canal ausente.

Comandos iniciais: `hello`, `capabilities`, `ping`, `hierarchy.dump`, `display.info`,
`video.start`, `video.stop`, `stats` e `shutdown`. São propostas de wire protocol,
não APIs públicas já implementadas do Geppetto.

Concluído quando: cliente de teste negocia sessão, recebe respostas correlacionadas,
rejeita framing inválido e continua recebendo controle com vídeo ativo.

**Evidências:** pendentes.

**Impedimentos técnicos:** nenhum registrado; validar as hipóteses do plano.


<a id="p4"></a>

### P4 — Hierarquia completa e contrato XML

Issue pai: [#5](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/5). Subissues: [P4.1 · #36](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/36), [P4.2 · #37](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/37), [P4.3 · #38](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/38), [P4.4 · #39](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/39), [P4.5 · #40](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/40), [P4.6 · #41](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/41), [P4.7 · #42](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/42), [P4.8 · #43](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/43).

**Estado:** Backlog  
**Dependências:** P1 e P3.

- [ ] **P4.1** — Enumerar janelas e árvore ativa; preservar hierarquia e identificar display.
- [ ] **P4.2** — Percorrer filhos e deduplicar por identidade de nó/janela dentro do snapshot.
- [ ] **P4.3** — Entregar inicialmente os 16 atributos do benchmark: text, resource-id, class,
  package, content-desc, bounds, enabled, clickable, focusable, focused, selected,
  scrollable, checkable, checked, long-clickable e password.
- [ ] **P4.4** — Preservar Unicode, escapar XML e manter nós estruturais sem texto.
- [ ] **P4.5** — Informar snapshot ID, tempos monotônicos de início/fim, display ID, dimensões,
  rotação e geração da geometria. IDs de nós não são seletores persistentes.
- [ ] **P4.6** — Evitar cache na primeira versão; receber invalidações e impedir resposta antiga
  de satisfazer requisição nova. Adicionar cache apenas com teste de invalidação.
- [ ] **P4.7** — Tratar janela removida, callback tardio e árvore mudando durante aquisição.
  Fazer retry limitado ou marcar snapshot inconsistente; não prometer atomicidade.
- [ ] **P4.8** — Declarar árvores vazias e truncamento; nunca devolver XML parcial como sucesso.

Concluído quando: paridade dos 51 alvos e atributos da fixture contra referência
Java, atualização após toque e cenários de teclado, diálogo e troca de janela.
Canvas/WebView/Compose dependem da árvore de acessibilidade exposta pelo app;
Carlo não inventará nós ausentes.

**Evidências:** pendentes.

**Impedimentos técnicos:** nenhum registrado; validar as hipóteses do plano.


<a id="p5"></a>

### P5 — Streaming H.264 utilizável

Issue pai: [#6](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/6). Subissues: [P5.1 · #44](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/44), [P5.2 · #45](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/45), [P5.3 · #46](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/46), [P5.4 · #47](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/47), [P5.5 · #48](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/48), [P5.6 · #49](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/49), [P5.7 · #50](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/50).

**Estado:** Backlog  
**Dependências:** P2 e P3.

- [ ] **P5.1** — `video.start` aceita display, tamanho máximo, fps, bitrate e preferência de
  encoder; resposta informa configuração realmente aceita e codec selecionado.
- [ ] **P5.2** — Reportar hardware/software quando houver informação confiável; caso contrário,
  usar `unknown`. Degradação de qualidade ou troca de encoder deve ser explícita.
- [ ] **P5.3** — Enviar configuração SPS/PPS, dimensões, stream generation, PTS, flags de codec
  e keyframe em formato documentado.
- [ ] **P5.4** — Preservar buffers de saída até concluir cópia/escrita; liberar todos os índices
  do codec inclusive em erros. Evitar caminho por pixels na CPU quando a rota Surface
  direta estiver demonstrada; não prometer zero-copy integral.
- [ ] **P5.5** — Consumidor lento deve atingir limite de fila/deadline e interromper somente
  vídeo. Não descartar NALs arbitrários: podem ser referência para frames seguintes.
- [ ] **P5.6** — Nova sessão/reconfiguração envia configuração e keyframe para permitir decode.
- [ ] **P5.7** — Parar vídeo mantém hierarquia disponível e libera display virtual, Surface e codec.

Concluído quando: host decodifica stream completo, inicia/paralisa repetidamente e
continua consultando hierarquia. Primeiro preset de comparação: 360×640, 15 fps,
1,5 Mbit/s; resultados não equivalem a capacidade máxima do dispositivo.

**Evidências:** pendentes.

**Impedimentos técnicos:** nenhum registrado; validar as hipóteses do plano.


<a id="p6"></a>

### P6 — Rotação e mudanças de display

Issue pai: [#7](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/7). Subissues: [P6.1 · #51](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/51), [P6.2 · #52](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/52), [P6.3 · #53](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/53), [P6.4 · #54](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/54), [P6.5 · #55](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/55).

**Estado:** Backlog  
**Dependências:** P4 e P5.

- [ ] **P6.1** — Começar pelo display principal; depois seleção de outro display existente.
- [ ] **P6.2** — Detectar rotação, tamanho, tela desligada, display removido e mudança de modo.
- [ ] **P6.3** — Reiniciar captura/encoder de forma ordenada quando necessário, emitindo evento
  com nova geração de geometria, dimensões e configuração.
- [ ] **P6.4** — Associar snapshots e pacotes à geração correspondente; expor timestamps sem
  afirmar sincronização atômica entre árvore e imagem.
- [ ] **P6.5** — Definir estado suspenso/erro para conteúdo indisponível ou protegido, sem
  tentar contornar restrições de captura.

Concluído quando: rotação durante streaming mantém vídeo decodificável, bounds
coerentes e sinalização suficiente para o host descartar geometria antiga.

**Evidências:** pendentes.

**Impedimentos técnicos:** nenhum registrado; validar as hipóteses do plano.


<a id="p7"></a>

### P7 — Recuperação e limites de recursos

Issue pai: [#8](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/8). Subissues: [P7.1 · #56](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/56), [P7.2 · #57](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/57), [P7.3 · #58](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/58), [P7.4 · #59](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/59), [P7.5 · #60](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/60), [P7.6 · #61](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/61).

**Estado:** Backlog  
**Dependências:** Transversal: começar proteção e cleanup em P0 e completar conforme recursos entram.

- [ ] **P7.1** — Estados explícitos para sessão e vídeo; start/stop/shutdown idempotentes.
- [ ] **P7.2** — Tratar EOF, perda de ADB, cliente ausente, SIGTERM/SIGINT, morte de serviço
  Binder e falha/revogação de codec, com erro tipado e retry limitado.
- [ ] **P7.3** — Ownership explícito de cada FD, referência Binder, listener, Surface e codec.
- [ ] **P7.4** — Deadline monotônico para operações e encerramento; chamadas Binder síncronas
  não são necessariamente canceláveis. Não criar workers ilimitados para abandoná-las.
  Host pode encerrar o processo após período de graça se uma chamada ficar presa.
- [ ] **P7.5** — Impedir sessões conflitantes por dispositivo e separar morte de vídeo de
  morte do canal de controle. Reconexão começa sessão nova na primeira versão.
- [ ] **P7.6** — Estatísticas de bytes, frames, filas, timeouts e duração de aquisição; logs
  não incluem texto da tela, XML completo ou conteúdo de campos protegidos.

Limites iniciais propostos, a confirmar com testes: comando 64 KiB, XML 4 MiB,
10.000 nós, profundidade 128, pacote de vídeo 8 MiB, uma aquisição de hierarquia
em voo e fila de vídeo limitada por bytes. Tamanhos de imagem, contagem de nós e
aritmética de offsets também precisam de validação antes de alocar.

Concluído quando: ciclos de abrir/fechar e falhas injetadas não acumulam FDs,
threads ou registros de automação, e agente encerra sem deixar sessão ativa.
SIGKILL não executa Drop: validar também limpeza feita pelo sistema após morte.

**Evidências:** pendentes.

**Impedimentos técnicos:** nenhum registrado; validar as hipóteses do plano.


<a id="p8"></a>

### P8 — Perfis de compatibilidade e distribuição

Issue pai: [#9](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/9). Subissues: [P8.1 · #62](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/62), [P8.2 · #63](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/63), [P8.3 · #64](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/64), [P8.4 · #65](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/65), [P8.5 · #66](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/66).

**Estado:** Backlog  
**Dependências:** Recursos nativos comprovados, recuperação e testes dos perfis anunciados.

- [ ] **P8.1** — Adicionar API 35 e 36 após API 34; avaliar API 29–33 posteriormente.
- [ ] **P8.2** — Separar adapters por contrato comprovado, com revisão AOSP, transações e
  layouts de parcel registrados. SDK_INT sozinho não prova compatibilidade OEM.
- [ ] **P8.3** — `doctor` e handshake anunciam recursos disponíveis por módulo, com razão
  para indisponibilidade. Versão desconhecida não recebe adapter por tentativa cega.
- [ ] **P8.4** — Validar arm64 e páginas de memória de 16 KiB; registrar dependências ELF e
  símbolos necessários, incluindo bibliotecas de plataforma não públicas.
- [ ] **P8.5** — Distribuir binários por ABI/perfil com checksums e manifesto de build.

Concluído quando: cada combinação anunciada tem teste executado e demais
combinações falham com diagnóstico legível, sem crash nem mudança de arquitetura.

**Evidências:** pendentes.

**Impedimentos técnicos:** nenhum registrado; validar as hipóteses do plano.


<a id="p9"></a>

### P9 — Cliente host e integração opt-in com Geppetto

Issue pai: [#10](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/10). Subissues: [P9.1 · #67](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/67), [P9.2 · #68](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/68), [P9.3 · #69](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/69), [P9.4 · #70](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/70), [P9.5 · #71](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/71), [P9.6 · #72](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/72), [P9.7 · #73](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/73), [P9.8 · #74](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/74).

**Estado:** Backlog  
**Dependências:** P3, P4, P5 e encerramento confiável; preservar backend atual.

- [ ] **P9.1** — Criar cliente mínimo para deploy, handshake, dump XML, recepção H.264 e teardown.
- [ ] **P9.2** — Verificar checksum, versão, diretório exclusivo, quote de comandos e limpeza.
- [ ] **P9.3** — Definir backend explicitamente selecionável; preservar uiautomator2 existente.
- [ ] **P9.4** — Integrar dump com WindowDump e vídeo com consumidor/recorder no host.
- [ ] **P9.5** — Mapear dependências atuais de `_u2`: construtor, window_size e esperas também
  usam uiautomator2. Um modo Carlo não deve instalar/iniciar esse agente sem necessidade.
- [ ] **P9.6** — Resolver esperas no host usando snapshots de Carlo, preservando semântica
  pública; toque/texto/comandos permanecem no transporte ADB existente.
- [ ] **P9.7** — Respeitar camadas Geppetto: execução de processos somente em `core/`, erros
  próprios, tipos estritos e fixtures unitárias sem dispositivo.
- [ ] **P9.8** — Documentar instalação e uso; integrar inspetor/artefatos sem falha de captura
  alterar resultado do teste. Migração padrão é decisão posterior aos benchmarks.

Concluído quando: Geppetto obtém hierarquia e vídeo pelo backend escolhido, sem
regressão no backend atual; cliente fecha sessão ao terminar ou falhar.

**Evidências:** pendentes.

**Impedimentos técnicos:** nenhum registrado; validar as hipóteses do plano.


<a id="p10"></a>

### P10 — Testes, benchmarks e polimento incremental

Issue pai: [#11](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/11). Subissues: [P10.1 · #75](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/75), [P10.2 · #76](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/76), [P10.3 · #77](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/77), [P10.4 · #78](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/78), [P10.5 · #79](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/79), [P10.6 · #80](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/80), [P10.7 · #81](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/81), [P10.8 · #82](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/82).

**Estado:** Backlog  
**Dependências:** Transversal: testes desde P0; benchmarks completos após hierarquia e vídeo.

- [ ] **P10.1** — Unitários de framing, limites, XML, estado, erros e codecs de parcel com fixtures
  sintéticas ou de app controlado. Testes de propriedades/fuzzing nos parsers.
- [ ] **P10.2** — Miri para wrappers/lógica executáveis no host com FFI simulada; testes Android
  são necessários para Binder/NDK reais e não são substituídos por Miri.
- [ ] **P10.3** — Integração em emulador: árvore fresca, múltiplas janelas, rotação, desconexão,
  consumidor lento, perda de serviço e ciclos start/stop.
- [ ] **P10.4** — Rodar fmt, Clippy, testes host e cross-build dos alvos suportados no CI.
- [ ] **P10.5** — Comparar Carlo nativo, agente Java mínimo e uiautomator2 na mesma tela,
  dispositivo, carga e transporte, com ordem alternada e aquecimento explícito.
- [ ] **P10.6** — Medir p50/p95/p99, CPU, PSS/RSS, FDs, threads, bytes, fps, configuração do codec
  e latência de vídeo com marcador visual. Reportar serviços compartilhados à parte.
- [ ] **P10.7** — Repetir sob carga contínua e amostrada, com/sem vídeo; sessões de 1, 10 e
  60 minutos. Medir atraso de consumidor e estabilidade após aquecimento.
- [ ] **P10.8** — Polir um gargalo por vez: transações por dump, prefetch, cópias, alocações,
  formato de saída, fila de vídeo e ajustes de encoder; medir antes/depois.

Concluído quando: funcionalidades e limites têm evidência reproduzível, sem
prometer memória menor ou velocidade maior apenas por usar Rust.

**Evidências:** pendentes.

**Impedimentos técnicos:** nenhum registrado; validar as hipóteses do plano.
