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
