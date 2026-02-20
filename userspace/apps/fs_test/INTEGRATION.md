# fs_test Integration Guide

## Objetivo
Validar integridade e crash-consistency (.Journal replay) do filesystem Atom OS.

## Quick Start

### 1. Build
```bash
cd /Users/fpedrolucas95/Documents/GitHub/Atom/userspace/apps/fs_test
cargo build --release
```

Output esperado:
```
   Compiling fs_test v0.1.0
    Finished `release` target(s) in X.XXs
```

Binary gerado: `target/x86_64-unknown-uefi/release/fs_test.efi`

### 2. Integração no Boot
Copiar `fs_test.efi` para:
```
efi/drivers/fs_test.atxf  (após elf2atxf)
```

Carregar após fsd estar pronto no init.rs:
```rust
// Em kernel/src/init_process.rs ou similar
if let Ok(fs_test) = load_and_execute("fs_test") {
    // fs_test roda autonomamente
    // Valida filesystem
}
```

## Expected Output

### Sucesso Completo
```
==========================================================
Atom OS Filesystem Integration Test Suite
==========================================================

[INFO] Waiting for filesystem daemon (fsd) to become ready...
[INFO] Filesystem daemon is ready

[TEST] Running: test_mkdir
[TEST] PASS: test_mkdir

[TEST] Running: test_create_and_write
[TEST] PASS: test_create_and_write

[TEST] Running: test_reopen_and_read
[INFO] Read 11 bytes from file
[TEST] PASS: test_reopen_and_read

[TEST] Running: test_fsync
[INFO] fsync completed successfully - file data guaranteed durable
[TEST] PASS: test_fsync

[TEST] Running: test_reboot_persistence
[INFO] Simulating reboot by re-verifying file accessibility...
[INFO] File persisted correctly: size=11 bytes, inode=12345
[TEST] PASS: test_reboot_persistence

[TEST] Running: test_dir_persistence
[INFO] Directory persisted correctly after reboot simulation
[TEST] PASS: test_dir_persistence

[TEST] Running: test_journal_recovery
[INFO] Checking journal structures and entry count...
[INFO] Journal replay appears successful - file data integrity verified
[TEST] PASS: test_journal_recovery

[TEST] Running: test_cleanup

[TEST] PASS: test_cleanup

==========================================================
Test Summary: 8 passed, 0 failed
==========================================================

[INFO] TEST SUITE PASSED - All critical checks passed
[INFO] Filesystem is operational and crash-consistent
```

### Failure Case: FSD não responde
```
[INFO] Waiting for filesystem daemon (fsd) to become ready...
[ERROR] FSD never became ready: fsd_timeout

==========================================================
Test Summary: 0 passed, 0 failed
==========================================================

[ERROR] Cannot proceed without FSD: fsd_timeout
```

### Failure Case: Perda de dados após "reboot"
```
[TEST] Running: test_reopen_and_read
[TEST] PASS: test_reopen_and_read

[TEST] Running: test_fsync
[TEST] PASS: test_fsync

[TEST] Running: test_reboot_persistence
[INFO] Simulating reboot by re-verifying file accessibility...
[ERROR] FAIL: test_reboot_persistence - File not found after reboot simulation: NotFound
STOPPING: File failed to persist across reboot simulation

==========================================================
Test Summary: 6 passed, 1 failed
==========================================================

[ERROR] TEST SUITE FAILED - Filesystem integrity issues detected
```

## Architecture

### Memory Model
```
Heap: 512 KB (bump allocator)
  ├─ TestContext (strings, counters)
  ├─ FD references (kernel-managed)
  └─ Read buffers (256 bytes stack-allocated)
```

### Error Handling Strategy
```
syscall() 
  → check(raw)
  → Result<T, FsError>
  → test function captures
  → ctx.fail() logs
  → early return if critical
```

### Access Pattern (Write → Read → Verify)
```
1. open(CREATE)
   └─ write("hello world")      [11 bytes]
   └─ close()
        ↓
2. open(RDONLY)
   └─ read(buf[])              [max 256 bytes]
   └─ validate buf[0..10] == "hello world"
   └─ close()
        ↓
3. stat()                       [verificar persistência]
   └─ tamanho = 11
   └─ inode válido
```

## Debugging

### Enable Verbose Logging
Modificar `main.rs`:
```rust
fn log(level: LogLevel, msg: &str) {
    // Adicionar timestamp
    let tick = read_cycle_counter(); // ou similar
    let mut buf = format!("[{}] {} {}", tick, level.prefix(), msg);
    atom_syscall::debug::log(&buf);
}
```

### Inspect File State
Antes de fs_test rodar:
```bash
# Via fileman shell
ls -la /test/
stat /test/file.txt
hexdump -C /test/file.txt
```

### Capture Journal Events
No fsd (se logging for ativado):
```rust
// Em fsd/src/journal.rs ou main.rs
log(&format!("Journal entry: {:?}", entry));
```

## Performance Characteristics

### Timeline
```
[FSD wait]        ≈100-200ms (exponential backoff × ~20 tentativas)
[mkdir]           ≈1-2ms
[write]           ≈1-2ms (syscall + IPC roundtrip)
[read]            ≈1-2ms (syscall + IPC roundtrip)
[fsync]           ≈10-50ms (journal I/O sync)
[stats]           ≈1ms each
───────────────────────────────────
Total            ≈120-280ms expected
```

## Configuration

### Tuning Parameters
Em `src/main.rs`:
```rust
const HEAP_SIZE: usize = 512 * 1024;        // Aumentar se OOM
const MAX_ATTEMPTS: u32 = 100;              // Aumentar timeout FSD wait
const BASE_DELAY_MS: u32 = 10;              // Ajustar backoff

// Read buffer
let mut buffer = [0u8; 256];                // Aumentar se ler >256 bytes
```

### Test Data
```rust
let test_data = b"hello world";             // Fixo 11 bytes
let test_dir = "/test";                     // Hardcoded
let test_file = "/test/file.txt";           // Hardcoded
```

## Validation Checklist

### Before Shipping
- [ ] Compila sem erros (apenas warnings OK)
- [ ] Roda com FSD operacional
- [ ] Todos 8 testes passam
- [ ] Arquivo persiste após "reboot" (stat + read)
- [ ] No leaks de FDs (close chamado sempre)
- [ ] Output é legível e estruturado

### Integration Testing
- [ ] Boot sequence: init → fsd → fs_test OK
- [ ] Múltiplas execuções do fs_test não conflitam
- [ ] Cleanup (/test/file.txt) não impede próximas rodadas
- [ ] Erros de FSD (crashing) detectados no wait_for_fsd timeout

## Known Limitations

1. **Reboot simulation**: usa stat() na mesma execução
   - Real reboot testing requer hardware ou QEMU
   - Detecção de journal replay necessita observar logs do fsd

2. **Concorrência**: fs_test é single-threaded
   - Não testa race conditions
   - Filesystem IPC é serializado naturalmente

3. **Espaço **: heap assume 512K disponível
   - Se mem constrained, reduzir HEAP_SIZE
   - Não há fallback para out-of-memory

## Future Enhancements

### Phase 2: Stress Testing
```rust
// Múltiplos arquivos
for i in 0..100 {
    create_write_verify(format!("/test/file_{}.txt", i))
}
```

### Phase 3: Concurrency
```rust
// Threads paralelos
spawn_thread(|| test_create_and_write());
spawn_thread(|| test_reopen_and_read());
// Sincronizar, validar ordem evento
```

### Phase 4: Recovery Recovery
```rust
// Simular crash durante write
write_partial_and_crash();
journal_recover();
verify_consistent_state();
```

## Support

### Compilation Issues
```bash
# Erro: cannot find type `FsError`
→ chmod +x build.sh && ./build.sh

# Erro: efi_main not found
→ Verifica se #[no_mangle] pub extern "C" fn main()

# Erro: workspace not found
→ Adicione a fs_test ao exclude[] em /Cargo.toml
```

### Runtime Issues
```bash
# fs_test nunca retorna (espera FSD)
→ Verifica se fsd está rodando (ps aux | grep fsd)
→ Aumentar MAX_ATTEMPTS ou timeout

# Arquivo não persiste
→ Verificar se fsync completou sem erro
→ Verifica journal logs do fsd
→ Reboot simulação não é reboot real

# Memory issues
→ Aumentar HEAP_SIZE em main.rs
→ Reduzir buffer[4096] → [1024]
```

## Architecture Diagram

```
┌─────────────────────────────────────┐
│   fs_test (userspace app)           │
├─────────────────────────────────────┤
│  test_mkdir                         │
│  test_create_and_write              │
│  test_reopen_and_read ←─── AUDIT ──→ content validation
│  test_fsync          ────── BARRIER ─ durability
│  test_reboot_persistence ─ CRASH ── persistence
│  test_journal_recovery ── RECOVERY - integrity
└─────────┬───────────────────────────┘
          │
          ├─ atom_syscall::fs::*
          │
          ├─ Kernel IPC layer
          │
          ├─ fsd (filesystem daemon)
          │
          ├─ Journal (WAL)
          │
          └─ FAT32 block device
```

## Legal/Licensing
Integrated into Atom OS project, licensed under same terms.
