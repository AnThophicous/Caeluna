# App Store e notificações

## Catálogo de aplicativos

`crate::app_store::AppStore` é o estado puro da galeria. O agente de
integração deve:

1. criar o estado com `AppStore::new()` e mostrar `CatalogState::Loading`;
2. chamar `crate::app_store_backend::FlatpakBackend::refresh()` fora do
   renderer;
3. passar `CatalogSnapshot::apps` para `set_apps`, `set_cached_apps`,
   `set_offline` ou `set_error` conforme `snapshot.state`;
4. usar `AppStore::filtered`, `AppStore::selected_app` e
   `AppStore::layout(work_area)` para desenhar;
5. encaminhar `AppStoreHit::Card(index)` para `select(index)` e então executar
   `install`, `update` ou `remove` no backend de acordo com
   `FlatpakApp::primary_action()`.

O backend é exposto no módulo raiz como `crate::app_store_backend` porque
`src/linux.rs` permanece deliberadamente intocado nesta frente. Em uma
integração Linux, o backend padrão consulta `flatpak remote-ls` e
`flatpak list` com `std::process::Command`; nenhum argumento passa por shell.
O cache fica em `$XDG_CACHE_HOME/rouch/app-store.tsv` ou
`$HOME/.cache/rouch/app-store.tsv`. Se o executável ou o remoto não estiverem
disponíveis, o snapshot informa `Offline` e mantém os dados cached/local; não
há uma lista falsa de aplicativos de servidor.

## Notificações

`crate::notifications::NotificationCenter` recebe drafts através de
`enqueue`/`push`. O relógio é fornecido pelo chamador em milissegundos, por
exemplo `Notification::new("org.example.App", "Título", "Texto",
NotificationLevel::Info, now_ms).expires_after(Duration::from_secs(5))`.

- `visible_at(now_ms)` retorna apenas itens não expirados e permitidos;
- `grouped_at(now_ms)` agrupa por `grouped_as(...)` ou pelo source;
- `expire`, `dismiss`, `dismiss_group`, `mark_read` e `activate_action`
  atualizam a fila sem executar efeitos colaterais;
- `NotificationPreferences::set_enabled` desliga globalmente novas entregas;
- `set_app_enabled` controla uma fonte específica;
- `set_do_not_disturb` enfileira, mas adia a exibição, sem perder eventos;
- `set_critical_allowed_during_dnd(true)` permite uma exceção explícita para
  níveis `Critical`.

O renderer pode tratar `EnqueueResult::Delivered`, `Deferred` e `Suppressed`
como estados distintos e exibir o badge offline/DND sem conhecer processos ou
persistência.

## Testes

Os modelos têm testes unitários determinísticos. O backend aceita um
`FlatpakCommandRunner` injetado para testar parsing, cache, fallback, validação
de IDs e argv sem depender de Flatpak instalado.
