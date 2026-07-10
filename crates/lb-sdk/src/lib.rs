//! The stable SDK boundary (README §11.2).
//!
//! Holds the WIT contract under `wit/` and pins its version. The host (`lb-runtime`) and the
//! guest extensions both generate bindings from that one WIT, so the contract cannot drift.
//!
//! ## Guest usage (WASM tier)
//!
//! A wasm extension generates its bindings from **this crate's** vendored WIT rather than a copied
//! `.wit` path — the copy is exactly what drifts. The crate ships `wit/` in its published `files`, so
//! a guest points `wit_bindgen::generate!` at the dependency's own WIT directory and implements the
//! generated `Guest` trait:
//!
//! ```ignore
//! wit_bindgen::generate!({
//!     // Resolved to this crate's checked-out/vendored `wit/` — one source of truth, no copy.
//!     path: concat!(env!("DEP_LB_SDK_WIT"), "/wit"),  // see build-script note below
//!     world: "extension",
//! });
//!
//! struct MyExt;
//! impl exports::lazybones::ext::tool::Guest for MyExt { /* fn call(..) */ }
//! export!(MyExt);
//! ```
//!
//! The bare-name world for that `generate!` is [`WORLD_NAME`]; the versioned world string the
//! manifest declares is [`WORLD`], gated by [`world_major_matches`]. (A zero-boilerplate
//! `lb_sdk::export!` macro that hides `generate!` entirely is deferred: re-exporting wit-bindgen's
//! generated `export!` across a published-crate boundary is version-fragile, and faking it would hide
//! that — see `docs/scope/extensions/ext-out-of-tree-scope.md`, guest-helper open item.)

/// The WIT world every extension targets. The loader refuses a component whose world major
/// does not match this (crate-layout scope: the SDK/WIT boundary decision).
///
/// `@0.2.0`: a minor bump that ADDED the `host.call-tool` import (host-callback scope). Major is
/// unchanged (0), so existing `@0.1.0` guests still load — a minor addition is backward safe.
pub const WORLD: &str = "lazybones:ext/extension@0.2.0";

/// The bare `namespace:package/world` name (no version) — what a guest passes as `world:` to
/// `wit_bindgen::generate!`. [`WORLD`] is the versioned form the manifest declares.
pub const WORLD_NAME: &str = "extension";

/// Major version of the world. Bumping this breaks every extension — a deliberate, rare act.
pub const WORLD_MAJOR: u64 = 0;

/// Returns true if a manifest-declared `world` string is compatible with this host's WIT
/// major. Compatibility is major-equality (semver); minor/patch additions are backward safe.
pub fn world_major_matches(declared: &str) -> bool {
    parse_major(declared) == Some(WORLD_MAJOR)
}

/// Extract the major from a `name@MAJOR.MINOR.PATCH` world string. `None` if unparseable.
fn parse_major(world: &str) -> Option<u64> {
    let version = world.rsplit_once('@')?.1;
    version.split('.').next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_same_major() {
        // Both the original @0.1.0 guests AND the @0.2.0 host-callback guests load: a MINOR bump is
        // backward safe (the loader checks major only). This is the ABI-compat guarantee in one line.
        assert!(world_major_matches("lazybones:ext/extension@0.1.0"));
        assert!(world_major_matches("lazybones:ext/extension@0.2.0"));
        assert!(world_major_matches("lazybones:ext/extension@0.9.4"));
    }

    #[test]
    fn rejects_different_major() {
        assert!(!world_major_matches("lazybones:ext/extension@1.0.0"));
    }

    #[test]
    fn rejects_unparseable() {
        assert!(!world_major_matches("nonsense"));
    }
}
