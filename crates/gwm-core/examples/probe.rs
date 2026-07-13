//! Manual check of `gw` detection, independent of the TUI.
//!
//!     cargo run -p gwm-core --example probe

fn main() {
    let status = gwm_core::device::probe();
    println!("available : {}", status.available);
    println!("version   : {:?}", status.version);
    println!("detail    : {}", status.detail);
}
