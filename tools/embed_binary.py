#!/usr/bin/env python3
import sys

def embed_binary(input_file, output_file, array_name):
    with open(input_file, 'rb') as f:
        data = f.read()
    
    with open(output_file, 'w') as f:
        f.write(f"// Auto-generated: Embedded {input_file}\n")
        f.write(f"// Size: {len(data)} bytes\n\n")
        f.write(f"pub const {array_name}: &[u8] = &[\n")
        
        for i in range(0, len(data), 16):
            chunk = data[i:i+16]
            hex_str = ', '.join(f'0x{b:02x}' for b in chunk)
            f.write(f"    {hex_str},\n")
        
        f.write("];\n")
    
    print(f"Embedded {len(data)} bytes from {input_file} to {output_file}")

if __name__ == '__main__':
    if len(sys.argv) != 4:
        print(f"Usage: {sys.argv[0]} <input> <output> <array_name>")
        sys.exit(1)
    
    embed_binary(sys.argv[1], sys.argv[2], sys.argv[3])
