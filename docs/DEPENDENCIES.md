# Dependências e toolchains

Toolchains fixadas: Rust 1.96.0 e Android NDK 28.2.13676358 (r28c).
Rust: https://www.rust-lang.org/policies/licenses
NDK: https://developer.android.com/ndk/downloads e arquivos NOTICE/LICENSE da instalação.

Dependências resolvidas por Cargo.lock; declarações SPDX vêm dos manifests dos pacotes. Os próprios pacotes contêm os textos de licença aplicáveis.

| Pacote | Versão | Licença declarada |
|---|---|---|
| itoa | 1.0.18 | MIT OR Apache-2.0 |
| memchr | 2.8.3 | Unlicense OR MIT |
| proc-macro2 | 1.0.107 | MIT OR Apache-2.0 |
| quote | 1.0.47 | MIT OR Apache-2.0 |
| serde | 1.0.229 | MIT OR Apache-2.0 |
| serde_core | 1.0.229 | MIT OR Apache-2.0 |
| serde_derive | 1.0.229 | MIT OR Apache-2.0 |
| serde_json | 1.0.154 | MIT OR Apache-2.0 |
| syn | 3.0.9 | MIT OR Apache-2.0 |
| unicode-ident | 1.0.27 | (MIT OR Apache-2.0) AND Unicode-3.0 |
| zmij | 1.0.23 | MIT |

O projeto está com `publish = false`. Este inventário não atribui uma licença nova ao código do mantenedor. Atualize-o ao alterar Cargo.lock.

## Dependências de runtime da rota de captura (P2.4)

Nenhuma crate nova: a rota usa só bibliotecas da plataforma, abertas com `dlopen`
e com cada símbolo resolvido em runtime (`dlsym`); a ausência de qualquer um vira
`library_unavailable`/`missing_symbol`, sem fallback. "NDK" indica export público
com header no sysroot do NDK 28.2.13676358; "plataforma" indica export de
`libbinder_ndk.so` declarado apenas em `frameworks/native`
(`libs/binder/ndk/include_platform/android/`), sem garantia de estabilidade.
A API mínima é a anotação `__INTRODUCED_IN` do header; "—" quando o header não anota.

| Biblioteca | Símbolo | Origem | API mín. | Uso |
|---|---|---|---|---|
| libbinder_ndk.so | `AServiceManager_checkService` | plataforma (`binder_manager.h`) | 29 | mirror |
| libbinder_ndk.so | `ABinderProcess_setThreadPoolMaxThreadCount` | plataforma (`binder_process.h`) | 29 | pool |
| libbinder_ndk.so | `ABinderProcess_startThreadPool` | plataforma (`binder_process.h`) | 29 | pool |
| libbinder_ndk.so | `ABinderProcess_isThreadPoolStarted` | plataforma (`binder_process.h`) | 34 | pool |
| libbinder_ndk.so | `AIBinder_Class_define`, `AIBinder_associateClass`, `AIBinder_new`, `AIBinder_getUserData`, `AIBinder_getCallingUid`, `AIBinder_prepareTransaction`, `AIBinder_transact` | NDK (`android/binder_ibinder.h`) | 29 | mirror |
| libbinder_ndk.so | `AIBinder_decStrong` | NDK (`android/binder_ibinder.h`) | 29 | mirror, surface |
| libbinder_ndk.so | `AIBinder_isRemote` | NDK (`android/binder_ibinder.h`) | 29 | surface |
| libbinder_ndk.so | `AParcel_writeInt32`, `AParcel_writeFloat`, `AParcel_writeString`, `AParcel_writeStrongBinder`, `AParcel_readStatusHeader` | NDK (`android/binder_parcel.h`) | 29 | mirror |
| libbinder_ndk.so | `AParcel_delete`, `AParcel_readInt32` | NDK (`android/binder_parcel.h`) | 29 | mirror, surface |
| libbinder_ndk.so | `AParcel_getDataSize` | NDK (`android/binder_parcel.h`) | 31 | mirror, surface |
| libbinder_ndk.so | `AParcel_create` | NDK (`android/binder_parcel.h`) | 31 | surface |
| libbinder_ndk.so | `AParcel_getDataPosition`, `AParcel_setDataPosition`, `AParcel_readUint32`, `AParcel_readString`, `AParcel_readStrongBinder` | NDK (`android/binder_parcel.h`) | 29 | surface |
| libbinder_ndk.so | `AStatus_getExceptionCode`, `AStatus_delete` | NDK (`android/binder_status.h`) | 29 | mirror |
| libnativewindow.so | `ANativeWindow_writeToParcel` | NDK (`android/native_window_aidl.h`) | 34 | mirror, surface |
| libnativewindow.so | `ANativeWindow_release` | NDK (`android/native_window.h`) | — | encoder |
| libmediandk.so | `AMediaCodec_createEncoderByType`, `AMediaCodec_configure`, `AMediaCodec_start`, `AMediaCodec_stop`, `AMediaCodec_delete`, `AMediaCodec_dequeueOutputBuffer`, `AMediaCodec_getOutputBuffer`, `AMediaCodec_releaseOutputBuffer`, `AMediaCodec_getOutputFormat` | NDK (`media/NdkMediaCodec.h`) | 21 | encoder |
| libmediandk.so | `AMediaCodec_createInputSurface`, `AMediaCodec_signalEndOfInputStream` | NDK (`media/NdkMediaCodec.h`) | 26 | encoder |
| libmediandk.so | `AMediaCodec_getName`, `AMediaCodec_releaseName` | NDK (`media/NdkMediaCodec.h`) | 28 | encoder |
| libmediandk.so | `AMediaFormat_new`, `AMediaFormat_delete`, `AMediaFormat_setString`, `AMediaFormat_setInt32`, `AMediaFormat_getInt32` | NDK (`media/NdkMediaFormat.h`) | 21 | encoder |

A tabela de funções do encoder (`src/video/native.rs`) é carregada inteira: a rota de
espelhamento também exige `libEGL.so` e `libGLESv2.so` com os símbolos do probe
sintético resolvíveis, embora não os chame. O maior mínimo declarado é API 34
(`ANativeWindow_writeToParcel`, `ABinderProcess_isThreadPoolStarted`); isso é piso de
símbolos, não suporte: só o perfil executado abaixo é aceito.

Serviço e contrato privado (específicos do build; nenhum ordinal AOSP é usado como
fallback, detalhes em [compat/samsung-a16x-api36-display.json](compat/samsung-a16x-api36-display.json)):

| Item | Valor no build fixado |
|---|---|
| Serviço | `display`, via `AServiceManager_checkService` |
| Descritor | `android.hardware.display.IDisplayManager` |
| `getDisplayIds` | transação 8 |
| `createVirtualDisplay` | transação 27 |
| `releaseVirtualDisplay` | transação 30 |
| Callback local | `android.hardware.display.IVirtualDisplayCallback` (1–3, oneway, só UID 1000) |

Permissões e processo:

- `android.permission.CAPTURE_VIDEO_OUTPUT`, obtida por executar como `shell`
  (UID 2000) e declarar o pacote `com.android.shell`; é o que autoriza `AUTO_MIRROR`
  sem MediaProjection. `CAPTURE_SECURE_VIDEO_OUTPUT` não é detida pelo shell: a
  flag `SECURE` resulta em `permission_denied` e nunca é enviada fora do
  `display-fault-probe`.
- Pool de threads Binder em execução, iniciado e possuído pelo executável antes de
  qualquer objeto local ser entregue (`src/binder/pool.rs`, 4 workers): o produtor
  gráfico do encoder é um Binder local, chamado por `system_server` e SurfaceFlinger.
- Um encoder H.264 (`video/avc`) com entrada por Surface disponível ao shell.

Dependências de host para validação (não fazem parte do binário): `adb`
(platform-tools) para a sessão limitada e `ffprobe`/`ffmpeg` para decodificar o
stream nos smokes de vídeo; Python 3 para `tools/`.

### Perfis

| Perfil | Estado | Motivo |
|---|---|---|
| Samsung a16x, API 36, arm64-v8a, `shell`, fingerprint exato | Executado (`display-mirror-probe`/`display-mirror-dump`); `getDisplayIds`, `display-fault-probe` e o smoke da fixture também executados | Contrato lido do `framework.jar`/`services.jar` do próprio aparelho; evidência em [validation/p2-2-display-mirror.md](validation/p2-2-display-mirror.md) e [validation/p2-4-capture-faults-fixture.md](validation/p2-4-capture-faults-fixture.md) |
| Emulador de referência API 34 x86_64 | **Não executado; não suportado** (`unsupported_profile`, nenhuma chamada Binder) | Não rodado: não há emulador disponível na máquina do mantenedor. Contrato não fixado para esse build: o AOSP 14 tem outro layout de `VirtualDisplayConfig` e outros códigos de transação, portanto não pode ser presumido a partir do build Samsung |
| Qualquer outro build, API, ABI ou identidade | Não suportado | Sem contrato fixado nem execução |
