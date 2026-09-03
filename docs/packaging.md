# Empacotamento e instalação oficial

O alvo oficial do Rouch é Linux Mint, Ubuntu e Arch Linux. A sessão mantém a
identidade Rouch Liquid Glass, inspirada no macOS Tahoe, mas usa os componentes
Wayland do Linux e fica desativada por padrão quando oferecida como unidade
systemd de usuário. Assim, a seleção pelo display manager continua sendo a
forma segura de obter posse do seat, VT, teclado e GPU.

## Instalação pelo GitHub

Substitua `OWNER/REPO` pelo repositório oficial publicado antes de executar o
comando. O instalador não inventa um endereço remoto válido:

```sh
curl -fsSL https://raw.githubusercontent.com/OWNER/REPO/main/scripts/install.sh \
  | ROUCH_GITHUB_REPO=OWNER/REPO bash -s -- --yes
```

Para conferir o plano sem instalar pacotes, baixar arquivos ou escrever no
sistema:

```sh
curl -fsSL https://raw.githubusercontent.com/OWNER/REPO/main/scripts/install.sh \
  | ROUCH_GITHUB_REPO=OWNER/REPO bash -s -- --dry-run --no-build
```

O modo padrão compila com `cargo --locked --release`. `--no-build` usa um
binário publicado ou o caminho fornecido em `ROUCH_BINARY_PATH`. O instalador
baixa somente a primeira etapa da interface, registra `rouch.desktop` no menu,
instala a entrada `/usr/share/wayland-sessions/rouch.desktop` e deixa a unidade
systemd de usuário desativada. Cada substituição pede confirmação, exceto com
`--yes`; falhas depois do primeiro arquivo ativado restauram os arquivos
anteriores e não removem configurações do usuário.

## Variáveis do instalador

| Variável | Função |
| --- | --- |
| `ROUCH_GITHUB_REPO` | Repositório `owner/repo` usado para releases e fonte. |
| `ROUCH_VERSION` | `latest` ou uma tag. |
| `ROUCH_PREFIX` | Prefixo absoluto; padrão `/usr/local` como root ou `$HOME/.local`. |
| `ROUCH_BINARY_URL` / `ROUCH_BINARY_PATH` | Release HTTPS ou binário local para `--no-build`. |
| `ROUCH_BINARY_SHA256` | SHA-256 opcional do binário baixado. |
| `ROUCH_SOURCE_URL` / `ROUCH_SOURCE_DIR` | Fonte HTTPS ou checkout local para compilação. |
| `ROUCH_SOURCE_SHA256` | SHA-256 opcional do arquivo de fonte. |
| `ROUCH_INTERFACE_URL` / `ROUCH_INTERFACE_DIR` | Primeiro pacote visual; só a etapa 1 é ativada. |
| `ROUCH_INTERFACE_SHA256` | SHA-256 opcional do pacote visual. |
| `ROUCH_BIN_DIR` / `ROUCH_SHARE_DIR` | Sobrescrevem os diretórios do binário e dos dados. |
| `ROUCH_DESKTOP_DIR` | Diretório do desktop entry comum. |
| `ROUCH_SESSION_DIR` | Diretório da sessão Wayland; root usa `/usr/share/wayland-sessions`. |
| `ROUCH_SYSTEMD_USER_DIR` | Diretório da unidade de usuário; root usa `/usr/lib/systemd/user`. |
| `TMPDIR` | Diretório temporário para staging e rollback. |

Os downloads exigem HTTPS, arquivos tar são verificados contra caminhos
absolutos, traversal e links, e os destinos existentes que sejam diretórios ou
links simbólicos são recusados.

## Matriz oficial

| Distribuição | Identificação aceita | Pacotes | Artefato recomendado | Sessão |
| --- | --- | --- | --- | --- |
| Ubuntu | `ID=ubuntu` | `apt` | `.deb` ou `install.sh` | `/usr/share/wayland-sessions/rouch.desktop` |
| Linux Mint | `ID=linuxmint` | `apt` | `.deb` ou `install.sh` | `/usr/share/wayland-sessions/rouch.desktop` |
| Arch Linux | `ID=arch` | `pacman` | `PKGBUILD` ou `install.sh` | `/usr/share/wayland-sessions/rouch.desktop` |

O instalador recusa Debian, Fedora, openSUSE, Alpine, Manjaro e outras
distribuições, mesmo quando elas possuem um gerenciador de pacotes parecido.
Isso evita declarar suporte de integração de seat, input e display manager que
não foi validado.

## Pacotes

### Debian, Ubuntu e Mint

O builder local não modifica o checkout e recusa sobrescrever o resultado:

```sh
ROUCH_BINARY_PATH="$PWD/target/release/rouch" \
  bash packaging/debian/build-deb.sh
sudo apt install ./dist/rouch_0.1.0_amd64.deb
```

Sem `ROUCH_BINARY_PATH`, o builder executa `cargo build --locked --release`.
O pacote declara a base DRM/libseat/libinput/Wayland, EGL/OpenGL,
Vulkan/software renderer, XWayland, Flatpak e portais. Mesa/llvmpipe e
`vulkan-swrast` ficam disponíveis como fallback de máquinas fracas quando o
repositório da distribuição os fornece.

### Arch Linux

```sh
cd packaging/arch
makepkg -f
sudo pacman -U ./rouch-0.1.0-1-$(uname -m).pkg.tar.zst
```

Para compilar a partir de outro checkout, use
`ROUCH_SOURCE_DIR=/caminho/absoluto makepkg -f`. O `PKGBUILD` instala os mesmos
launchers, sessão Wayland e unidade systemd de usuário. A opção RPM não é
publicada neste ciclo: manter apenas os alvos oficiais evita prometer suporte
não testado.

## Modos e remoção

Os launchers tornam os dois contratos explícitos. `--session` tenta adquirir o
seat nativo e recorre ao nested somente quando a política de fallback permite;
`--nested` sempre abre o backend de desenvolvimento:

```sh
rouch-nested --nested       # janela dentro de uma sessão existente
rouch-session --session     # entrada escolhida pelo display manager (native, sem fallback)
```

O arquivo de sessão usa `rouch-session --session`; ele não habilita
automaticamente `rouch.service`. A unidade pode ser inspecionada com:

```sh
systemctl --user cat rouch.service
```

Para remoção, use o gerenciador do pacote (`sudo apt remove rouch` ou
`sudo pacman -R rouch`). Os scripts de pacote não param uma sessão, não fazem
`rm -rf` em `~/.config/rouch` e não removem estado de setup, preferências ou
cache. Uma atualização pode ser revertida instalando novamente o pacote
anterior; uma instalação interrompida pelo `install.sh` restaura os destinos
que existiam antes do staging.
