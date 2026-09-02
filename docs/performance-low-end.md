# Liquid Glass e desempenho em PC fraco

Este documento registra a política de performance do Rouch para o tema Liquid
Glass inspirado na hierarquia do macOS Tahoe: material translúcido fica nos
ancoradores do shell (top bar, dock e sheets/overlays), enquanto conteúdo de
aplicativo e chrome estrutural podem permanecer opacos para leitura e
previsibilidade.

## O que é medido e o que é proxy

Os limites abaixo são **INFERRED**: são guardrails de fill-rate, memória e
cadência definidos antes de existir uma medição de GPU por máquina. Eles não
significam que o Rouch entrega um FPS específico em todo hardware.

| Perfil | DPR máximo | Backdrop amostrado por frame | Cache de backdrop | Animação | Cadência alvo |
| --- | ---: | ---: | ---: | ---: | ---: |
| Balanced | 1.5x | 1.048.576 px | 8 MiB | 360 ms | VSync/60 |
| Visual | 1.5x | 1.048.576 px | 8 MiB | 360 ms | VSync/60 |
| LowEnd | 1.0x | 0 px | 0 B | 0 ms | VSync/30 |
| BatterySaver | 1.0x | 0 px | 0 B | 0 ms | VSync/30 |

`Quarter` significa um quarto da resolução em cada dimensão, portanto cerca
de 1/16 dos pixels; `Half` significa metade em cada dimensão, portanto cerca
de 1/4 dos pixels. O proxy de memória considera RGBA e um segundo scratch
surface para o blur limitado. A decisão vive em `MaterialBudget` e
`RenderPolicy`, sem alocações durante o cálculo.

## Regras do Liquid Glass

- Top bar, dock, central de notificações, Control Center e chrome da galeria
  podem usar backdrop limitado e tint frio.
- Conteúdo de aplicativos nunca recebe blur automático.
- Window chrome não amostra backdrop; usa tint/borda opacos para manter texto e
  controles legíveis.
- Transparência reduzida, LowEnd, BatterySaver, cache excedido ou output
  grande degradam a superfície inteira para uma receita opaca. A hierarquia é
  preservada por borda, contraste, espaçamento e seleção cyan, não por glow.
- Refração fica restrita ao perfil Visual e às sheets/overlays aprovados; não
  existe drift contínuo da wallpaper.
- O cache é por output/superfície aprovada e tem limite explícito. O renderer
  deve invalidá-lo quando o output ou o conteúdo sob a sheet mudar.

## Redraw, VSync e animações

`RenderPolicy::should_request_redraw` só permite novo frame quando há dano
visível ou uma animação visível ainda dentro do limite. Uma sheet escondida ou
uma janela fora do workspace não deve manter loop de desenho. A cadência é
VSync-paceda; perfis LowEnd/BatterySaver acrescentam o teto de 30 Hz.

As animações são curtas, interrompíveis e podem virar troca imediata de estado
no perfil LowEnd. O limite de 360 ms é um proxy de custo/continuidade para o
perfil padrão, não uma medição de latência.

## Swapfile sem gargalo

O swapfile é opcional. A recomendação padrão usa aproximadamente metade da
RAM, com limite de 1 a 8 GiB; mais swap não cria RAM e pode apenas prolongar
thrashing. O tamanho não é aplicado automaticamente e um arquivo existente
com tamanho diferente é bloqueado para revisão manual.

O planner escolhe a criação conforme o filesystem observado: ext4 e XFS usam
prealocação; filesystem desconhecido usa `dd` com `/dev/zero`, sem holes;
Btrfs cria um arquivo vazio, aplica `chattr +C` e só então aloca os blocos. A
assinatura `mkswap` exige confirmação explícita. O executor privilegiado deve
revalidar caminho, symlink, proprietário, tamanho e filesystem antes de cada
ação.

## Caminho gráfico e limitações reais

**VERIFIED:** `src/graphics.rs` seleciona Vulkan primeiro, OpenGL depois e
Software/opaque por último, registra probes e representa `ContextLost` e
`Recovering`. Também há testes para fallback, perda de contexto e recuperação.

**VERIFIED:** a política visual tem testes para 1080p, 4K, DPR, cache,
transparência reduzida, perfil LowEnd e redraw de superfícies invisíveis.

**INFERRED:** os limites de pixels/cache são proxies conservadores; ainda não
há telemetria de frame time, uso de VRAM ou comparação antes/depois em Mint,
Ubuntu e Arch.

**UNVERIFIED:** o nested atual do Smithay/Winit continua usando
`GlesRenderer`/OpenGL/EGL. A política é Vulkan-first e está pronta para um
probe/renderizador Vulkan real, mas não transforma essa superfície GLES em
Vulkan por configuração. A integração nativa deve provar criação da surface,
swapchain, context-loss e fallback no hardware de cada distro.

## Checklist de benchmark futuro

Executar em Mint, Ubuntu e Arch, em GPU integrada e em modo software:

1. Capturar frame time p50/p95, redraws por segundo, RSS e VRAM em idle.
2. Repetir com galeria, 20 notificações, três janelas, sheet aberta e uma
   animação interrompida por Alt-Tab.
3. Repetir em 1x/1.5x/2x, 1920x1080 e 3840x2160.
4. Forçar Vulkan indisponível, OpenGL indisponível, context-loss e
   transparência reduzida; confirmar que a tela continua utilizável e que o
   diagnóstico explica a degradação.

Esses resultados devem substituir os proxies somente quando forem coletados
no produto real; não se deve converter um screenshot ou um único FPS em uma
promessa geral de performance.
