// The agent-facing document API. Each call marshals its arguments as JSON to the host's `__call`,
// which applies it inside the script's transaction and returns the result (or throws).
"use strict";
const __c = (name, ...args) => __call(name, JSON.stringify(args));
const __q = (q) => (typeof q === "string" ? { id: q } : q ?? {});
globalThis.insert = (parentId, node, index) => __c("insert", parentId ?? null, node, index ?? null);
globalThis.update = (id, props) => __c("update", id, props);
globalThis.replace = (id, node) => __c("replace", id, node);
globalThis.move = (id, parentId, index) => __c("move", id, parentId ?? null, index ?? null);
globalThis.copy = (id, parentId, overrides, index) => __c("copy", id, parentId ?? null, overrides ?? null, index ?? null);
globalThis.remove = (id) => __c("delete", id);
globalThis.get = (idOrQuery, options) =>
  typeof idOrQuery === "string" ? __c("get", idOrQuery, options ?? {}) : __c("query", __q(idOrQuery), options ?? {});
globalThis.find = (queryOrFn) => {
  if (typeof queryOrFn === "function") {
    return __c("summaries", {}).filter((n) => queryOrFn(n)).map((n) => n.id);
  }
  return __c("find", __q(queryOrFn));
};
globalThis.setVariables = (collections) => __c("setVariables", collections);
globalThis.getVariables = () => __c("getVariables");
globalThis.createComponent = (ids, name) => __c("createComponent", [].concat(ids), name ?? null);
globalThis.detach = (id) => __c("detach", id);
globalThis.findEmptySpace = (width, height, options) => __c("findEmptySpace", width, height, options ?? {});
globalThis.screenshot = (ids, options) => __c("screenshot", ids == null ? null : [].concat(ids), options ?? {});
globalThis.exportImage = (ids, format, path, options) => __c("export", ids == null ? null : [].concat(ids), format, path, options ?? {});
globalThis.select = (ids) => __c("select", ids == null ? [] : [].concat(ids));
globalThis.setPage = (id) => __c("setPage", id);
globalThis.print = (...a) => __c("print", a.map((v) => (typeof v === "string" ? v : JSON.stringify(v))).join(" "));
Object.defineProperty(globalThis, "page", { get: () => __c("state").page, configurable: true });
Object.defineProperty(globalThis, "selection", { get: () => __c("state").selection, configurable: true });
Object.defineProperty(globalThis, "viewport", { get: () => __c("state").viewport, configurable: true });
// Configurable, so a script may declare its own `page`, `selection` or `viewport`.
// `delete` and `export` are reserved words, so they are also reachable as properties.
globalThis.doc = { insert, update, replace, move, copy, delete: remove, get, find, setVariables, getVariables, createComponent, detach, findEmptySpace, screenshot, export: exportImage, select, setPage, print };
