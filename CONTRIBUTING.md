# Contribuindo com Carlo

## Issue, branch e PR

O [projeto 11](https://github.com/users/desodre/projects/11/views/1) acompanha o trabalho.
As etapas P0–P10 são issues pai com label `type:epic`; as 71 tarefas do plano são
subissues nativas. A relação pai/filha deve existir no GitHub, além de referências
no texto. O índice está em [docs/ISSUES.md](docs/ISSUES.md).

Uma entrega deve ter resultado verificável, critérios de conclusão e evidência.
Divida uma issue quando reunir resultados independentes ou exigir mais de uma
entrega revisável. Mantenha o escopo e a aceitação completa na issue pai.

Crie branch quando iniciar trabalho, a partir de `main`, vinculada à issue:

```sh
gh issue develop 18 --repo desodre/Carlo-puppet-of-geppetto \
  --base main --name codex/spike/binder-symbols-18 --checkout
```

O exemplo mostra uma nova entrega, a criar quando a issue estiver pronta. A entrega
inicial P0 usa `codex/feat/p0-foundation-1`, já vinculada à issue pai #1.
Para outra entrega, escolha número e nome correspondentes. Convenção: `codex/<feat|fix|docs|spike>/<slug>-<issue>`.
Uma entrega coesa pode resolver várias subissues na mesma branch/PR, listando-as
explicitamente. Branch planejada no texto não significa branch já criada.

Use Conventional Commits em inglês e referência à issue, por exemplo:
`feat(cli): add native foundation diagnostics (refs #15, #16)`.

O PR deve explicar comportamento, validação e limitações. Use `Closes #N` para subissues implementadas integralmente e `Refs #N` para a
etapa pai em entregas parciais. Um PR que implemente toda a etapa, valide seu
critério completo e cubra todas as filhas pode também usar `Closes #<pai>`; nesse
caso, o merge representa o aceite da etapa. Merge em `main` fecha as subissues referenciadas pelo mecanismo do GitHub.
Não faça push direto para `main`; merge, tags e publicação dependem do mantenedor.

## Estados e labels

| Estado | Condição |
|---|---|
| Backlog | Escopo registrado, ainda não iniciado |
| Pronto | Dependências resolvidas e aceitação clara |
| Em andamento | Branch vinculada e implementação ativa |
| Em revisão | Implementação e evidência prontas, PR disponível |
| Concluído | Entrega aceita e PR integrado, ou investigação aceita com evidência |

Tipos: `type:epic`, `type:task`, `type:spike`. Use uma ou mais labels `area:*` e
prioridade `priority:high` ou `priority:normal`. `roadmap` identifica o plano.
`blocked` requer impedimento concreto e condição de desbloqueio registrados;
dependência futura em backlog não exige essa label.

O workflow de ciclo de vida usa apenas `GITHUB_TOKEN` com permissão de issues:

- Reabre pai fechado que ainda tenha subissue aberta.
- Marca pai aberto com `needs:acceptance` após todas as filhas fecharem.
- Remove essa marca se uma filha voltar a ficar pendente.
- Mantém o fechamento final do pai dependente da aceitação completa da etapa.

O workflow passa a valer quando estiver em `main`. Não escreve no Project pessoal
com token de repositório. Atualize Status no board pela interface ou `gh project
item-edit`; o workflow protege o estado das issues e a necessidade de aceite.

## Qualidade

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
python3 -m unittest discover -s tools -p 'test_*.py' -v
```

Build Android usa [android-build.json](android-build.json) e NDK fixado:

```sh
export ANDROID_NDK_HOME=/path/to/android-sdk/ndk/28.2.13676358
python3 tools/build_android.py
```

Rust puro, parsers e política de lifecycle têm testes host. FFI Android, permissões,
ABI e execução precisam de teste Android. Registrar build bem-sucedido separadamente
de execução. Cada mudança inclui docs e CHANGELOG quando alterar uso ou arquitetura.

Não registrar texto de UI, XML completo, tokens ou credenciais nos logs. Shell
com argumentos dinâmicos deve ser cotado também para o `sh` do dispositivo.
Emulador descartável é o alvo padrão; execução em aparelho pessoal depende da
autorização do usuário para aquela sessão e escopo.
