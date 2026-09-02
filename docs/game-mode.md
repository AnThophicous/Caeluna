# Caelune Game Mode

O Game Mode do Caelune existe para reduzir o trabalho do próprio ambiente
gráfico quando um jogo está em primeiro plano. Ele não promete criar FPS, não
mata processos e não tenta adivinhar um jogo pelo nome do executável.

## Como a detecção pensa

O detector coleta amostras pequenas e espaçadas do estado observável da sessão:

- janela em primeiro plano, estado fullscreen e `app_id`/classe Wayland ou X11;
- árvore de processos e grupo de controle ao qual a janela pertence;
- sinais de runtimes de jogos, como Steam, Proton, Wine, pressure-vessel,
  gamescope e Sober;
- categoria e metadados do desktop entry quando existirem;
- atividade recente e persistente do processo, em vez de um único pico de CPU.

Cada sinal recebe uma força diferente. Categoria `Game` ou um nome conhecido
sozinho nunca confirma nada: são apenas indícios fracos. A confirmação exige
evidência suficiente e coerente, normalmente combinando foco/fullscreen com uma
árvore ou runtime de jogo. Quando a evidência é insuficiente, o estado fica
`unknown` ou `ordinary app` e nenhuma política agressiva é aplicada.

As razões da decisão ficam disponíveis nas configurações. Isso torna o modo
auditável e permite desligá-lo quando um aplicativo comum for classificado de
forma incorreta.

## O que muda ao ativar

No modo automático, o Caelune guarda em uma estrutura limitada o perfil que
estava ativo e aplica apenas mudanças reversíveis do shell:

- reduz ou remove blur, refração, transparência e animações não essenciais;
- pausa consultas em segundo plano de superfícies ocultas;
- agrupa atualizações visuais não urgentes e limita trabalho de widgets;
- usa a política de renderização mais econômica disponível, mantendo Vulkan,
  OpenGL e renderização opaca/software como fallback;
- conserva entrada, áudio, notificações críticas, segurança, lock screen e
  composição das janelas.

O “cache temporário” é um snapshot pequeno das preferências e tarefas do
Caelune. Ele não é um despejo de memória de aplicativos, não grava dados
secretos no disco e não desloca o jogo para swap. Ao detectar a saída ou perda
do jogo, o snapshot é aplicado uma vez e descartado. Se algo falhar, o estado
fica visível e o usuário pode restaurar ou revisar manualmente.

## Controles

- **Desligado:** apenas observa, sem alterar o perfil.
- **Automático:** aplica a política somente depois de uma detecção com
  confiança suficiente e restaura ao terminar.
- **Ligado:** o usuário autoriza o perfil econômico; a evidência continua
  visível e a restauração permanece reversível.

O Game Mode não altera serviços do sistema, não suspende aplicativos aleatórios,
não modifica swapfile automaticamente e não envia `SIGKILL` para processos de
terceiros. Ações que exigem privilégio continuam sendo explícitas nas
configurações.

## Limites honestos

A sessão nativa precisa ser validada em Linux com DRM/KMS, libseat/logind,
libinput, XWayland e diferentes drivers. No modo nested, a detecção só enxerga
as informações que o host fornece. Desempenho em Celeron, GPUs integradas e
hardware high-end deve ser medido no dispositivo real; os limites do Game Mode
são uma política conservadora, não uma promessa de FPS.
