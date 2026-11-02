# P1 — registro e ciclo de vida nativos

Entrega: #19 (contratos), #20 (pool/callbacks), #21 (registro/init) e #24
(teardown/conflito). O [plano](../plans/p1-accessibility-session.md) foi aplicado na
branch `codex/feat/accessibility-session-19`. Aceite/fechamento dependem do PR;
a issue pai #2 permanece aberta para consulta de nó e mudança observável.

## Resultado demonstrado

Carlo registra um cliente `IAccessibilityServiceClient` nativo como `shell`, recebe
`init` com `IAccessibilityServiceConnection` e ID válido, preserva essa referência
durante a sessão e executa unregister. Nenhum APK, DEX, JNI, root ou mudança de
SELinux. O processo mantido durante o smoke não mapeou `libart.so`/`libartd.so`.
Hierarquia continua indisponível: nenhum nó foi consultado nesta entrega.

| Prova | Resultado |
|---|---|
| Runtime executado | Emulador Google APIs Android 14/API 34, x86_64, usuário 0 |
| Identidade | UID 2000; contexto `u:r:shell:s0`; SELinux Enforcing |
| Registro/init/unregister | Sucesso, callback único com ID não negativo |
| Ciclos no mesmo processo | 20 após aquecimento; 7 FDs e 4 threads em todas as amostras do harness |
| Concorrência | Segunda sessão recusada; dono continuou ativo e fez seu próprio unregister |
| Close repetido | Idempotente; segundo close não faz transação |
| SIGINT/SIGTERM | Interrupção observada, unregister explícito e novo registro após saída |
| SIGKILL | Sistema removeu registro por morte Binder; nova sessão funcionou |
| Timeout do host | Exit 124, diretório/processos da sessão removidos, registro posterior funcionou |
| Parcels inválidos | Truncado, conexão null, ID negativo, dados extras e payload acima de 64 KiB rejeitados |
| Callback tardio/duplicado | Estado fechado não aceita nova conexão; referências descartadas sem sobrescrever estado |
| ARM64 | Cross-build/Clippy; execução e contrato privado não anunciados |

O total 7 FDs/4 threads inclui o harness Rust e a enumeração procfs; não representa
medição de RSS/PSS nem orçamento definitivo do agente de release. É evidência de
estabilidade nesses 20 ciclos, não teste prolongado de vazamento de memória.

## Contratos e perfil

O [manifesto de compatibilidade](../compat/api34-accessibility.json) lista descritores,
transações/flags, assinaturas dos 24 bindings Binder de produção e dos callbacks C, permissão, configuração,
layout de ServiceInfo, dois bindings exclusivos de teste, fontes e hashes. Revisões de referência:

- `frameworks/base`: `299fe6f5d6fc6f1af7c3411dcf4e5efdf7217368`.
- `frameworks/native`: `bfcf75076e562945bae131a4929f29a90d0d2481`.
- Tag comum: `android-14.0.0_r1`; Rust 1.96.0 e NDK 28.2.13676358.

Fingerprint executado e permitido:
`google/sdk_gphone64_x86_64/emu64xa:14/UE1A.230829.050/12077443:userdebug/dev-keys`.
A revisão AOSP é referência do contrato validado empiricamente, **não comprovação
da revisão exata usada na imagem Google**. API 34 por si só não habilita a rota;
fingerprint, ABI x86_64 e UID shell também precisam coincidir. Root é recusado.

| Interface | Método/código | Request/reply e autoridade |
|---|---|---|
| `android.view.accessibility.IAccessibilityManager` | `registerUiTestAutomationService`, 10, síncrona/flags 0 | owner Binder, client Binder, objeto ServiceInfo presente, userId 0, flags 1; reply com status Java; requer RETRIEVE_WINDOW_CONTENT |
| mesma | `unregisterUiTestAutomationService`, 11, síncrona/flags 0 | Mesmo client Binder; servidor confere identidade do registro; reply com status Java |
| `android.accessibilityservice.IAccessibilityServiceClient` | `init`, 1, oneway | Binder connection, int connectionId, Binder windowToken nullable |
| mesma | `onInterrupt`, 3; `clearAccessibilityCache`, 5 | Sem argumentos; restante do parcel precisa estar vazio |

As fontes primárias são
[`IAccessibilityManager.aidl`](https://android.googlesource.com/platform/frameworks/base/+/299fe6f5d6fc6f1af7c3411dcf4e5efdf7217368/core/java/android/view/accessibility/IAccessibilityManager.aidl),
[`IAccessibilityServiceClient.aidl`](https://android.googlesource.com/platform/frameworks/base/+/299fe6f5d6fc6f1af7c3411dcf4e5efdf7217368/core/java/android/accessibilityservice/IAccessibilityServiceClient.aidl),
[`AccessibilityServiceInfo.java`](https://android.googlesource.com/platform/frameworks/base/+/299fe6f5d6fc6f1af7c3411dcf4e5efdf7217368/core/java/android/accessibilityservice/AccessibilityServiceInfo.java) e
[`UiAutomationManager.java`](https://android.googlesource.com/platform/frameworks/base/+/299fe6f5d6fc6f1af7c3411dcf4e5efdf7217368/services/accessibility/java/com/android/server/accessibility/UiAutomationManager.java).

NDK escreve/verifica o interface token; Carlo não monta esse header privado à mão.
ServiceInfo tem 22 campos (92 bytes de dados neste preset, além do prefixo typed-object).
Parcelables Java null usam String de nome de classe null, sem presença typed-object.
Configuração: eventTypes 0 (não recebe conteúdo de eventos), feedback GENERIC,
flags INCLUDE_NOT_IMPORTANT_VIEWS | REPORT_VIEW_IDS, capability de recuperar
conteúdo e accessibilityTool true. Flags de registro incluem
DONT_SUPPRESS_ACCESSIBILITY_SERVICES: outras acessibilidades não são suprimidas.
A exclusividade de UiAutomation ainda se aplica.

## Ownership, threads e falhas

A tabela de funções, as três classes Binder e o pool são recursos únicos do
processo. O executável configura o máximo de um worker iniciado pelo kernel,
além do worker de startThreadPool; main não se junta ao pool. Uma instância já
configurada por código externo é recusada. Classes/tabela permanecem vivas para
impedir dispatch em código descarregado; não são alocadas a cada sessão.

Cada local Binder possui um Box de Arc do estado; onDestroy libera esse ownership.
O estado tem Mutex/Condvar e um único slot de conexão, sem fila crescente. Somente
UID 1000 é aceito nos callbacks. Parsers validam status, tamanho até 64 KiB, ID,
conexão não null e consumo integral; unwind não atravessa os callbacks C.
Referências recebidas são RAII, inclusive no caminho inválido e após fechamento.

Antes de unregister, o estado passa para closing e solta a conexão. Callback tardio
é descartado. Erro conhecido de conflito não solicita unregister; falha de transporte
ambígua solicita cleanup usando apenas o próprio client Binder. Esse client não
pode remover um registro com outra identidade. Erros registram código/operação fixa,
sem mensagens remotas arbitrárias nem texto da UI.

SIGINT/SIGTERM só alteram AtomicBool no handler; thread chamadora faz cleanup e
restaura handlers. Init usa deadline monotônico de 5 s; hold aceita 0..10000 ms.
Transações Binder síncronas não são canceláveis por essas esperas. O helper host
força término se houver travamento. SIGKILL depende da limpeza de morte Binder do
Android, comprovada pelo registro seguinte.

## Reprodução e evidências

Com emulador descartável do fingerprint indicado já iniciado:

```sh
export ANDROID_NDK_HOME=/path/to/android-sdk/ndk/28.2.13676358
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo clippy --locked --all-targets --target x86_64-linux-android -- -D warnings
cargo clippy --locked --all-targets --target aarch64-linux-android -- -D warnings
cargo test --locked
python3 -m unittest discover -s tools -p 'test_*.py' -v
python3 tools/build_android.py
python3 tools/build_accessibility_tests.py
python3 tools/smoke_accessibility.py --serial emulator-5554 --output results/p1-session
python3 tools/smoke_android.py --serial emulator-5554 --manifest dist/manifest.json \
  --output results/p0-regression
python3 tools/android_session.py --serial emulator-5554 --command binder-probe
```

O smoke recusa aparelho físico e registro de automação já existente. Lê apenas
presença de UiAutomation no dumpsys, sem salvar seu conteúdo; sinais só atingem PID
com executável confirmado no diretório criado pela própria sessão.

[Evidências do smoke](p1-session/summary.json),
[testes Android e amostras de recursos](p1-session/native-tests.json),
[conflito](p1-session/conflict.json),
[SIGTERM](p1-session/term.json), [SIGINT](p1-session/int.json),
[SIGKILL/recuperação](p1-session/kill-recovery.json),
[manifesto de release](p1-session/build-manifest.json) e
[manifesto do harness](p1-session/test-build-manifest.json).
Foram executados 20 testes Rust host, 21 Python e 15 testes Rust de biblioteca no
Android; parte dos testes puros repete entre host e Android. fmt, Clippy nas três
plataformas e builds das duas ABIs passaram. P0 e lookup/ping P1.1 mantiveram resultado.

Próximo passo funcional: #22 + #23, consulta correlacionada e decoder de nó.
Captura/MediaCodec pertencem à trilha P2. Nenhum desses recursos está implementado
por este bootstrap, e não há promessa de compatibilidade OEM ou arm64 em runtime.
