# Carlo

*Puppet of Geppetto.*

Agente Android nativo em Rust para fornecer **hierarquia de interface** e
**streaming de vídeo** ao [Geppetto](https://github.com/desodre/Geppetto).
Arquitetura escolhida: Binder + NDK, executando como usuário `shell` por meio do ADB.

**Estado atual:** fundação nativa, lookup Binder e bootstrap de sessão de
acessibilidade validados no emulador de referência API 34 x86_64. Registro, callback
`init` e unregister funcionam sem Java. Hierarquia, captura e protocolo ainda
não estão implementados. Versão de desenvolvimento:
`0.1.0`, sem release publicada do agente completo.

## Propósito

Carlo concentra a aquisição de dados do dispositivo: árvores de acessibilidade,
informações de display e vídeo H.264. O host recebe esses dados para inspeção,
automação e coleta de evidências.

| Carlo | ADB e Geppetto |
|---|---|
| Hierarquia XML e metadados do snapshot | Toque, entrada de texto e comandos de sistema |
| Captura de display e encoder H.264 | Implantação e início do agente |
| Canais de controle e vídeo | Gerenciamento da conexão com o dispositivo |
| Capacidades, diagnóstico e encerramento da sessão | Seletores, esperas, asserts e execução de testes |
| Limites de buffers e recursos do agente | Decodificação, visualização e gravação no host |

A meta é um processo nativo próprio, sem APK, DEX ou ponte Java. ADB permanece no
transporte. O acesso aos serviços privados do Android precisa ser provado por versão;
a escolha de Rust não garante, por si, menor memória ou maior desempenho.

## Roadmap

Os estados, checklists e evidências ficam no [board do GitHub](https://github.com/users/desodre/projects/11/views/1).
As 11 etapas são issues pai, com [71 subissues nativas](docs/ISSUES.md).
O [kanban local](KANBAN.md) preserva a referência inicial das tarefas; o
[plano técnico](PLANO.md) registra escopo, decisões, limites e critérios de conclusão.

| Etapa | Entrega | Dependências principais |
|---|---|---|
| [P0](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/1) | Build Android, CLI inicial, diagnóstico e execução nativa | Nenhuma |
| [P1](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/2) | Binder, callbacks e leitura de nó real | P0 |
| [P2](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/3) | Encoder NDK e prova de captura real | P0 |
| [P3](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/4) | Protocolo v1 e canais independentes | P0; consolidar após provas P1/P2 |
| [P4](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/5) | Hierarquia completa, XML e atualização consistente | P1 e P3 |
| [P5](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/6) | Streaming H.264 configurável | P2 e P3 |
| [P6](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/7) | Rotação, geometria e mudanças de display | P4 e P5 |
| [P7](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/8) | Recuperação e limites de recursos | Acompanha implementação desde P0 |
| [P8](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/9) | Compatibilidade Android/ABI e distribuição | Recursos nativos comprovados e recuperação |
| [P9](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/10) | Cliente host e integração opcional com Geppetto | P3, P4, P5 e encerramento confiável |
| [P10](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/11) | Testes, benchmarks e otimização gradual | Acompanha cada etapa desde P0 |

P1 e P2 têm dependências independentes; falha de uma prova não torna a outra
concluída nem exige mudança silenciosa de arquitetura. P7 e P10 são trabalho
contínuo, mesmo aparecendo como cartões próprios no quadro.

## Primeiro marco

Executar um binário nativo no Android 14/API 34 que:

- Inicie como `shell` e informe capacidades do ambiente.
- Leia um nó real e observe atualização após mudança na tela.
- Gere H.264 sintético e comprove captura de imagem real.
- Libere referências Binder, sockets, Surface e codec ao encerrar.

Esse marco valida a rota nativa antes da consolidação do protocolo e da integração
com Geppetto. Vídeo sintético isolado não comprova captura da tela.

## Plataformas planejadas

| Alvo | Papel no desenvolvimento | Estado |
|---|---|---|
| Android 14/API 34, x86_64 | Primeira referência em emulador | Registro/init/unregister validados; hierarquia pendente |
| arm64 em dispositivo físico | Validação em dispositivo físico | A validar após primeira referência |
| APIs 35 e 36 | Próximos perfis de compatibilidade | Planejado |
| APIs 29–33 | Avaliação posterior de viabilidade | Sem suporte anunciado |

Cada perfil deverá registrar revisão AOSP, contratos Binder, dependências e testes
executados. Não há matriz de dispositivos suportados nesta fase.

## Desenvolvimento

Rust **1.96.0** é fixado em `rust-toolchain.toml`; NDK **28.2.13676358 (r28c)**
em `android-build.json`. Ferramentas host requerem Python 3.11+ e ADB.

```sh
cargo run -- --help
cargo run -- --version
cargo run -- doctor --json
```

Doctor no host produz JSON e retorna **69** porque não é um runtime Android.
No emulador, verifica a fundação e informa explicitamente capacidades ainda
indisponíveis. `serve` está reservado para P3 e também retorna 69 nesta versão.

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
python3 -m unittest discover -s tools -p 'test_*.py' -v
export ANDROID_NDK_HOME=/path/to/android-sdk/ndk/28.2.13676358
python3 tools/build_android.py
python3 tools/android_session.py --serial emulator-5554 --manifest dist/manifest.json
```

O build gera dois binários e `dist/manifest.json`, verificando ELF/ABI, alinhamento
16 KiB e hashes. API 29 é piso de compilação candidato; API 34 é alvo inicial de
execução. Alinhamento ELF não equivale a teste em kernel com páginas de 16 KiB.

A sessão recusa aparelho físico por padrão, valida API/ABI/hash e executa em
diretório exclusivo, removido ao terminar. `--allow-physical` exige autorização
prévia para esse aparelho. Os comandos disponíveis são `doctor`, `version`,
`binder-probe`, `accessibility-probe`, `accessibility-hold`, `node-query-probe`, `node-read-probe` e `serve`; use
`--report` para salvar evidência JSON e `--timeout` para prazo limitado.
Smoke test em emulador já iniciado:

```sh
python3 tools/smoke_android.py --serial emulator-5554 \
  --manifest dist/manifest.json --output results/p0-smoke
```

[Evidência de P0](docs/validation/p0-2026-10-31.md) distingue execução x86_64 de
cross-build arm64.

A prova Binder resolve quatro funções C de `libbinder_ndk.so`, consulta
`accessibility`, faz ping e verifica um nome de controle ausente. Executa somente
em Android API 34 sob UID 2000. Usa FFI mínima, sem dependências novas; a API de
ServiceManager é de plataforma e não faz parte do stub público do NDK.

```sh
python3 tools/android_session.py --serial emulator-5554 --command binder-probe \
  --timeout 15 --report results/binder-probe.json
python3 tools/binder_inventory.py --serial emulator-5554 \
  --output results/binder-symbols.json
```

`binder-probe` emite resultado em JSON: retorna 0 para a prova completa de lookup/ping,
69 para alvo, biblioteca, símbolos ou serviços indisponíveis. `hierarchy_available`
continua `false`. Chamadas Binder síncronas dependem do timeout do helper para
encerramento forçado; o comando direto não oferece cancelamento interno.
[Decisão, fontes e evidência P1.1](docs/validation/p1-1-binder.md).

`accessibility-probe` registra uma sessão exclusiva, espera o callback `init` por
até 5 segundos e faz unregister. Opcionalmente mantém a conexão por até 10 segundos:

```sh
cargo run -- accessibility-probe --hold-ms 1000
python3 tools/android_session.py --serial emulator-5554 --command accessibility-probe
```

No host, `cargo run` valida a CLI e retorna indisponível. Para executar a prova,
use o helper ADB no emulador. A chamada privada requer o fingerprint exato da imagem de referência; outro
runtime recebe JSON indisponível/exit 69. Sessão concorrente é recusada. SIGINT e
SIGTERM solicitam cleanup; o host mantém timeout para chamadas Binder presas.
`hierarchy_available` continua `false`: conexão inicializada não significa leitura
de nós. [Plano aplicado](docs/plans/p1-accessibility-session.md),
[contratos e prova da sessão](docs/validation/p1-session.md).

`node-query-probe` consulta e decodifica a raiz da janela ativa; emite apenas
metadados (`fields_decoded=true`). Para receber os campos como dados em stdout:

```sh
python3 tools/android_session.py --serial emulator-5554 --command node-read-probe
```

`node-read-probe` inclui texto, pacote/classe, descrição, IDs, filhos, bounds e
booleanos. Não salva ou registra conteúdo em logs do agente. O helper, quando
invocado diretamente, imprime o JSON como dados do comando; `--report` persiste
esse resultado se solicitado. O smoke da fixture compara dados em memória e salva
apenas verificações sem texto.

Decoder inicial API 34: strings UTF-8/UTF-16, texto simples, limites de parcel e
coleções. Spans não vazios, TouchDelegate, ExtraRendering e referências Binder/FD
retornam `unsupported_node_layout`. Bundles extras têm limites validados, mas seu
conteúdo é omitido (`extras_omitted=true`). Não há promessa de suporte a qualquer UI.

Resultado nulo difere de `query_rejected` (janela/permissão/estado remoto não
distinguíveis). A janela pode ficar disponível após init; a prova informa rejeição
sem retry automático. Callback tem deadline de 5 s; host cobre Binder preso.
Hierarquia completa/XML continuam pendentes. [Plano #23/#78](docs/plans/p1-node-decoder.md)
e [validação](docs/validation/p1-node-decoder.md).

Códigos do agente: 0 sucesso, 64 argumentos inválidos, 69 indisponível, 74 I/O/JSON.
Timeout do helper retorna 124. Detalhes em [CONTRIBUTING.md](CONTRIBUTING.md).

## Acompanhamento

- [Board no GitHub](https://github.com/users/desodre/projects/11/views/1): acompanhamento operacional, tarefas e evidências.
- [Issues](docs/ISSUES.md): 11 pais e 71 subissues nativas, com labels e branches planejadas.
- [CONTRIBUTING.md](CONTRIBUTING.md): ciclo issue → branch → PR → aceite.
- [KANBAN.md](KANBAN.md): referência inicial das tarefas.
- [PLANO.md](PLANO.md): arquitetura e detalhamento técnico P0–P10.

Cada avanço deve registrar comportamento entregue, validação e limitações.
Benchmarks do agente Java anterior servem como referência experimental; desempenho
e viabilidade da implementação nativa ainda precisam ser medidos.
