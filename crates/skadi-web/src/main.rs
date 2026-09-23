//! skadi-web entry point — mounts the Leptos CSR [`App`] shell. All testable
//! logic + components live in the library crate (`lib.rs`); this bin is just the
//! mount point so `wasm-bindgen-test` can exercise the lib (SKADI-T-0118).

use leptos::prelude::mount_to_body;
use skadi_web::App;

fn main() {
    console_error_panic_hook::set_once();
    mount_to_body(App);
}
