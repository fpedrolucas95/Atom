# Atom OS — Roadmap Técnico

> **Status do Projeto**: Microkernel funcional com IPC, capabilities, user space, ambiente desktop, libc, OpenGL por software e suporte a aplicações nativas C/Rust  
> **Última atualização**: 04 de Março de 2026  
> **Release mais recente**: alpha_4 (`3b85b99`)  
> **Arquitetura**: x86_64

---

## Concluído (até alpha_4)

### Fases 1–6: Fundação do Kernel

- [x] Boot UEFI em x86_64 com transição para kernel
- [x] Physical Memory Manager — bitmap allocator com bootstrap em duas fases (bitmap estático → dinâmico), autoproteção das páginas do bitmap, suporte a até 16 GiB
- [x] Virtual Memory Manager — paging 4-level, deep-copy de page tables com passo de verificação/reparo, rastreamento de VMAs com guard pages e backing types (anonymous, stack, device)
- [x] Heap allocator do kernel (alocações pequenas via slab + fallback para páginas)
- [x] Tratamento de interrupções/exceções (IDT, APIC Local, preempção por timer)
- [x] Context switching em assembly x86-64 — trampoline higher-half, validação de stack canaries, verificação de endereços canônicos em RIP/RSP, validação de seletores CS/SS, enforcement de consistência de CR3
- [x] Scheduler preemptivo por prioridade com round-robin dentro de cada nível
- [x] Subsistema de IPC — portas, mensagens, síncrono/assíncrono, batching, wait_any, detecção real de ciclo em deadlocks, priority inheritance, wait queues, zero-copy via memória compartilhada
- [x] Sistema de capabilities — acesso via handles, bitflags de permissão, derivação, revogação transitiva, audit log
- [x] Init process totalmente isolado com PML4 próprio
- [x] Service manager com boot declarativo
- [x] ~80 syscalls (threads, IPC, capabilities, memória compartilhada, filesystem, vídeo, spawn de processos)
- [x] Logging estruturado e observabilidade (serial, debugcon, tags por subsistema)
- [x] Gerenciador de memória compartilhada com alocação dinâmica de janela VA e cleanup no exit do processo dono

### Fase 6.5: Stack de User Space (alpha_3 → alpha_4)

- [x] libc freestanding — string, stdlib, stdio, ctype, errno, assert, math (FPU x87), unistd, time, crt0.S, malloc/free via mmap/munmap, vsnprintf completo
- [x] Port do TinyGL 0.4.1 — biblioteca estática freestanding, bridge de blit RGB565→ARGB32, integrado com o compositor
- [x] Port do Doom via doomgeneric — janela de 640×400 no compositor, mapeamento completo de teclado PS/2, valida toda a pipeline de gráficos/IPC/compositor
- [x] Lançamento de aplicações em runtime — syscall `SYS_SPAWN_FROM_PATH`, loader ATXF, serviço privilegiado app_launcher, lançamento por duplo-clique no gerenciador de arquivos
- [x] Driver Bochs Graphics Adapter — troca de resolução em runtime, tabela de 12 modos de vídeo, aplicação Display Settings
- [x] Rasterizador SVG (no_std) — rect, circle, ellipse, polygon, polyline, line, path (M/L/H/V/Z/C/S/Q/A), fill/stroke, transformações de grupo, seletores CSS, cores nomeadas
- [x] Ambiente desktop — compositor com superfícies compartilhadas, dock em formato de pílula, retângulos arredondados, alpha blending, sombras suaves, controles circulares de janela, ícones SVG, shutdown gracioso de janelas (estado PendingClose)
- [x] Layout de filesystem reorganizado — `/system/services`, `/apps/system`, `/apps/user`, `/user/home`, `/user/config`, `/user/data`
- [x] Crate atom_abi compartilhado como fonte única de verdade para tipos e constantes entre kernel e user space

---

## Fase 7: Hardening de Segurança e Robustez

*Objetivo: fechar as lacunas estruturais de segurança antes de adicionar novas funcionalidades. A infraestrutura para tudo isso já existe — esta fase consiste em conectar o enforcement em todos os caminhos de código.*

**Prioridade: CRÍTICA** — Essas lacunas comprometem o modelo de segurança do microkernel.

### 7.1 Tabela de File Descriptors por Processo

**Problema**: `KERNEL_FD_TABLE` é uma tabela estática global compartilhada por todos os processos, com 128 slots. Sem rastreamento de propriedade — qualquer processo pode fechar, ler ou acessar file descriptors de outro processo. `sys_fs_close` verifica apenas se o slot está `in_use`, não se o chamador é o dono.

- [ ] Adicionar campo `owner_pid` (ou `owner_pml4`) ao `KernelFd`
- [ ] Definir owner no `alloc_kernel_fd`
- [ ] Verificar propriedade em todas as operações de FD: close, read, seek, readdir, fstat
- [ ] Retornar `EPERM` em caso de mismatch de propriedade
- [ ] Limpar todos os FDs pertencentes a um processo na terminação

**Esforço**: Baixo — mudança estrutural em uma tabela + verificações em ~5 syscalls.  
**Impacto**: Corrige a falha de isolamento mais fundamental do sistema.

### 7.2 Validação de Ponteiros de User Space nas Syscalls Legadas

**Problema**: Syscalls mais recentes (filesystem, spawn) validam ponteiros de user space corretamente via `validate_user_pointer` (verificação de range canônico) e `write_buffer_to_user` (start + end + overflow + cap de 64 MB). Porém, syscalls mais antigas (mouse, keyboard, framebuffer, debug_log) fazem apenas null check. Um processo malicioso pode passar um ponteiro de kernel space e o kernel escreverá nele — escalação de privilégios.

Syscalls vulneráveis:
- `sys_mouse_poll` — apenas null check, escreve diretamente via raw pointer
- `sys_keyboard_poll` — mesmo padrão
- `sys_get_framebuffer` — escreve 5 valores u64 sem verificação canônica
- `sys_debug_log` — lê de ponteiro de usuário sem verificação canônica

- [ ] Adicionar chamadas a `validate_user_pointer` em todas as syscalls legadas listadas acima
- [ ] Adicionar `validate_user_buffer(ptr, len)` para operações de leitura (debug_log)
- [ ] Auditar todas as syscalls restantes para dereferences de ponteiro de usuário não validados
- [ ] Longo prazo: implementar `copy_from_user` / `copy_to_user` com verificação de page-walk (não apenas verificação canônica)

**Esforço**: Muito baixo — os helpers já existem, é trabalho mecânico.  
**Impacto**: Elimina o vetor de escalação de privilégios mais óbvio.

### 7.3 Enforcement de Capabilities em Todas as Syscalls

**Problema**: A infraestrutura de capabilities está completa (handles, permissões, derivação, revogação transitiva, audit log, `validate_thread_capability_by_type`). Mas o enforcement é inconsistente:

- **Padrão A (correto)**: `sys_thread_create` e `sys_ipc_send_with_cap` verificam capabilities e retornam `EPERM` em caso de falha
- **Padrão B (bypass MVP)**: `sys_map_region`, `sys_unmap_region`, `sys_remap_region` verificam capabilities mas logam um aviso e continuam mesmo assim ("proceeding anyway (MVP)")
- **Padrão C (código morto)**: `validate_required_capability` aceita `_resource_type` (não utilizado) e sempre retorna `Ok(caller)`
- Acesso a portas I/O usa allow-list hardcoded em vez de capabilities

- [ ] Alterar syscalls do Padrão B para retornar `EPERM` em vez de "proceeding anyway"
- [ ] Remover `validate_required_capability` (Padrão C) — é código morto que cria falsa sensação de enforcement
- [ ] Adicionar verificações de capability em `sys_io_port_read` / `sys_io_port_write` usando `DeviceCap` em vez de allow-list hardcoded
- [ ] Auditar `sys_cap_check` — atualmente só verifica se total de capabilities > 0, não se uma capability específica é possuída
- [ ] Remover concessão automática de `FramebufferCap` / `InputCap` para todos os processos; apenas ui_shell recebe InputCap, apenas o display driver recebe FramebufferCap
- [ ] Enforçar: apps recebem apenas capabilities explicitamente delegadas no momento do spawn

**Esforço**: Baixo-médio — a infraestrutura funciona, é enforcement de política.  
**Impacto**: Transforma o sistema de capabilities de "implementado mas não enforçado" para "modelo de segurança operacional."

### 7.4 Abstração de Processo

**Problema**: Não existe `struct Process`. Tudo é `Thread`. Isso causa:
- Tabela de FDs global (sem dono)
- Mapas de VMA indexados por endereço físico do PML4, não por processo
- Propriedade de address spaces rastreada por ThreadId
- Cleanup de memória compartilhada itera todos os threads para achar siblings (mesmo PML4 = "mesmo processo")
- Terminação de entidade requer pipeline de 10 passos que reconstrói implicitamente os limites do processo

- [ ] Introduzir struct `Process` consolidando: pid, pml4, lista de threads, tabela de FDs, tabela de capabilities, propriedade de VMAs
- [ ] Refatorar criação/terminação de threads para operar através de Process
- [ ] Simplificar o pipeline de terminate_entity tornando a propriedade de recursos explícita
- [ ] Habilitar semânticas futuras: fork/exec, limites de recursos por processo, grupos de processos

**Esforço**: Médio — refatoração estrutural em thread.rs, camada de syscalls e IPC.  
**Impacto**: Simplifica gerenciamento de recursos, habilita cleanup mais limpo, e é pré-requisito para SMP e fork/exec.

### 7.5 Terminação Robusta de Processos

- [ ] Implementar `sys_thread_exit` completo
- [ ] No exit do processo: liberar todos os frames de memória, revogar todas as capabilities, fechar todas as portas IPC, fechar todos os FDs, desalocar PML4 e page tables, remover da lista global de threads
- [ ] Verificar zero vazamentos de memória via harness de teste
- [ ] Tratar edge cases: último thread de um processo, threads bloqueados em IPC no momento da terminação

### 7.6 Atomicidade do IPC Blocking

**Problema**: Em `sys_ipc_recv`, registrar como receiver bloqueado, mudar estado do thread para `Blocked` e ceder para o scheduler são três operações separadas. Em single-core com interrupções mascaradas durante syscall isso funciona, mas é uma race condition latente que vai quebrar sob SMP: outro core poderia executar `send()` entre os passos 1 e 2, chamar `mark_thread_ready()`, e o passo 2 sobrescreveria o estado de volta para `Blocked`.

- [ ] Mover `set_thread_state(Blocked)` para dentro de `block_receive`, protegido pelo mesmo lock
- [ ] Mesma correção para `sys_ipc_wait_any`
- [ ] Auditar ordenação de locks em `close_all_thread_ports` (dropa e re-adquire lock de portas no meio da iteração)
- [ ] Adicionar flag atômica "expecting block" como alternativa, verificada por `mark_thread_ready`

**Esforço**: Baixo.  
**Impacto**: Previne uma classe de bugs que apareceria assim que SMP fosse habilitado.

---

## Fase 8: SMP (Symmetric Multiprocessing)

*Objetivo: habilitar execução multi-core. Sistemas modernos exigem paralelismo.*

**Prioridade: ALTA** — bloqueada pela Fase 7.4 (abstração de processo) e 7.6 (atomicidade de IPC).

### 8.1 Detecção e Bootstrap de CPUs

- [ ] Parsing de ACPI MADT — identificar número de CPUs, APIC IDs, endereço base do Local APIC
- [ ] Startup de APs (Application Processors) via sequência SIPI
- [ ] GDT, IDT, TSS, stacks IST por CPU

### 8.2 Estruturas Per-CPU

- [ ] Dados per-CPU via MSR `GS_BASE` — struct `CpuLocal` com ID da CPU, thread atual, idle thread
- [ ] Stacks de interrupção per-CPU (IST)
- [ ] Decidir: filas de ready per-CPU vs fila global com lock (começar com fila global para 2–4 cores)

### 8.3 Sincronização Kernel SMP-Safe

- [ ] Auditar todos os `Mutex` / `RwLock` no kernel — lista de threads, tabela de capabilities, filas de IPC, bitmap do PMM, tabela de FDs
- [ ] Verificar uso correto de operações atômicas e ordenação de memória (acquire/release)
- [ ] Decidir: contador global de ticks (atômico) vs arrays de ticks per-CPU
- [ ] Garantir que timestamps de IPC permaneçam coerentes entre cores

### 8.4 Scheduler SMP

- [ ] Load balancing — atribuição round-robin de novos threads entre cores (MVP)
- [ ] Migração de threads entre cores quando desbalanceado
- [ ] Idle threads per-CPU

### 8.5 Validação

- [ ] Teste de stress: N threads em M cores (N >> M)
- [ ] Verificar ausência de race conditions, deadlocks, starvation
- [ ] Validar que o scheduler distribui carga entre cores
- [ ] Rodar userspace completo (compositor + apps) em multi-core

---

## Fase 9: Maturidade do Filesystem

*Objetivo: filesystem robusto com suporte a escrita, FDs por processo e abstração VFS.*

### 9.1 Camada VFS

- [ ] Abstração de Virtual File System com mount table
- [ ] Resolução de paths (`/mnt/disk/arquivo.txt`)
- [ ] Registro de filesystems (FAT32 inicialmente, ext2 ou outro depois)

### 9.2 FAT32 Leitura-Escrita

- [ ] Suporte a escrita: create, write, mkdir, delete
- [ ] Cache de blocos em memória
- [ ] Journaling ou ao menos semântica de fsync para segurança contra crashes

### 9.3 Tabela de FDs por Processo (migrada do global do kernel)

- [ ] Array de file descriptors por processo (0=stdin, 1=stdout, 2=stderr)
- [ ] Herança de FDs no spawn
- [ ] Syscalls `dup`, `dup2`, `pipe`

---

## Fase 10: Drivers em User Space

*Objetivo: migrar drivers remanescentes no kernel para userland, completando o modelo microkernel.*

### 10.1 Driver de Dispositivo de Bloco (AHCI → userland)

- [ ] Portar lógica AHCI para processo em user space
- [ ] Kernel concede `DeviceCap(BDF)` + `IRQCap`
- [ ] Driver mapeia MMIO do controlador via syscall
- [ ] Protocolo de requisição/resposta de blocos via IPC

### 10.2 Servidor de Filesystem (userland)

- [ ] Servidor `fs_fat32` lê partições via IPC com driver de bloco
- [ ] VFS delega operações → IPC → fs_fat32
- [ ] Apps usam syscalls padrão de forma transparente

### 10.3 Device Manager

- [ ] Serviço `device_manager` — recebe lista de dispositivos PCI do kernel, mantém mapa BDF → driver, faz spawn de drivers sob demanda
- [ ] Capabilities por dispositivo: kernel cria `DeviceCap(BDF)` por dispositivo PCI, manager delega ao driver correto
- [ ] Hotplug USB: driver xHCI notifica device_manager, manager identifica tipo do dispositivo, faz spawn do driver apropriado

---

## Fase 11: Rede

- [ ] Driver NIC em userland (VirtIO-net para QEMU, Intel E1000 para suporte mais amplo)
- [ ] Stack TCP/IP como serviço `netd` (portar smoltcp ou lwIP, ou implementar stack mínima)
- [ ] Interface de sockets para apps via IPC com netd: `socket()`, `bind()`, `connect()`, `send()`, `recv()`
- [ ] Serviço de resolução DNS

---

## Fase 12: Desktop Multi-Janelas

*Objetivo: desktop multi-janelas real com apps concorrentes.*

Muito disso já funciona (compositor, superfícies compartilhadas, criação de janela via IPC, Z-order). Trabalho restante:

### 12.1 Gerenciamento de Janelas

- [ ] Minimizar, maximizar, redimensionar, arrastar janelas
- [ ] Alternador de aplicações (Alt-Tab)
- [ ] Serviço de clipboard (copiar/colar entre apps)

### 12.2 Expansão da libGUI

- [ ] Biblioteca de widgets: botões, labels, input de texto, scroll bars, menus
- [ ] Primitivas de layout (empilhamento horizontal/vertical, grid)
- [ ] Event loop padrão para apps

### 12.3 Threads de Usuário

- [ ] `sys_thread_create` para user space — address space compartilhado, stack separado
- [ ] `sys_thread_join(tid)` — espera conclusão do thread
- [ ] Validar: app com thread de UI + thread worker, worker faz computação pesada sem travar a UI

---

## Fase 13: Áudio

- [ ] Driver de áudio (AC97 ou Intel HDA) em user space
- [ ] Serviço de mixer (`audiod`) — mistura streams de múltiplos apps, controle de volume por app
- [ ] API de playback de áudio para apps via IPC

---

## Fase 14: OS Composable — Perfis de Sistema

*Objetivo: aproveitar o microkernel orientado a serviços para suportar diferentes personalidades de sistema a partir do mesmo kernel.*

A arquitetura já suporta isso — o kernel fornece infraestrutura (memória, scheduling, IPC, capabilities), e toda a política vive no user space. Perfis diferentes são composições diferentes de serviços de user space.

### 14.1 Carregador de Perfis

- [ ] Arquivos de configuração de perfil em `/system/profiles/` (TOML ou similar)
- [ ] `init` lê o perfil e inicia apenas os serviços/shell especificados
- [ ] Seleção no boot: `--profile=desktop`, `--profile=tv`, `--profile=kiosk`, `--profile=embedded`
- [ ] Conjuntos de capabilities definidos por perfil (quais serviços recebem quais capabilities)

### 14.2 Perfil Desktop

O padrão atual — compositor, ui_shell, terminal, gerenciador de arquivos, apps de uso geral.

### 14.3 Perfil TV / HTPC (UI 3-foot)

- [ ] `tv_shell` — launcher fullscreen com carrosséis horizontais, elementos grandes, texto mínimo
- [ ] Modo TV do compositor — superfícies fullscreen + overlays de sistema (volume, notificações), sem modo de janelas
- [ ] Input de D-pad / controle remoto — navegação espacial com árvore de foco, KEY_UP/DOWN/LEFT/RIGHT/OK/BACK/HOME
- [ ] Serviço de mídia — pipeline de decodificação de vídeo, controles play/pause/seek, saída de superfície para o compositor
- [ ] Runtime de apps HTML5 (opcional) — web engine leve (WPE WebKit ou Servo) para web apps, com bridge JS↔Rust para APIs do sistema
- [ ] Formato de manifesto de app para web apps (nome, entry point, permissões, ícone)

### 14.4 Perfil Kiosk

- [ ] Modo single-app — boot diretamente em uma aplicação especificada
- [ ] Conjunto restrito de capabilities — sem acesso a filesystem, sem spawn, sem rede (a menos que explicitamente concedido)
- [ ] Sem desktop shell, sem launcher

### 14.5 Perfil Embedded / Headless

- [ ] Sem display driver, sem compositor, sem UI
- [ ] Apenas serviços: rede, filesystem, daemons específicos da aplicação
- [ ] Footprint mínimo de memória
- [ ] Adequado para roteadores, dispositivos IoT, appliances

---

## Fase 15: Port para ARM64

- [ ] Boot UEFI em ARM64
- [ ] Setup de MMU (TTBR0/TTBR1, páginas de 4KB)
- [ ] GIC (Generic Interrupt Controller)
- [ ] Context switching para ARM64
- [ ] Validação em QEMU aarch64 e Raspberry Pi 4

---

## Fase 16: Otimizações Avançadas

- [ ] Huge Pages (2 MiB, 1 GiB) para kernel e apps
- [ ] PCID (Process Context IDs) para evitar TLB flush em context switch
- [ ] SMEP/SMAP para hardening do kernel
- [ ] Upgrade do scheduler (CFS ou BFS para substituir round-robin simples)
- [ ] Alocação de memória NUMA-aware

---

## Fase 17: Auditoria de Segurança

- [ ] ASLR (Address Space Layout Randomization) para processos de user space
- [ ] Stack canaries em user space
- [ ] Auditoria completa de código `unsafe` no kernel
- [ ] Fuzzing de syscalls (AFL, libFuzzer, ou harness customizado)
- [ ] Verificação formal de componentes críticos (enforcement de capabilities, invariantes de IPC) via TLA+ ou similar

---

## Decisões Arquiteturais

### Tomadas

- Microkernel com drivers em user space
- Controle de acesso baseado em capabilities
- IPC message-passing com caminhos zero-copy via memória compartilhada
- Scheduler por prioridade com round-robin por nível
- SYSCALL/SYSRET para transições rápidas user↔kernel
- ATXF como formato binário de user space
- Crate atom_abi compartilhado para ABI kernel↔userspace
- User space orientado a serviços (init → service_manager → namesvc → serviços)

### Em Aberto

- **Filesystem**: continuar com FAT32 ou portar ext2/ext4?
- **Stack de rede**: smoltcp vs lwIP vs customizado?
- **Scheduler SMP**: fila global vs filas per-CPU?
- **Web runtime para perfil TV**: WPE WebKit vs Servo vs renderer de subconjunto HTML customizado?
- **Modelo de processos**: processos single-threaded apenas, ou fork/exec estilo POSIX completo?

---

## Métricas de Sucesso

### Alcançadas
- [x] Boot em < 5s (QEMU)
- [x] Latência IPC < 10μs (média)
- [x] Suporte a aplicações C nativas (libc + crt0)
- [x] Renderização OpenGL por software (TinyGL)
- [x] Aplicação real rodando (Doom)
- [x] Lançamento de aplicações em runtime a partir do filesystem
- [x] Troca dinâmica de resolução de display

### Próximos Alvos
- [ ] Todas as syscalls enforçam verificações de capabilities
- [ ] Zero acesso cruzado de FDs entre processos
- [ ] Todos os ponteiros de usuário validados antes de dereference no kernel
- [ ] SMP: estável em 2–4 cores
- [ ] Rede: requisição HTTP a partir de app em user space
- [ ] 1000+ processos concorrentes sem kernel panic
- [ ] Teste de stress de 24h com zero panics

---

## Contribuindo

Contribuições são bem-vindas, especialmente em:

- **Hardening de segurança** — enforcement de capabilities, validação de ponteiros de usuário, isolamento de FDs
- **Documentação** — protocolo IPC, modelo de capabilities, referência de syscalls, layout de memória, formato ATXF
- **Testes** — smoke tests automatizados em QEMU, CI, fuzzing de syscalls
- **Serviços de user space** — novos drivers, melhorias de filesystem, rede
- **Ferramentas de debugging e tracing**

---

**Mantenedor**: [fpedrolucas95](https://github.com/fpedrolucas95/)  
**Licença**: Apache 2.0  
**Repositório**: [GitHub — Atom OS](https://github.com/fpedrolucas95/atom)
