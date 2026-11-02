# P1.1 — exports Binder e acesso ao ServiceManager

Issue [#18](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/18), parte de
[P1/#2](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/2).
Execução: 2026-10-31 em America/Manaus (2026-11-01 UTC).

**Resultado:** FFI C mínima viável para lookup/ping no alvo testado. O binário
Carlo abriu `libbinder_ndk.so`, resolveu quatro exports, obteve o Binder remoto
`accessibility` e recebeu ping `0`. O nome `carlo.probe.absent` retornou null.
Hierarquia permanece indisponível: registro, callbacks e leitura de nós ainda
não foram implementados nem demonstrados por esta prova.

## Alvo e evidências

| Item | Observado |
|---|---|
| Runtime executado | Android 14/API 34, x86_64, Google APIs, emulador descartável |
| Fingerprint | `google/sdk_gphone64_x86_64/emu64xa:14/UE1A.230829.050/12077443:userdebug/dev-keys` |
| Identidade/contexto | UID 2000, `u:r:shell:s0`; SELinux `Enforcing` |
| Toolchains | Rust 1.96.0; NDK 28.2.13676358/r28c; piso de build API 29 |
| Exports da biblioteca real | 145, com versões ELF preservadas no inventário |
| Exports do stub público NDK API 34 | 107; 38 nomes adicionais no sistema |
| Probe | Exit 0, stderr vazio, quatro símbolos presentes, ping 0 |
| Teardown | Diretório/processos da sessão removidos pelo helper |
| arm64 | Cross-build/ELF validado; execução não realizada |

Artefatos textuais versionados:

- [Relatório estruturado do probe](p1-1-probe.json).
- [Sessão ADB completa e confirmação de cleanup](p1-1-probe-session.json).
- [Inventário de todos os exports, versões, hashes e fingerprint](p1-1-binder-symbols.json).
- [Manifesto dos dois builds e hashes](p1-1-build-manifest.json).
- [Revisão, URLs e hashes das fontes consultadas](p1-1-aosp-sources.json).

A biblioteca foi lida de `/system/lib64/libbinder_ndk.so`, sem instalar APK,
DEX, código Java ou alterar permissões/configuração de acessibilidade. O emulador
foi iniciado com GPU host e Vulkan desativado, pois SwiftShader havia falhado
neste host. O helper implantou apenas o executável em diretório privado próprio.

## Contratos usados e fonte fixada

Referência de contratos: `frameworks/native`, tag `android-14.0.0_r1`, commit
`bfcf75076e562945bae131a4929f29a90d0d2481`.
Essa revisão é referência Android 14; **não há comprovação de que seja a revisão
exata usada para compilar a imagem Google APIs UE1A**. O inventário e a execução
comprovam somente a biblioteca/fingerprint registrados acima.

| Símbolo C | Assinatura usada | Ownership/resultado |
|---|---|---|
| `AServiceManager_checkService` | `AIBinder* (const char*)` | Null ou uma referência forte transferida ao chamador |
| `AIBinder_decStrong` | `void (AIBinder*)` | Libera exatamente a referência recebida |
| `AIBinder_isRemote` | `bool (const AIBinder*)` | Consulta sem transferir ownership |
| `AIBinder_ping` | `binder_status_t (AIBinder*)` | Status signed 32-bit; 0 significa sucesso |

As assinaturas, o typedef de status e a transferência de referência vêm de
[`binder_manager.h`](https://android.googlesource.com/platform/frameworks/native/+/bfcf75076e562945bae131a4929f29a90d0d2481/libs/binder/ndk/include_platform/android/binder_manager.h),
[`binder_ibinder.h`](https://android.googlesource.com/platform/frameworks/native/+/bfcf75076e562945bae131a4929f29a90d0d2481/libs/binder/ndk/include_ndk/android/binder_ibinder.h) e
[`service_manager.cpp`](https://android.googlesource.com/platform/frameworks/native/+/bfcf75076e562945bae131a4929f29a90d0d2481/libs/binder/ndk/service_manager.cpp) e
[`binder_status.h`](https://android.googlesource.com/platform/frameworks/native/+/bfcf75076e562945bae131a4929f29a90d0d2481/libs/binder/ndk/include_ndk/android/binder_status.h).

`checkService` está em `include_platform`, marcado `systemapi llndk` no
[mapa de exports](https://android.googlesource.com/platform/frameworks/native/+/bfcf75076e562945bae131a4929f29a90d0d2481/libs/binder/ndk/libbinder_ndk.map.txt).
O símbolo existe no runtime observado, com versão `LIBBINDER_NDK`, mas está ausente
do stub público do NDK. Os outros três existem no sistema e no stub. Não usar casts
para estruturas C++ privadas; `AIBinder` permanece opaco.

Também foram encontrados exports de pool (`ABinderProcess_*`), criação de Binder,
transações e parcels. Isso apenas inventaria pré-requisitos das próximas tarefas;
ainda não prova a execução dessas funções nem os contratos de acessibilidade.

## Decisão: FFI C mínima nesta etapa

O [backend Rust oficial](https://source.android.com/docs/core/architecture/aidl/aidl-backends)
se apoia em `libbinder_ndk`; gestão de serviços inclui APIs de plataforma. Na
revisão fixada, [`libbinder_rs`](https://android.googlesource.com/platform/frameworks/native/+/bfcf75076e562945bae131a4929f29a90d0d2481/libs/binder/rust/Android.bp)
é um módulo Soong com `libbinder_ndk_sys`, bindings gerados, `libutils`, `libc` e
`downcast-rs`. O diretório consultado não fornece um Cargo.toml para uso direto
no build Cargo/NDK independente de Carlo. Adaptá-lo exigiria trabalho de integração
que quatro assinaturas não justificam agora. Isso não é medição de memória.

Escolha: quatro funções C resolvidas por `dlopen`/`dlsym`, sem dependências Cargo
novas. Uma tabela privada possui a biblioteca; cada referência forte emprestada
dessa tabela chama `decStrong` em Drop, antes de `dlclose`. Os handles não são
Send/Sync; toda a prova roda na thread chamadora. Biblioteca ou símbolo ausente
produz resultado indisponível antes de qualquer lookup.

Reavaliar reaproveitamento dos wrappers Rust oficiais durante P1.3 se o custo de
manter callbacks/parcels manualmente superar a adaptação. A escolha atual não torna
AIDL Java nem parcelables privados automaticamente compatíveis com Rust.

## Limites e falhas

- Probe restrito ao API 34, ABI inicial e UID 2000. Host, root e outras APIs recebem
  `unsupported_target`/exit 69 antes de carregar Binder.
- Null em `checkService` não separa ausência, acesso negado e falha de comunicação.
  `service_unavailable` conserva essa ambiguidade.
- Controle encontrado ou ping não zero falham explicitamente; sucesso nunca altera
  `hierarchy_available=false` nem capacidades anunciadas pelo `doctor`.
- Não se usa `getService` com polling nem `waitForService`. Mesmo `checkService`
  e ping são chamadas síncronas, sem garantia interna de deadline. O helper host
  limita execução e termina o processo caso trave; não cria workers abandonados.
- Sem pool, callbacks, registro de automação, transações AIDL privadas ou codecs
  de parcel nesta tarefa. Entrada inválida de parcel/callback tardio pertence às
  próximas provas, e não tem cobertura alegada aqui.
- Não anuncia suporte a outro OEM, APIs 29–33/35+, runtime arm64 ou páginas 16 KiB.
  Alinhamento ELF 16 KiB foi checado nos builds; runtime com essas páginas não.

## Reprodução e checks

Com emulador API 34 descartável já iniciado e SDK/NDK instalados:

```sh
export ANDROID_NDK_HOME=/path/to/android-sdk/ndk/28.2.13676358
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
python3 -m unittest discover -s tools -p 'test_*.py' -v
python3 tools/build_android.py
python3 tools/android_session.py --serial emulator-5554 --command binder-probe \
  --timeout 15 --report results/p1-1-probe-session.json
python3 tools/binder_inventory.py --serial emulator-5554 \
  --output results/p1-1-binder-symbols.json
python3 tools/smoke_android.py --serial emulator-5554 --manifest dist/manifest.json \
  --output results/p0-regression
```

Validados: 14 testes Rust e 17 testes Python, incluindo referência liberada uma
vez em sucesso/erro de ping, null sem release, biblioteca/símbolos ausentes,
restrição de alvo, parsing de exports e recusa de aparelho físico. fmt, Clippy
(host e ambas as ABIs Android),
cross-build das duas ABIs e regressão P0 passaram. Emulador executou o binário
x86_64 de release; `doctor`/`version` retornaram 0, `serve` retornou 69, todos
com cleanup confirmado. Contagem real de memória/referências do sistema não
foi medida: ownership é validado pelos wrappers/testes e término da sessão.
