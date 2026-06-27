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

## Verify the loop end to end

```sh
nix develop                                   # pinned Rust + node dev shell
cargo test                                    # all crates
node --test suite/*.test.js                   # JS runtime helpers

# Serve the committed demo deck and open it:
cargo run -p gui-server -- --artifacts ./artifacts
#   http://localhost:8080/            inspector (live)
#   http://localhost:8080/demo/sales/ real interactive HTML; nav to region-detail/settings

# Drive the loop from an agent (in another shell, same artifacts dir):
cargo run -p example-agent -- --artifacts ./artifacts wait sales   # blocks
# …click "Refresh" in the browser → an event file lands in
#   artifacts/demo/main/sales/events/ and the agent prints it.

# Or without a browser, via the CLI:
cargo run -p gui-cli -- --artifacts ./artifacts backflow emit demo sales --action refresh
cargo run -p gui-cli -- --artifacts ./artifacts index
```

The MCP server (`cargo run -p gui-mcp -- --artifacts ./artifacts`) exposes the same
authoring/awaiting flow to any MCP-speaking agent (`publish_surface`, `define_deck`,
`wait_for_backflow`, …).

## Direct access (server + agent in one binary)

The `direct-access` example runs the HTTP server and a [swiftide](https://swiftide.rs)
LLM agent **in one process** (each is a separate library, just invoked together). The agent
conducts a whole conversation through interactive UI — publish a prompt surface → wait for
the user's submission → ask the LLM → republish with the answer (the view re-renders live).

```sh
cargo run -p direct-access -- --artifacts ./run --mock        # deterministic, offline
# open the printed URL (http://127.0.0.1:8080/assistant/chat/) and chat

# Real / OpenAI-compatible backend (opt-in feature; custom base URL supported):
OPENAI_API_KEY=… cargo run -p direct-access --features openai -- \
    --model gpt-4o-mini --base-url https://your-endpoint/v1
```

The default build uses a built-in mock LLM (no network) and is what the tests exercise. The
real backend is gated behind `--features openai` (uses swiftide from git, pinned to
`async-openai 0.33`; needs a recent cargo for git swiftide's 2024 edition). Pass a `--model`
your endpoint actually hosts (e.g. query `GET {base-url}/models`). The session loop is generic
over any swiftide `ChatCompletion`, so the backend is pluggable. Verified end to end against a
custom base URL — the agent calls the endpoint and republishes the answer onto the surface.

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
