---
name: browser-verify
description: Verify that a published surface actually renders correctly in a real browser — components upgraded, plain HTML rewritten, no duplicated controls — using headless chromium from the dev shell. Use after changing the suite runtime, the HTML injection, or anything affecting how surfaces render.
---

# Browser-verifying surfaces

The suite runtime (`suite/runtime.js`) does real work in the browser: it upgrades `<ui-*>`
custom elements, live-rewrites plain HTML into components, wires backflow, and reloads on new
versions. Rust tests can't see any of that — they only check the served HTML. Use headless
**chromium** (in the Nix dev shell) to render a surface and assert the post-JS DOM.

## Prerequisites

`chromium` ships in the dev shell (`flake.nix`). Run everything inside `nix develop`.
`gui-server` embeds the suite at compile time, so **rebuild it after editing `suite/`** before
verifying.

## Recipe

```sh
# 1. Build + serve (the server embeds the current suite/).
cargo build -p gui-server
gui-server --artifacts ./artifacts --addr 127.0.0.1:8062 &

# 2. Dump the rendered DOM after JS runs (--virtual-time-budget lets the module script execute).
chromium --headless --no-sandbox --dump-dom --virtual-time-budget=3000 \
    http://127.0.0.1:8062/demo/sales/ > /tmp/dom.html

# 3. Assert. Examples:
grep -c '<ui-button' /tmp/dom.html      # components upgraded
grep -c '<button'    /tmp/dom.html      # exactly one inner <button> per ui-button (no nesting)
```

## What to check

- **Components upgraded:** `<ui-*>` elements are present and contain their rendered controls
  (e.g. each `<ui-button>` wraps exactly one `<button class="ui-btn">`).
- **No duplicated controls:** a control rendered twice means the rewrite re-processed a
  component's own internal element — the rewrite must run *before* `customElements.define`.
- **Plain HTML rewritten:** serve a surface authored with `<h1>`/`<button>`/`<form>`/`<a data-to>`
  and confirm they become `<ui-heading>`/`<ui-button>`/`<ui-form>`/`<ui-link>` in the dumped DOM.
- **Backflow wiring:** optionally drive a click with `--remote-debugging-port` + a CDP script,
  or POST to `/backflow/...` and assert an event file appears (cheaper; see the gui-server tests).

## Notes

- `--no-sandbox` is required in most CI/container environments.
- `--dump-dom` serializes `document` after load; pair it with `--virtual-time-budget` so the
  deferred ES module and custom-element upgrades have run.
- For interaction (clicks, form submits), prefer the Playwright MCP when a display is available;
  fall back to `--dump-dom` for static render checks.
