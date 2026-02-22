# Especificação Técnica e de Produto: Desktop Modular Expandível (MVP)

**Versão:** 1.0
**Status:** Produção-Ready
**Responsável:** Jules (Senior Product Designer & System Architect)

---

## 1. Modelo Conceitual do Desktop

O Desktop Atom OS baseia-se na metáfora de **"Superfície de Trabalho Funcional"**. Diferente de sistemas focados em estética decorativa, este modelo prioriza a densidade de informação, previsibilidade e baixo overhead cognitivo.

### 1.1 Metáfora e Estados Globais
*   **Superfície Base:** Um plano infinito (virtualmente) onde janelas são organizadas. No MVP, limitado à resolução física do framebuffer.
*   **Estado Normal:** Interação livre com janelas e ícones.
*   **Estado de Foco Único (Modal):** Quando um diálogo de sistema ou aplicação exige atenção exclusiva, bloqueando a interação com a camada inferior.
*   **Estado de Visão Geral (Alt+Tab):** Uma interrupção temporária para navegação rápida entre contextos.

### 1.2 Hierarquia de Janelas e Entrada
*   **Hierarquia:** Camada de Fundo (Wallpaper/Ícones) < Camada de Aplicações (Windows) < Camada de Sobreposição (Menus/Tooltips) < Camada de Sistema (Cursores/Notificações Críticas).
*   **Gerenciamento de Entrada:**
    *   **Mouse:** Modelo *Click-to-Focus*. O clique em qualquer parte da janela a traz para o topo do Z-order.
    *   **Teclado:** Eventos direcionados estritamente à janela com foco ativo.
    *   **Captura:** Suporte a captura de mouse para operações de drag-and-drop e redimensionamento.

---

## 2. Sistema de Janelas (Window Manager)

O Window Manager (WM) é integrado ao Compositor para garantir latência mínima e evitar "window tearing".

### 2.1 Estrutura Visual
*   **Bordas:** Moldura sólida de 1px (Cor: `WINDOW_BORDER`).
*   **Cabeçalho (Title Bar):** Altura fixa de 32px.
*   **Estados:**
    *   **Ativo:** Cabeçalho com cor de destaque (`WINDOW_HEADER_FOCUSED`), título legível.
    *   **Inativo:** Cabeçalho em tom neutro (`WINDOW_HEADER`), opacidade total (sem transparência).
*   **Controles:** Três botões circulares ou quadrados (14x14px) no canto superior direito:
    *   Fechar (Ação: Terminate)
    *   Maximizar/Restaurar (Ação: Resize para Work Area)
    *   Minimizar (Ação: Ocultar e manter na Barra de Apps)

### 2.2 Regras de Layout e Snapping
*   **Snapping Inteligente:** Ao arrastar uma janela para as bordas da tela:
    *   Laterais: Ocupa 50% da largura (Split Screen).
    *   Cantos: Ocupa 25% (Quadrantes).
*   **Redimensionamento:** Permitido em todas as bordas e cantos, com limite mínimo de 120x80px.

### 2.3 Navegação por Teclado
*   **Alt+Tab:** Alternador linear que percorre o Z-order das janelas. Exibe uma lista centralizada com ícones e títulos.
*   **Teclas de Atalho:** `Super+Seta` para snapping rápido.

### 2.4 Contrato Técnico WM <> Aplicações
*   **Eventos:** O WM envia `WmWindowEventMsg` (Focus, Unfocus, Resize, Close).
*   **Resize:** O WM desaloca a superfície antiga e envia um `SurfaceAssignMsg` com a nova região de memória compartilhada. A aplicação deve redesenhar conforme as novas dimensões.
*   **Repaint:** Baseado em `SurfacePresentMsg`. O WM só atualiza a tela quando a aplicação sinaliza que o frame está pronto.

---

## 3. Barra Inferior (Barra de Tarefas)

Dividida em três zonas funcionais distintas, com altura fixa de 48px a 60px.

### 3.1 Zona 1: Comando (Esquerda)
*   **Campo Unificado:** Não é apenas um botão "Iniciar", mas um campo de entrada de texto permanente ou acionável.
*   **Parsing e Priorização:**
    1.  *Comandos de Sistema:* (ex: `shutdown`, `reboot`).
    2.  *Aplicações:* Busca por nome no diretório `/bin`.
    3.  *Arquivos:* Busca recente.
    4.  *Cálculos:* Avaliação matemática simples on-the-fly.

### 3.2 Zona 2: Aplicativos (Centro)
*   **Modelo de Pinagem:** Apps fixos à esquerda, apps abertos à direita.
*   **Indicadores de Atividade:** Ponto ou linha de 2px abaixo do ícone para janelas abertas; destaque extra para a janela focada.
*   **Overflow:** Se o número de ícones exceder o espaço, ativa-se o scroll horizontal suave (passo fixo, sem aceleração).

### 3.3 Zona 3: Sistema (Direita)
*   **Elementos:** Relógio (HH:MM), Indicador de Rede, Volume.
*   **Notificações:** Pequenos ícones contadores. Ao clicar, abre uma fila vertical acima da barra.
*   **Modelo de Fila:** Notificações não invasivas desaparecem após 5 segundos, mas permanecem na fila de histórico.

---

## 4. Área de Trabalho (Desktop Workspace)

### 4.1 Modelo de Ícones
*   **Arquivos Reais:** O Desktop reflete o conteúdo de uma pasta específica (ex: `/home/user/Desktop`).
*   **Alinhamento:** Grid invisível (ex: 80x80px) com alinhamento automático inteligente que evita sobreposição de ícones.

### 4.2 Interação
*   **Seleção:** Suporte a retângulo de seleção (Marquee Tool).
*   **Menu Contextual:** Acionado por botão direito, oferece: Novo Arquivo, Organizar Ícones, Alterar Papel de Parede, Abrir Terminal aqui.

---

## 5. Diretrizes Visuais Formais

### 5.1 Sistema de Cores (Paleta Base)
*   **Fundo:** Dark Grey (`#1E2128`)
*   **Superfícies:** Darker Grey (`#282C34`)
*   **Destaque (Accent):** Configurável pelo usuário (Padrão: Frost Blue `#88C0D0`).
*   **Texto:** High Contrast Silver (`#DCDFE4`) para leitura, Grey para metadados.

### 5.2 Tipografia
*   **Interface:** Fonte Sans-Serif de largura fixa ou variável altamente legível (ex: Inter ou similar embutida).
*   **Tamanho Base:** 12px para sistema, 14px para títulos.

### 5.3 Proibições Explícitas
*   **Sem Glassmorphism / Blur:** Todas as superfícies são opacas.
*   **Sem Transparência Alpha:** Exceto para o cursor do mouse e ícones anti-aliased.
*   **Sem Animações Contínuas:** Animações permitidas apenas para feedback de estado (ex: clique) e transições curtas (<200ms).

---

## 6. Base Técnica e Renderização

### 6.1 Arquitetura do Compositor 2D
*   **Double Buffering:** O compositor mantém um backbuffer do tamanho da tela.
*   **Dirty Regions:** O redesenho não ocorre na tela inteira. O WM rastreia regiões "sujas" (janelas que pediram `present` ou movimentação do cursor) e blita apenas essas áreas.
*   **Z-Order Management:** Lista encadeada de estruturas de janela ordenadas da base para o topo.

### 6.2 Pipeline de Renderização
1.  Limpar regiões sujas no Backbuffer com a cor do Desktop/Wallpaper.
2.  Blitar janelas da mais profunda para a mais superficial (respeitando clip do dirty region).
3.  Desenhar sobreposições de sistema (menus).
4.  Desenhar cursor.
5.  Blit atômico do Backbuffer para o Framebuffer de hardware (VGA/GOP).

### 6.3 Requisitos de Hardware (Mínimos)
*   **GPU:** Não requerida. Renderização puramente via CPU (Software Rendering).
*   **CPU:** x86_64 ou ARM64 com suporte a instruções SIMD (SSE/Neon) para otimização de blit.
*   **Memória:** ~32MB para o subsistema gráfico (incluindo buffers de tela 1080p).

---

## 7. Estrutura de Evolução em Camadas

*   **Camada 1 (MVP):** Desktop clássico funcional, janelas empilháveis, barra de tarefas básica.
*   **Camada 2 (Produtividade):** Suporte a desktops virtuais, busca global avançada, suporte a temas (cores).
*   **Camada 3 (Inteligência):** "Ambientes Restauráveis" (salvar o estado de todas as janelas abertas e reabri-las após o reboot), agrupamento automático de janelas por tarefa.

**Garantia de Compatibilidade:** O SDK (`libgui`) utiliza abstrações de superfície (`SharedSurface`). Mudanças no compositor (ex: adição de aceleração futura) não devem alterar o contrato de desenho da aplicação.

---

## 8. Escopo do MVP

### O que entra:
*   Compositor 2D com suporte a múltiplas janelas.
*   Window Manager com movimentação, fechamento e maximização.
*   Barra de tarefas com relógio, campo de comando (lançador) e ícones de apps.
*   Sistema de eventos IPC (teclado/mouse).
*   Biblioteca cliente (`libgui`) básica.

### O que NÃO entra:
*   Aceleração por hardware (GPU).
*   Efeitos de transparência ou blur.
*   Suporte multi-monitor.
*   Sistema de temas dinâmico (além da cor de destaque).

---

## 9. Requisitos Não-Funcionais

*   **Performance:** Abertura de janela em < 100ms; Redesenho do cursor a 60 FPS estáveis.
*   **Consistência:** Comportamentos de janelas e menus devem ser idênticos em todo o sistema.
*   **Previsibilidade:** Nenhuma janela deve mudar de posição ou tamanho sem ação explícita do usuário ou regra de snapping clara.
*   **Robustez:** A falha de uma aplicação GUI não deve derrubar o compositor (isolamento via IPC).
