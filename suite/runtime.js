// Generative UI Suite runtime — native ES module, no build step.
//
// Registers the <ui-*> custom elements, wires interactions back (backflow), and resolves
// <ui-link>/<ui-deck-nav> navigation. The artifact's HTML is real, interactive DOM.
//
// Two transports, picked by `<meta name="gui-transport">`:
// - "http" (default): gui-server — backflow/KV over fetch, live reload over SSE.
// - "postmessage": embedded in a host's sandboxed iframe (the plan.ai chat, any MCP Apps
//   host). The page is an MCP Apps view: JSON-RPC 2.0 to `window.parent` (`ui/initialize`,
//   size-changed, host-context-changed) plus the suite's own `gui/*` methods for backflow and
//   the KV view. The host decides everything (theme, tools, approvals); the page has no network.
//
// Pages may script against `window.gui`: callTool, sendMessage, openLink, updateContext,
// backflow, kv, onKv. Methods the transport lacks reject with an error.

import {
  backflowPayload,
  backflowUrl,
  chartData,
  chartExtent,
  formatNumber,
  hostTheme,
  hostVariables,
  kvDisplay,
  kvUrl,
  kvValue,
  navTargets,
  needsReload,
  niceTicks,
  rpc,
  scale,
  surfaceUrl,
  versionsUrl,
} from "./lib.js";

// --- surface context, injected by the server into the document head ---
const ctx = (() => {
  const meta = (n) => document.querySelector(`meta[name="${n}"]`)?.content || "";
  const surface = meta("surface");
  const version = meta("version");
  const transport = meta("gui-transport") || "http";
  let deck = {};
  const el = document.getElementById("gui-deck");
  if (el) {
    try {
      deck = JSON.parse(el.textContent);
    } catch {
      /* malformed deck graph — navigation degrades, surface still renders */
    }
  }
  const [agent = "", deckSlug = "", surfaceSlug = ""] = surface.split("/");
  return { surface, version, deck, agent, deckSlug, surfaceSlug, transport };
})();

// --- KV bindings ---

const kvListeners = new Set();

/** Show a KV view in every `[data-kv="key"]` element and tell `gui.onKv` listeners. */
function applyKv(kv) {
  if (!kv || typeof kv !== "object") return;
  document.querySelectorAll("[data-kv]").forEach((el) => {
    el.textContent = kvDisplay(kvValue(kv, el.getAttribute("data-kv")));
  });
  kvListeners.forEach((f) => {
    try {
      f(kv);
    } catch (e) {
      console.error("gui.onKv listener", e);
    }
  });
}

// --- transports ---

/** gui-server: backflow and KV over fetch, new versions over SSE. */
function httpTransport() {
  const unsupported = (what) => () => Promise.reject(new Error(`${what} needs a host (this page is served by gui-server)`));
  return {
    async backflow(action, component, value) {
      if (!ctx.surface) return null;
      const body = backflowPayload(action, component, value, ctx.version || null, new Date().toISOString());
      const res = await fetch(backflowUrl(ctx.surface), {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(body),
      });
      let payload = null;
      if (res.headers.get("content-type")?.includes("application/json")) {
        payload = await res.json().catch(() => null);
      }
      refreshKv(); // a sync handler may have updated the KV view
      return payload;
    },
    async kv() {
      if (!ctx.surface) return {};
      const res = await fetch(kvUrl(ctx.surface));
      return res.ok ? res.json() : {};
    },
    callTool: unsupported("callTool"),
    sendMessage: unsupported("sendMessage"),
    updateContext: unsupported("updateContext"),
    openLink: async (url) => {
      window.open(url, "_blank", "noopener");
    },
    start() {
      subscribeReload();
      refreshKv();
    },
  };
}

/** Embedded: an MCP Apps view talking JSON-RPC to the host over postMessage. */
function postMessageTransport() {
  let nextId = 1;
  const pending = new Map();
  const host = window.parent;
  const send = (m) => host.postMessage(m, "*");
  const request = (method, params) =>
    new Promise((resolve, reject) => {
      const id = nextId++;
      pending.set(id, { resolve, reject });
      send(rpc(method, params, id));
    });
  const notify = (method, params) => send(rpc(method, params));

  const applyContext = (hc) => {
    const theme = hostTheme(hc);
    if (theme) document.documentElement.setAttribute("data-theme", theme);
    for (const [k, v] of hostVariables(hc)) document.documentElement.style.setProperty(k, v);
    const fonts = hc?.styles?.css?.fonts;
    if (typeof fonts === "string" && fonts && !document.getElementById("gui-host-fonts")) {
      const s = document.createElement("style");
      s.id = "gui-host-fonts";
      s.textContent = fonts;
      document.head.appendChild(s);
    }
  };

  window.addEventListener("message", (e) => {
    // Only the host speaks to us; anything else (a nested frame, an extension) is ignored.
    if (e.source !== host) return;
    const m = e.data;
    if (!m || m.jsonrpc !== "2.0") return;
    if (m.id != null && !m.method) {
      const p = pending.get(m.id);
      if (!p) return;
      pending.delete(m.id);
      if (m.error) p.reject(Object.assign(new Error(m.error.message || "host error"), { code: m.error.code, data: m.error.data }));
      else p.resolve(m.result);
      return;
    }
    switch (m.method) {
      case "ui/notifications/host-context-changed":
        applyContext(m.params);
        break;
      case "gui/notifications/kv-changed":
        applyKv(m.params?.kv);
        break;
      case "gui/notifications/stream":
        document.dispatchEvent(new CustomEvent("ui-stream", { detail: m.params }));
        applyKv(m.params?.kv);
        break;
      case "ui/notifications/tool-input":
      case "ui/notifications/tool-result":
        document.dispatchEvent(new CustomEvent(m.method.slice("ui/notifications/".length), { detail: m.params }));
        break;
      case "ui/resource-teardown":
        if (m.id != null) send({ jsonrpc: "2.0", id: m.id, result: {} });
        break;
      case "ping":
        if (m.id != null) send({ jsonrpc: "2.0", id: m.id, result: {} });
        break;
    }
  });

  const resultValue = (r) => r?.structuredContent ?? r;
  return {
    async backflow(action, component, value) {
      const out = await request("gui/backflow", { action, component, value: value ?? null, version: ctx.version || null });
      if (out?.kv) applyKv(out.kv);
      return out?.response ?? null;
    },
    async kv() {
      return (await request("gui/kv", {}))?.kv ?? {};
    },
    async callTool(name, args) {
      const r = await request("tools/call", { name, arguments: args ?? {} });
      if (r?.isError) {
        const text = (r.content ?? []).map((c) => c.text ?? "").join("\n");
        throw Object.assign(new Error(text || "the tool failed"), { result: r });
      }
      return resultValue(r);
    },
    sendMessage: (text) => request("ui/message", { role: "user", content: { type: "text", text: String(text) } }),
    updateContext: (structuredContent, text) =>
      request("ui/update-model-context", { structuredContent, content: text ? [{ type: "text", text: String(text) }] : undefined }),
    openLink: (url) => request("ui/open-link", { url: String(url) }),
    async start() {
      document.documentElement.classList.add("gui-embedded");
      try {
        const init = await request("ui/initialize", {
          protocolVersion: "2026-01-26",
          clientInfo: { name: "generative-ui-suite", version: "1" },
          appCapabilities: { availableDisplayModes: ["inline", "fullscreen"] },
        });
        applyContext(init?.hostContext);
      } catch (e) {
        console.error("ui/initialize failed", e);
      }
      notify("ui/notifications/initialized", {});
      // Report the content height so the host sizes the iframe (no inner scrollbar).
      let last = 0;
      const report = () => {
        // The body's own height: the document's would never shrink below the iframe's.
        const h = Math.ceil(document.body.getBoundingClientRect().height);
        if (h !== last) {
          last = h;
          notify("ui/notifications/size-changed", { width: Math.ceil(document.body.getBoundingClientRect().width), height: h });
        }
      };
      new ResizeObserver(report).observe(document.body);
      report();
      refreshKv();
    },
  };
}

const transport = ctx.transport === "postmessage" && window.parent !== window ? postMessageTransport() : httpTransport();

/** Send an interaction back (backflow). Returns the response a handler defined (or null). */
async function sendBackflow(action, component, value) {
  try {
    return await transport.backflow(action, component, value);
  } catch (e) {
    console.error("backflow failed", e);
    return null;
  }
}

/** Fetch the KV view and update every `[data-kv]` binding. */
async function refreshKv() {
  if (!document.querySelector("[data-kv]") && !kvListeners.size) return; // nothing bound
  try {
    applyKv(await transport.kv());
  } catch {
    /* KV unavailable — bound nodes keep their rendered text */
  }
}

const sibling = (slug) => surfaceUrl(ctx.agent, ctx.deckSlug, slug);

/** Parse a JSON attribute; a plain string stays a string. */
function jsonAttr(el, name, fallback = null) {
  const raw = el.getAttribute(name);
  if (raw == null) return fallback;
  try {
    return JSON.parse(raw);
  } catch {
    return raw;
  }
}

class UiButton extends HTMLElement {
  connectedCallback() {
    if (this._wired) return;
    this._wired = true;
    const label = this.textContent.trim();
    this.textContent = "";
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "ui-btn";
    btn.textContent = label;
    if (this.hasAttribute("disabled")) btn.disabled = true;
    btn.addEventListener("click", async () => {
      const action = this.getAttribute("data-action");
      const value = jsonAttr(this, "data-value");
      this.dispatchEvent(new CustomEvent("ui-action", { bubbles: true, detail: { action, value } }));
      if (!action) return;
      btn.disabled = true;
      try {
        await sendBackflow(action, "ui-button", value);
      } finally {
        btn.disabled = this.hasAttribute("disabled");
      }
    });
    this.appendChild(btn);
  }
}

function field(labelText) {
  const wrap = document.createElement("label");
  wrap.className = "ui-field";
  if (labelText) {
    const s = document.createElement("span");
    s.className = "ui-label";
    s.textContent = labelText;
    wrap.appendChild(s);
  }
  return wrap;
}

class UiInput extends HTMLElement {
  connectedCallback() {
    if (this._wired) return;
    this._wired = true;
    const name = this.getAttribute("name") || "";
    const wrap = field(this.getAttribute("label"));
    const multiline = this.getAttribute("type") === "textarea";
    const input = document.createElement(multiline ? "textarea" : "input");
    input.name = name;
    if (!multiline) input.type = this.getAttribute("type") || "text";
    else input.rows = Number(this.getAttribute("rows")) || 4;
    input.placeholder = this.getAttribute("placeholder") || "";
    if (this.hasAttribute("value")) input.value = this.getAttribute("value");
    if (this.hasAttribute("required")) input.required = true;
    input.className = "ui-control";
    this.innerHTML = "";
    wrap.appendChild(input);
    this.appendChild(wrap);
  }
}

class UiSelect extends HTMLElement {
  connectedCallback() {
    if (this._wired) return;
    this._wired = true;
    const name = this.getAttribute("name") || "";
    const options = [...this.querySelectorAll("option")];
    const wrap = field(this.getAttribute("label"));
    const select = document.createElement("select");
    select.name = name;
    select.className = "ui-control";
    options.forEach((o) => select.appendChild(o));
    this.innerHTML = "";
    wrap.appendChild(select);
    this.appendChild(wrap);
  }
}

class UiCheckbox extends HTMLElement {
  connectedCallback() {
    if (this._wired) return;
    this._wired = true;
    const name = this.getAttribute("name") || "";
    const checked = this.hasAttribute("checked");
    const text = this.textContent.trim();
    this.innerHTML = "";
    const wrap = document.createElement("label");
    wrap.className = "ui-check";
    const input = document.createElement("input");
    input.type = "checkbox";
    input.name = name;
    input.checked = checked;
    const span = document.createElement("span");
    span.textContent = text;
    wrap.append(input, span);
    this.appendChild(wrap);
  }
}

class UiForm extends HTMLElement {
  connectedCallback() {
    if (this._wired) return;
    this._wired = true;
    const action = this.getAttribute("data-action") || "";

    // Submit on any button inside the form that isn't itself a backflow button.
    this.addEventListener("click", (e) => {
      const btn = e.target.closest("button, ui-button");
      if (!btn || !this.contains(btn)) return;
      if (btn.closest("ui-button")?.getAttribute("data-action") || btn.getAttribute("data-action")) return; // its own action
      if (btn.classList.contains("ui-tabbtn")) return;
      e.preventDefault();
      const values = this.collect();
      this.dispatchEvent(new CustomEvent("ui-submit", { bubbles: true, detail: { action, values } }));
      sendBackflow(action, "ui-form", values);
    });

    // Provide a default submit button only if the author didn't include one.
    if (!this.querySelector("button, ui-button")) {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "ui-btn ui-submit";
      btn.textContent = this.getAttribute("submit-label") || "Submit";
      this.appendChild(btn);
    }
  }

  collect() {
    const out = {};
    this.querySelectorAll("input[name], select[name], textarea[name]").forEach((el) => {
      out[el.name] = el.type === "checkbox" ? el.checked : el.type === "number" && el.value !== "" ? Number(el.value) : el.value;
    });
    return out;
  }
}

class UiLink extends HTMLElement {
  connectedCallback() {
    if (this._wired) return;
    this._wired = true;
    const to = this.getAttribute("to");
    const label = this.textContent.trim() || to || "";
    this.textContent = "";
    const a = document.createElement("a");
    a.className = "ui-link";
    a.href = to ? sibling(to) : "#";
    a.textContent = label;
    this.appendChild(a);
  }
}

class UiDeckNav extends HTMLElement {
  connectedCallback() {
    if (this._wired) return;
    this._wired = true;
    const order = ctx.deck.order || [];
    const { prev, next } = navTargets(order, ctx.surfaceSlug);
    const nav = document.createElement("nav");
    nav.className = "ui-deck-nav";

    const link = (slug, text, cls) => {
      const a = document.createElement("a");
      a.className = "ui-deck-link" + (cls ? " " + cls : "");
      a.href = sibling(slug);
      a.textContent = text;
      return a;
    };

    if (prev) nav.appendChild(link(prev, "← " + prev, "prev"));
    const mid = document.createElement("span");
    mid.className = "ui-deck-list";
    order.forEach((slug) => mid.appendChild(link(slug, slug, slug === ctx.surfaceSlug ? "current" : "")));
    nav.appendChild(mid);
    if (next) nav.appendChild(link(next, next + " →", "next"));

    this.appendChild(nav);
  }
}

class UiHeading extends HTMLElement {
  connectedCallback() {
    this.setAttribute("role", "heading");
    this.setAttribute("aria-level", this.getAttribute("level") || "2");
  }
}

/** <ui-stat label value delta trend="up|down"> — one key figure. */
class UiStat extends HTMLElement {
  connectedCallback() {
    if (this._wired) return;
    this._wired = true;
    const part = (cls, text, kv) => {
      const d = document.createElement("div");
      d.className = cls;
      d.textContent = text;
      if (kv) d.setAttribute("data-kv", kv);
      return d;
    };
    const value = this.getAttribute("value") ?? this.textContent.trim();
    this.textContent = "";
    this.append(part("ui-stat-label", this.getAttribute("label") || ""), part("ui-stat-value", value, this.getAttribute("data-kv")));
    this.removeAttribute("data-kv"); // the value carries the binding, not the whole tile
    if (this.hasAttribute("delta")) this.append(part("ui-stat-delta", this.getAttribute("delta")));
  }
}

/** <ui-callout tone title>body</ui-callout> */
class UiCallout extends HTMLElement {
  connectedCallback() {
    if (this._wired) return;
    this._wired = true;
    this.setAttribute("role", this.getAttribute("tone") === "danger" ? "alert" : "note");
    const title = this.getAttribute("title");
    if (title) {
      const t = document.createElement("strong");
      t.className = "ui-callout-title";
      t.textContent = title;
      this.prepend(t);
    }
  }
}

/** <ui-table columns='[…]' rows='[[…]]'> or <ui-table data='[{…}]'>, or a child <table>.
 *  Numeric columns align right. */
class UiTable extends HTMLElement {
  connectedCallback() {
    if (this._wired) return;
    this._wired = true;
    let columns = jsonAttr(this, "columns");
    let rows = jsonAttr(this, "rows");
    const data = jsonAttr(this, "data");
    if (Array.isArray(data) && data.length && typeof data[0] === "object") {
      columns = columns ?? [...new Set(data.flatMap((r) => Object.keys(r)))];
      rows = data.map((r) => columns.map((c) => r[c]));
    }
    if (!Array.isArray(columns) || !Array.isArray(rows)) return; // a child <table> stays as is
    const numeric = columns.map((_, i) => rows.length > 0 && rows.every((r) => r[i] == null || typeof r[i] === "number"));
    const table = document.createElement("table");
    const head = table.createTHead().insertRow();
    columns.forEach((c, i) => {
      const th = document.createElement("th");
      th.textContent = String(c);
      if (numeric[i]) th.className = "ui-num";
      head.appendChild(th);
    });
    const body = table.createTBody();
    rows.forEach((r) => {
      const tr = body.insertRow();
      columns.forEach((_, i) => {
        const td = tr.insertCell();
        const v = r[i];
        td.textContent = v == null ? "" : typeof v === "object" ? JSON.stringify(v) : String(v);
        if (numeric[i]) td.className = "ui-num";
      });
    });
    this.replaceChildren(table);
  }
}

/** <ui-tabs><ui-tab label="A">…</ui-tab>…</ui-tabs> */
let tabSeq = 0;
class UiTabs extends HTMLElement {
  connectedCallback() {
    if (this._wired) return;
    this._wired = true;
    const tabs = [...this.children].filter((c) => c.tagName === "UI-TAB");
    if (!tabs.length) return;
    const list = document.createElement("div");
    list.className = "ui-tablist";
    list.setAttribute("role", "tablist");
    const id = `ui-tabs-${++tabSeq}`;
    const select = (i) => {
      tabs.forEach((t, j) => {
        t.hidden = i !== j;
        list.children[j].setAttribute("aria-selected", String(i === j));
        list.children[j].tabIndex = i === j ? 0 : -1;
      });
      // Charts and other measured content in a newly shown tab resize with it.
      window.dispatchEvent(new Event("resize"));
    };
    tabs.forEach((t, i) => {
      const b = document.createElement("button");
      b.type = "button";
      b.className = "ui-tabbtn";
      b.setAttribute("role", "tab");
      b.id = `${id}-${i}`;
      b.textContent = t.getAttribute("label") || `Tab ${i + 1}`;
      b.addEventListener("click", () => select(i));
      b.addEventListener("keydown", (e) => {
        const d = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
        if (!d) return;
        const n = (i + d + tabs.length) % tabs.length;
        select(n);
        list.children[n].focus();
      });
      t.setAttribute("role", "tabpanel");
      t.setAttribute("aria-labelledby", b.id);
      list.appendChild(b);
    });
    this.prepend(list);
    select(Math.max(0, tabs.findIndex((t) => t.hasAttribute("selected"))));
  }
}

const SVG = "http://www.w3.org/2000/svg";
function svgEl(tag, attrs, parent) {
  const el = document.createElementNS(SVG, tag);
  for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, String(v));
  parent?.appendChild(el);
  return el;
}

/** <ui-chart type="line|area|bar" stacked title height data='…'> — an SVG chart in the theme's
 *  series colours. ponytail: line/area/bar only; pie and scatter when a surface needs them. */
class UiChart extends HTMLElement {
  connectedCallback() {
    if (this._wired) return;
    this._wired = true;
    this.classList.add("ui-chart");
    const raw = jsonAttr(this, "data") ?? (() => {
      try {
        return JSON.parse(this.textContent);
      } catch {
        return null;
      }
    })();
    this.data = chartData(raw);
    this.textContent = "";
    this.draw();
    let w = 0;
    new ResizeObserver(() => {
      if (this.clientWidth && Math.abs(this.clientWidth - w) > 1) {
        w = this.clientWidth;
        this.draw();
      }
    }).observe(this);
  }

  draw() {
    const { labels, series } = this.data;
    const type = this.getAttribute("type") || "line";
    const stacked = this.hasAttribute("stacked") && type !== "line";
    const W = Math.max(240, this.clientWidth || 600);
    const H = Number(this.getAttribute("height")) || 220;
    const pad = { l: 40, r: 18, t: 8, b: 24 };
    const [lo, hi] = chartExtent(this.data, stacked);
    const ticks = niceTicks(lo, hi, 4);
    const y0 = ticks[0];
    const y1 = ticks.at(-1);
    const y = (v) => scale(v, y0, y1, H - pad.b, pad.t);
    const n = labels.length;
    const band = (W - pad.l - pad.r) / Math.max(1, n);
    const x = (i) => (type === "bar" ? pad.l + band * (i + 0.5) : scale(i, 0, Math.max(1, n - 1), pad.l, W - pad.r));

    this.replaceChildren();
    if (this.getAttribute("title")) {
      const t = document.createElement("div");
      t.className = "ui-chart-title";
      t.textContent = this.getAttribute("title");
      this.appendChild(t);
    }
    const svg = svgEl("svg", { viewBox: `0 0 ${W} ${H}`, role: "img", "aria-label": this.getAttribute("title") || `${type} chart` });
    this.appendChild(svg);
    for (const t of ticks) {
      svgEl("line", { class: "grid", x1: pad.l, x2: W - pad.r, y1: y(t), y2: y(t) }, svg);
      svgEl("text", { class: "axis", x: pad.l - 6, y: y(t) + 4, "text-anchor": "end" }, svg).textContent = formatNumber(t);
    }
    const every = Math.max(1, Math.ceil(n / Math.max(1, Math.floor((W - pad.l) / 64))));
    labels.forEach((l, i) => {
      if (i % every) return;
      svgEl("text", { class: "axis", x: x(i), y: H - 6, "text-anchor": "middle" }, svg).textContent = l;
    });

    const base = labels.map(() => 0);
    series.forEach((s, si) => {
      const g = svgEl("g", { class: `ui-s${(si % 6) + 1}` }, svg);
      const title = (i, v) => `${s.name ? s.name + " · " : ""}${labels[i]}: ${formatNumber(v)}`;
      if (type === "bar") {
        const groups = stacked ? 1 : series.length;
        const bw = (band * 0.7) / groups;
        s.values.forEach((v, i) => {
          if (v == null) return;
          const from = stacked ? base[i] : 0;
          const to = from + v;
          const bx = x(i) - (band * 0.7) / 2 + (stacked ? 0 : si * bw);
          const r = svgEl("rect", { class: "bar", x: bx, y: Math.min(y(from), y(to)), width: Math.max(1, bw - 1), height: Math.abs(y(from) - y(to)), rx: 2 }, g);
          svgEl("title", {}, r).textContent = title(i, v);
          if (stacked) base[i] = to;
        });
        return;
      }
      const pts = s.values.map((v, i) => (v == null ? null : [x(i), y((stacked ? base[i] : 0) + v), y(stacked ? base[i] : 0)]));
      // Split at gaps (null values) so a missing point breaks the line.
      const runs = [];
      let cur = [];
      pts.forEach((p) => {
        if (p) cur.push(p);
        else if (cur.length) runs.push(cur), (cur = []);
      });
      if (cur.length) runs.push(cur);
      for (const run of runs) {
        const d = run.map((p, i) => `${i ? "L" : "M"}${p[0].toFixed(1)},${p[1].toFixed(1)}`).join("");
        if (type === "area") {
          const back = [...run].reverse().map((p) => `L${p[0].toFixed(1)},${p[2].toFixed(1)}`).join("");
          svgEl("path", { class: "area", d: d + back + "Z" }, g);
        }
        svgEl("path", { class: "line", d }, g);
      }
      if (n <= 40) {
        pts.forEach((p, i) => {
          if (!p) return;
          const c = svgEl("circle", { class: "dot", cx: p[0], cy: p[1], r: 3 }, g);
          svgEl("title", {}, c).textContent = title(i, s.values[i]);
        });
      }
      if (stacked) s.values.forEach((v, i) => (base[i] += v ?? 0));
    });

    if (series.length > 1 || series[0]?.name) {
      const legend = document.createElement("div");
      legend.className = "ui-legend";
      series.forEach((s, si) => {
        const item = document.createElement("span");
        item.className = `ui-s${(si % 6) + 1}`;
        item.append(document.createElement("i"), document.createTextNode(s.name || `Series ${si + 1}`));
        legend.appendChild(item);
      });
      this.appendChild(legend);
    }
  }
}

/** <ui-grid cols="3"> — equal columns. */
class UiGrid extends HTMLElement {
  connectedCallback() {
    const cols = Number(this.getAttribute("cols")) || 2;
    this.style.setProperty("--cols", String(Math.min(6, Math.max(1, cols))));
  }
}

// Live rewrite: translate ordinary HTML elements into the matching <ui-*> components, so an
// author (or agent) can write plain HTML and still get styling + backflow wiring. Authors are
// encouraged to use the <ui-*> components directly; this is a convenience translation.
function upgrade(el, tag, setup) {
  const ne = document.createElement(tag);
  for (const attr of el.attributes) ne.setAttribute(attr.name, attr.value);
  while (el.firstChild) ne.appendChild(el.firstChild);
  if (setup) setup(el, ne);
  el.replaceWith(ne);
}

function rewritePlainHtml(root) {
  // Snapshot first: replaced nodes move (not removed), so the list stays valid. Only buttons
  // and forms that take part in backflow are rewritten; a page's own scripted controls
  // (no data-action, no form) stay plain.
  root.querySelectorAll("h1, h2, h3, p, button, form, select").forEach((el) => {
    switch (el.tagName) {
      case "H1":
        upgrade(el, "ui-heading", (_o, n) => n.setAttribute("level", "1"));
        break;
      case "H2":
        upgrade(el, "ui-heading", (_o, n) => n.setAttribute("level", "2"));
        break;
      case "H3":
        upgrade(el, "ui-heading", (_o, n) => n.setAttribute("level", "3"));
        break;
      case "P":
        upgrade(el, "ui-text");
        break;
      case "BUTTON":
        if (el.hasAttribute("data-action") || el.closest("form")) upgrade(el, "ui-button");
        else el.classList.add("ui-btn");
        break;
      case "SELECT":
        if (el.closest("form")) upgrade(el, "ui-select");
        else el.classList.add("ui-control");
        break;
      case "FORM":
        upgrade(el, "ui-form", (o, n) => {
          const a = o.getAttribute("action");
          if (a && !n.hasAttribute("data-action")) n.setAttribute("data-action", a);
        });
        break;
    }
  });
  root.querySelectorAll("input:not([type=checkbox]):not([type=radio]), textarea").forEach((el) => el.classList.add("ui-control"));
  // <a data-to="slug"> → <ui-link to="slug"> (plain <a href> links are left alone).
  root.querySelectorAll("a[data-to]").forEach((el) =>
    upgrade(el, "ui-link", (o, n) => n.setAttribute("to", o.getAttribute("data-to"))),
  );
}

// Embedded, a plain link opens through the host (the page has no navigation of its own).
if (ctx.transport === "postmessage") {
  document.addEventListener("click", (e) => {
    const a = e.target.closest?.("a[href]");
    if (!a || a.closest("ui-link, ui-deck-nav")) return;
    const href = a.getAttribute("href");
    if (!href || href.startsWith("#")) return;
    e.preventDefault();
    transport.openLink(a.href).catch((err) => console.error("open link", err));
  });
}

// Rewrite authored plain HTML BEFORE defining the components, so the components' own internal
// elements (e.g. the inner <button> that <ui-button> renders) are never re-processed — which
// would otherwise nest a <ui-button> inside a <ui-button> and render the control twice.
rewritePlainHtml(document.body);

customElements.define("ui-button", UiButton);
customElements.define("ui-input", UiInput);
customElements.define("ui-select", UiSelect);
customElements.define("ui-checkbox", UiCheckbox);
customElements.define("ui-form", UiForm);
customElements.define("ui-link", UiLink);
customElements.define("ui-deck-nav", UiDeckNav);
customElements.define("ui-heading", UiHeading);
customElements.define("ui-stat", UiStat);
customElements.define("ui-callout", UiCallout);
customElements.define("ui-table", UiTable);
customElements.define("ui-tabs", UiTabs);
customElements.define("ui-chart", UiChart);
customElements.define("ui-grid", UiGrid);

// Streamable views (gui-server): re-render in realtime when *this* surface gets a new version
// (e.g. an agent regenerates it in response to backflow). Reloads only on a version change,
// not on backflow events, so a user's own click never reloads the page under them.
async function liveVersion() {
  try {
    const res = await fetch(versionsUrl(ctx.surface));
    if (!res.ok) return null;
    return (await res.json()).live ?? null;
  } catch {
    return null;
  }
}

function subscribeReload() {
  if (!ctx.surface) return;
  try {
    const es = new EventSource("/events");
    // On any change, check whether *this* surface has a newer live version and reload if so.
    // A backflow event never bumps the version, so the user's own click won't reload them.
    es.onmessage = async (m) => {
      let msg = {};
      try {
        msg = JSON.parse(m.data);
      } catch {
        /* not JSON — treat as a plain change ping */
      }
      // A stream event (progress/status) never changes the surface: update its KV bindings
      // and let custom code listen via document.addEventListener("ui-stream", …).
      if (msg.type === "stream") {
        if (msg.surface !== ctx.surface) return;
        document.dispatchEvent(new CustomEvent("ui-stream", { detail: msg }));
        refreshKv();
        return;
      }
      if (needsReload(ctx.version, await liveVersion())) {
        location.reload();
        return;
      }
      // Not a republish — a handler may have folded new KV state; refresh bound nodes.
      refreshKv();
    };
  } catch {
    /* SSE unavailable — the surface still works, just not live */
  }
}

const gui = {
  ctx,
  callTool: (name, args) => transport.callTool(name, args),
  sendMessage: (text) => transport.sendMessage(text),
  updateContext: (data, text) => transport.updateContext(data, text),
  openLink: (url) => transport.openLink(url),
  backflow: (action, value) => sendBackflow(action, "script", value),
  kv: () => transport.kv(),
  onKv(f) {
    kvListeners.add(f);
    return () => kvListeners.delete(f);
  },
};
window.gui = gui;
transport.start();

export { ctx, gui, sendBackflow };
