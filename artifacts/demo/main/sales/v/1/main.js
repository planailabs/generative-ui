// `refresh` handler: simulate pulling fresh numbers from a source system. Folds the result
// into the surface's KV view; the figures below are bound in the UI via data-kv and update
// in place (no surface republish, no agent in the loop).
// QuickJS/Luau have no Intl; format thousands separators by hand.
function money(x) {
  return "$" + String(x).replace(/\B(?=(\d{3})+(?!\d))/g, ",");
}
function handle(event, host) {
  const n = (host.kv.get("refreshes") || 0) + 1;
  const revenue = 120000 + n * 1532; // a figure that visibly moves each refresh
  host.kv.set("refreshes", n);
  host.kv.set("revenue", money(revenue));
  host.kv.set("change_pct", (8 + ((n * 7) % 9)) + "%");
  host.kv.set("updated", event.received_at);
  return { revenue, refreshes: n };
}
