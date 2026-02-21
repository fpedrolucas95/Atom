# Atom Operating System

<img width="912" height="744" alt="Atom Desktop Environment with Terminal" src="https://github.com/user-attachments/assets/2e9248ef-fae0-4720-a3bd-035e511b5c5d" />
<img width="912" height="744" alt="Atom Desktop Environment with File Manager" src="https://github.com/user-attachments/assets/44b7a931-3c27-4073-83f8-1f4ce8b8b013" />

**Atom** is an experimental operating system kernel written in **Rust**, following a **capability-based microkernel design**.

The project is structured around a small, reliable kernel core responsible for bootstrapping, memory management, interrupts, scheduling, and system calls, while higher-level services and policies—such as drivers, filesystems, and networking—are designed to live in user space.

### Why this project exists

Atom Kernel is built to explore and validate OS principles in a practical, incremental way:

- **Security by design** with *capabilities* (least privilege, explicit delegation, revocation)
- **Strong isolation** using separate address spaces and validated memory mappings
- **Message-passing first** architecture via IPC (with engineering for real-world scheduling problems)
- **Observability** early in the stack (logs, tracing, stats) to make kernel development debuggable
- **A roadmap-driven approach**, delivered in phases, to keep the system evolving without losing coherence

### Architecture at a glance

> The diagram below reflects the current structure and the planned migration of drivers/services to user space.

```mermaid
flowchart TB
    %% ============================================================================
    %% FIRMWARE & BOOT LAYER
    %% ============================================================================
    subgraph Firmware["🔌 Firmware & Boot Layer"]
        direction LR
        FW["UEFI Firmware"]
        BootEntry["Boot Entry Point"]
        BootInfo["Boot Information<br/>(Memory Map, Devices)"]
        
        FW --> BootEntry --> BootInfo
    end

    %% ============================================================================
    %% MICROKERNEL CORE
    %% ============================================================================
    subgraph Kernel["⚙️ Microkernel Core (Ring 0)"]
        direction TB
        
        %% Memory Management Subsystem
        subgraph Memory["💾 Memory Management"]
            direction TB
            PhysMem["Physical Memory Manager<br/>(Bitmap Allocator)"]
            VirtMem["Virtual Memory Manager<br/>(4-Level Paging)"]
            AddrSpace["Isolated Address Spaces<br/>(Per-Process PML4)"]
            KHeap["Kernel Heap<br/>(Bump Allocator)"]
            
            PhysMem --> VirtMem
            VirtMem --> AddrSpace
            VirtMem --> KHeap
        end
        
        %% CPU & Privilege Control
        subgraph Execution["🔐 CPU & Privilege Control"]
            direction TB
            CPU["CPU Primitives<br/>(MSRs, CR3, Segments)"]
            Priv["Privilege Levels<br/>(Ring 0/3)"]
            TSS["Task State Segments<br/>(Stack Switching)"]
            IDT["Interrupt Descriptor Table<br/>(Exceptions & IRQs)"]
            APIC["APIC Controller<br/>(Local & I/O)"]
            Timer["Timer & IRQ Handlers<br/>(Preemption)"]
            
            CPU --> Priv
            Priv --> TSS
            TSS --> IDT
            IDT --> APIC
            APIC --> Timer
        end
        
        %% Scheduling Subsystem
        subgraph Scheduling["⏱️ Scheduling & Threading"]
            direction TB
            Threads["Thread Control Blocks<br/>(TCBs)"]
            Context["Context Switching<br/>(Assembly Routines)"]
            Scheduler["Preemptive Scheduler<br/>(Priority + Round-Robin)"]
            
            Threads --> Context
            Context --> Scheduler
            Timer -.-> Scheduler
        end
        
        %% Kernel Interfaces
        subgraph Interfaces["🔗 Kernel Interfaces"]
            direction TB
            Syscalls["System Call Handler<br/>(SYSCALL/SYSRET)"]
            IPC["IPC Core<br/>(Ports, Messages, Batching)"]
            Shm["Shared Memory<br/>(Zero-Copy)"]
            Caps["Capability System<br/>(Access Control)"]
            IRQCaps["Device & IRQ Capabilities<br/>(Hardware Access)"]
            
            Syscalls --> IPC
            Syscalls --> Shm
            Syscalls --> Caps
            Caps --> IRQCaps
        end
    end

    %% ============================================================================
    %% USER SPACE LAYER
    %% ============================================================================
    subgraph UserSpace["👥 User Space (Ring 3)"]
        direction TB
        
        %% Core System Processes
        subgraph CoreProc["Core System Processes"]
            direction LR
            Init["Init Process<br/>(PID 1, Isolated)"]
            ServiceMgr["Service Manager<br/>(Lifecycle & Discovery)"]
            NameSvc["Name Service<br/>(Service Lookup)"]
        end
        
        %% System Services & Drivers
        subgraph SystemLayer["System Services & Drivers"]
            direction LR
            UI["UI Shell / Compositor<br/>(Graphics, Input)"]
            UserDrivers["User-Space Drivers<br/>(Disk, USB, Network)"]
            Services["System Services<br/>(Filesystem, Network)"]
        end
        
        %% Applications
        Apps["📱 Applications<br/>(Terminal, Browser, etc.)"]
        
        Init --> ServiceMgr
        ServiceMgr --> NameSvc
        ServiceMgr --> UI
        ServiceMgr --> UserDrivers
        ServiceMgr --> Services
        ServiceMgr --> Apps
    end

    %% ============================================================================
    %% CROSS-LAYER CONNECTIONS
    %% ============================================================================
    
    %% Boot Flow
    BootInfo ==> Kernel
    
    %% Kernel to User Space
    Scheduler -.schedules.-> Init
    Scheduler -.schedules.-> SystemLayer
    Scheduler -.schedules.-> Apps
    
    %% IPC Connections
    IPC <===> Init
    IPC <===> SystemLayer
    IPC <===> Apps
    
    %% Capability Distribution
    Caps ==> Init
    Init -.delegates.-> SystemLayer
    Init -.delegates.-> Apps
    
    %% UI to Applications
    UI <-.input/output.-> Apps
    UI <-.rendering.-> Services

    %% ============================================================================
    %% STYLING
    %% ============================================================================
    classDef firmwareStyle fill:#2c3e50,stroke:#34495e,stroke-width:2px,color:#ecf0f1
    classDef kernelStyle fill:#27ae60,stroke:#229954,stroke-width:2px,color:#ecf0f1
    classDef userStyle fill:#3498db,stroke:#2980b9,stroke-width:2px,color:#ecf0f1
    classDef coreStyle fill:#e74c3c,stroke:#c0392b,stroke-width:2px,color:#ecf0f1
    
    class Firmware,FW,BootEntry,BootInfo firmwareStyle
    class Kernel,Memory,Execution,Scheduling,Interfaces kernelStyle
    class UserSpace,SystemLayer userStyle
    class CoreProc,Init,ServiceMgr,NameSvc coreStyle
````

### Current status (high level)

Atom Kernel is under active development and already includes the foundations needed for a real kernel “spine”:

* UEFI boot + boot info handoff
* Physical & virtual memory management
* Preemptive scheduling for kernel threads
* System calls and IPC
* Capability-based access control with delegation and revocation
* Memory syscalls for user space with isolated address spaces and mapping validation
* Basic in-kernel drivers (graphics output + keyboard input), planned to migrate to user space

### Design principles

* **Minimal kernel, maximum clarity**: the kernel should do what must be trusted.
* **Policy outside the kernel**: anything configurable should move to user space over time.
* **Least privilege everywhere**: every operation must be authorized by a capability.
* **Debuggability is a feature**: tracing and observability are part of the system, not an afterthought.

### Roadmap (summary)

The roadmap is delivered in phases. The near-term direction is:

* Expand and harden the **memory syscalls** and user-space-driven memory policies (e.g., file-backed mappings, swap manager)
* Introduce the first **init process** and a minimal executable format/loader
* Start migrating drivers and system services to **user space** (VFS/FS server, storage, networking, etc.)

### Building & running (very short)

This repository targets bare metal + virtualization (commonly QEMU).

Typical requirements:

* Rust toolchain (nightly)
* QEMU
* UEFI firmware for QEMU (e.g., OVMF)

> See the repo’s scripts (e.g., Windows `build.ps1`) and workspace configuration for the up-to-date build/run flow.

### Contributing

Contributions are welcome, especially around:

* Tests, tracing, and debugging tools
* Documentation (architecture notes, “how it works” guides)
* Roadmap tasks (phased issues)

If you’re unsure where to start, open an issue describing what you want to explore.
