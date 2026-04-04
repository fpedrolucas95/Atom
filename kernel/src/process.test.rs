#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_limit_initialization() {
        // Test that new processes have unlimited memory (0)
        let process = Process {
            id: ProcessId::from_raw(1),
            pml4_phys: 0x1000,
            primary_thread: ThreadId::from_raw(1),
            threads: vec![ThreadId::from_raw(1)],
            capability_table: crate::cap::create_process_capability_table(ProcessId::from_raw(1)),
            cleaned: false,
            resident_private_pages: 0,
            resident_shared_pages: 0,
            reserved_bytes: 0,
            memory_limit_pages: 0,
            terminating: false,
            oom_killed: false,
            terminated: false,
        };
        
        assert_eq!(process.memory_limit_pages, 0, "New process should have unlimited memory");
    }

    #[test]
    fn test_memory_usage_struct() {
        // Test MemoryUsage struct creation
        let usage = MemoryUsage {
            resident_pages: 100,
            resident_private_pages: 70,
            resident_shared_pages: 30,
            limit_pages: 1000,
            reserved_bytes: 4096 * 100,
        };
        
        assert_eq!(usage.resident_pages, 100);
        assert_eq!(usage.resident_private_pages, 70);
        assert_eq!(usage.resident_shared_pages, 30);
        assert_eq!(usage.limit_pages, 1000);
        assert_eq!(usage.reserved_bytes, 409600);
    }
}
