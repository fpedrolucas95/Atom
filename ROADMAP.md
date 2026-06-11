# Atom OS — Roadmap Técnico Atualizado

> **Status do projeto**: microkernel funcional e experimental, com kernel, user space, runtime C/Rust, desktop, rede funcional em QEMU e suporte a aplicações nativas  
> **Última revisão deste roadmap**: 10 de Junho de 2026  
> **Release de referência**: alpha_5  
> **Arquitetura atual**: x86_64  
> **Escopo atual validado**: QEMU/OVMF

---

## Visão Geral

O Atom já passou da fase de “fundação do kernel”. O estado atual do código mostra um sistema com:

- boot UEFI funcional
- PMM/VMM/heap
- scheduler preemptivo
- syscalls amplas
- IPC + shared memory
- capability system
- modelagem de processo
- spawn de executáveis ATXF
- `mmap`, `munmap`, `mprotect`, `brk`, `fork`
- filesystem FAT32 com leitura/escrita
- ambiente desktop
- libc freestanding
- TinyGL
- infraestrutura de rede em user space com `nic_driver` + `netd`

O foco do roadmap deixa de ser “construir o básico” e passa a ser:

1. **consolidar o que já existe**
2. **fechar lacunas de segurança e coerência**
3. **remover caminhos híbridos / transitórios**
4. **preparar o sistema para hardware real, SMP e perfis de produto**

---

## Concluído até alpha_5

### 1. Fundação do Kernel

- [x] Boot UEFI em x86_64 com transição para o kernel
- [x] Physical Memory Manager com bootstrap em duas fases e suporte a até 16 GiB
- [x] Virtual Memory Manager com paginação 4-level, deep-copy de page tables, verificação/reparo e rastreamento de VMA
- [x] Guard pages e metadados de stack em user space
- [x] Heap allocator do kernel (slab + fallback por páginas)
- [x] Tratamento de interrupções e exceções (IDT + APIC Local + timer)
- [x] Context switching em assembly x86_64 com trampoline higher-half, validação de stack canaries, validação de endereços canônicos e consistência de CR3
- [x] Scheduler preemptivo por prioridade com round-robin por nível
- [x] Logging estruturado por subsistema
- [x] Self-tests arquiteturais no boot

### 2. IPC, Shared Memory e Capabilities

- [x] IPC com portas, mensagens, sync/async, batching e `wait_any`
- [x] Detecção de deadlock em IPC
- [x] Priority inheritance
- [x] Shared memory com mapeamento dinâmico e cleanup na saída do dono
- [x] Capability system com handles, permissões, derivação e revogação transitiva
- [x] Audit log de capabilities

### 3. Modelo de Processo, Thread e Address Space

- [x] Processo isolado com PML4 próprio
- [x] Registry de processos com `ProcessId`, `Process`, `PROCESS_REGISTRY` e `PML4_TO_PROCESS`
- [x] Tabela de capabilities canônica por processo
- [x] Metadados de processo: threads, PML4, accounting de memória, memory limit, flags de lifecycle
- [x] Pipeline determinístico de terminação de processo
- [x] Cleanup de threads, address space, shared memory, FDs e capabilities no teardown
- [x] `mmap`, `munmap`, `mprotect`, `brk`
- [x] `fork()` com clonagem de address space e COW

### 4. Syscall Layer

- [x] ~116 syscalls cobrindo threads, IPC, capabilities, memória compartilhada, vídeo, processos, MM, PCI/MMIO/DMA/IRQ e filesystem
- [x] Taxonomia de erros operacionais/contextuais em syscall path
- [x] Containment local de corrupção operacional
- [x] Validação ABI-typed para boa parte dos ponteiros de user space
- [x] Testes de hardening na syscall layer

### 5. Runtime, Executáveis e User Space

- [x] `init` isolado em Ring 3
- [x] `service_manager` com boot declarativo
- [x] `namesvc`
- [x] Loader ATXF v3 com assinatura Ed25519, verificada contra raiz de confiança no kernel antes do mapeamento
- [x] `SYS_SPAWN_PROCESS`
- [x] `SYS_SPAWN_FROM_PATH`
- [x] `app_launcher`
- [x] Lançamento de apps em runtime a partir do filesystem

### 6. libc, ABI e Ferramentas

- [x] libc freestanding com `crt0.S`
- [x] `malloc/free` via `mmap/munmap`
- [x] stdio/string/stdlib/assert/ctype/time/math básicos
- [x] `vsnprintf`
- [x] crate `atom_abi` como fonte única de verdade de ABI
- [x] toolchain de build ATXF

### 7. Gráficos, Desktop e Apps

- [x] Driver BGA com troca de resolução em runtime
- [x] Query de modos de vídeo
- [x] Framebuffer / compositor com superfícies compartilhadas
- [x] Dock, janelas, z-order, shutdown gracioso de janela
- [x] `libgui`
- [x] TinyGL portado e integrado
- [x] Terminal, file manager, display settings e demos

### 8. Filesystem

- [x] FAT32 no kernel com leitura e escrita
- [x] Syscalls de backend para fsd (`SYS_KERN_FS_*`)
- [x] Syscalls POSIX básicas visíveis para apps
- [x] `open`, `close`, `read`, `write`, `seek`, `stat`, `fstat`, `mkdir`, `rmdir`, `unlink`, `rename`, `readdir`, `fsync`
- [x] Estrutura reorganizada de diretórios do sistema
- [x] Ownership básico de FDs por processo e cleanup por processo no kernel

### 9. Infraestrutura de Drivers e Rede

- [x] Infraestrutura PCI
- [x] Query de BARs e identidade de dispositivo
- [x] Binding/listen/ack de IRQ em user space
- [x] `dma_alloc`
- [x] `map_mmio`
- [x] `nic_driver` (e1000)
- [x] `netd` com ARP, IPv4, ICMP, UDP, TCP e DNS em QEMU user-net
- [x] Casos funcionais de rede em user space (`ping`, HTTP GET e serviço `timesync`)

---

## Em consolidação após alpha_5

Estas áreas já existem, mas ainda não devem ser tratadas como “fechadas”:

- [~] modelo de processo consolidado, porém ainda com vestígios thread-centric
- [~] capability system completo em infraestrutura, mas com enforcement desigual
- [~] networking funcional, mas ainda limitado ao ambiente QEMU user-net
- [~] filesystem funcional, porém com dual path (kernel backend + fsd path)
- [~] desktop funcional, mas ainda com ergonomia e primitives incompletas
- [~] user pointer validation amplamente melhorada, mas ainda não auditada ponta a ponta
- [~] threading/process teardown robusto, porém ainda precisando de stress e simplificação
- [~] infra de drivers userspace boa, mas com lifecycle incompleto de DMA/MMIO/IRQ

---

## Fase 7 — Hardening, Coerência e Fechamento de Lacunas

**Prioridade: CRÍTICA**

Objetivo: transformar a base atual em um sistema coerente, previsível e seguro o suficiente para sustentar SMP, hardware real e perfis de produto.

### 7.1 Enforcement real de capabilities

- [ ] Remover over-provisioning no spawn
- [ ] Parar de conceder `Framebuffer`, `Keyboard`, `Mouse` e portas PS/2 a todos os processos
- [ ] Restringir capabilities de input/display apenas aos serviços corretos
- [ ] Adicionar autoridade explícita para `SYS_SPAWN_FROM_PATH`
- [ ] Auditar syscalls ainda não protegidas por capability checks consistentes
- [ ] Revisar `sys_io_port_read` / `sys_io_port_write` para modelo capability-first por recurso
- [ ] Revisar todas as rotas “MVP/bypass” remanescentes e convertê-las para `EPERM`/`EINVAL` real

### 7.2 Auditoria final de ponteiros de user space

- [ ] Auditar todas as syscalls restantes para dereference de ponteiro de user space
- [ ] Eliminar caminhos que ainda dependem de validação parcial ou indireta
- [ ] Padronizar leitura/escrita de memória de user space em helpers únicos
- [ ] Introduzir estratégia incremental de `copy_from_user` / `copy_to_user` mais forte
- [ ] Garantir cobertura de testes para ranges canônicos, overflow, alinhamento e janela userspace

### 7.3 Consolidação do modelo processo/thread/capability table

- [ ] Reduzir inconsistência entre capability table do processo e espelhos por thread
- [ ] Clarificar a autoridade canônica: processo como dono, thread como cache/mirror
- [ ] Desacoplar gradualmente recursos globais ainda modelados por thread
- [ ] Revisar o acoplamento entre `ProcessId` e `ThreadId` da thread primária
- [ ] Preparar terreno para `exec`, grupos de processos e resource limits mais fortes

### 7.4 Coerência da semântica de IPC

- [ ] Corrigir `sys_ipc_send` para enviar payload real
- [ ] Corrigir `sys_ipc_send_with_cap` para enviar payload real
- [ ] Uniformizar semântica entre send sync, async e send-with-cap
- [ ] Garantir testes para payload, capability delegation e ordering
- [ ] Validar compatibilidade da wire format entre serviços e kernel

### 7.5 Atomicidade do blocking em IPC

- [ ] Tornar `block_receive` + `set_thread_state(Blocked)` atomicamente coerentes
- [ ] Aplicar mesma correção a `sys_ipc_wait_any`
- [ ] Revisar wake-up path e janelas de race de `mark_thread_ready`
- [ ] Formalizar regras de lock ordering no subsistema IPC
- [ ] Garantir que a solução seja SMP-safe desde o desenho

### 7.6 Robustez de terminação e accounting

- [ ] Stress-test de `terminate_entity` e `terminate_process`
- [ ] Garantir zero drift de accounting em cenários de fork/exit/shared memory
- [ ] Validar edge cases: último thread, processos em blocking IPC, OOM kill, cleanup duplicado
- [ ] Consolidar invariantes de teardown em testes automatizados
- [ ] Medir vazamentos de páginas, handles, FDs e portas IPC sob carga longa

### 7.7 Sincronização da documentação

- [x] Alinhar `README.md`, `README-PTBR.md` e `ROADMAP.md` ao estado real do código
- [x] Remover itens já implementados do backlog “futuro” (rede e SMP saíram de “futuro”)
- [ ] Publicar matriz simples “implementado / parcial / futuro” por subsistema
- [x] Documentar explicitamente limitações reais atuais: QEMU-only, SMP só em QEMU, enforcement parcial, hardware real pendente

---

## Fase 8 — Arquitetura de Filesystem e I/O

**Prioridade: ALTA**

Objetivo: sair do estado híbrido atual e estabilizar o subsistema de arquivos e FDs.

### 8.1 Migrar de tabela global de FD para tabela por processo

- [ ] Substituir `KERNEL_FD_TABLE` global como autoridade principal
- [ ] Introduzir tabela de FDs por processo
- [ ] Manter `stdin/stdout/stderr` por processo
- [ ] Preservar cleanup no teardown
- [ ] Eliminar a dependência de um pool global fixo de 128 FDs
- [ ] Definir semântica de herança/duplicação de FDs

### 8.2 Desacoplar I/O de disco dos locks globais

- [ ] Refatorar `sys_fs_read` para não manter lock de FD table durante I/O FAT32
- [ ] Refatorar `sys_fs_seek` e caminhos correlatos com a mesma filosofia
- [ ] Reduzir tempo de lock em operações de cache e leitura
- [ ] Medir contenção e concorrência entre processos/apps usando FS

### 8.3 Escolher e consolidar o caminho arquitetural do FS

- [ ] Definir se o caminho canônico será `app -> fsd -> backend` ou acesso direto temporário no kernel
- [ ] Remover duplicação estrutural entre syscalls de backend e syscalls de app
- [ ] Preservar backend mínimo no kernel apenas quando necessário
- [ ] Tornar o modelo microkernel de filesystem explícito e consistente

### 8.4 Completar a superfície POSIX faltante

- [ ] Implementar `truncate`
- [ ] Implementar `dup`, `dup2`
- [ ] Implementar `pipe`
- [ ] Revisar viabilidade de `chmod`, `utimes`, `statvfs`
- [ ] Decidir sobre `link`, `symlink`, `readlink`
- [ ] Definir claramente o subconjunto POSIX suportado

### 8.5 Maturidade do FAT32 e recuperação

- [ ] Expandir cobertura de replay/recovery
- [ ] Revisar semântica de flush e durability
- [ ] Melhorar cache de blocos
- [ ] Validar comportamento em crash/reboot
- [ ] Avaliar quando FAT32 deixa de ser suficiente para próximos perfis de sistema

### 8.6 VFS

- [ ] Criar camada VFS com mount table
- [ ] Suportar múltiplos backends
- [ ] Resolver paths com mount points
- [ ] Preparar o sistema para ext2/ext4 ou FS próprio no futuro

---

## Fase 9 — Drivers em User Space e Modelo de Dispositivos

**Prioridade: ALTA**

Objetivo: concluir o desenho microkernel para hardware e serviços de dispositivo.

### 9.1 Completar lifecycle de DMA/MMIO/IRQ

- [ ] Implementar `sys_dma_map`
- [ ] Implementar `sys_dma_free`
- [ ] Garantir cleanup/revogação de buffers DMA
- [ ] Consolidar ownership e teardown de regiões MMIO
- [ ] Testar exaustivamente falhas e revogação durante uso

### 9.2 Device manager

- [ ] Criar serviço `device_manager`
- [ ] Manter mapa BDF -> driver
- [ ] Delegar capabilities para drivers corretos
- [ ] Suportar discovery mais limpo de dispositivos PCI
- [ ] Preparar caminho para hotplug e enumeração mais sofisticada

### 9.3 Migração progressiva de drivers para userland

- [ ] Definir roadmap de migração de AHCI
- [ ] Definir roadmap de migração/isolamento mais forte de xHCI
- [ ] Reduzir TCB do kernel onde fizer sentido
- [ ] Medir custo de IPC + batching + shared memory nesses caminhos

---

## Fase 10 — Rede: de funcional para produto

**Prioridade: MÉDIA-ALTA**

Objetivo: transformar a stack atual de rede em base robusta para apps, serviços e perfis headless/kiosk.

### 10.1 Estabilização da stack existente

- [ ] Expandir testes de `nic_driver` + `netd`
- [ ] Cobrir falhas de link, timeouts, resets e recovery
- [ ] Melhorar tracing e diagnóstico de rede
- [ ] Medir throughput, latência e consumo de memória

### 10.2 API de rede para apps

- [ ] Consolidar interface de sockets ou IPC-friendly API para apps
- [ ] Expor bind/connect/send/recv de forma estável
- [ ] Validar integração com libc e apps C/Rust
- [ ] Garantir uma história clara para DNS do ponto de vista de app

### 10.3 Produção e portabilidade da rede

- [ ] Validar backends além de QEMU user-net
- [ ] Melhorar suporte real a e1000
- [ ] Avaliar suporte a VirtIO-net
- [ ] Definir estratégia de DHCP, hostname e configuração persistente
- [ ] Tornar rede utilizável em perfis headless e kiosk

---

## Fase 11 — SMP (Symmetric Multiprocessing)

**Prioridade: ALTA, bloqueada por Fases 7–9**

Objetivo: habilitar execução multi-core com invariantes corretos.

### 11.1 Bootstrap de CPUs

- [x] Parsing de ACPI MADT
- [x] Startup de APs via SIPI
- [x] GDT, IDT, TSS e stacks por CPU
- [x] Inicialização de `CpuLocal`

### 11.2 Estruturas per-CPU

- [x] Dados per-CPU via `GS_BASE` (syscall stack state)
- [x] Idle thread por CPU
- [x] IST/stacks por CPU
- [x] Ready queues per-CPU

### 11.3 Kernel SMP-safe

- [ ] Auditar todos os locks do kernel
- [x] Revisar atomicidade de IPC, scheduler, PMM, e registries críticos para execução multicore
- [ ] Garantir regras formais de ordering de locks
- [x] Revisar ticks/time slice em contexto per-CPU

### 11.4 Scheduler SMP

- [x] Load balancing inicial
- [x] Migração de threads
- [x] Estratégia de afinidade
- [ ] Medir fairness e starvation

### 11.5 Validação SMP

- [ ] Stress test com N threads / M cores
- [x] Rodar user space completo em SMP
- [ ] Validar que não há races de teardown, IPC ou scheduler
- [ ] Estabelecer baseline de throughput

---

## Fase 12 — Plataforma de Aplicações e Desktop

**Prioridade: MÉDIA**

Objetivo: tornar o sistema utilizável como plataforma multi-app, sem confundir isso com prioridade antes do hardening.

### 12.1 Window management

- [ ] Minimizar
- [ ] Maximizar
- [ ] Redimensionar
- [ ] Arrastar janelas
- [ ] Alt-Tab
- [ ] Clipboard

### 12.2 Expansão da `libgui`

- [ ] Widgets básicos
- [ ] Inputs de texto
- [ ] Scrollbars
- [ ] Menus
- [ ] Layout horizontal/vertical/grid
- [ ] Event loop padrão

### 12.3 Threading de user space

- [x] `sys_thread_create` já existe
- [ ] Definir API estável de criação de threads para apps
- [ ] Implementar `join`
- [ ] Revisar stacks e lifecycle para threads userspace
- [ ] Validar apps com UI thread + worker thread

### 12.4 Empacotamento e UX de apps

- [ ] Melhorar manifestos/metadados de app
- [ ] Definir convenção de instalação/ícones/categorias
- [ ] Melhorar integração com launcher/file manager
- [ ] Preparar terreno para atualização e remoção de apps

---

## Fase 13 — Perfis de Sistema / OS Composable

**Prioridade: MÉDIA**

Objetivo: transformar a arquitetura orientada a serviços em produto configurável por perfil.

### 13.1 Loader de perfil

- [ ] Arquivos de perfil em `/system/profiles/`
- [ ] Seleção por argumento de boot
- [ ] `init` carrega apenas os serviços daquele perfil
- [ ] Capabilities e permissões definidas por perfil

### 13.2 Perfil Desktop

- [ ] Consolidar o perfil atual como baseline oficial

### 13.3 Perfil Kiosk

- [ ] Boot direto em single-app
- [ ] Conjunto mínimo de capabilities
- [ ] Sem shell desktop
- [ ] Política de atualização e recuperação simplificada

### 13.4 Perfil Embedded / Headless

- [ ] Sem display/compositor
- [ ] Apenas rede/filesystem/daemons
- [ ] Otimização de footprint
- [ ] Foco em appliances e IoT

### 13.5 Perfil TV / HTPC

- [ ] `tv_shell`
- [ ] Navegação por D-pad / foco espacial
- [ ] Fullscreen surfaces + overlays
- [ ] Pipeline de mídia
- [ ] Decisão sobre runtime web opcional

---

## Fase 14 — ARM64

**Prioridade: MÉDIA**

- [ ] Boot UEFI em ARM64
- [ ] MMU ARM64
- [ ] GIC
- [ ] Context switch ARM64
- [ ] QEMU aarch64
- [ ] Raspberry Pi 4 ou plataforma equivalente

---

## Fase 15 — Otimização e Performance

**Prioridade: MÉDIA**

- [ ] Huge pages
- [ ] PCID
- [ ] Melhorias de TLB behavior
- [ ] Revisão do scheduler para além do round-robin por prioridade
- [ ] Perfilamento de IPC, FS, rede e compositor
- [ ] Redução de contenção em locks globais remanescentes
- [ ] Footprint tuning para perfis kiosk/embedded

---

## Fase 16 — Auditoria e Segurança Avançada

**Prioridade: MÉDIA-ALTA**

- [ ] ASLR
- [ ] Hardening adicional de user space
- [ ] Auditoria completa de `unsafe`
- [ ] Fuzzing de syscalls
- [ ] Fuzzing de parsers e IPC
- [ ] Verificação formal de invariantes críticas
- [ ] Política de segurança e threat model documentados
- [ ] Base para certificação futura, onde fizer sentido

---

## Decisões Arquiteturais

### Consolidadas

- microkernel com user space orientado a serviços
- capabilities como modelo de autoridade
- IPC como backbone de composição
- shared memory para zero-copy
- scheduler por prioridade
- `SYSCALL/SYSRET` como caminho principal user↔kernel
- ATXF como formato binário de user space
- `atom_abi` como ABI compartilhada
- `init -> service_manager -> namesvc` como estrutura de boot do user space

### Em aberto

- filesystem futuro além de FAT32
- desenho final da VFS
- estratégia final de socket API
- ready queue global vs per-CPU no SMP
- nível de compatibilidade POSIX desejado
- alcance do runtime web em perfis de TV/kiosk
- estratégia de hardware real prioritário

---

## Métricas de Sucesso

### Já alcançadas

- [x] Boot em QEMU/OVMF
- [x] User space funcional e isolado
- [x] Apps C/Rust executando
- [x] Spawn em runtime via filesystem
- [x] Desktop funcional
- [x] TinyGL funcionando
- [x] Troca dinâmica de resolução
- [x] Rede funcional em QEMU user-net com `ping`/HTTP

### Próximos alvos técnicos

- [ ] Todas as syscalls sensíveis com enforcement consistente de capabilities
- [ ] Nenhum payload path quebrado em IPC send
- [ ] Nenhum lock global mantido durante I/O de disco
- [ ] Tabela de FDs por processo
- [ ] FS com caminho arquitetural único
- [ ] Networking estável fora do caso feliz de QEMU
- [x] SMP funcional em 2–4 cores (QEMU)
- [ ] 24h de stress sem panic
- [ ] Hardware real validado

---

## Prioridade executiva resumida

### Agora
1. hardening de capabilities  
2. auditoria final de ponteiros  
3. corrigir IPC send / send_with_cap  
4. atomicidade de IPC blocking  
5. consolidar processo/thread/cap tables  

### Em seguida
6. refatorar FD table e locks de I/O  
7. unificar arquitetura do filesystem  
8. completar lifecycle de DMA/MMIO/IRQ  
9. estabilizar networking para uso real  

### Depois
10. SMP  
11. perfis de produto  
12. ARM64  
13. otimizações avançadas  
14. auditoria de segurança profunda

---

## Contribuindo

Contribuições são especialmente valiosas em:

- hardening de segurança
- enforcement de capabilities
- FS/VFS
- testes e stress
- networking
- SMP hardening e stress testing
- documentação arquitetural
- tracing/debugging/observabilidade

---

**Mantenedor**: `fpedrolucas95`  
**Licença**: Apache 2.0  
**Repositório**: Atom OS
