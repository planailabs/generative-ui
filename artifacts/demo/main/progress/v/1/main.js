// Each click advances the job a quarter and emits it as a public event, which the server
// also appends to this surface's stream: <span data-kv="stream.progress.pct"> updates live.
function handle(event, host) {
  const step = Math.min((host.kv.get("step") || 0) + 1, 4);
  host.kv.set("step", step);
  const labels = ["waiting", "fetching", "parsing", "indexing", "done"];
  host.emit("progress", { pct: step * 25, label: labels[step] });
  return { step };
}
