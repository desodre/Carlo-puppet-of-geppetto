# Sessão nativa de acessibilidade — próxima entrega P1

## Escopo e ordem

Entrega coesa: [#19](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/19),
[#20](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/20),
[#21](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/21) e
[#24](https://github.com/desodre/Carlo-puppet-of-geppetto/issues/24).
Branch `codex/feat/accessibility-session-19`, a partir de main após merge de #84.
Pai #2 permanece aberto até a prova de um nó real atualizado.

1. **#19 — contratos:** fixar frameworks/base e frameworks/native em Android 14;
   registrar fontes/hashes, descritores, códigos, flags, permissões e campos de
   AccessibilityServiceInfo/init. Restringir chamadas privadas ao fingerprint
   comprovado; a tag de referência não prova a proveniência exata da imagem Google.
2. **#20 — runtime:** FFI pequena com RAII para parcels/strong refs; classe Binder
   local e cliente IAccessibilityServiceClient, pool limitado e callbacks sem
   unwind através da ABI C. Estado compartilhado com ownership definido, validação
   de origem/tamanho/status e descarte de callbacks tardios.
3. **#21 — registro:** serializar configuração mínima de AccessibilityServiceInfo,
   registrar via IAccessibilityManager, esperar init com deadline e preservar a
   referência da conexão. Não anunciar hierarquia por sucesso de registro.
4. **#24 — lifecycle:** unregister explícito/idempotente, encerramento em erro e
   sinais, recusar sessão concorrente sem desmontar registro alheio. Testar morte
   forçada: limpeza pelo sistema seguida de novo registro bem-sucedido.
5. **Aceitação:** fmt, Clippy host/Android, testes host, builds de duas ABIs e prova
   real x86_64 API 34. Salvar resultados de ciclos, conflito, morte/recuperação e
   regressão P0/P1.1. Cleanup do emulador/artefatos temporários ao terminar.

Não usar Java/JNI/APK/root nem mudar SELinux. Uma tabela FFI e as classes Binder
permanecem vivas pelo processo após iniciar o pool: callbacks tardios não podem
executar código descarregado. Documentar recursos permanentes versus por sessão.
Chamadas Binder síncronas não ganham cancelamento artificial; host mantém timeout
com encerramento forçado. Sem logging de UI ou mensagens remotas arbitrárias.

## Depois desta entrega

- **#22 + #23:** consulta única correlacionada e codec mínimo de AccessibilityNodeInfo,
  com fixture, entrada inválida, limites e resultado tardio. Implementar juntos
  para provar leitura de um nó e mudança de texto; concluir P1 somente após isso.
- **P2/#25 em diante:** encoder H.264 sintético por NDK e prova separada de captura,
  sem depender da conclusão de hierarquia. Não confundir encoder com captura.

Se uma prova falhar, registrar chamada/status/permissão/formato exato, manter
capacidade indisponível e concluir apenas issues cujo escopo tenha evidência.

## Aplicação

#19/#20/#21/#24 implementadas nesta branch e validadas no emulador de referência.
[Contratos fixados](../compat/api34-accessibility.json) e
[relatório de execução](../validation/p1-session.md) registram evidências e limites.
Próximo incremento funcional continua sendo #22 + #23. A etapa P1 não foi concluída:
ainda é necessário obter um nó real e observar mudança de texto.
