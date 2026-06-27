# handler-wasm — example WASM handler guest

A core-module WASM handler for the Generative UI handler ABI, used as the committed
`.wasm` for both the `gui-runtime` engine tests and the example deck. Committing the built
artifact means **no wasm toolchain is needed to test or serve** — only to rebuild this.

It mirrors the JS/Lua example handlers and exercises the full `host` API, routing on the
backflow `event.action` (`bump`, `go`, `respond`). See `src/lib.rs` and the ABI docs in
`gui-runtime`'s `wasm` module.

## Rebuild

This crate is excluded from the host workspace (it only builds for wasm). Build it and copy
the artifact to its consumers:

```sh
cargo build --manifest-path examples/handler-wasm/Cargo.toml \
    --target wasm32-unknown-unknown --release
cp examples/handler-wasm/target/wasm32-unknown-unknown/release/handler_wasm.wasm \
   crates/gui-runtime/tests/fixtures/handler.wasm
# and into the example deck's surface version dir (see artifacts/.../counter-wasm/v/1/main.wasm)
```

The `wasm32-unknown-unknown` target is provided by the pinned toolchain
(`rust-toolchain.toml`); run inside `nix develop`.
