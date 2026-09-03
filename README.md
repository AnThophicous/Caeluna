# Caelune — desktop Wayland Liquid Glass para Linux

Caelune é um ambiente gráfico Linux escrito em Rust. Ele cria um desktop
Wayland com janelas, dock, barra superior, notificações, configurações,
galeria de aplicativos e uma aparência Liquid Glass inspirada no macOS Tahoe.

O objetivo é simples: trazer para o Linux a calma, a organização e os detalhes
de uso que muitas pessoas associam ao macOS, sem copiar os arquivos ou a marca
da Apple. Caelune não é um produto da Apple e não tem afiliação com ela. Apple,
macOS e Tahoe aparecem neste projeto apenas como referência visual e de
experiência; o código e os assets distribuídos são próprios ou abertos.

> Estado atual: `--nested` continua sendo o caminho de desenvolvimento; a
> sessão `--session` agora possui o caminho nativo de Wayland + libinput +
> GBM/EGL/GLES + DRM/KMS. A validação final ainda precisa ser feita em Linux
> com seatd/logind e uma GPU/saída compatíveis.

## O que é o Caelune

Caelune é um compositor e desktop Wayland. Ele gerencia o espaço de trabalho,
recebe clientes Wayland/XDG, desenha o shell e controla ações comuns de
janelas.

O visual usa a ideia de Liquid Glass de forma limitada: transparência e blur
ficam em superfícies do shell, como barra, dock, notificações e folhas de
configuração. O conteúdo de aplicativos continua legível e pode ficar opaco
em computadores fracos.

O projeto é desenvolvido para três sistemas oficiais:

- Linux Mint;
- Ubuntu;
- Arch Linux.

Outras distribuições podem até compilar partes do código, mas não fazem parte
da promessa oficial de compatibilidade nesta fase.

## O que já existe

### Shell inspirado no macOS Tahoe

- barra superior com distribuição, aplicativo ativo, bateria, relógio e
  Control Centre;
- dock com ícones fixados, indicador de aplicativo aberto, badge de janelas
  minimizadas, tooltip e bounce de abertura;
- wallpaper WebP oceânico carregado uma vez e ajustado ao tamanho da tela;
- fontes do sistema com fallback para fontes comuns do Linux;
- janelas com barra de título própria e botões fechar, minimizar e maximizar;
- mover, redimensionar, maximizar, tela cheia, minimizar e restaurar;
- workspaces, camadas, snap, tiling e ordem MRU para Alt-Tab;
- Finder, launcher, widgets, Settings e folhas de interface com geometria
  própria.

### App Gallery com Flatpak e Flathub

A App Gallery é uma superfície nativa do compositor. Ela consulta o remote
`flathub` usando o executável `flatpak` e argumentos separados, sem passar
comandos por um shell.

Ela pode mostrar:

- nome, resumo, categoria, versão e origem quando esses dados estão disponíveis;
- busca e filtros por categoria;
- Install, Update e Open;
- estado instalado;
- catálogo em cache para quando a rede ou o Flatpak não estiver disponível;
- aplicativos locais encontrados em arquivos `.desktop`, marcados como
  locais e não como Flatpaks falsos.

O modelo e o backend estão descritos em
[docs/app-store-notifications.md](docs/app-store-notifications.md). O
resultado real depende do Flatpak, do remote configurado e da rede do sistema.

### Notificações e configurações

O centro de notificações é local ao Caelune e organiza eventos por origem. Ele
tem expiração, grupos, contagem de não lidas, ações, Do Not Disturb e níveis de
importância.

Em Settings > Notifications é possível:

- ligar ou desligar a entrega de notificações;
- ativar Do Not Disturb;
- controlar som e posição;
- limpar o histórico.

As preferências ficam em `~/.config/rouch`. O bridge completo para o serviço
externo `org.freedesktop.Notifications`, D-Bus, clipboard, drag-and-drop,
portais e screencast ainda precisa ser ligado à sessão nativa. Hoje esses
limites são reportados como disponíveis, degradados ou indisponíveis, sem
simular sucesso.

### Configurações do sistema

O app Settings tem panes para Wi-Fi, Bluetooth, Appearance, Notifications,
Displays, Sound, Performance, About, Widgets, Wallpaper, Users, Mouse e
Keyboard.

Quando os serviços existem, os backends consultam ferramentas e arquivos reais
do Linux, como `nmcli`, `bluetoothctl`, `pactl`, `/proc`, `/sys` e `df`.
Recursos ausentes aparecem como indisponíveis ou com valor desconhecido.

O painel Performance mostra a preferência gráfica, o renderer ativo no nested,
a política de transparência e a recomendação de swapfile. O planner do
swapfile usa comandos com argv explícito, permissões `0600`, tamanho limitado e
confirmação antes de escrever uma assinatura de swap. Ele não redimensiona um
arquivo existente em silêncio.

## Primeiro uso e tela inicial

Na primeira execução, o Caelune mostra uma tela de apresentação e depois um
cartão de configuração inicial. O fluxo atual é retomável e guarda o estado em
`~/.local/state/rouch/setup.state`.

As etapas atuais são:

1. confirmar a configuração;
2. conferir dependências;
3. escolher a preferência do renderer;
4. revisar o swapfile recomendado, que é opcional;
5. registrar o aplicativo no menu;
6. escolher se o Caelune inicia com a sessão, também opcional;
7. ativar somente a primeira etapa da interface.

A ideia da experiência inicial é explicar, em linguagem simples, como usar a
barra, o dock, a App Gallery, as notificações, o Settings, o Alt-Tab e o
terminal. O código já contém o modelo retomável e o renderer do tutorial, com
seis passos sobre Top Bar, Dock, App Gallery, notificações, janelas e terminal,
além de foco por teclado, Skip, Back, Next e uma opção visual opaca para PC
fraco. Essa camada ainda não está ligada ao fluxo principal de entrada e
renderização da sessão; portanto, o tutorial completo não deve ser tratado
como pronto para uso diário.

## Terminal próprio

O terminal próprio faz parte do desenho do desktop Caelune e deve vir fixado no
dock quando sua integração estiver concluída. A proposta é ter:

- PTY real, com comandos executados pelo sistema Linux;
- UTF-8 ativo desde a abertura, com recuperação segura de sequências inválidas;
- abas, títulos personalizados e ciclo de sessões;
- scrollback limitado para não consumir memória sem fim;
- transparência, blur e fundos configuráveis, sempre com fallback legível;
- atalho e foco integrados ao gerenciamento de janelas.

Já existe um núcleo em `src/terminal.rs` com PTY Linux, parser UTF-8/VT,
scrollback limitado, títulos e ciclo de abas. A integração desse núcleo com o
renderer, o dock e os atalhos do shell ainda está em andamento, e o dock deste
snapshot ainda usa o ID de um terminal externo (`weston-terminal`). Portanto
esta parte segue em desenvolvimento e não é uma promessa de que o emulador
já está pronto para uso diário.

## Gráficos e fallback

A política de gráficos é Vulkan primeiro, OpenGL depois e software opaco por
último:

```text
Vulkan -> OpenGL -> Software/opaque
```

O módulo `src/graphics.rs` possui o seletor, diagnósticos, perda de contexto e
recuperação, com testes para os caminhos de fallback.

Há uma diferença importante entre política e implementação atual:

- o nested usa hoje Smithay/Winit com `GlesRenderer`, ou seja, OpenGL/EGL;
- a preferência Vulkan está pronta no modelo e nas decisões de startup, mas um
  renderer Vulkan real para o nested ainda não foi ligado;
- o caminho nativo liga DRM/KMS, GBM/EGL/GLES, libseat, libinput e udev; ainda
  precisa de validação em uma instalação Linux real;
- software/opaque é a última política de segurança visual; isso não significa
  que todos os fluxos nativos já estejam prontos nesse modo.

## Desempenho em PC fraco

Liquid Glass não precisa deixar o sistema pesado. A política do Caelune:

- nunca aplica blur no conteúdo de um aplicativo;
- limita a amostra do wallpaper às superfícies aprovadas do shell;
- não mantém loop de renderização para uma folha escondida;
- usa redraw por dano e VSync quando possível;
- reduz escala, cache, sombra e animação em LowEnd e BatterySaver;
- transforma o shell em superfície opaca quando o efeito não é seguro.

Os valores de pixels, cache e FPS são limites de política, não benchmark. Ainda
não há medição comparável de frame time, VRAM ou consumo em todas as combinações
de Mint, Ubuntu e Arch. Os detalhes estão em
[docs/performance-low-end.md](docs/performance-low-end.md) e no contrato visual
em [docs/tahoe-visual-contract.md](docs/tahoe-visual-contract.md).

### Game Mode

O Caelune tem um Game Mode reversível para reduzir o trabalho do próprio shell
quando um jogo está em primeiro plano. Ele não depende de uma lista de títulos:
observa foco, fullscreen, árvore de processos, grupo de controle, desktop entry
e runtimes como Steam, Proton, Wine, pressure-vessel, gamescope e Sober. A tela
de Settings mostra a confiança e os motivos antes de aplicar a política.

No modo automático, ele guarda um snapshot pequeno das preferências e reduz
blur, transparência, animações e consultas ocultas. Entrada, áudio, segurança,
lock screen e notificações críticas continuam ativos. Ao sair do jogo, o perfil
anterior é restaurado e o cache temporário é descartado. O detector não mata,
suspende nem rebaixa processos aleatórios.

O contrato técnico está em [docs/game-mode.md](docs/game-mode.md).

## Bibliotecas principais

O projeto mantém a maior parte do estado e da geometria em módulos puros, com
testes unitários. As bibliotecas principais são:

- [Smithay](https://github.com/Smithay/smithay) 0.7: servidor Wayland, XDG
  shell, seats, renderização e backends Linux;
- Winit e EGL/GLES: janela host e renderer da sessão `--nested` atual;
- libseat/logind, DRM/KMS, GBM, udev e libinput: base do caminho nativo;
- `fontdue`: rasterização da fonte da interface;
- `image`: leitura do wallpaper WebP;
- `tracing` e `tracing-subscriber`: logs de diagnóstico;
- Flatpak, portais e ferramentas do sistema: integrações opcionais executadas
  somente quando presentes.

As versões e features ficam em [Cargo.toml](Cargo.toml), e o lockfile é
mantido em [Cargo.lock](Cargo.lock).

## Instalação teórica

Os comandos abaixo descrevem o fluxo previsto para um Linux Mint, Ubuntu ou
Arch instalado no computador. Eles ainda dependem de um repositório e de uma
release reais.

### Dependências de compilação

Para Mint e Ubuntu:

```bash
sudo apt update
sudo apt install build-essential pkg-config cargo rustc \
  libwayland-dev libxkbcommon-dev libegl1-mesa-dev libgl1-mesa-dev \
  libvulkan-dev libdrm-dev libgbm-dev libudev-dev libinput-dev libseat-dev \
  wayland-protocols xwayland flatpak mesa-vulkan-drivers libgl1-mesa-dri \
  seatd dbus-user-session xdg-desktop-portal xdg-desktop-portal-gtk
```

Para Arch Linux:

```bash
sudo pacman -S --needed base-devel rust cargo pkgconf wayland-protocols \
  wayland libxkbcommon libdrm libinput seatd xorg-xwayland \
  vulkan-headers vulkan-icd-loader vulkan-swrast mesa libglvnd flatpak \
  xdg-desktop-portal xdg-desktop-portal-gtk dbus
```

### Compilar pelo código

No checkout do projeto:

```bash
cargo build --locked --release --features native-session
```

Para abrir a sessão nested dentro de uma sessão gráfica Linux já existente:

```bash
cargo run --locked -- --nested
```

O log informa o socket Wayland criado, por exemplo `wayland-1`. A sessão
`--nested` é o caminho indicado para desenvolvimento e para a primeira
verificação visual.

### Instalador Bash pelo GitHub

O ponto de entrada amigável está em [installer.sh](installer.sh). Ele detecta
Linux Mint, Ubuntu e Arch, mostra um diagnóstico do computador, instala as
dependências, verifica a GPU, prepara Vulkan com fallback para OpenGL/software,
configura o Flathub e instala somente a primeira etapa da interface. O script
mantém os nomes `rouch-*` internamente para não quebrar os binários atuais,
mas a sessão aparece como **Caelune**.

```bash
curl -fsSL https://raw.githubusercontent.com/AnThophicous/Caeluna/main/installer.sh \
  | bash -s -- --yes
```

Para revisar o plano sem baixar, compilar, instalar pacote ou alterar arquivos:

```bash
curl -fsSL https://raw.githubusercontent.com/AnThophicous/Caeluna/main/installer.sh \
  | bash -s -- --dry-run --no-build
```

Para apenas verificar a máquina, use `--diagnose`. `--skip-drivers` evita
qualquer tentativa de correção da GPU e `--skip-flatpak` não adiciona o Flathub.
O instalador usa HTTPS, valida downloads quando um SHA-256 é fornecido, recusa
caminhos perigosos em arquivos tar, pede confirmação para substituições e tenta
fazer rollback se a ativação falhar. `--no-build` usa um binário pré-compilado
somente quando uma release fornecer um asset compatível; sem release, o modo
padrão compila o código-fonte da branch `main`.

### Pacote `.deb`

O builder local para Debian, Ubuntu e Mint está em
[packaging/debian/build-deb.sh](packaging/debian/build-deb.sh):

```bash
ROUCH_BINARY_PATH="$PWD/target/release/rouch" \
  bash packaging/debian/build-deb.sh
sudo apt install ./dist/rouch_0.1.0_amd64.deb
```

Sem `ROUCH_BINARY_PATH`, o script compila o binário. O pacote instala os
launchers, a entrada em `/usr/share/wayland-sessions/rouch.desktop`, a unidade
de usuário desativada e os assets da primeira interface.

### Pacote Arch Linux

O recipe está em [packaging/arch/PKGBUILD](packaging/arch/PKGBUILD):

```bash
cd packaging/arch
makepkg -f
sudo pacman -U ./rouch-0.1.0-1-$(uname -m).pkg.tar.zst
```

### RPM

Um pacote RPM ainda não é publicado neste ciclo. Não use um nome de pacote ou
uma URL RPM inventada; o caminho oficial atual é Bash, `.deb` ou Arch.

## Primeiros passos no ambiente gráfico

Depois da instalação, escolha a sessão Caelune (a entrada compatível ainda se
chama `rouch`) no seu gerenciador de login ou
abra o launcher nested pelo terminal. O primeiro fluxo recomendado é:

1. deixe o cartão inicial terminar ou avance suas etapas;
2. olhe a barra superior: o relógio abre notificações e o ícone de controles
   abre o Control Centre;
3. abra a App Gallery pelo dock ou com `Super+G`;
4. use `Super+Space` para pesquisar os aplicativos instalados;
5. abra Finder com `Super+E`;
6. abra Settings com `Super+,` e entre em Notifications ou Performance;
7. abra uma segunda janela e use `Alt+Tab` para trocar pela ordem de uso;
8. use `Ctrl+Alt+Left/Right` para alternar entre workspaces.

Atalhos de janela disponíveis no nested:

| Atalho | Ação |
| --- | --- |
| `Super+M` | minimizar ou restaurar |
| `Super+W` | maximizar ou voltar ao tamanho normal |
| `Super+F` | entrar ou sair da tela cheia |
| `Super+Q` | fechar a janela focada |
| `Alt+Tab` | trocar pela ordem MRU |
| `Esc` | fechar uma folha aberta quando aplicável |

## Sessão nativa e estado real

O modo `--session` é separado do modo `--nested`:

```bash
rouch --nested
rouch --session
```

O `--session` adquire o seat com libseat, abre o dispositivo DRM primário,
cria o socket Wayland e entra no loop nativo de renderização. O binário ainda
aceita `--fallback-nested` para desenvolvimento, mas o launcher do display
manager usa `--no-fallback`, evitando que uma sessão gráfica real seja
silenciosamente substituída por uma janela nested.

O que já foi preparado:

- módulos de probing para seat, DRM e hotplug udev;
- normalização de teclado, mouse, touchpad, touchscreen, toque e gestos com
  libinput;
- descoberta de conectores, modos, escala e geometria de outputs;
- contratos de compatibilidade para XWayland, clipboard, drag-and-drop,
  portais, screencast e notificações externas;
- arquivo de sessão em `/usr/share/wayland-sessions/rouch.desktop` no pacote.

O que ainda falta para chamar a sessão nativa de pronta:

- validar pause/activate, VT, hotplug e troca de modo em hardware real;
- completar suporte multi-output e a política de seleção de output;
- ligar de ponta a ponta a ponte XWayland e os protocolos de clipboard,
  portais, screencast e notificações externas;
- testar a matriz completa em hardware real Mint, Ubuntu e Arch.

Essa distinção é importante: encontrar um executável ou uma biblioteca não é a
mesma coisa que confirmar uma sessão nativa funcionando.

## Desenvolvimento e testes

O projeto usa Rust 2024 e mantém os testes dos modelos junto do código:

```bash
cargo fmt --all -- --check
cargo test --locked
cargo clippy --all-targets -- -D warnings
```

No Windows, a suíte dos módulos puros pode ser executada, mas o compositor
Linux e suas bibliotecas de sistema não podem ser validados por completo. A
checagem nativa deve ser feita em Mint, Ubuntu ou Arch com `pkg-config` e as
bibliotecas listadas acima.

Para entender os limites de Wayland, outputs, empacotamento e desempenho,
consulte:

- [Contrato visual Tahoe](docs/tahoe-visual-contract.md);
- [App Gallery e notificações](docs/app-store-notifications.md);
- [Entrada e outputs](docs/input-outputs.md);
- [Compatibilidade Wayland/X11](docs/wayland-compat.md);
- [Empacotamento](docs/packaging.md);
- [Desempenho em PC fraco](docs/performance-low-end.md).

## Nome do produto

O nome público escolhido é **Caelune**. Ele combina com a referência visual do
macOS Tahoe 26 e com a ideia de uma camada calma, luminosa e organizada sobre
o Linux. Durante a transição, o crate, os binários, os caminhos de configuração
e os pacotes ainda usam `rouch` para preservar compatibilidade.

Outros nomes considerados, mas não escolhidos, foram:

| Candidato | Ponto forte |
| --- | --- |
| Caelune | curto, calmo e combina céu, luz e o wallpaper oceânico |
| Auralis | soa elegante e passa ideia de ambiente completo |
| Nuvella | leve, memorável e lembra uma camada suave sobre o desktop |
| Veylune | diferente, visual e fácil de transformar em nome de produto |
| Solvane | transmite clareza e organização sem parecer uma marca oficial |
| Aurelume | mistura brilho e calor em um nome original de trabalho |
| Lumessa | simples, amigável e ligado à ideia de luz na interface |
| Cielora | tem som aberto e combina com o tema de céu e água |
| Orivane | distinto, neutro e adequado para um projeto de software |
| Calyra | curto, suave e funciona bem em texto e ícone |

O nome público não muda automaticamente o crate, os binários, os caminhos de
instalação ou os arquivos de empacotamento; essa migração será feita em uma
etapa própria para não quebrar instalações existentes.

## Próximos passos

1. integrar o tutorial completo da primeira execução;
2. concluir o terminal próprio com PTY, UTF-8, abas e configuração visual;
3. validar pause/activate, hotplug, VT e múltiplos outputs em hardware real;
4. validar Vulkan real e fallback OpenGL/software em hardware suportado;
5. completar XWayland, clipboard, portais, screencast e notificações externas;
6. medir frame time, memória e consumo em PC fraco, substituindo proxies por
   dados reais.

## Licença e referência visual

O crate declara MIT OR Apache-2.0. O Caelune busca a sensação de um desktop
organizado e refinado, com referência no macOS Tahoe e na linguagem Liquid
Glass, mas distribui código, geometria e assets próprios ou abertos. Marcas,
ícones e fontes proprietárias da Apple não fazem parte dos assets distribuídos.
