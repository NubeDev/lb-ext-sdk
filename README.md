# lb-ext-sdk

The **authoritative Rust SDK for Lazybones extensions**. This repo owns the extension
contract; the Lazybones platform (`lb`) and every extension consume it. Nothing here
depends on `lb` — that is the whole point: a downstream team can build extensions against
these crates with **no access to the private `lb` repo**.

## Crates

| Crate | Tier | What it is |
|---|---|---|
| [`lb-sdk`](crates/lb-sdk) | WASM | The WIT world (`lazybones:ext`), `WORLD_MAJOR`, and the `world_major_matches` version gate every WASM guest targets. |
| [`lb-ext-native`](crates/lb-ext-native) | native | The child side of the host↔sidecar wire: the `init` handshake (with `PROTOCOL_MAJOR`), the request/response shapes, and the host-callback client. A native extension imports only this — never the host's supervisor. |
| [`lb-ext`](crates/lb-ext) | tool | The developer CLI: `new` / `build` / `pack` / `publish` — the out-of-tree replacement for `make publish-ext`. |

## The version gates

Both tiers refuse an ABI mismatch **loudly, at load**:

- **WASM:** the host refuses a component whose world major ≠ `lb_sdk::WORLD_MAJOR`.
- **native:** the host refuses a child whose `init.protocol_major` ≠ the host's
  `lb_ext_native::PROTOCOL_MAJOR`.

Bumping either major is a deliberate, rare, breaking act.

## Build

```sh
cargo build --workspace
cargo test  --workspace
```

The `lb-sdk` crate needs the `wasm32-wasip2` target only when a guest builds against it;
the crates themselves are host-target and build anywhere. Linker wiring
(`.cargo/config.toml`) is machine-specific and git-ignored — provide your own.

## Consuming it

Until the first `sdk-v*` release lands on crates.io (published under `NubeDev`), pin a git
tag:

```toml
# a native extension
lb-ext-native = { git = "https://github.com/NubeDev/lb-ext-sdk", tag = "sdk-v0.2.0" }
# a wasm extension
lb-sdk        = { git = "https://github.com/NubeDev/lb-ext-sdk", tag = "sdk-v0.2.0" }
```

## Relationship to `lb`

`lb` consumes these crates in `ext-loader`/`runtime`/the extension tiers. The WIT world
**moved out of** `lb` into this repo — there is one source of truth, not a mirror. The
owning design doc lives in `lb`: `docs/scope/extensions/ext-out-of-tree-scope.md`.

## License

MIT (see [LICENSE](LICENSE)).
