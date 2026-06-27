// Sandboxed JS handler for the `bump` backflow action. Folds a counter into the surface's
// KV view and returns it as the response body. Never edits the surface.
function handle(event, host) {
  host.kv.set("hits", (host.kv.get("hits") || 0) + 1);
  return { hits: host.kv.get("hits") };
}
