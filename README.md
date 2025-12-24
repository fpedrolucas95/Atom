# Atom Kernel

![Atom with UI loaded on QEMU](.images/ui.png)
![Atom kernel showing terminal on QEMU](.images/qemu_screenshot.png)

**Atom Kernel** is an experimental operating system kernel written in **Rust**, following a **capability-based microkernel design**.

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
---
config:
  layout: dagre
  look: classic
  theme: neutral
---
flowchart TB
 subgraph Firmware["Firmware / Boot"]
    direction TB
        FW["UEFI Firmware"]
        BootEntry["Boot Entry"]
        BootInfo["Boot Information"]
  end

 subgraph Memory["Memory Management"]
    direction TB
        PhysMem["Physical Memory"]
        VirtMem["Virtual Memory"]
        KHeap["Kernel Heap"]
        AddrSpace["Address Spaces"]
  end

 subgraph Execution["Execution & CPU"]
    direction TB
        CPU["CPU Primitives"]
        Priv["Privilege & Task State"]
        IDT["Interrupt Table"]
        IntCtrl["Interrupt Controller"]
        Handlers["Interrupt Handlers & Timer"]
  end

 subgraph Scheduling["Process Control"]
        Context["Threads & Context Switching"]
        Scheduler["Preemptive Scheduler (Kernel Threads)"]
        Proc["Process Abstraction (Planned)"]
  end

 subgraph KServices["System Interfaces"]
        Syscalls["System Calls"]
        IPC["Inter-Process Communication"]
        Shm["Shared Memory"]
        Caps["Capabilities"]
        IRQCaps["IRQ / Device Caps (Planned)"]
  end

 subgraph KDrivers["Internal Drivers"]
        Graphics["Graphics Output"]
        Input["Keyboard Input"]
  end

 subgraph Kernel["Kernel Core"]
        KernelInit["Kernel Entry"]
        Memory
        Execution
        Scheduling
        KServices
        KDrivers
  end

 subgraph UserSpace["User Space (Planned)"]
        Init["Init Process"]
        UserDrivers["User-Space Drivers"]
        Services["System Services (FS, Net, Storage)"]
        VFS["VFS / FS Server"]
  end

    %% Boot flow
    FW --> BootEntry --> BootInfo --> KernelInit

    %% Memory
    PhysMem --> VirtMem
    VirtMem --> KHeap & AddrSpace

    %% CPU / Interrupts
    CPU --> Priv --> IDT --> IntCtrl --> Handlers

    %% Kernel init fan-out
    KernelInit --> PhysMem
    KernelInit --> CPU
    KernelInit --> Graphics

    %% Scheduling
    KHeap --> Context
    AddrSpace --> Context
    Context --> Scheduler
    Handlers --> Scheduler
    Handlers --> Input
    Scheduler -. schedules .-> Init
    Scheduler -. schedules .-> Services

    %% Syscalls & IPC
    Syscalls --> Scheduler
    Syscalls --> IPC
    Syscalls --> Shm
    Syscalls --> Caps
    Caps -.-> IRQCaps

    Handlers -- timer tick --> IPC

    %% User space (future)
    IPC ==> Init
    Caps -.-> Init
    Init --> Services
    Services -.-> VFS

    Graphics -. future migration to .-> UserDrivers
    Input -. future migration to .-> UserDrivers
    UserDrivers -. IPC + Caps .-> Services

    %% Classes
     BootInfo:::firmware
     BootEntry:::firmware
     FW:::firmware

     PhysMem:::memory
     VirtMem:::memory
     KHeap:::memory
     AddrSpace:::memory

     CPU:::execution
     Priv:::execution
     IDT:::execution
     IntCtrl:::execution
     Handlers:::execution
     Context:::execution
     Scheduler:::execution

     Syscalls:::services
     IPC:::services
     Shm:::services
     Caps:::services

     Graphics:::drivers
     Input:::drivers

     KernelInit:::kernel

     Init:::user
     UserDrivers:::user
     Services:::user
     VFS:::user
     Proc:::planned
     IRQCaps:::planned

    classDef firmware fill:#fde2ff,stroke:#6b2c91,stroke-width:1.5px
    classDef kernel fill:#f8faff,stroke:#2c4f91,stroke-width:2px
    classDef memory fill:#e8f6ff,stroke:#1f6fa5,stroke-width:1.5px
    classDef execution fill:#eef3ff,stroke:#3a4a8f,stroke-width:1.5px
    classDef services fill:#eef7ee,stroke:#2f7a2f,stroke-width:1.5px
    classDef drivers fill:#fff4e6,stroke:#a65b1f,stroke-width:1.5px
    classDef user fill:#e9f7ef,stroke:#2e8b57,stroke-width:1.5px
    classDef planned opacity:0.7,stroke-dasharray: 5 5
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
