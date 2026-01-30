# Atom OS — Roadmap Técnico Atualizado

> **Status do Projeto**: Microkernel Funcional com IPC, Capabilities e User Space  
> **Última atualização**: 30 de Janeiro de 2025  
> **Commit de referência**: `47ac0d8`  
> **Arquitetura**: x86_64 (port ARM64 planejado)

---

## 📊 Estado Atual (Pós-Análise Técnica)

### ✅ Implementado com Sucesso

**Fase 1-6: Fundação Completa**
- [x] Boot UEFI em x86_64 com transition para kernel
- [x] Physical Memory Manager (PMM) com bitmap allocator
- [x] Virtual Memory Manager (VMM) com paging 4-level
- [x] Sistema de interrupções/exceções (IDT, APIC Local)
- [x] Context switching funcional (kernel ↔ user mode)
- [x] Scheduler preemptivo por prioridade com round-robin
- [x] IPC completo (portas, mensagens, batching, async)
- [x] Sistema de capabilities (criação, transferência, revogação)
- [x] Priority inheritance em IPC
- [x] Init process isolado em user space
- [x] Service manager e boot declarativo
- [x] Syscalls abrangentes (~50 chamadas)
- [x] Logging estruturado e observabilidade (serial, VGA, tracing)
- [x] Drivers básicos (display, UI shell unificado, terminal)
- [x] Memória compartilhada para IPC zero-copy

**Arquitetura sólida**:
- Separação clara kernel/userland
- Modelo de capabilities robusto
- IPC message-passing eficiente
- Isolamento de memória por processo

---

## 🎯 Fases Prioritárias

### **FASE 7: Consolidação da Base**
*Objetivo: Eliminar débitos técnicos e fortalecer fundação antes de adicionar features*

#### 7.1 Isolamento Completo de Processos 

**Problema**: Init process (PID 1) ainda compartilha kernel_cr3 em vez de ter PML4 próprio

- [x] **Isolar init process completamente**
  - [x] Criar PML4 dedicado para init no boot
  - [x] Copiar entradas de kernel (higher-half) para novo PML4
  - [x] Carregar .text/.data/.bss do init no novo address space
  - [x] Configurar CR3 do init thread para novo PML4
  - [x] Validar isolamento (init não pode endereçar kernel)
  
- [ ] **Finalização robusta de processos**
  - [ ] Implementar `sys_thread_exit` completo
  - [ ] Liberar todos frames de memória do processo
  - [ ] Revogar todas capabilities possuídas
  - [ ] Fechar portas IPC abertas
  - [ ] Desalocar PML4 e page tables
  - [ ] Remover thread da lista global
  - [ ] Testes: verificar ausência de memory leak

**Resultado esperado**: Todos os processos (incluindo init) 100% isolados, sem vazamento de recursos.

---

#### 7.2 Segurança Reforçada em Capabilities

**Problema**: Distribuição liberal de capabilities (todos processos recebem framebuffer/input)

- [ ] **Princípio do menor privilégio**
  - [ ] Remover concessão automática de FramebufferCap/InputCap
  - [ ] Apenas ui_shell recebe InputCap (teclado/mouse)
  - [ ] Apenas display/compositor recebe FramebufferCap (escrita)
  - [ ] Apps recebem apenas capabilities explicitamente delegadas
  
- [ ] **Timeouts obrigatórios em IPC bloqueante**
  - [ ] Adicionar parâmetro `timeout_ms` em `sys_ipc_recv`
  - [ ] Implementar `ipc_ping(port)` para heartbeat
  - [ ] Service manager: monitor de saúde de serviços críticos
  - [ ] Auto-restart se serviço não responder em X ms
  
- [ ] **Sandbox de syscalls (opcional)**
  - [ ] Bitmap de syscalls permitidas por processo
  - [ ] Filtro no dispatcher de syscalls
  - [ ] Configurável via service manager

**Resultado esperado**: Modelo de segurança "zero-trust" — processos só acessam o que for explicitamente permitido.

---

#### 7.3 Expansão de Memória Física

**Problema**: PMM limitado a 1 GiB de RAM (MAX_PAGES = 256k)

- [ ] **Suporte a 16+ GiB de RAM**
  - [ ] Aumentar `MAX_PAGES` para 4M (16 GiB) ou
  - [ ] Tornar bitmap dinâmico (alocar baseado em memory map)
  - [ ] Ajustar `tracked_end_page` conforme RAM detectada
  - [ ] Validar com QEMU `-m 8G` e `-m 16G`

**Resultado esperado**: Atom roda em máquinas modernas sem recompilação.

---

#### 7.4 Preparação para Demand Paging

**Não bloqueia features futuras, mas habilita otimizações**

- [ ] **Page fault handler user-space aware**
  - [ ] Distinguir page fault legítimo vs acesso ilegal
  - [ ] Syscall `register_fault_handler(handler_fn)` (já existe)
  - [ ] Kernel notifica processo via IPC se página é COW ou swappable
  - [ ] Processo pode responder com `provide_page(addr, frame)`
  
- [ ] **Flags de página custom**
  - [ ] Bit PRESENT=0 mas logicamente alocada (lazy alloc)
  - [ ] Bit COW (copy-on-write) para fork() futuro
  - [ ] Estrutura interna para rastrear estado de páginas

**Resultado esperado**: Infraestrutura para lazy loading e swap futuro sem redesign.

---

### **FASE 8: SMP (Symmetric Multiprocessing)** 
*Objetivo: Habilitar múltiplos núcleos para melhor utilização de CPU*

**Prioridade: ALTA** — Desktop moderno exige paralelismo

#### 8.1 Detecção e Boot de CPUs

- [ ] **Parsing de ACPI MADT**
  - [ ] Identificar número de CPUs e APIC IDs
  - [ ] Detectar Local APIC base address

#### 8.2 Estruturas Per-CPU

- [ ] **Per-CPU data**
  - [ ] Usar MSR GS_BASE (x86_64) ou equivalente
  - [ ] Estrutura `CpuLocal` com: CPU ID, current thread, idle thread
  - [ ] Stack de interrupção por CPU (IST)
  
- [ ] **Scheduler per-CPU**
  - [ ] Fila de ready threads por CPU ou
  - [ ] Fila global com lock (MVP para 2-4 cores)
  - [ ] Load balancing rudimentar (round-robin de threads novas)

#### 8.3 Sincronização Kernel SMP-safe

- [ ] **Audit de spinlocks**
  - [ ] Revisar todos Mutex/RwLock no kernel
  - [ ] Verificar proteção de: thread list, capability table, IPC queues, PMM bitmap
  - [ ] Usar atomic operations onde aplicável (PMM já usa)
  
- [ ] **Timer ticks global vs per-CPU**
  - [ ] Decidir: GLOBAL_TICKS atomic ou PER_CPU_TICKS[]
  - [ ] Garantir coerência em IPC timestamps

#### 8.4 Validação SMP

- [ ] **Testes de stress**
  - [ ] Rodar N threads em M cores (N >> M)
  - [ ] Verificar ausência de race conditions
  - [ ] Memory barriers corretos (acquire/release)
  - [ ] Validar que scheduler distribui carga

**Resultado esperado**: Atom roda eficientemente em 2-8 cores, com preempção local e global.

---

### **FASE 9: Drivers em User Space**
*Objetivo: Mover drivers restantes para userland e implementar descoberta dinâmica*

#### 9.1 Migração de Drivers Embarcados

**Problema**: Driver AHCI e FAT32 ainda no kernel

- [ ] **Driver de Disco SATA (userland)**
  - [ ] Portar lógica AHCI para processo em user space
  - [ ] Kernel concede DeviceCap(BDF) + IRQCap
  - [ ] Driver mapeia MMIO do controlador (via syscall)
  - [ ] Comunicação com kernel via IPC para requisições de bloco
  
- [ ] **Serviço de Sistema de Arquivos**
  - [ ] Implementar VFS básico em userland
  - [ ] Suporte a FAT32 (reusar código do kernel)
  - [ ] Syscalls de alto nível: `open`, `read`, `write`, `close`
  - [ ] Kernel traduz syscalls → mensagens IPC para fs_server

#### 9.2 Device Manager e Hotplug

- [ ] **Serviço device_manager**
  - [ ] Recebe lista de dispositivos PCI do kernel (via IPC)
  - [ ] Mantém mapa: Device BDF → Driver name
  - [ ] Spawn de drivers sob demanda
  
- [ ] **Hotplug USB**
  - [ ] Driver xHCI notifica device_manager de novos devices
  - [ ] Manager identifica tipo (HID, storage, etc.)
  - [ ] Spawn driver apropriado com DeviceCap + IRQCap
  
- [ ] **Capabilities por dispositivo**
  - [ ] Kernel cria DeviceCap(BDF) único por PCI device
  - [ ] Manager delega ao driver correto via IPC

**Resultado esperado**: Kernel 100% policy-free, todos drivers isolados em userland.

---

### **FASE 10: Multitarefa Gráfica (Multi-Window)**
*Objetivo: Desktop real com múltiplas janelas e apps concorrentes*

#### 10.1 Window Manager no ui_shell

- [ ] **Protocolo de janelas**
  - [ ] Apps criam janelas via IPC: `create_window(title, w, h)`
  - [ ] Shell retorna WindowID
  - [ ] Apps enviam comandos de desenho: `draw_rect`, `draw_text`, etc.
  
- [ ] **Compositor**
  - [ ] Shell mantém lista de janelas (Z-order)
  - [ ] Renderiza cada janela em buffer off-screen
  - [ ] Compõe framebuffer final (ordem Z)
  - [ ] Suporte a overlap, minimizar, maximizar

#### 10.2 Roteamento de Input

- [ ] **Foco de janela**
  - [ ] Shell rastreia janela ativa
  - [ ] Eventos de teclado → IPC para app focado
  - [ ] Eventos de mouse → app sob cursor ou focado
  
- [ ] **Biblioteca libGUI**
  - [ ] Wrapper em Rust para protocolo de janelas
  - [ ] Widgets básicos: Button, Label, TextBox
  - [ ] Event loop: apps recebem KeyDown, MouseClick, etc.

#### 10.3 Multithreading em Apps

- [ ] **Syscall `thread_create` para user space**
  - [ ] Thread compartilha address space do processo
  - [ ] Aloca stack separada (256 KiB)
  - [ ] Contexto inicial: RIP = função, RSP = stack
  
- [ ] **Syscall `thread_join(tid)`**
  - [ ] Espera thread filha terminar
  - [ ] Retorna exit code
  
- [ ] **Validação**
  - [ ] App com 2 threads: uma UI, uma worker
  - [ ] Worker faz cálculo pesado sem travar UI

**Resultado esperado**: Desktop com múltiplas janelas, apps responsivos, input roteado.

---

### **FASE 11: Sistema de Arquivos Completo**
*Objetivo: Persistência de dados e carregamento dinâmico de apps*

#### 11.1 VFS e Mount Table

- [ ] **Virtual File System**
  - [ ] Abstração de mountpoints
  - [ ] Path resolution (`/mnt/disk/file.txt`)
  - [ ] Registro de filesystems (FAT32, ext2 futuro, etc.)

#### 11.2 Filesystem FAT32 em Userland

- [ ] **Servidor fs_fat32**
  - [ ] Lê partições via driver de disco (IPC)
  - [ ] Implementa operações: open, read, write, mkdir, delete
  - [ ] Cache de blocos em memória
  
- [ ] **Integração com VFS**
  - [ ] VFS delega operações → IPC para fs_fat32
  - [ ] Apps usam syscalls padrão, transparente

#### 11.3 File Descriptor Table

- [ ] **FD por processo**
  - [ ] Array de file descriptors (0=stdin, 1=stdout, 2=stderr)
  - [ ] Kernel mantém mapeamento FD → (filesystem, inode, offset)
  - [ ] Syscalls: `dup`, `dup2`, `pipe` (futuro)

**Resultado esperado**: Apps leem/escrevem arquivos, init carrega binários do disco.

---

### **FASE 12: Rede e Conectividade**

- [ ] Driver NIC em userland (Intel E1000 ou VirtIO-net)
- [ ] Stack TCP/IP (serviço `netd`)
  - [ ] Portar LwIP ou implementar básico
  - [ ] Suporte a UDP, TCP, ICMP, ARP
- [ ] Interface de sockets para apps
  - [ ] `socket()`, `bind()`, `connect()`, `send()`, `recv()`
  - [ ] Via IPC com netd

### **FASE 13: Áudio e Multimídia**

- [ ] Driver de áudio (AC97 ou Intel HDA)
- [ ] Serviço de mixer (`audiod`)
  - [ ] Mistura streams de múltiplos apps
  - [ ] Controle de volume por app

### **FASE 14: Port para ARM64**

- [ ] Boot UEFI ARM64
- [ ] MMU ARM64 (TTBR0/TTBR1, 4KB pages)
- [ ] GIC (Generic Interrupt Controller)
- [ ] Context switching ARM64
- [ ] Validação em hardware (Raspberry Pi 4, QEMU)

### **FASE 15: Otimizações Avançadas**

- [ ] Huge Pages (2 MiB, 1 GiB) para kernel e apps
- [ ] PCID (Process Context ID) para evitar TLB flush
- [ ] SMEP/SMAP (x86_64) para hardening
- [ ] Scheduler CFS ou BFS (substituir round-robin)
- [ ] NUMA-aware memory allocation

### **FASE 16: Segurança e Auditing**

- [ ] ASLR (Address Space Layout Randomization)
- [ ] Stack canaries em user space
- [ ] Audit completo de código `unsafe`
- [ ] Fuzzing de syscalls (AFL, libFuzzer)
- [ ] Formal verification de componentes críticos (TLA+, Coq)

---

## 🎯 Decisões Arquiteturais Chave

### Já Tomadas ✅
- Microkernel puro (drivers em userland)
- Capabilities para controle de acesso
- IPC message-passing com zero-copy
- Scheduler por prioridade + round-robin
- SYSCALL/SYSRET para transições rápidas

### A Tomar 🤔
- **Filesystem**: Continuar FAT32 ou portar ext2?
- **Rede**: LwIP vs stack próprio?
- **Scheduler SMP**: Global queue vs per-CPU queues?
- **GUI**: Compositor Wayland-like ou X11-like?

---

## 📊 Métricas de Sucesso

### Técnicas
- [x] Boot em <5s (QEMU)
- [x] Latência IPC <10μs (média)
- [ ] Suporte a 1000+ processos simultâneos
- [ ] Zero kernel panics em 24h de stress test
- [ ] <100 linhas de `unsafe` no código crítico

### Funcionais
- [ ] Rodar navegador web simples
- [ ] Compilar código Rust no próprio OS
- [ ] Reproduzir áudio e vídeo
- [ ] Conectar à internet e fazer HTTP requests
- [ ] Suporte a hotplug USB (teclado, mouse, storage)

---

## 🤝 Contribuições e Revisão

**Mantenedores**: [fpedrolucas95](https://github.com/fpedrolucas95/)
**Licença**: Apache 2.0  
**Repositório**: [GitHub - Atom OS](https://github.com/fpedrolucas95/atom)

**Este roadmap será revisado mensalmente.** Pull requests com sugestões de priorização ou novas features são bem-vindos!

---

**Última revisão**: 30/01/2025  
**Próxima revisão**: 28/02/2025
