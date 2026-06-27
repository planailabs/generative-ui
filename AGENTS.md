# Agent guide

Agent-facing entry point for the **Generative UI Suite**. Facts, tables, commands.

## What this is

Agents publish **interactive HTML surfaces** (real `<ui-*>` Web Components, not pictures)
by **writing files into a directory** that follows a fixed schema. A Rust server
**discovers** them (no submission API), serves the **live (newest) version** of each
surface, and writes user interactions — **backflow** — back into the same directory as
event files. The filesystem is the bus; discovery and backflow are symmetric.

## Vocabulary

| Term | Meaning |
|---|---|
| **agent** | A producer, identified by a globally unique slug. |
| **deck** | A navigable set of interconnected surfaces under an agent. `main` is the default. |
| **surface** | A stable, regenerable UI key (the "dashboard"). Newest version wins; history kept. |
| **version** | A published snapshot of a surface (`v/{unix-ms}/`). Sorts by id; max = live. |
| **backflow** | A user interaction written back into the surface's `events/` dir. |

## Directory schema (the contract)

```
artifacts/{agent}/{deck}/{surface}/v/{version}/{frame.json,index.html,assets/}
artifacts/{agent}/{deck}/{surface}/events/{unix-ms}-{action}.json   # server-written
artifacts/{agent}/{deck}/deck.json                                  # optional: entry, order
artifacts/{agent}/agent.json                                        # optional: name, description
```

The **path is the identity** — agent/deck/surface/version are not duplicated in manifests.

## Layout

| Path | What |
|---|---|
| `crates/gui-schema` | Shared types: manifest, deck, event, component tree → `<ui-*>` HTML. |
| `crates/gui-agent` | Agent API: publish surfaces, define decks, read/wait on backflow. |
| `crates/gui-server` | axum server: discover, serve, route backflow, SSE, inspector. |
| `crates/gui-mcp` | `rmcp` stdio MCP server exposing the agent API as tools. |
| `crates/gui-cli` | `gui` debug CLI: index/validate/deck/surface/publish/backflow/watch. |
| `suite/` | Web Components runtime (native ESM, no build) + CSS. |
| `schemas/` | Published JSON Schema for the on-disk contract. |
| `artifacts/` | Example deck of surfaces. |
| `examples/` | Example Rust agent (publish + wait for backflow). |

## Commands (run inside the Nix dev shell)

```bash
nix develop                 # enter the dev shell (pinned Rust + node)
cargo build                 # build the workspace
cargo test                  # run all tests (the gate)
cargo run -p gui-server -- --artifacts ./artifacts
cargo run -p gui-cli -- index ./artifacts
nix flake check             # cargo test + clippy as flake checks
nix build .#gui-server      # build a single binary
```

## Skills

- [`.agents/skills/authoring-ui`](.agents/skills/authoring-ui/SKILL.md) — how an agent uses
  the `gui-mcp` tools to show users interactive UI and react to their input.

## Hard rules

- **No submission API.** Producers create artifacts by writing files in the schema above.
- **Latest-wins, append-only.** Publishing drops a newer `v/{version}/`; never mutate old versions.
- **Atomic writes.** Write to a temp file + `rename` so concurrent readers never see partials.
- **Cross-OS.** Build paths with `Path::join`; no hardcoded separators or `/tmp`.
- **Native ESM only** in the suite. No bundler, no transpile, no CommonJS.
- **Commit closes every stage** once it builds and tests pass (Conventional Commits).
