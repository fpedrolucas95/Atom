use std::env;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();
    
    if args.len() != 4 {
        eprintln!("Usage: {} <input> <output> <array_name>", args[0]);
        std::process::exit(1);
    }
    
    let input_path = &args[1];
    let output_path = &args[2];
    let array_name = &args[3];
    
    // Read input binary file
    let data = fs::read(input_path)?;
    
    // Create output Rust source file
    let mut output = File::create(output_path)?;
    
    // Write header comments
    writeln!(output, "// Auto-generated: Embedded {}", Path::new(input_path).file_name().unwrap().to_string_lossy())?;
    writeln!(output, "// Size: {} bytes\n", data.len())?;
    
    // Write array definition
    writeln!(output, "pub const {}: &[u8] = &[", array_name)?;
    
    // Write data in chunks of 16 bytes per line
    for chunk in data.chunks(16) {
        write!(output, "    ")?;
        for (i, byte) in chunk.iter().enumerate() {
            if i > 0 {
                write!(output, ", ")?;
            }
            write!(output, "0x{:02x}", byte)?;
        }
        writeln!(output, ",")?;
    }
    
    writeln!(output, "];")?;
    
    println!("Embedded {} bytes from {} to {}", data.len(), input_path, output_path);
    
    Ok(())
}
