//! The stable SDK boundary (README §11.2).
//!
//! Holds the WIT contract under `wit/` and pins its version. The host (`lb-runtime`) and the
//! guest extensions both generate bindings from that one WIT, so the contract cannot drift.

/// The WIT world every extension targets. The loader refuses a component whose world major
/// does not match this (crate-layout scope: the SDK/WIT boundary decision).
///
/// `@0.2.0`: a minor bump that ADDED the `host.call-tool` import (host-callback scope). Major is
/// unchanged (0), so existing `@0.1.0` guests still load — a minor addition is backward safe.
pub const WORLD: &str = "lazybones:ext/extension@0.2.0";

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
