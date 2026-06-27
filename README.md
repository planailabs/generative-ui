# Generative UI Suite

Agents publish **interactive HTML surfaces** — real Web Components, not screenshots — by
**writing files into a directory**. A Rust server **discovers** them (no submission API),
serves the **live version** of each surface, and writes user interactions (**backflow**)
back into the same directory, where the producing agent reads them. The filesystem is the
bus; discovery and backflow are symmetric.

It re-works the core idea of [`ui.plan.ai`](https://ui.plan.ai) — *make agent-generated UI
work inspectable* — but swaps pictures+click-zones for real HTML, the submission API for a
directory schema, and one-way events for a directory-based backflow loop.

## Concepts

- **agent** → **deck** → **surface** → **version**. A *deck* is a navigable set of
  interconnected surfaces (`main` by default). A *surface* is a regenerable "dashboard"
  whose newest version is live; history is retained.
- **backflow**: every interaction is written to the surface's `events/` directory.

```
artifacts/{agent}/{deck}/{surface}/v/{version}/{frame.json,index.html}
artifacts/{agent}/{deck}/{surface}/events/{unix-ms}-{action}.json
artifacts/{agent}/{deck}/deck.json     # optional: entry surface + nav order
```

## Quick start

Everything runs through the Nix flake (pinned Rust + node, cross-platform):

```sh
nix develop                                   # dev shell
cargo run -p gui-server -- --artifacts ./artifacts
# open http://localhost:8080/ for the inspector, or a surface directly:
# http://localhost:8080/demo/sales/
```

Build a single binary without the shell:

```sh
nix build .#gui-server && ./result/bin/gui-server --artifacts ./artifacts
```

## Crates

| Crate | Role |
|---|---|
| `gui-schema` | The on-disk contract: manifests, decks, events, component tree → `<ui-*>` HTML. |
| `gui-agent` | Agent API: publish surfaces, define decks, read/wait on backflow. |
| `gui-server` | Discover + serve surfaces, route backflow, SSE, inspector. |
| `gui-mcp` | `rmcp` stdio MCP server — `publish_surface`, `define_deck`, `wait_for_backflow`, … |
| `gui-cli` | `gui` debug CLI — `index`, `validate`, `deck`, `surface`, `publish`, `backflow`, `watch`. |

See [`AGENTS.md`](./AGENTS.md) for the agent-facing reference and hard rules.

## License

MIT.
