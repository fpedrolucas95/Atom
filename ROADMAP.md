# 🛣️ Atom Kernel — Roadmap de Desenvolvimento

> **Status do Projeto**: Experimental / Em desenvolvimento ativo
> **Última atualização**: 2025-12-21
> **Última revisão**: 2025-12-21 - Fase 6 (6.1-6.3) CORRIGIDA ✅
> **Context Switching**: Correção crítica implementada - scheduler agora efetivamente executa threads em user space. Timer interrupts fazem context switching real, permitindo que o init process e service threads executem corretamente 🔄⚡🎯✨

Este documento descreve o plano de desenvolvimento incremental do Atom Kernel, organizado em fases lógicas e priorizadas.

---

## 📊 Estado Atual (v0.1)

### ✅ Já Implementado

- [x] Estrutura básica do projeto Rust (no_std, workspace)
- [x] Boot UEFI em x86_64 (QEMU + máquinas reais)
- [x] Entry point assembly (boot.asm) com stack setup correto
- [x] Transição UEFI → Kernel (GetMemoryMap + ExitBootServices)
- [x] Tela de boot com mensagem usando UEFI ConOut
- [x] Panic handler básico
- [x] Função halt() multiplataforma (x86_64/aarch64)
- [x] Script de build automatizado para Windows (build.ps1)
- [x] VGA text mode driver (não utilizado, mas disponível)
- [x] Estrutura modular (arch, uefi, vga, mm)
- [x] Physical Memory Manager (PMM) completo com bitmap allocator
- [x] Parser do memory map UEFI
- [x] Kernel heap allocator (bump allocator)
- [x] Serial port driver (COM1) para debug output
- [x] Macros serial_print! e serial_println! para logging
- [x] Framebuffer GOP/UEFI para saída gráfica
- [x] Sistema de graphics com renderização de pixels e fontes bitmap
- [x] Terminal interativo com suporte a comandos (modo gráfico e VGA text)
- [x] Sistema IPC completo com portas, envio/recebimento de mensagens
- [x] Priority Inheritance para prevenir priority inversion
- [x] Transferência de capabilities via IPC (grant e move)
- [x] Sistema de capabilities completo com audit logging e revogação recursiva

**Instruções**: 
- A cada fase implementada, atualizar a mensagem exibida em `// Display welcome message` com o identificador da fase (ex.: “Welcome to Atom kernel v0.1 - 1.2 — mensagem”)
- Em todas as fases, tenha em mente o multithread e paralelismo futuro para evitar grandes refatorações no futuro.

---

## 🎯 Fase 1: Fundação do Kernel (MVP)

**Objetivo**: Estabelecer a base mínima para um kernel funcional com gerenciamento de memória e execução.

### 1.1 Gerenciamento de Memória Física

- [x] Criar módulo `mm` (memory management)
- [x] Implementar Physical Memory Manager (PMM)
  - [x] Parser do memory map UEFI
  - [x] Bitmap allocator para páginas físicas
  - [x] Funções: `alloc_page()`, `free_page()`
  - [x] Funções: `alloc_pages()`, `free_pages()` (alocação contígua)
  - [x] Funções: `alloc_page_zeroed()`, `alloc_pages_zeroed()`
  - [x] Tracking de memória disponível vs. usada
  - [x] Funções auxiliares: `is_page_aligned()`, `align_up()`, `align_down()`
  - [x] Estatísticas detalhadas com `get_stats()` e `get_detailed_stats()`
- [x] Implementar kernel heap allocator
  - [x] Bump allocator inicial
  - [x] Integração com `#[global_allocator]`
  - [x] Suporte para `alloc::vec::Vec`, `alloc::boxed::Box`
- [x] Testes de alocação/dealocação de páginas
- [x] Logging básico para debug de memória
  - [x] Serial port driver (COM1)
  - [x] Macros `serial_print!` e `serial_println!`
  - [x] Estatísticas de memória em tempo real

### 1.2 Gerenciamento de Memória Virtual (VMM)

- [x] Criar módulo `mm/vm`
- [x] Implementar estruturas de page tables (x86_64)
  - [x] PML4, PDPT, PD, PT (4-level paging)
  - [x] Funções para mapear/unmapear páginas
  - [x] Suporte a flags (presente, writable, user, NX)
- [x] Criar kernel address space
  - [x] Identity mapping para kernel code/data
  - [x] Higher-half kernel mapping (espelhado para 512 MiB iniciais)
- [x] Implementar funções de mapeamento
  - [x] `map_page(virt, phys, flags)`
  - [x] `unmap_page(virt)`
  - [x] `remap_page(virt, new_phys, flags)`
- [x] TLB invalidation (invlpg)
- [x] Testes de mapeamento/proteção de memória

### 1.3 Interrupções e Exceções (x86_64)

- [x] Criar módulo `interrupts`
- [x] Configurar IDT (Interrupt Descriptor Table)
  - [x] Estrutura IDT com 256 entries
  - [x] Criar handlers assembly para cada vetor
- [x] Implementar exception handlers
  - [x] #DE (Divide Error)
  - [x] #PF (Page Fault) — crítico para VM
  - [x] #GP (General Protection Fault)
  - [x] #UD (Invalid Opcode)
  - [x] Double Fault (#DF)
- [x] Stack tracing em panics/exceptions
- [x] Suporte a APIC Local (substituir PIC 8259)
  - [x] Detecção de APIC via ACPI/CPUID
  - [x] Configuração de APIC registers
- [x] Timer interrupt (APIC timer ou PIT)
  - [x] Handler de timer tick
  - [x] Contador de ticks global

### 1.4 Output e Debugging

- [x] Serial port output (COM1) para debug
  - [x] Driver básico de serial port
  - [x] Macros `serial_print!` e `serial_println!`
  - [x] Suporte a formatting (`core::fmt`)
- [x] Substituir/complementar UEFI ConOut
  - [x] VGA text mode completo após ExitBootServices
  - [x] VGA Writer com scroll automático
  - [x] Suporte a cores customizáveis
  - [x] Macros `vga_print!` e `vga_println!`
  - [x] Integração com logging framework
- [x] Logging framework avançado
  - [x] Níveis: DEBUG, INFO, WARN, ERROR, PANIC
  - [x] Timestamps (via timer ticks)
  - [x] Macros `log_debug!`, `log_info!`, `log_warn!`, `log_error!`, `log_panic!`
  - [x] Suporte a file e line number nos logs
  - [x] Output formatado para serial port
  - [x] Output formatado e colorido para VGA
  - [x] Output dual (serial + VGA simultâneo)
- [x] Framebuffer gráfico (GOP/UEFI)
  - [x] Suporte a GOP (Graphics Output Protocol)
  - [x] Mapeamento de framebuffer no espaço de memória virtual
  - [x] Sistema de renderização de pixels
  - [x] Conversão de formatos RGB/BGR
  - [x] Renderização de fontes bitmap (8x16)
  - [x] Suporte a desenho de caracteres gráficos
- [x] Terminal interativo gráfico
  - [x] Terminal em modo gráfico (quando GOP disponível)
  - [x] Fallback para VGA text mode
  - [x] Buffer de linha com histórico
  - [x] Comandos integrados (help, clear, about, etc.)
  - [x] Integração com teclado PS/2
  - [x] Scroll automático e controle de cursor

---

## 🎯 Fase 2: Threading e Scheduling

**Objetivo**: Permitir execução de múltiplas threads com preempção e IPC seguro.

### 2.1 Estruturas de Dados de Thread

- [x] Criar módulo `thread`
- [x] Definir `struct Thread`
  - [x] Thread ID (único)
  - [x] Estado (Running, Ready, Blocked, Exited)
  - [x] Registradores salvos (context)
  - [x] Stack pointer (kernel stack)
  - [x] Address space (ponteiro para page table)
  - [x] Prioridade (fixed priority no MVP)
- [x] Implementar Thread Control Block (TCB)
- [x] Criar lista global de threads (lock-free ou spinlock)

### 2.2 Context Switching

- [x] Implementar `switch_context(old, new)` em assembly
  - [x] Salvar registradores (RAX, RBX, ..., RSP, RBP, RIP)
  - [x] Trocar CR3 (page table) se necessário
  - [x] Restaurar registradores do novo contexto
- [x] Testar troca manual de contexto entre 2 threads
- [x] Validar corretude com thread_local stacks

### 2.3 Scheduler

- [x] Criar módulo `sched`
- [x] Implementar scheduler round-robin
  - [x] Fila circular de threads prontas
  - [x] Função `schedule()` — escolhe próxima thread
  - [x] Integração com timer interrupt para preempção
- [x] Prioridades fixas (MVP)
  - [x] 4 níveis de prioridade
  - [x] Round-robin dentro de cada nível
- [x] Idle thread (roda quando não há trabalho)
- [x] Testes: criar N threads, verificar que todas executam

### 2.4 Syscalls Básicos de Thread

- [x] Implementar mecanismo de syscall (SYSCALL/SYSRET em x86_64)
  - [x] MSR setup (STAR, LSTAR, SFMASK)
  - [x] Handler de syscall em assembly
  - [x] Dispatcher de syscalls em Rust
- [x] Implementar syscalls:
  - [x] `thread_create(entry_point, stack, flags) -> ThreadID`
  - [x] `thread_exit(exit_code)`
  - [x] `thread_yield()` — cede CPU voluntariamente
  - [x] `thread_sleep(ticks)` — bloqueia por tempo
- [x] User mode threads (ring 3)
  - [x] Criar user stacks
  - [x] Configurar segmentos (GDT)
  - [x] Transição kernel ↔ user mode

### 2.5 Priority Inheritance para IPC (NOVA FASE)

- [x] Criar módulo IPC básico
  - [x] Estrutura de portas IPC (`IPCPort`)
  - [x] Estrutura de mensagens (`Message`)
  - [x] Gerenciador global de IPC (`IpcManager`)
  - [x] Fila de mensagens por porta
  - [x] Rastreamento de threads bloqueadas esperando mensagens
- [x] Implementar mecanismo de Priority Inheritance
  - [x] Separar prioridade base e prioridade efetiva no scheduler
  - [x] Função `boost_priority()` para herança temporária de prioridade
  - [x] Função `restore_original_priority()` para restaurar prioridade base
  - [x] Rastreamento de dependências entre threads (quem espera por quem)
  - [x] Atualização de prioridade quando thread de alta prioridade bloqueia
- [x] Implementar syscalls de IPC
  - [x] `ipc_create_port() -> PortID` — criar porta IPC
  - [x] `ipc_close_port(port_id)` — fechar porta
  - [x] `ipc_send(port_id, msg_type, payload, len)` — enviar mensagem
  - [x] `ipc_recv(port_id, buffer, size)` — receber mensagem (blocking)
- [x] Testes e validação
  - [x] Testes unitários do módulo IPC
  - [x] Testes de criação e fechamento de portas
  - [x] Testes de envio e recebimento de mensagens
  - [x] Testes de fila de mensagens (FIFO)
  - [x] Testes de permissões (apenas owner pode fechar)

**Resultado**: Sistema IPC funcional com priority inheritance implementado, prevenindo priority inversion quando threads de alta prioridade bloqueiam esperando por threads de baixa prioridade.

**Detalhes da Implementação**:
- **Priority Inheritance Protocol**: Quando uma thread de alta prioridade bloqueia esperando mensagem de uma thread de baixa prioridade, a thread de baixa prioridade temporariamente herda a prioridade alta para completar seu trabalho rapidamente.
- **Tracking de Dependências**: O sistema mantém um mapa de quais threads estão esperando em quais portas, permitindo propagar herança de prioridade através de cadeias de dependências.
- **Prioridade Efetiva vs Base**: Cada thread tem uma prioridade base (original) e uma prioridade efetiva (que pode ser aumentada via herança). O scheduler usa a prioridade efetiva para decisões de scheduling.
- **Restauração Automática**: Quando uma mensagem é enviada e a thread bloqueada é acordada, sua prioridade é automaticamente restaurada ao valor base.

**Arquivos Modificados**:
- `kernel/src/ipc.rs` — Novo módulo IPC com suporte a priority inheritance
- `kernel/src/sched.rs` — Adicionado suporte a prioridades base e efetivas
- `kernel/src/syscall/mod.rs` — Adicionados syscalls IPC (4-7)
- `kernel/src/kernel.rs` — Inicialização do subsistema IPC

---

## 🎯 Fase 3: Sistema de Capabilities

**Objetivo**: Implementar controle de acesso baseado em capabilities.

### 3.1 Arquitetura de Capabilities

- [x] Criar módulo `cap`
- [x] Definir tipos de capabilities:
  - [x] `ThreadCap` — controle sobre threads
  - [x] `MemRegionCap` — acesso a regiões de memória
  - [x] `IPCPortCap` — envio/recebimento de mensagens
  - [x] `IRQCap` — receber interrupções de hardware
  - [x] `DeviceCap` — acesso a dispositivos PCIe
  - [x] `DmaBufferCap` — buffers DMA
- [x] Estrutura `Capability`:
  - [x] ID único (handle)
  - [x] Tipo
  - [x] Permissões (read, write, grant, revoke, execute)
  - [x] Referência ao recurso protegido
  - [x] Parent/children tracking para delegação
- [x] Capability table por thread/process
  - [x] BTreeMap indexado por CapHandle
  - [x] Integrado na estrutura Thread
- [x] Capabilities são índices opacos
  - [x] Não são ponteiros diretos
  - [x] Tabela kernel mapeia handle → objeto
  - [x] Impossível forjar handle válido
  - [x] CapabilityManager global para operações cross-table
- [x] Syscalls básicos implementados
  - [x] `cap_create` — criar capability
  - [x] `cap_check` — verificar permissões
  - [x] `cap_revoke` — revogar capability
  - [x] `cap_derive` — derivar com permissões reduzidas
  - [x] `cap_list` — listar capabilities
- [x] Validação em todas syscalls ✅ (implementado em 3.3)
  - [x] Verificar handle pertence ao processo
  - [x] Verificar direitos suficientes
  - [x] Retornar erro se inválido

### 3.2 Operações de Capabilities

- [x] Criar capability (`cap_create`) ✅
- [x] Transferir capability entre threads (`cap_transfer`) ✅
- [x] Revogar capability (`cap_revoke`) ✅
- [x] Completar derivação de capabilities (`cap_derive`) ✅
- [x] Verificação de capabilities em syscalls ✅
  - [x] Antes de `thread_create`, verificar `ThreadCap` ✅
  - [x] Antes de IPC, verificar `IPCPortCap` ✅
  - [x] Documentação de requisitos em todos os syscalls ✅
- [x] Integração CapabilityTable com Thread ✅
- [x] Testes de isolamento: thread sem cap não pode acessar recurso ✅

**Resultado**: Sistema de capabilities totalmente operacional com transferência, derivação e validação em syscalls. Foram adicionados 8 novos testes unitários garantindo isolamento e validação de permissões.

### 3.3 Integração com Threads e IPC

- [x] Associar capabilities a recursos:
  - [x] Thread só pode enviar IPC se possui `IPCPortCap`
  - [x] Thread só pode criar threads se possui `ThreadCap`
  - [ ] Thread só pode mapear memória se possui `MemRegionCap` (será feito quando syscalls de memória existirem)
- [x] Delegação de capabilities via IPC (grant e move)
  - [x] Syscall `ipc_send_with_cap` para enviar mensagens com capabilities
  - [x] Modo Grant: cria capability derivada com permissões reduzidas
  - [x] Modo Move: transfere ownership completamente
- [x] Auto-grant de `IPCPortCap` ao criar portas IPC
- [x] Enforcement real de validação em syscalls
  - [x] `sys_thread_create` valida `ThreadCap` com WRITE
  - [x] `sys_ipc_send` valida `IPCPortCap` com WRITE
  - [x] `sys_ipc_recv` valida `IPCPortCap` com READ
- [x] Capabilities granulares por porta IPC (não globais)
- [x] Testes de segurança focados em enforcement

**Resultado**: Sistema de capabilities totalmente integrado com threads e IPC, com validação obrigatória de permissões em todas as operações sensíveis. O princípio de least privilege é enforçado no nível do kernel.

### 3.4 Delegação e Revogação de Capabilities ✅

**Objetivo**: Controle completo do ciclo de vida de capabilities

#### Delegação com Redução de Direitos
- [x] Operação `cap_derive(parent_cap, reduced_rights) -> ChildCap`
  - [x] Validar que reduced_rights ⊆ parent_rights
  - [x] Criar child cap derivado do parent
  - [x] Marcar relação parent→child para revogação
- [x] Monotonicidade de direitos
  - [x] Child nunca tem mais direitos que parent
  - [x] Delegação só pode reduzir, nunca ampliar

#### Revogação
- [x] Árvore de derivação
  - [x] Cada capability conhece seu parent e children
  - [x] Estrutura: `parent_id`, `Vec<child_id>`
- [x] Operação `cap_revoke(cap_id)`
  - [x] Revoga capability especificada
  - [x] Revoga recursivamente todos os children
  - [x] Remove da capability table (global + thread tables)
  - [x] Invalida handles existentes
- [x] ~~Epoch-based invalidation (alternativa)~~ (não necessário com current implementation)
  - [x] ~~Cada objeto tem generation counter~~
  - [x] ~~Incrementa ao revogar~~
  - [x] ~~Capabilities antigas se tornam inválidas~~

#### Auditoria
- [x] Logging de operações
  - [x] cap_create, cap_derive, cap_revoke, cap_transfer
  - [x] Timestamp + thread_id + cap_id
  - [x] Ring buffer de 1000 entradas
- [x] Query de origem
  - [x] `cap_query_parent(cap_id) -> ParentCapID`
  - [x] `cap_query_children(cap_id) -> Vec<ChildCapID>`
  - [x] Visualização da árvore de derivação
- [x] Testes de revogação
  - [x] Revogar parent invalida todos children
  - [x] Uso de capability revogada retorna erro
  - [x] 5 novos testes unitários adicionados

**Resultado**: Sistema de capabilities completo com ciclo de vida gerenciado, audit trail e APIs de inspeção. Fase 3 100% completa!

---

## 🎯 Fase 4: IPC (Inter-Process Communication)

**Objetivo**: Comunicação eficiente e segura entre processos/serviços.

### 4.1 Portas IPC ✅

- [x] Criar módulo `ipc`
- [x] Definir `struct IPCPort`
  - [x] Port ID (único)
  - [x] Fila de mensagens pendentes
  - [x] Capabilities associadas
  - [x] Thread bloqueada esperando mensagem (receiver)
- [x] Syscall `ipc_create_port() -> PortID`
- [x] Syscall `ipc_close_port(port_id)`

### 4.2 Envio e Recebimento de Mensagens ✅

- [x] Definir formato de mensagem IPC:
  - [x] Header: sender, receiver, message_type, length
  - [x] Payload: buffer inline (até 256 bytes)
  - [x] Payload via shared memory (shared regions + zero-copy)
- [x] Syscall `ipc_send(port_id, message, flags)`
  - [x] Verificar `IPCPortCap`
  - [x] Copiar mensagem para fila do receptor
  - [x] Acordar thread bloqueada (se houver)
- [x] Syscall `ipc_recv(port_id, buffer, timeout)`
  - [x] Bloquear thread se fila vazia
  - [x] Copiar mensagem do sender para buffer
  - [x] Retornar sender ID e tamanho
- [x] Testes: ping-pong entre 2 threads

### 4.3 Memória Compartilhada (para payloads grandes) ✅

- [x] Syscall `shared_region_create(size) -> RegionID`
- [x] Syscall `shared_region_map(region_id, virt_addr, flags)`
- [x] Syscall `shared_region_unmap(region_id)`
- [x] Syscall `shared_region_destroy(region_id)`
- [x] Passar `RegionID` via IPC para zero-copy
- [x] Sincronização via IPC (mensagens de controle)
- [x] SharedMemoryRegion capability type
- [x] Zero-copy message passing support

### 4.4 Otimizações de IPC

- [x] Fast path: mensagens pequenas inline (até MAX_MESSAGE_SIZE)
- [x] Evitar cópias desnecessárias (zero-copy quando possível)
- [x] Batching de mensagens (enviar múltiplas de uma vez)

### 4.5 Transferência de Capabilities via IPC ✅

**Objetivo**: Passar capabilities entre processos de forma segura

#### Grant (Transferência Temporária)
- [x] Mecanismo de grant
  - [x] Sender "empresta" capability via mensagem IPC
  - [x] Receiver ganha acesso temporário (via derivação)
  - [x] Sender mantém ownership
- [x] Syscall `ipc_send_with_cap(port_id, msg, cap_id, grant_rights)`
  - [x] Validar sender possui cap_id
  - [x] Criar temporary child cap com grant_rights
  - [x] Enviar cap na mensagem IPC
  - [x] Receiver recebe temp cap_id

#### Move (Transferência Permanente)
- [x] Mecanismo de move
  - [x] Sender transfere ownership completamente
  - [x] Sender perde acesso à capability
  - [x] Receiver se torna novo owner
- [x] Implementado via `ipc_send_with_cap` com flag de mode
  - [x] Validar sender possui cap_id
  - [x] Remover cap da sender capability table
  - [x] Adicionar cap na receiver capability table
  - [x] Marcar transferência no audit log

#### Validação
- [x] Verificar direitos na transferência
  - [x] Sender deve ter direito "grant" ou "transfer"
  - [x] Grant só funciona com cap que permite delegation
- [x] Prevenir forja de capabilities
  - [x] Capabilities são handles opacos (índices)
  - [x] Kernel valida todos os handles
  - [x] Receiver não pode "adivinhar" cap_ids

### 4.6 Prevenção de Deadlocks em IPC

**Objetivo**: Evitar que IPC síncrono trave o sistema

#### Timeouts
- [x] Timeout obrigatório em blocking calls
  - [x] `ipc_recv(port_id, buffer, timeout_ms)`
  - [x] timeout=0: non-blocking (try)
  - [x] timeout=INFINITE: explícito, não default
- [x] Timeout em send (se fila cheia)
  - [x] `ipc_send(port_id, msg, timeout_ms)`
  - [x] Retorna erro se timeout

#### IPC Assíncrono
- [x] Syscall `ipc_send_async(port_id, msg)`
  - [x] Nunca bloqueia sender
  - [x] Retorna imediatamente
  - [x] Mensagem entra na fila
  - [x] Receiver processa quando chamar recv
- [x] Syscall `ipc_try_recv(port_id, buffer)`
  - [x] Non-blocking receive
  - [x] Retorna imediatamente
  - [x] EWOULDBLOCK se fila vazia

#### Detecção de Deadlock (Debug)
- [x] Rastreamento de dependências IPC
  - [x] Thread A espera Thread B
  - [x] Thread B espera Thread A → deadlock
- [x] Apenas para debugging (overhead alto)
  - [x] Flag de compilação CONFIG_DEADLOCK_DETECT
  - [x] Log de ciclos detectados

### 4.7 IPC Observability e Tracing

**Objetivo**: Debug e performance analysis de IPC

#### Tracing de Mensagens
- [x] Flag de compilação CONFIG_IPC_TRACE
- [x] Log de eventos IPC
  - [x] send: sender_tid, receiver_port, msg_size
  - [x] recv: receiver_tid, sender_tid, msg_size
  - [x] Timestamp de cada operação
- [x] Ring buffer de eventos
  - [x] Últimas 1000 mensagens (circular)
  - [x] Syscall para ler buffer (debugging)

#### Métricas de Performance
- [x] Por IPC port
  - [x] Contador de mensagens enviadas/recebidas
  - [x] Latência min/max/avg
  - [x] Taxa de mensagens/segundo
- [x] Syscall `ipc_port_stats(port_id) -> Stats`
  - [x] Retorna métricas agregadas
  - [x] Útil para profiling

---

## 🎯 Fase 5: Syscalls de Memória

**Objetivo**: Expor gerenciamento de memória para user space.

### 5.1 Address Spaces ✅

- [x] Syscall `addrspace_create() -> AddressSpaceID`
  - [x] Criar nova page table (independente)
  - [x] Retornar handle protegido por capability
- [x] Syscall `addrspace_destroy(as_id)`
  - [x] Liberar page tables e memória associada

### 5.2 Mapeamento de Regiões ✅

- [x] Syscall `map_region(as_id, virt, phys, size, flags)`
  - [x] Verificar `MemRegionCap`
  - [x] Mapear páginas no address space especificado
  - [x] Configurar flags (read, write, execute, user)
- [x] Syscall `unmap_region(as_id, virt, size)`
  - [x] Unmapear e liberar páginas
- [x] Syscall `remap_region(as_id, old_virt, new_virt, size)`

### 5.3 Proteção e Isolamento ✅

- [x] Validar que user space não pode mapear kernel memory
- [x] Enforçar que threads só podem modificar seu próprio address space
- [x] Testes de segurança: tentar acessar memória sem permissão

### 5.4 Políticas de Memória em User Space (Opcional)

**Objetivo**: Mover políticas complexas para user space

#### Swap Manager (Servidor Opcional)
- [x] Recebe page fault notifications
  - [x] Kernel envia IPC ao swap manager com addr/error/RIP/TID
  - [x] Decisão de swap in/out fica em user space (handler registrado)
- [x] Comunica com storage server
  - [x] Kernel provê canal de IPC, fluxo de dados ocorre em user space
  - [x] Page faults carregam contexto suficiente para decidir o frame
- [x] Políticas configuráveis
  - [x] LRU, FIFO, etc., ficam sob responsabilidade do servidor user space
  - [x] Kernel permanece neutro (policy-free), apenas notifica

#### File-Backed Mapping
- [x] Servidor de mapeamento de arquivos
  - [x] Pode receber faults para popular cache via shared memory
  - [x] Lazy loading orientado por page fault notifications

---

## 🎯 Fase 6: Init Process e User Space

**Objetivo**: Executar o primeiro processo em user space.

### 6.1 Formato de Executável (MVP)

- [x] Definir formato simples de executável:
  - [x] Header: magic number, entry point, tamanho de code/data
  - [x] Seções: .text, .data, .bss
  - [x] Ou usar ELF simplificado (parser básico)
- [x] Loader de executável no kernel
  - [x] Ler binário da memória (passado pelo bootloader)
  - [x] Alocar pages para code/data
  - [x] Mapear no address space do processo
  - [x] Configurar entry point

### 6.2 Processo Init

- [x] Criar `init` process (PID 1)
  - [x] Binário embarcado no kernel (ou carregado de ramdisk)
  - [x] Address space próprio
  - [x] Thread inicial rodando entry point
- [x] `init` em Rust (no_std):
  - [x] Loop básico
  - [x] Criar outros processos/services
  - [x] Responder a syscalls básicos
- [x] Testes: verificar que init roda em user mode

### 6.3 Service Manager e Boot Declarativo

**Objetivo**: Orquestração de serviços com políticas explícitas

**CORREÇÃO CRÍTICA (2025-12-21)**: A infraestrutura da Fase 6 estava implementada mas não conectada. O scheduler decidia qual thread executar mas nunca fazia o context switch real. Corrigido:
- ✅ Scheduler inicializado com idle thread durante boot
- ✅ `on_timer_tick()` agora retorna (prev, next) ThreadIDs
- ✅ Timer interrupt handler faz context switching real após scheduler decidir
- ✅ Syscalls (yield, exit, sleep, ipc_recv) fazem context switching quando apropriado
- ✅ `start_scheduling()` transfere controle do kernel para a primeira thread ready
- ✅ Init process e service threads agora executam corretamente em user space

#### Manifesto de Boot
- [x] Formato declarativo (TOML ou similar)
  ```toml
  [service.fs_server]
  binary = "/init/fs.elf"
  capabilities = ["MemRegionCap", "IPCPortCap"]
  depends_on = ["storage_driver"]

  [service.storage_driver]
  binary = "/init/nvme_driver.elf"
  capabilities = ["IRQCap:33", "DeviceCap:0000:01:00.0", "DMABufferCap"]
    ```
- [x] Parser de manifesto
- [x] Embarcado no init process
- [x] Validar sintaxe e dependências
- [x] Resolver ordem de inicialização
### Distribuição Inicial de Capabilities
- [x] Init recebe "god capabilities"
  - [x] Pode criar qualquer tipo de capability
  - [x] Pode distribuir para serviços filhos
- [x] Init distribui conforme manifesto
  - [x] Princípio do menor privilégio
  - [x] Cada serviço recebe apenas o necessário
  - [x] Auditoria completa da distribuição
### Lifecycle Management
- [x] Protocolo de registro de serviços
  - [x] Serviço informa "estou pronto"
  - [x] Service manager rastreia estado
- [x] Descoberta de serviços (lookup por nome)
  - [x] Retorna IPC port do serviço
  - [x] Verificar permissão de acesso
- [x] Operações de ciclo de vida
  - [x] start, stop, restart
  - [x] Monitoramento de crashes
  - [x] Respawn automático (configurável)

---

## 🎯 Fase 7: Drivers em User Space

**Objetivo**: Mover drivers para fora do kernel.


---

### Fase 7.1 - Framework de Drivers (REVISAR)

**Revisar syscall irq_register**:
- [ ] Syscall `irq_register(irq_cap, ipc_port_id)`
  - [ ] Validar IRQCap (não apenas número de IRQ)
  - [ ] Associar IRQ a IPC port específico do driver
  - [ ] Kernel entrega IRQ como mensagem IPC
  - [ ] Mensagem contém: irq_num, timestamp
- [ ] Prevenir "IRQ global"
  - [ ] Apenas holder de IRQCap recebe notificações
  - [ ] Múltiplos holders → todos notificados (shared IRQ)
- [ ] Syscall `irq_ack(irq_cap)`
  - [ ] Driver confirma tratamento
  - [ ] Re-enable IRQ line (se masked)

### 7.2 Driver de Timer (user space)

- [ ] Mover timer driver para user space
- [ ] Comunicação via IPC:
  - [ ] Kernel → Driver: IRQ notification
  - [ ] Driver → Apps: timer events
- [ ] Testes: apps requisitando timer events

### 7.3 Driver de Teclado (user space)

- [ ] Driver básico de teclado PS/2
  - [ ] Ler scancode via porta I/O
  - [ ] Traduzir para keycodes
  - [ ] Enviar eventos via IPC
- [ ] Integração com init/compositor (futuro)

### 7.4 Driver de Serial Port (user space)

- [ ] Mover logging para driver serial em user space
- [ ] Kernel envia logs via IPC
- [ ] Driver escreve em COM1

### 7.5 Controle de DMA e IOMMU

**Objetivo**: Isolar drivers de forma segura com DMA

#### Descoberta de IOMMU
- [ ] Parser de ACPI DMAR (Intel) ou IVRS (AMD)
  - [ ] Detectar presença de IOMMU
  - [ ] Mapear MMIO registers da IOMMU
  - [ ] Enumerar domínios de isolamento
- [ ] Fallback sem IOMMU
  - [ ] Documentar limitações de segurança
  - [ ] Permitir apenas drivers trusted (configuração)

#### Mapeamento de Buffers DMA
- [ ] Syscall `dma_map_buffer(device_cap, phys_addr, size, perms)`
  - [ ] Validar DeviceCap do driver
  - [ ] Validar buffer pertence ao processo
  - [ ] Com IOMMU:
    - [ ] Configurar translation entry
    - [ ] Device só pode acessar este buffer
  - [ ] Sem IOMMU:
    - [ ] Log da operação (auditoria)
    - [ ] Validação best-effort
  - [ ] Retorna DMABufferCap
- [ ] Syscall `dma_unmap_buffer(dma_buffer_cap)`
  - [ ] Remover mapeamento da IOMMU
  - [ ] Invalidar DMABufferCap
  - [ ] TLB invalidation (se necessário)

#### Gestão de Dispositivos
- [ ] DeviceCap associado a BDF (Bus/Device/Function)
  - [ ] PCIe config space access restrito
  - [ ] Apenas driver com DeviceCap pode acessar
- [ ] Syscall `device_mmio_map(device_cap, bar_num) -> VirtAddr`
  - [ ] Mapeia BAR do dispositivo no espaço do driver
  - [ ] Read-only ou read-write conforme DeviceCap

#### Testes
- [ ] Driver de teste com DMA
  - [ ] Alocar buffer
  - [ ] Mapear para DMA
  - [ ] Verificar dispositivo consegue ler/escrever
  - [ ] Tentar acessar buffer não mapeado → falha

### 7.6 PCIe Enumeration e MSI/MSI-X

**Objetivo**: Suporte a interrupções modernas e descoberta de dispositivos

#### PCIe Configuration
- [ ] Parser de ACPI MCFG
  - [ ] Enhanced Configuration Access Mechanism (ECAM)
  - [ ] Mapear MMIO configuration space
- [ ] Enumerar dispositivos PCIe
  - [ ] Scan de bus/device/function
  - [ ] Ler Vendor ID, Device ID, Class Code
  - [ ] Detectar capabilities (MSI, MSI-X, etc.)
- [ ] Expor dispositivos para user space
  - [ ] Lista de dispositivos disponíveis
  - [ ] Service manager distribui DeviceCap

#### MSI/MSI-X Support
- [ ] Configurar MSI capability
  - [ ] Allocate interrupt vector
  - [ ] Programar Message Address
  - [ ] Programar Message Data
- [ ] Configurar MSI-X capability
  - [ ] Mapear MSI-X table e PBA
  - [ ] Programar múltiplos vetores
- [ ] Vantagens sobre APIC
  - [ ] Menos contenção (cada device tem vetores próprios)
  - [ ] Melhor performance em multi-core

---

## 🎯 Fase 8: SMP (Symmetric Multiprocessing)

**Objetivo**: Suporte a múltiplos CPUs.

### 8.1 Detecção e Boot de CPUs

- [ ] Parsing de ACPI MADT (Multiple APIC Description Table)
  - [ ] Identificar número de CPUs
  - [ ] Obter APIC IDs
- [ ] Boot de Application Processors (APs)
  - [ ] Trampoline code em low memory
  - [ ] Enviar INIT-SIPI-SIPI via APIC
  - [ ] APs entram em `ap_main()`

### 8.2 Per-CPU Data Structures

- [ ] Per-CPU variables (GS base em x86_64)
- [ ] Per-CPU stacks
- [ ] Per-CPU scheduler run queues
- [ ] Spinlocks para estruturas compartilhadas

### 8.3 Scheduler SMP-aware

- [ ] Load balancing entre CPUs
- [ ] CPU affinity (pinnar threads a CPUs)
- [ ] IPI (Inter-Processor Interrupts) para preempção remota

### 8.4 Sincronização

- [ ] Spinlocks (já implementados)
- [ ] RWLocks (readers-writer locks)
- [ ] Seqlocks
- [ ] Atomic operations (já disponíveis via Rust)

---

## 🎯 Fase 9: Filesystem em User Space

**Objetivo**: Implementar VFS e filesystems básicos.

### 9.1 VFS (Virtual File System)

- [ ] Criar módulo `vfs` (em user space)
- [ ] Definir interface de FS:
  - [ ] `open(path, flags) -> FileDescriptor`
  - [ ] `read(fd, buffer, count) -> bytes_read`
  - [ ] `write(fd, buffer, count) -> bytes_written`
  - [ ] `close(fd)`
  - [ ] `stat(path) -> FileInfo`
- [ ] Mount table
  - [ ] Registrar filesystems
  - [ ] Lookup de paths

### 9.2 RAMDisk Filesystem

- [ ] Implementar filesystem em memória
  - [ ] Estrutura de inodes simplificada
  - [ ] Diretórios e arquivos
  - [ ] Read/write em buffers de memória
- [ ] Comunicação com kernel via IPC
  - [ ] Kernel não conhece detalhes do FS
  - [ ] Apps fazem syscalls → kernel → FS server

### 9.3 Syscalls de Filesystem

- [ ] Syscall `open(path, flags) -> fd`
- [ ] Syscall `read(fd, buf, count)`
- [ ] Syscall `write(fd, buf, count)`
- [ ] Syscall `close(fd)`
- [ ] Syscall `stat(path, stat_buf)`
- [ ] File descriptor table por processo

---

## 🎯 Fase 10: Port para ARM64 (AArch64)

**Objetivo**: Tornar o kernel verdadeiramente multiplataforma.

### 10.1 Boot ARM64

- [ ] UEFI boot (similar a x86_64)
- [ ] Device Tree parsing (se não usar UEFI ACPI)
- [ ] Configurar exception level (EL1 para kernel)

### 10.2 MMU ARM64

- [ ] Implementar page tables (4-level ou 3-level)
  - [ ] Translation tables (TTBR0/TTBR1)
  - [ ] Page sizes: 4KB, 64KB
- [ ] TLB invalidation (TLBI)

### 10.3 Interrupções ARM64

- [ ] GIC (Generic Interrupt Controller)
  - [ ] Configurar GIC distributor
  - [ ] Configurar GIC CPU interface
- [ ] Exception vectors (synchronous, IRQ, FIQ, SError)
- [ ] Timer (ARM Generic Timer)

### 10.4 Context Switching ARM64

- [ ] Salvar/restaurar registradores (X0-X30, SP, PC, PSTATE)
- [ ] Trocar TTBR (page table)

### 10.5 Validação

- [ ] Rodar mesmo código de testes em ARM64
- [ ] Verificar portabilidade do código Rust
- [ ] Benchmark de desempenho comparado a x86_64

---

## 🎯 Fase 11: Tooling, Debug e Observabilidade

**Objetivo**: Facilitar desenvolvimento e diagnóstico.

### 11.1 Debugging

- [ ] Suporte a QEMU GDB stub
  - [ ] Breakpoints
  - [ ] Step execution
  - [ ] Memory inspection
- [ ] Stack unwinding com símbolos
  - [ ] Integrar com Rust panic handler
  - [ ] Backtrace legível em panics

### 11.2 Logging e Tracing

- [ ] Framework de logging estruturado
  - [ ] Níveis (trace, debug, info, warn, error)
  - [ ] Contexto (CPU, thread, timestamp)
- [ ] Tracing de syscalls
  - [ ] Log de todas as chamadas e resultados
  - [ ] Estatísticas de uso
- [ ] Profiling
  - [ ] Sampling de instruction pointer
  - [ ] Flamegraphs de CPU usage

### 11.3 Testes Automatizados

- [ ] Suite de testes de integração
  - [ ] Boot test (kernel inicia e não crasha)
  - [ ] Memory allocation tests
  - [ ] Thread creation/switching tests
  - [ ] IPC ping-pong tests
- [ ] CI/CD pipeline
  - [ ] Build em múltiplas plataformas
  - [ ] Run tests em QEMU
  - [ ] Code coverage

### 11.4 Documentação

- [ ] Documentação de arquitetura
  - [ ] Diagramas de componentes
  - [ ] Fluxos de execução críticos
- [ ] API documentation (rustdoc)
  - [ ] Documentar todos os syscalls
  - [ ] Exemplos de uso
- [ ] Porting guide
  - [ ] Como portar para nova arquitetura
  - [ ] Checklist de validação

---

## 🎯 Fase 12: Otimizações e Hardening

**Objetivo**: Melhorar desempenho e segurança.

### 12.1 Performance

- [ ] Hot path optimization
  - [ ] Syscall fast path (evitar locks desnecessários)
  - [ ] IPC zero-copy enforcement
- [ ] Scheduler improvements
  - [ ] CFS (Completely Fair Scheduler) ou BFS
  - [ ] NUMA-aware scheduling
- [ ] Memory allocator tuning
  - [ ] Benchmark de allocators (slab vs buddy vs jemalloc)
  - [ ] Reduzir fragmentação

### 12.2 Segurança

- [ ] Auditar código unsafe
  - [ ] Minimizar uso de unsafe
  - [ ] Documentar invariantes
- [ ] Mitigações de exploits
  - [ ] SMEP/SMAP (x86_64)
  - [ ] PAN/PXN (ARM64)
  - [ ] Stack canaries
  - [ ] ASLR (Address Space Layout Randomization)
- [ ] Fuzzing
  - [ ] Fuzz syscalls com AFL/libFuzzer
  - [ ] Fuzz IPC messages

### 12.3 Formal Verification (pesquisa)

- [ ] Modelar componentes críticos em TLA+ ou Coq
  - [ ] Scheduler correctness
  - [ ] IPC message ordering
- [ ] Property-based testing (proptest)

---

## 📈 Métricas de Progresso

### Cobertura de Features (vs. README)

- [x] Boot mínimo em x86_64 → **100% completo** ✅
- [x] Fase 1: Fundação do Kernel (MVP) → **100% completo** ✅
  - [x] 1.1: Gerenciamento de Memória Física → **100% completo** ✅
  - [x] 1.2: Gerenciamento de Memória Virtual (VMM) → **100% completo** ✅
  - [x] 1.3: Interrupções e Exceções (x86_64) → **100% completo** ✅
  - [x] 1.4: Output e Debugging → **100% completo** ✅
- [x] Fase 2: Threading e Scheduling → **100% completo** ✅
  - [x] 2.1: Estruturas de Dados de Thread → **100% completo** ✅
  - [x] 2.2: Context Switching → **100% completo** ✅
  - [x] 2.3: Scheduler → **100% completo** ✅
  - [x] 2.4: Syscalls Básicos de Thread → **100% completo** ✅
  - [x] 2.5: Priority Inheritance para IPC → **100% completo** ✅
- [x] Fase 3: Sistema de Capabilities → **100% completo** ✅
  - [x] 3.1: Arquitetura de Capabilities → **100% completo** ✅
  - [x] 3.2: Operações de Capabilities → **100% completo** ✅
  - [x] 3.3: Integração com Threads e IPC → **100% completo** ✅
  - [x] 3.4: Delegação e Revogação → **100% completo** ✅
- [x] Fase 4: IPC (Inter-Process Communication) → **100% completo** ✅
  - [x] 4.1: Portas IPC → **100% completo** ✅
  - [x] 4.2: Envio e Recebimento de Mensagens → **100% completo** ✅
  - [x] 4.3: Memória Compartilhada → **100% completo** ✅
  - [x] 4.4: Otimizações de IPC → **100% completo** ✅
  - [x] 4.5: Transferência de Capabilities via IPC → **100% completo** ✅
  - [x] 4.6: Prevenção de Deadlocks → **100% completo** ✅
  - [x] 4.7: Observability e Tracing → **100% completo** ✅
- [x] Fase 5: Syscalls de Memória → **100% completo** ✅
  - [x] 5.1: Address Spaces → **100% completo** ✅
  - [x] 5.2: Mapeamento de Regiões → **100% completo** ✅
  - [x] 5.3: Proteção e Isolamento → **100% completo** ✅
  - [x] 5.4: Políticas de Memória em User Space → **100% completo** ✅ (Opcional)
- [x] Fase 6: Init Process e User Space → **100% completo** ✅ (corrigido em 2025-12-21)
  - [x] 6.1: Formato de Executável (MVP) → **100% completo** ✅
  - [x] 6.2: Processo Init → **100% completo** ✅
  - [x] 6.3: Service Manager e Boot Declarativo → **100% completo** ✅
- [x] Scheduler preemptivo → **100% completo** ✅
- [x] IPC funcional (com priority inheritance) → **100% completo** ✅
- [x] Sistema de capabilities → **100% completo** ✅
- [x] Transferência de capabilities via IPC → **100% completo** ✅
- [x] Init em user space → **100% completo** ✅ (corrigido em 2025-12-21)
- [ ] Drivers básicos em user space → **0% completo**
- [ ] Port para ARM64 → **0% completo**
- [ ] FS em user space → **0% completo**
- [x] Tooling de debug e tracing → **100% completo** ✅ (serial, VGA, logging framework, timestamps)

### Linhas de Código (atualizado 2025-12-21)

- **Atual**: ~8.400 LoC Rust + 485 LoC assembly = **~8.885 LoC total**
- **Meta MVP (Fase 1-6)**: ~10.000 LoC
- **Meta Completo (Todas as fases)**: ~15.000-20.000 LoC
- **Progresso**: ~100% da meta MVP alcançada ✅ (Fases 1-6 completas com context switching funcionando)

---

## 🚀 Priorização Recomendada

### Curto Prazo (próximas 2-4 semanas)

1. **Fase 1.1**: Gerenciamento de Memória Física
2. **Fase 1.2**: Memória Virtual básica
3. **Fase 1.3**: Interrupções (pelo menos Page Fault e Timer)
4. **Fase 1.4**: Serial output para debugging

### Médio Prazo (1-3 meses)

5. **Fase 2**: Threading e Scheduling completo
6. **Fase 3**: Sistema de Capabilities (MVP)
7. **Fase 4**: IPC básico

### Longo Prazo (3-6 meses)

8. **Fase 5**: Syscalls de memória
9. **Fase 6**: Init process
10. **Fase 7**: Primeiro driver em user space
11. **Fases 8-12**: Features avançadas (SMP, FS, ARM64, etc.)

---

## 📝 Notas de Implementação

### Decisões Arquiteturais Pendentes

- **Allocator de páginas**: Bitmap vs Buddy vs Free List?
- **Formato de executável**: ELF custom vs formato proprietário simples?
- **Scheduler**: Round-robin vs CFS desde o início?
- **IPC**: Síncrono vs assíncrono vs híbrido?

### Riscos e Desafios

- **Complexidade de MMU**: Page tables são error-prone; considerar usar crate externo auditado (page_table_entry).
- **Race conditions em SMP**: Testes extensivos necessários.
- **Performance de IPC**: Pode requerer múltiplas iterações de otimização.
- **Portabilidade ARM64**: Falta de hardware físico para testes pode atrasar validação.

### Recursos e Ferramentas

- **Documentação**: Intel SDM (x86_64), ARM Architecture Reference Manual
- **Debugging**: QEMU + GDB, serial logging
- **Testes**: Custom test harness, QEMU automation
- **CI**: GitHub Actions com QEMU runners

---

## 🤝 Contribuindo com o Roadmap

Este roadmap é um documento vivo. Contribuições são bem-vindas:

- Questionar priorização
- Sugerir features adicionais
- Reportar tarefas completadas
- Identificar dependências faltantes

**Como atualizar**:
1. Marcar checkboxes quando tasks forem completadas
2. Adicionar notas de implementação em tasks complexas
3. Atualizar métricas de progresso mensalmente
4. Revisar priorização a cada fase completada

---

**Mantido por**: Atom Kernel Team
**Licença**: MIT (conforme LICENSE no repositório)





