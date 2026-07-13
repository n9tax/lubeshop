//! Exercise any image driver end-to-end.
//!
//!     cargo run -p gwm-core --example fs_demo -- <driver-id> <image> [insert-file]

use std::path::Path;

use gwm_core::imagefs::FsKind;

fn main() {
    let mut args = std::env::args().skip(1);
    let id = args.next().expect("driver id (cpm|fat|cbm)");
    let image = args.next().expect("image path");
    let insert = args.next();
    let image = Path::new(&image);

    let kind = FsKind::from_id(&id).expect("unknown driver id");
    let fs = kind.open(None);

    if let Some(src) = insert {
        let src = Path::new(&src);
        let name = src
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("FILE")
            .to_uppercase();
        match fs.insert(image, src, &name, 0) {
            Ok(()) => println!("inserted {name}"),
            Err(e) => eprintln!("insert failed: {e}"),
        }
    }

    match fs.list(image) {
        Ok(entries) => {
            println!("{} file(s):", entries.len());
            for e in &entries {
                println!("  {:<16} {} bytes", e.name, e.size);
            }
            if let Some(first) = entries.first() {
                let dest = std::env::temp_dir().join(format!("fsdemo-{}", first.name));
                match fs.extract(image, first, &dest) {
                    Ok(()) => println!("extracted {} -> {}", first.name, dest.display()),
                    Err(e) => eprintln!("extract failed: {e}"),
                }
            }
        }
        Err(e) => eprintln!("list failed: {e}"),
    }

    match fs.usage(image) {
        Ok(u) => println!("usage: {} used, {} free, {} total", u.used, u.free, u.total()),
        Err(e) => eprintln!("usage: {e}"),
    }
}
