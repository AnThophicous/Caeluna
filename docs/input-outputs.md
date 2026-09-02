# Rouch: entrada nativa e saídas

Este documento descreve os dois módulos entregues pelo agente de entrada e
monitores:

- `src/linux/input.rs` normaliza libinput/seat para uma sequência única de
  teclado, mouse, touchpad, touchscreen, toque e gestos.
- `src/linux/outputs.rs` mantém descoberta, hotplug, modos, escala, geometria
  multi-output e estado de VT/foco.

Os módulos estão registrados em `src/linux.rs`. No modo nativo, as integrações
ficam disponíveis pela feature `native-session`; no nested, os contratos puros
continuam disponíveis para diagnóstico e testes sem tocar no seat real.

## Direção Liquid Glass

O DesignDNA define o Liquid Glass como uma camada limitada do shell: top bar,
dock e sheets podem usar translucidez, enquanto o conteúdo e o caminho de
entrada continuam legíveis e rápidos. O ExperienceIR também exige que foco,
estado de janela e recuperação sobrevivam a perda de GPU, output ou sessão.

Por isso estes módulos não aplicam blur nem fazem alocação de imagem por
evento. Eles entregam foco, escala e geometria para o renderer escolher:

1. Liquid Glass com amostra de backdrop limitada quando há orçamento de GPU;
2. glass tintado sem amostra em modo de transparência reduzida;
3. superfície opaca e sem blur em PC fraco ou renderer degradado.

Nenhuma dessas escolhas altera a entrega de teclado/mouse. A estética Tahoe é
responsabilidade do shell; a entrada deve continuar operacional quando o
efeito visual for desligado.

## Entrada

`InputNormalizer` oferece uma API independente de Smithay para testes e para
backends futuros. Ele faz quatro garantias importantes:

- timestamps nunca voltam no tempo;
- NaN, infinito e deltas absurdos viram valores seguros e limitados;
- key-repeat é gerado por timer, com atraso/cadência configuráveis e limite de
  quatro eventos por tick por padrão;
- perda de foco, pausa de seat ou remoção de dispositivo cancela o grab antes
  de propagar foco novo.

`LibinputSeatSource` é um `calloop::EventSource` real. Ele cria
`input::Libinput::new_with_udev`, atribui o seat, usa
`smithay::backend::libinput::LibinputSessionInterface<S>` e entrega eventos
`NormalizedInputEvent`. Assim os dispositivos são abertos pelo `Session`
(libseat/logind), e não por acesso manual a `/dev/input`.

O notifier de sessão é separado porque Smithay 0.7 expõe o backend de libseat
e seu notifier como dois objetos. O integrador deve encaminhar
`PauseSession`/`ActivateSession` para `LibinputSeatSource::session_event` e
para `OutputRegistry::session_event`.

## Saídas e hotplug

`SysfsOutputDiscovery` lê `/sys/class/drm/card*-*`, filtra conectores cujo
`status` é `connected` e interpreta as linhas de `modes`. A descoberta é
intencionalmente anterior ao modeset: ela pode funcionar enquanto o renderer
DRM ainda está sendo criado. `UdevOutputSource`, quando a feature nativa está
ligada, observa alterações DRM através da API `smithay::backend::udev` e
emite um sinal barato para repetir a varredura.

O fluxo correto é:

```text
scan inicial -> reconcile -> arrange -> criar/configurar wl_output
udev change -> scan -> diff_snapshots -> reconcile -> reconfigurar compositor
udev remove -> scan -> remover output -> recuperar foco/VT
```

`OutputRegistry` não tenta adivinhar um monitor que não foi reportado. Se o
modo salvo sumiu, escolhe preferred/current e depois o maior modo válido. Se a
escala é inválida, usa 1.0; se o output focado some, restaura o primary ou o
primeiro output disponível. Durante pausa de VT, os outputs são preservados
como estado, mas foco é removido e nenhum frame deve ser submetido pelo
backend DRM.

`OutputSnapshot::smithay_output()` e
`OutputSnapshot::configure_smithay_output()` atualizam a abstração pública de
`smithay::output::Output`, incluindo modo, `Scale`, transformação e posição.
Esses métodos não fazem modeset físico: o integrador deve criar os
`DrmSurface`/`DrmCompositor` usando os handles e o modo que selecionou.

## Integração nativa e compilação

O `Cargo.toml` atual já habilita DRM, udev e libseat para o trabalho nativo,
além de Winit/GL. Para compilar estes dois adapters e completar a sessão,
adicione o bloco abaixo sem alterar a versão `smithay = 0.7.0`:

```toml
[features]
default = ["native-session"]
native-session = [
    "smithay/backend_gbm",
    "smithay/backend_libinput",
    "smithay/backend_vulkan",
    "smithay/renderer_pixman",
    "smithay/xwayland",
]
```

O lockfile já contém essas dependências. Para recompilar a sessão nativa:

```bash
cargo check --features native-session
```

Os crates opcionais `input`, `udev`, `drm`, `libseat`, `gbm`, `ash` e
dependências XWayland podem não aparecer no `Cargo.lock` enquanto essas
features estão desligadas; isso é uma consequência normal do resolver do
Cargo, não uma versão nova exigida por estes módulos.

Os módulos já estão registrados no integrador Linux:

```rust
mod compat;
mod input;
mod outputs;
```

Os tipos que dependem de Smithay/libseat/libinput continuam protegidos pela
própria feature. Para uma compilação sem as integrações nativas, use
`--no-default-features`.

Dependências de sistema para os alvos oficiais:

```bash
# Mint/Ubuntu
sudo apt install libinput-dev libseat-dev libdrm-dev libgbm-dev libudev-dev \
  libxkbcommon-dev libwayland-dev libegl1-mesa-dev libgl1-mesa-dev \
  libvulkan-dev xwayland

# Arch
sudo pacman -S libinput libseat libdrm mesa libudev libxkbcommon wayland \
  vulkan-headers vulkan-icd-loader xorg-xwayland
```

Exemplo de ligação de alto nível no event loop:

```rust
let session = outputs::LibseatOutputSession::new()?;
let mut input_source = input::LibinputSeatSource::new(
    session.session().clone(),
    "seat0",
)?;
let mut registry = outputs::OutputRegistry::new();
let discovery = outputs::SysfsOutputDiscovery::new();
registry.reconcile(
    discovery.scan().unwrap_or_default(),
    outputs::LayoutStrategy::Preserve,
);

loop_handle.insert_source(input_source, |event, _, state| {
    state.consume_normalized_input(event);
})?;
loop_handle.insert_source(session.notifier_mut(), |event, _, state| {
    state.apply_session_event(event);
})?;
```

O snippet mostra a ordem e os limites; o tipo concreto de `state` pertence ao
integrador e não foi inventado neste módulo.

## Verificação feita

Foi aplicado `rustfmt`/checagem de parsing aos dois módulos. Os testes dentro
dos módulos são puros: seleção de backend, repeat limitado, sanitização de
motion, cancelamento de grab, seleção de modo, escala inválida, layout
HiDPI, foco direcional, hotplug, pausa/retorno de VT e parsing de sysfs.

## Limitações explícitas

- Os módulos não editam o event loop nem fazem modeset DRM automaticamente.
- Sysfs fornece dimensões e modos básicos; EDID detalhado (make/model/mm) e
  seleção de CRTC/connector precisam ser preenchidos pelo scanner DRM do
  integrador quando necessário.
- `UdevOutputSource` sinaliza uma nova varredura; não presume que um evento de
  GPU seja um monitor específico.
- Clipboard, drag-and-drop, portais, PipeWire/screencast, notificações
  Wayland e XWayland exigem protocolos/handlers próprios e ficam fora destes
  dois módulos.
- Vulkan, OpenGL e pixman/software são políticas do renderer. A entrada e a
  geometria funcionam em qualquer um deles, inclusive no fallback opaco de PC
  fraco.
- A troca de VT é encaminhada por libseat somente depois de
  `OutputRegistry::request_vt` validar o número; a criação da sessão, o
  display manager e o arquivo `.desktop` de `/usr/share/wayland-sessions`
  continuam sendo responsabilidade do instalador/integrador.

## Navegacao no Caelune

O ponteiro e a area de toque sao a entrada principal. Todo clique de shell
usa hit-testing por retangulos finitos e sem sobreposicao intencional; o
clique primario aciona dock, janelas, Finder, Settings, galeria, terminal,
notificacoes e widgets. Botoes secundarios continuam sendo enviados ao
aplicativo focado, em vez de ativarem um controle do shell por engano.

O nested encaminha cada movimento ao cliente Wayland, mas so agenda uma nova
composicao quando o ponteiro muda de pixel visual ou entra/sai da faixa do
dock. Isso preserva o hover e a magnification do Liquid Glass sem transformar
um touchpad barulhento em um loop de renderizacao em PC fraco.

Atalhos shell implementados (pressione a tecla Super/Command no teclado):

| Atalho | Acao |
| --- | --- |
| Super+Enter | Abrir/fechar Terminal |
| Super+Space | Abrir/fechar Launcher |
| Super+G | Abrir/fechar Galeria Flatpak |
| Super+N | Abrir/fechar Notificacoes |
| Super+E | Abrir/fechar Finder |
| Super+, | Abrir/fechar Settings |
| Alt+Tab ou Super+Tab | Alternar janelas por MRU |
| Alt+Shift+Tab ou Super+Shift+Tab | Alternar janelas para tras |
| Super+1 ... Super+9 | Focar uma janela MRU numerada |
| Ctrl+Alt+Esquerda/Direita | Trocar workspace |
| Super+Ctrl+Esquerda/Direita | Trocar workspace |
| Super+Q | Fechar a janela focada |
| Super+M | Minimizar a janela focada |
| Super+W | Alternar maximizada |
| Super+F | Alternar tela cheia |
| Escape | Fechar o overlay do shell mais acima |

Esses atalhos sao consumidos antes do forwarding para o cliente. O Terminal
mantem seus atalhos de abas (`Ctrl+Shift+T`, `Ctrl+Shift+W`), tema (`Ctrl+Shift+P`),
blur (`Ctrl+Shift+G`), transparencia (`Ctrl+Shift+A`) e titulo (`Ctrl+Shift+E`),
e recebe texto normal sem que Super/Alt de navegacao seja confundido com bytes
do PTY.
