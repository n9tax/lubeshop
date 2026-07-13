//! Non-destructive check of the storage-folder scanner: scans a directory into a
//! throwaway catalog and prints what would be imported.
//!
//!     cargo run -p gwm-core --example scan_demo -- <dir>

use std::path::Path;

use gwm_core::catalog::Catalog;
use gwm_core::library::scan_import;

fn main() {
    let dir = std::env::args().nth(1).expect("need a directory to scan");
    let tmp = std::env::temp_dir().join("gwm-scan-demo.db");
    let _ = std::fs::remove_file(&tmp);
    let catalog = Catalog::open(&tmp).unwrap();

    let count = scan_import(&catalog, Path::new(&dir)).unwrap();
    println!("would import {count} file(s):");
    for item in catalog.list().unwrap() {
        println!("  [{}] {}", item.kind.as_str(), item.path);
    }
    let _ = std::fs::remove_file(&tmp);
}
