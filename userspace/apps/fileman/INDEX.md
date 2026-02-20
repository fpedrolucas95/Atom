# Fileman - Documentation Index

## 📖 Documentation Overview

All fileman documentation is organized by purpose. Start here to find what you need.

## For Users

### [QUICKSTART.md](QUICKSTART.md) - Start Here 🚀
**Best for**: Getting started quickly, learning basic commands  
**Contents**:
- Installation overview
- Common usage patterns
- Command reference
- Error handling guide
- Tips & tricks
- Troubleshooting

**Read this if you**:
- Just installed fileman
- Want to learn basic commands
- Need quick help with a task

---

### [README.md](README.md) - Complete User Guide
**Best for**: Detailed feature documentation and usage  
**Contents**:
- Feature descriptions
- Complete command reference
- Path handling explanation
- Error codes reference
- Integration notes
- Performance info

**Read this if you**:
- Want comprehensive documentation
- Need to understand all features
- Are integrating fileman into something

---

## For Developers

### [ARCHITECTURE.md](ARCHITECTURE.md) - Technical Design 🏗️
**Best for**: Understanding how fileman works internally  
**Contents**:
- System overview diagrams
- Module architecture
- Design patterns (RAII, etc)
- Command implementation details
- Memory management strategy
- Error handling strategy
- Performance characteristics
- Security considerations
- Future enhancement ideas

**Read this if you**:
- Want to extend fileman
- Need to understand the code
- Want to optimize performance
- Are contributing code

**Key Sections**:
- System Overview
- Module Architecture
- Memory Management
- Error Handling Strategy
- Performance Analysis

---

### [BUILD.md](BUILD.md) - Build Instructions 🔨
**Best for**: Building from source, troubleshooting builds  
**Contents**:
- Prerequisites
- Build commands
- Integration options
- Troubleshooting
- Build variants
- Testing builds
- CI/CD examples
- Makefile template

**Read this if you**:
- Want to build from source
- Have compilation errors
- Need custom build options
- Want to integrate into CI/CD

---

### [INTEGRATION.md](INTEGRATION.md) - System Integration 🔗
**Best for**: Integrating fileman into Atom OS  
**Contents**:
- Project structure explanation
- File descriptions
- Building the application
- Boot integration options
- Installation & packaging
- API usage for other programs
- Testing & validation
- Performance tuning
- Troubleshooting integration
- Version management
- Contributing guide

**Read this if you**:
- Are adding fileman to the system
- Want to launch from init
- Need to use fileman's API
- Are extending the project

---

## For Maintainers

### Project Files

| File | Purpose | Size |
|------|---------|------|
| `Cargo.toml` | Rust package manifest | ~50 lines |
| `Makefile` | Build automation | ~100 lines |
| `src/main.rs` | CLI & command dispatch | ~500 lines |
| `src/error.rs` | Error handling | ~300 lines |
| `src/fs.rs` | Filesystem abstraction | ~700 lines |

**Total Code**: ~1500 lines of production Rust

---

## Quick Navigation

### I want to...

**Learn what fileman does** → [README.md](README.md)

**Use fileman's commands** → [QUICKSTART.md](QUICKSTART.md)

**Build fileman** → [BUILD.md](BUILD.md)

**Integrate into Atom OS** → [INTEGRATION.md](INTEGRATION.md)

**Understand the code** → [ARCHITECTURE.md](ARCHITECTURE.md)

**Build from workspace** → Run `./build.sh`

**Build just fileman** → Run `make build` in this directory

**Run tests** → Run `make test` in this directory

**See build help** → Run `make help` in this directory

---

## Documentation Structure

```
fileman/
├── README.md              ← User guide & features
├── QUICKSTART.md          ← Quick command reference
├── BUILD.md              ← How to build
├── ARCHITECTURE.md       ← How it works internally
├── INTEGRATION.md        ← How to integrate
├── INDEX.md              ← You are here
├── Cargo.toml            ← Build config
├── Makefile              ← Build automation
└── src/
    ├── main.rs           ← Entry point & CLI
    ├── error.rs          ← Error handling
    └── fs.rs             ← Filesystem layer
```

---

## Key Concepts

### Commands Implemented
- **Navigation**: pwd, cd, ls
- **Files**: cat, cp, mv, rm
- **Directories**: mkdir, rmdir, rm -r
- **Help**: help, exit

### Design Goals
- 🎯 Production-ready code
- ✅ Complete error handling
- 🔒 Memory-safe (Rust)
- ⚡ Efficient I/O operations
- 📚 Well documented

### Resource Limits
- **Heap**: 1 MB
- **Max path**: 4096 bytes
- **Max open files**: Kernel enforced
- **Buffer size**: 64 KB for I/O

---

## Version Info

- **Current Version**: 0.1.0
- **Status**: Production Ready
- **Rust Edition**: 2021
- **Target**: x86_64-unknown-uefi
- **Last Updated**: 2026-02-20

---

## Running Fileman

### From Terminal
```bash
$ fileman
fileman:/> help
```

### Build & Run
```bash
make build
make test
```

### Install System-wide
```bash
make install
```

---

## Support Resources

- **Questions**: See [README.md](README.md)
- **How to use**: See [QUICKSTART.md](QUICKSTART.md)
- **Build help**: See [BUILD.md](BUILD.md)
- **Implementation details**: See [ARCHITECTURE.md](ARCHITECTURE.md)
- **Integration**: See [INTEGRATION.md](INTEGRATION.md)

---

## Document Hierarchy

```
Documentation
├── User Level
│   ├── QUICKSTART.md (Quick Reference)
│   └── README.md (Complete Guide)
├── Developer Level
│   ├── BUILD.md (How to Build)
│   ├── ARCHITECTURE.md (How it Works)
│   └── INTEGRATION.md (How to Integrate)
└── Meta
    └── INDEX.md (This File)
```

---

## Reading Recommendations

### For New Users (15 minutes)
1. [QUICKSTART.md](QUICKSTART.md) - Command reference

### For Regular Users (30 minutes)
1. [README.md](README.md) - Full feature guide
2. [QUICKSTART.md](QUICKSTART.md) - For reference

### For Developers (1-2 hours)
1. [BUILD.md](BUILD.md) - Get it building
2. [ARCHITECTURE.md](ARCHITECTURE.md) - Understand design
3. [Code](src/) - Read implementation

### For Integrators (30 minutes)
1. [INTEGRATION.md](INTEGRATION.md) - Integration guide
2. [Makefile](Makefile) - Build automation

---

## Glossary

| Term | Meaning |
|------|---------|
| **CLI** | Command Line Interface |
| **RAII** | Resource Acquisition Is Initialization |
| **FD** | File Descriptor |
| **ENOENT** | Error: No Entity (file not found) |
| **POSIX** | Portable Operating System Interface |
| **UEFI** | Unified Extensible Firmware Interface |
| **cwd** | Current Working Directory |

---

## File Maps

### [README.md](README.md) Maps
- Features → Commands
- Usage → Examples
- Building → Instructions
- Integration → Notes
- Enhancements → Future ideas

### [QUICKSTART.md](QUICKSTART.md) Maps
- Installation → Steps
- Usage → Patterns
- Commands → Reference
- Errors → Solutions
- Tips → Tricks

### [BUILD.md](BUILD.md) Maps
- Prerequisites → Checks
- Build → Steps
- Troubleshooting → Solutions
- Variants → Options
- Testing → Verification

### [ARCHITECTURE.md](ARCHITECTURE.md) Maps
- Overview → Diagrams
- Modules → Details
- Commands → Algorithms
- Memory → Analysis
- Performance → Metrics

### [INTEGRATION.md](INTEGRATION.md) Maps
- Structure → Explanation
- Building → Instructions
- Boot → Options
- API → Usage
- Testing → Checklist

---

## FAQ

**Q: Should I read all docs?**  
A: Start with QUICKSTART, read others as needed.

**Q: Which doc is most important?**  
A: README for features, ARCHITECTURE for code.

**Q: Where's the code?**  
A: In `src/` directory. ARCHITECTURE.md explains it.

**Q: How do I build?**  
A: Run `make build` or see BUILD.md.

**Q: How do I use it?**  
A: See QUICKSTART.md or README.md.

**Q: How do I extend it?**  
A: See ARCHITECTURE.md and INTEGRATION.md.

---

## Last Updated

- **README.md**: 2026-02-20
- **QUICKSTART.md**: 2026-02-20  
- **BUILD.md**: 2026-02-20
- **ARCHITECTURE.md**: 2026-02-20
- **INTEGRATION.md**: 2026-02-20
- **This Index**: 2026-02-20

**All documentation current and consistent.**

---

> 💡 **Pro Tip**: Keep this INDEX open while reading other docs for quick navigation!
