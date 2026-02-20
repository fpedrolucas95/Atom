# Fileman - Quick Start Guide

## Installation

### From Workspace Root

```bash
# Build the complete system with fileman included
./build.sh

# Or build just fileman
cd userspace/apps/fileman
cargo build --release --target=x86_64-unknown-uefi

# Install to EFI (if building standalone)
make install
```

## Running Fileman

### Method 1: From Terminal

```bash
# In Atom OS terminal/shell
$ fileman

# Fileman will enter interactive mode
fileman:/> help
```

### Method 2: Direct Execution

```bash
# Launch with command and exit
$ fileman --help
$ fileman ls /home
```

## Common Usage Patterns

### 1. List Files

```bash
fileman:/> ls
etc/
home/
tmp/
var/

fileman:/> ls /home
user/
root/

# List with sizes
fileman:/home> ls -la
# (future version with -la support)
```

### 2. Create and Populate Directory

```bash
fileman:/> mkdir /home/project
mkdir: created directory '/home/project'

fileman:/> cd /home/project
fileman:/home/project> pwd
/home/project
```

### 3. Work with Files

```bash
# Create a file using redirects (via shell)
fileman:/> cat > notes.txt
Type '.' on new line to end
...content...
.

# View file
fileman:/> cat notes.txt
...content...

# Copy file
fileman:/> cp notes.txt backup.txt
cp: copied 'notes.txt' to 'backup.txt'

# Rename file
fileman:/> mv backup.txt archive.txt
mv: moved 'backup.txt' to 'archive.txt'

# Remove file
fileman:/> rm archive.txt
rm: removed 'archive.txt'
```

### 4. Directory Navigation

```bash
# Change directory
fileman:/> cd /etc
fileman:/etc> pwd
/etc

# Go back to root
fileman:/etc> cd /
fileman:/> 

# Go to home (relative path)
fileman:/> cd home
fileman:/home>

# Go up one level
fileman:/home> cd ..
fileman:/>
```

### 5. Cleanup Operations

```bash
# Remove empty directory
fileman:/> rmdir /home/empty_folder
rmdir: removed '/home/empty_folder'

# Remove directory with contents
fileman:/> rm -r /home/old_project
rm: removed '/home/old_project' and its contents

# Remove multiple files
fileman:/> rm file1.txt file2.txt
# (future version supporting multiple args)
```

## Command Reference

### Basic Navigation

```bash
pwd                    # Print current directory
cd [path]              # Change directory
ls [path]              # List contents
```

### File Operations

```bash
cat <file>             # View file contents
cp <src> <dst>         # Copy file
mv <src> <dst>         # Move/rename file  
rm <file>              # Delete file
```

### Directory Operations

```bash
mkdir <path>           # Create directory
rmdir <path>           # Remove empty directory
rm -r <path>           # Remove directory tree
```

### Help

```bash
help                   # Show command list
exit                   # Exit application
```

## Path Examples

### Absolute Paths

```bash
fileman:/> cd /home/user/documents
fileman:/home/user/documents> pwd
/home/user/documents

fileman:/home/user/documents> ls /etc
```

### Relative Paths

```bash
fileman:/home/user> cd documents
fileman:/home/user/documents> 

fileman:/home/user/documents> cd ..
fileman:/home/user>

fileman:/home/user> cd ../../
fileman:/>
```

### Special Cases

```bash
fileman:/home> cd .
fileman:/home>       # Stay in same directory

fileman:/home> cd ..
fileman:/>           # Go to parent

fileman:/> cd /
fileman:/>           # Go to root (already there)

fileman:/tmp> cd /
fileman:/>           # Absolute path works from anywhere
```

## Error Handling

### Common Errors

**File Not Found**
```bash
fileman:/> cat /nonexistent/file.txt
error: fileman: open: File or directory not found
```

**Is a Directory**
```bash
fileman:/> rm /home
error: fileman: rm: Is a directory
# Use: rm -r /home
```

**Directory Not Empty**
```bash
fileman:/> rmdir /home
error: fileman: rmdir: Directory not empty
# Use: rm -r /home
```

**Permission Denied**
```bash
fileman:/> mkdir /root/admin
error: fileman: mkdir: Permission denied
# Your user may not have write access
```

**Path Too Long**
```bash
fileman:/> mkdir /very/long/path/that/exceeds/the/limit...
error: fileman: mkdir: Path exceeds maximum length
# Maximum path: 4096 characters
```

## Tips & Tricks

### 1. Copy Entire Directories (future)

```bash
fileman:/> cp -r /home/project /backup/project
# Not yet supported, use rm -r + mkdir + cp until then
```

### 2. Search Files (future)

```bash
fileman:/> find /home -name "*.txt"
# Not yet implemented, use ls recursively
```

### 3. View Directory Structure (future)

```bash
fileman:/> tree /home
# Not yet implemented, use repeated ls commands
```

### 4. Create Multiple Directories

```bash
fileman:/> mkdir /tmp/a
fileman:/> mkdir /tmp/a/b
fileman:/> mkdir /tmp/a/b/c
# Nested creation not atomic; create parent first
```

### 5. Backup Before Deleting

```bash
fileman:/> cp important.txt important.bak
fileman:/> rm important.txt
# Always backup important files
```

## Performance Tips

### File Copy

```bash
# For large files, copy is efficient (64KB buffers)
fileman:/> cp large_file.iso /backup/
# Will stream efficiently without loading entire file
```

### Directory Listing

```bash
# ls performance with many files
fileman:/home> ls
# Built-in sorting, displays in order
# Can handle 100+ files efficiently
```

### Batch Operations

```bash
# Copy multiple files (currently one at a time)
fileman:/> cp file1.txt backup/
fileman:/> cp file2.txt backup/
fileman:/> cp file3.txt backup/
# (Future: support multiple args)
```

## Integration Examples

### From Shell

```bash
# Launch fileman from terminal
$ fileman

# Run fileman command (future)
$ fileman ls /etc
$ fileman cat /etc/hostname
```

### In Scripts

```bash
#!/bin/bash
# Run fileman commands in sequence
fileman << 'EOF'
cd /home
mkdir projects
cd projects
EOF
```

### With Other Tools (future)

```bash
# Pipe support (future)
$ cat /home/file.txt | less

# glob expansion (future)  
$ fileman ls /home/*.txt

# find integration (future)
$ fileman find /home -type f -name "*.txt"
```

## Troubleshooting

### Fileman Crashes

```bash
# Check system logs
fileman:/> cat /var/log/system.log

# Try with smaller operations
fileman:/> ls /    # Simple list

# Check disk space
fileman:/> ls /full_disk
# May fail if disk is full
```

### Slow File Operations

```bash
# Check if filesystem is busy
fileman:/> cat /dev/null   # Test I/O

# Large file copy with progress (future)
fileman:/> cp large.bin /storage/
# Currently: no progress indicator
```

### Path Not Found

```bash
fileman:/> cd /nonexistent
error: fileman: cd: Not a directory

# First check if path exists
fileman:/> ls /
# Navigate to existing parent
fileman:/> cd /home
```

## Next Steps

### Learn More

- See `README.md` for full documentation
- See `BUILD.md` for build instructions
- See `ARCHITECTURE.md` for technical details

### Advanced Usage (future versions)

- Configuration files
- Custom themes
- Automation scripting
- Remote filesystem support
- Plugin architecture

### Contribute

Fileman is part of Atom OS. To contribute:
1. Fork the repository
2. Create feature branch
3. Add tests for new code
4. Submit pull request

---

**Quick Reference Card**

```
NAVIGATION        FILE OPS          DIRECTORY OPS
───────────────   ─────────────     ──────────────
pwd               cat <file>        mkdir <path>
cd <path>         cp <s> <d>        rmdir <path>
ls [path]         mv <s> <d>        rm -r <path>
                  rm <file>

GLOBAL
──────
help              show this help
exit              quit fileman
```

---

**Version**: 0.1.0  
**Compatible With**: Atom OS 0.1.0+  
**Last Updated**: 2026-02-20
