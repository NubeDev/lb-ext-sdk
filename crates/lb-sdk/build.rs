//! Export this crate's vendored WIT directories to dependents.
//!
//! `bindgen!`/`generate!` resolve their `path:` against the *consuming* crate's `CARGO_MANIFEST_DIR`,
//! so a downstream host (lb's `lb-runtime`) or guest cannot point at *this* crate's `wit/` by a
//! relative path once the SDK is a git/registry dependency instead of a sibling dir. The cargo-native
//! fix is a `links` build script: it emits the absolute WIT paths as metadata, and cargo hands them to
//! every direct dependent as `DEP_LB_SDK_WIT` / `DEP_LB_SDK_WIT_COMPAT` (the `links` key upper-cased).
//! A dependent's own `build.rs` reads those and materializes the WIT into its `OUT_DIR` for `bindgen!`
//! `inline:`/`include!` — so there is ONE authoritative WIT (here), reachable from anywhere, no copy.

use std::path::Path;

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let wit = Path::new(&manifest).join("wit");
    let compat = Path::new(&manifest).join("wit-compat-0_1");

    // `cargo:<key>=<val>` on a `links` crate becomes `DEP_LB_SDK_<KEY>` for direct dependents.
    println!("cargo:wit={}", wit.display());
    println!("cargo:wit_compat={}", compat.display());

    // Rebuild dependents if the contract text changes.
    println!("cargo:rerun-if-changed={}", wit.join("world.wit").display());
    println!(
        "cargo:rerun-if-changed={}",
        compat.join("world.wit").display()
    );
}
