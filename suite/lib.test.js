// Pure-helper tests for the suite runtime. Run with: node --test suite/
// No npm install — uses the built-in node:test runner and node:assert.

import { test } from "node:test";
import assert from "node:assert/strict";

import {
  backflowPayload,
  surfaceUrl,
  backflowUrl,
  navTargets,
  needsReload,
  versionsUrl,
  kvUrl,
  kvValue,
  kvDisplay,
} from "./lib.js";

test("backflowPayload normalizes nullables", () => {
  assert.deepEqual(backflowPayload("buy", "ui-button", { id: 1 }, "100", "t"), {
    action: "buy",
    component: "ui-button",
    value: { id: 1 },
    version: "100",
    timestamp: "t",
  });
  assert.deepEqual(backflowPayload("x", undefined, undefined, undefined, "t"), {
    action: "x",
    component: null,
    value: null,
    version: null,
    timestamp: "t",
  });
});

test("surfaceUrl / backflowUrl", () => {
  assert.equal(surfaceUrl("demo", "main", "sales"), "/demo/main/sales/");
  assert.equal(backflowUrl("demo/main/sales"), "/backflow/demo/main/sales");
});

test("needsReload only when the live version differs", () => {
  assert.equal(needsReload("1", "2"), true, "republished → reload");
  assert.equal(needsReload("1", "1"), false, "same version (e.g. a backflow event) → no reload");
  assert.equal(needsReload("1", null), false);
  assert.equal(needsReload(null, "2"), false);
  assert.equal(versionsUrl("demo/main/sales"), "/api/demo/main/sales/versions.json");
});

test("kvUrl / kvValue / kvDisplay drive [data-kv] bindings", () => {
  assert.equal(kvUrl("demo/main/sales"), "/api/demo/main/sales/kv.json");
  const kv = { hits: 3, user: { name: "Ada" }, ready: false };
  assert.equal(kvValue(kv, "hits"), 3);
  assert.equal(kvValue(kv, "user.name"), "Ada"); // dotted path
  assert.equal(kvValue(kv, "missing"), undefined);
  assert.equal(kvValue(kv, "user.nope.deep"), undefined); // safe past a missing branch
  assert.equal(kvValue(null, "x"), undefined);
  // display: objects → JSON, nullish → "", primitives → String (incl. falsey)
  assert.equal(kvDisplay(kvValue(kv, "hits")), "3");
  assert.equal(kvDisplay(kvValue(kv, "user")), '{"name":"Ada"}');
  assert.equal(kvDisplay(kvValue(kv, "missing")), "");
  assert.equal(kvDisplay(kvValue(kv, "ready")), "false");
});

test("navTargets resolves prev/next", () => {
  assert.deepEqual(navTargets(["a", "b", "c"], "b"), { prev: "a", next: "c" });
  assert.deepEqual(navTargets(["a", "b", "c"], "a"), { prev: null, next: "b" });
  assert.deepEqual(navTargets(["a", "b", "c"], "c"), { prev: "b", next: null });
  assert.deepEqual(navTargets([], "x"), { prev: null, next: null });
  assert.deepEqual(navTargets(["a"], "missing"), { prev: null, next: null });
});

import { niceTicks, scale, chartData, chartExtent, formatNumber, rpc, hostTheme, hostVariables } from "./lib.js";

test("niceTicks: round steps covering the range", () => {
  assert.deepEqual(niceTicks(0, 100, 5), [0, 20, 40, 60, 80, 100]);
  assert.deepEqual(niceTicks(0.1, 0.3, 2), [0.1, 0.2, 0.3]);
  assert.deepEqual(niceTicks(5, 5, 2), [0, 5, 10]);
  assert.deepEqual(niceTicks(NaN, 1), [0, 1]);
  const t = niceTicks(-3, 7, 5);
  assert.ok(t[0] <= -3 && t.at(-1) >= 7);
});

test("scale maps a domain onto a range", () => {
  assert.equal(scale(5, 0, 10, 0, 100), 50);
  assert.equal(scale(0, 0, 10, 100, 0), 100, "inverted (SVG y)");
  assert.equal(scale(3, 2, 2, 0, 10), 5, "empty domain → middle");
});

test("chartData normalizes the three input shapes", () => {
  assert.deepEqual(chartData([{ label: "a", value: 1 }, { label: "b", value: "2" }]), { labels: ["a", "b"], series: [{ name: "", values: [1, 2] }] });
  assert.deepEqual(chartData({ labels: ["x"], values: [3], name: "n" }), { labels: ["x"], series: [{ name: "n", values: [3] }] });
  const d = chartData({ series: [{ name: "s", values: [1, null, "x", 4] }] });
  assert.deepEqual(d.labels, ["1", "2", "3", "4"], "missing labels count from 1");
  assert.deepEqual(d.series[0].values, [1, null, null, 4], "non-numbers are gaps");
});

test("chartExtent includes zero; stacked sums", () => {
  const d = chartData({ labels: ["a", "b"], series: [{ values: [2, 3] }, { values: [1, 4] }] });
  assert.deepEqual(chartExtent(d), [0, 4]);
  assert.deepEqual(chartExtent(d, true), [0, 7]);
  assert.deepEqual(chartExtent(chartData({ series: [] })), [0, 1]);
});

test("formatNumber is compact", () => {
  assert.equal(formatNumber(1234), "1.2k");
  assert.equal(formatNumber(25000), "25k");
  assert.equal(formatNumber(2_500_000), "2.5M");
  assert.equal(formatNumber(0.256), "0.26");
  assert.equal(formatNumber(42), "42");
  assert.equal(formatNumber(null), "");
});

test("bridge helpers: rpc envelopes, theme and safe variables", () => {
  assert.deepEqual(rpc("ui/initialize", { a: 1 }, 1), { jsonrpc: "2.0", method: "ui/initialize", params: { a: 1 }, id: 1 });
  assert.deepEqual(rpc("ui/notifications/initialized"), { jsonrpc: "2.0", method: "ui/notifications/initialized" });
  assert.equal(hostTheme({ theme: "light" }), "light");
  assert.equal(hostTheme({ theme: "sepia" }), null);
  const vars = hostVariables({ styles: { variables: { "--color-text-primary": "#111", "color": "red", "--x": "a;b", "--y": 3 } } });
  assert.deepEqual(vars, [["--color-text-primary", "#111"]]);
});
