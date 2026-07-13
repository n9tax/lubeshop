//! Exercise the CP/M image driver against a real image.
//!
//!     cargo run -p gwm-core --example cpm_demo -- <format> <image> [host-file-to-insert]

use std::path::Path;

use gwm_core::imagefs::{CpmFs, ImageFs};

fn main() {
    let mut args = std::env::args().skip(1);
    let format = args.next().expect("need a cpmtools format");
    let image = args.next().expect("need an image path");
    let insert = args.next();
    let image = Path::new(&image);

    let fs = CpmFs::new(format);

    println!("== available cpm formats: {} ==", gwm_core::imagefs::cpm_formats().len());

    if let Some(src) = insert {
        let src = Path::new(&src);
        let name = src.file_name().and_then(|s| s.to_str()).unwrap_or("FILE");
        match fs.insert(image, src, name, 0) {
            Ok(()) => println!("inserted {name}"),
            Err(e) => eprintln!("insert failed: {e}"),
        }
    }

    match fs.list(image) {
        Ok(entries) => {
            println!("== {} file(s) ==", entries.len());
            for e in &entries {
                println!("  {}:{:<14} {:>7} bytes", e.user, e.name, e.size);
            }
            // Extract the first file to prove extract works.
            if let Some(first) = entries.first() {
                let dest = std::env::temp_dir().join(format!("cpm-demo-out-{}", first.name));
                match fs.extract(image, first, &dest) {
                    Ok(()) => println!("extracted {} -> {}", first.name, dest.display()),
                    Err(e) => eprintln!("extract failed: {e}"),
                }
            }
        }
        Err(e) => eprintln!("list failed: {e}"),
    }
}
