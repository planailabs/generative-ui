// Pure helpers for the suite — no DOM access, so they import cleanly under `node --test`.

/** Build the JSON body the browser POSTs to the backflow endpoint. */
export function backflowPayload(action, component, value, version, timestamp) {
  return {
    action,
    component: component ?? null,
    value: value ?? null,
    version: version ?? null,
    timestamp,
  };
}

/** Canonical live URL for a sibling surface in the same deck. */
export function surfaceUrl(agent, deck, surface) {
  return `/${agent}/${deck}/${surface}/`;
}

/** The backflow endpoint for a surface id of the form `agent/deck/surface`. */
export function backflowUrl(surfaceId) {
  return `/backflow/${surfaceId}`;
}

/** Versions API URL for a surface id of the form `agent/deck/surface`. */
export function versionsUrl(surfaceId) {
  return `/api/${surfaceId}/versions.json`;
}

/** KV view API URL for a surface id of the form `agent/deck/surface`. */
export function kvUrl(surfaceId) {
  return `/api/${surfaceId}/kv.json`;
}

/** Resolve a (optionally dotted) key against the KV object; `undefined` if absent. */
export function kvValue(kv, key) {
  if (!kv || !key) return undefined;
  return String(key).split(".").reduce((o, k) => (o == null ? undefined : o[k]), kv);
}

/** Render a KV value for display in a bound element: objects as JSON, nullish as "". */
export function kvDisplay(value) {
  if (value == null) return "";
  return typeof value === "object" ? JSON.stringify(value) : String(value);
}

/** Should a surface reload? True only when a different live version has been published. */
export function needsReload(currentVersion, liveVersion) {
  return Boolean(liveVersion) && Boolean(currentVersion) && liveVersion !== currentVersion;
}

/** Previous/next surface slugs for deck navigation. */
export function navTargets(order, current) {
  const list = Array.isArray(order) ? order : [];
  const i = list.indexOf(current);
  return {
    prev: i > 0 ? list[i - 1] : null,
    next: i >= 0 && i < list.length - 1 ? list[i + 1] : null,
  };
}

// --- charts (pure math; <ui-chart> draws the SVG) ---

/** Round, human-friendly axis ticks covering [min, max]: ~`count` steps of 1/2/5×10ⁿ. */
export function niceTicks(min, max, count = 5) {
  if (!Number.isFinite(min) || !Number.isFinite(max)) return [0, 1];
  if (min === max) {
    const pad = Math.abs(min) || 1;
    min -= pad;
    max += pad;
  }
  const raw = (max - min) / Math.max(1, count);
  const mag = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 5, 10].map((m) => m * mag).find((s) => s >= raw) ?? 10 * mag;
  const lo = Math.floor(min / step) * step;
  const hi = Math.ceil(max / step) * step;
  const out = [];
  // Rounded to the step's precision, so 0.1+0.2 shows as 0.3.
  const digits = Math.max(0, -Math.floor(Math.log10(step)));
  for (let v = lo; v <= hi + step / 2; v += step) out.push(Number(v.toFixed(digits)));
  return out;
}

/** Map `v` from the domain [d0, d1] onto the range [r0, r1]. */
export function scale(v, d0, d1, r0, r1) {
  return d1 === d0 ? (r0 + r1) / 2 : r0 + ((v - d0) / (d1 - d0)) * (r1 - r0);
}

/** Normalize chart data: `{labels, series: [{name, values}]}`, or a bare `[{label, value}]`
 *  list, or `{labels, values}` (one series). Non-numbers become null (gaps). */
export function chartData(input) {
  let d = input;
  if (Array.isArray(d)) d = { labels: d.map((p) => p.label ?? p.x ?? ""), series: [{ name: "", values: d.map((p) => p.value ?? p.y) }] };
  else if (d && Array.isArray(d.values) && !d.series) d = { labels: d.labels, series: [{ name: d.name ?? "", values: d.values }] };
  const series = (d?.series ?? []).map((s) => ({
    name: String(s.name ?? ""),
    values: (s.values ?? []).map((v) => (typeof v === "number" && Number.isFinite(v) ? v : v == null || v === "" ? null : Number.isFinite(Number(v)) ? Number(v) : null)),
  }));
  const n = Math.max(0, ...series.map((s) => s.values.length));
  const labels = Array.from({ length: n }, (_, i) => String(d?.labels?.[i] ?? i + 1));
  return { labels, series };
}

/** Min/max over every series' values (nulls skipped); `stacked` sums per label. */
export function chartExtent(data, stacked = false) {
  const vals = stacked
    ? data.labels.map((_, i) => data.series.reduce((a, s) => a + (s.values[i] ?? 0), 0))
    : data.series.flatMap((s) => s.values).filter((v) => v != null);
  if (!vals.length) return [0, 1];
  return [Math.min(0, ...vals), Math.max(0, ...vals)];
}

/** Compact number for axes and stats: 1234 → "1.2k", 0.25 → "0.25". */
export function formatNumber(v) {
  if (v == null || !Number.isFinite(v)) return "";
  const a = Math.abs(v);
  for (const [n, s] of [[1e9, "B"], [1e6, "M"], [1e3, "k"]]) {
    if (a >= n) return `${Number((v / n).toFixed(a >= 10 * n ? 0 : 1))}${s}`;
  }
  return String(Number(v.toFixed(a >= 10 ? 0 : a >= 1 ? 1 : 2)));
}

// --- the host bridge (MCP Apps: JSON-RPC 2.0 over postMessage) ---

/** A JSON-RPC 2.0 request/notification. */
export function rpc(method, params, id) {
  const m = { jsonrpc: "2.0", method };
  if (params !== undefined) m.params = params;
  if (id !== undefined) m.id = id;
  return m;
}

/** The theme a host context asks for: "light" | "dark" | null (keep the current one). */
export function hostTheme(hostContext) {
  const t = hostContext?.theme;
  return t === "light" || t === "dark" ? t : null;
}

/** Only CSS custom properties from a host's `styles.variables` (nothing else gets applied). */
export function hostVariables(hostContext) {
  const vars = hostContext?.styles?.variables ?? {};
  return Object.entries(vars).filter(([k, v]) => /^--[a-z0-9-]+$/i.test(k) && typeof v === "string" && !/[;{}<]/.test(v));
}
