"use strict";
// Rago Billing console — core: API client, router, and UI building blocks.
// Views register themselves through Rago.route(...) (see views.js / views2.js).
(function () {
  const KEY_STORE = "rago_admin_key";
  const $ = (sel, root) => (root || document).querySelector(sel);
  const $$ = (sel, root) => Array.from((root || document).querySelectorAll(sel));

  // ---- icons (inline SVG, stroke style) -----------------------------------
  const ICON_PATHS = {
    logo: "M4 7l8-4 8 4-8 4-8-4zm0 5l8 4 8-4M4 17l8 4 8-4",
    overview: "M4 20V10m6 10V4m6 16v-7m4 7H2",
    customers: "M16 19v-1a4 4 0 00-4-4H7a4 4 0 00-4 4v1m6.5-9a3.5 3.5 0 100-7 3.5 3.5 0 000 7zM21 19v-1a4 4 0 00-3-3.87M16 3.13a4 4 0 010 7.75",
    subscriptions: "M17 2l4 4-4 4M3 11V9a3 3 0 013-3h15M7 22l-4-4 4-4m14-1v2a3 3 0 01-3 3H3",
    plans: "M3 6h18M3 12h18M3 18h12",
    metrics: "M3 3v18h18M7 15l4-4 3 3 5-6",
    invoices: "M6 2h9l5 5v15H6zM14 2v6h6M9 13h6M9 17h6",
    credit_notes: "M6 2h12v20l-3-2-3 2-3-2-3 2zM9 8h6M9 12h6",
    wallets: "M3 7h15a3 3 0 013 3v7a3 3 0 01-3 3H3zM3 7V5a2 2 0 012-2h11v4M16 14h2",
    coupons: "M3 9V5h18v4a3 3 0 000 6v4H3v-4a3 3 0 000-6zM13 5v14",
    addons: "M12 5v14M5 12h14",
    taxes: "M19 5L5 19M7 7h.01M17 17h.01",
    webhooks: "M18 16a3 3 0 11-2.83 4M6 16a3 3 0 102.83 4H15M12 4a3 3 0 00-1.5 5.6L7 16",
    dunning: "M12 8v4l3 3M3 12a9 9 0 1018 0 9 9 0 00-18 0",
    events: "M13 2L3 14h8l-1 8 10-12h-8z",
    exports: "M12 3v12m0 0l-4-4m4 4l4-4M4 17v3h16v-3",
    settings: "M12 15a3 3 0 100-6 3 3 0 000 6zM19.4 15a1.7 1.7 0 00.3 1.8l.1.1a2 2 0 11-2.8 2.8l-.1-.1a1.7 1.7 0 00-1.8-.3 1.7 1.7 0 00-1 1.5V21a2 2 0 11-4 0v-.1a1.7 1.7 0 00-1.1-1.5 1.7 1.7 0 00-1.8.3l-.1.1a2 2 0 11-2.8-2.8l.1-.1a1.7 1.7 0 00.3-1.8 1.7 1.7 0 00-1.5-1H3a2 2 0 110-4h.1a1.7 1.7 0 001.5-1.1 1.7 1.7 0 00-.3-1.8l-.1-.1a2 2 0 112.8-2.8l.1.1a1.7 1.7 0 001.8.3H9a1.7 1.7 0 001-1.5V3a2 2 0 114 0v.1a1.7 1.7 0 001 1.5 1.7 1.7 0 001.8-.3l.1-.1a2 2 0 112.8 2.8l-.1.1a1.7 1.7 0 00-.3 1.8V9a1.7 1.7 0 001.5 1H21a2 2 0 110 4h-.1a1.7 1.7 0 00-1.5 1z",
    plus: "M12 5v14M5 12h14",
    refresh: "M21 12a9 9 0 11-3-6.7L21 8M21 3v5h-5",
    download: "M12 3v12m0 0l-4-4m4 4l4-4M4 17v3h16v-3",
    trash: "M3 6h18M8 6V4h8v2M6 6l1 14h10l1-14",
    edit: "M4 20h4L20 8l-4-4L4 16zM14 6l4 4",
    link: "M10 14a5 5 0 007 0l3-3a5 5 0 00-7-7l-1 1M14 10a5 5 0 00-7 0l-3 3a5 5 0 007 7l1-1",
    back: "M15 18l-6-6 6-6",
    close: "M18 6L6 18M6 6l12 12",
    check: "M20 6L9 17l-5-5",
    alert: "M12 9v4m0 4h.01M10.3 3.9L2 18a2 2 0 001.7 3h16.6a2 2 0 001.7-3L13.7 3.9a2 2 0 00-3.4 0z",
    logout: "M9 21H5a2 2 0 01-2-2V5a2 2 0 012-2h4M16 17l5-5-5-5M21 12H9",
    retry: "M1 4v6h6M23 20v-6h-6M20.5 9A9 9 0 005.6 5.6L1 10m22 4l-4.6 4.4A9 9 0 013.5 15",
    pay: "M2 7h20v10H2zM2 11h20",
  };
  function icon(name, label) {
    const d = ICON_PATHS[name] || ICON_PATHS.plus;
    const aria = label ? ' role="img" aria-label="' + esc(label) + '"' : ' aria-hidden="true"';
    return '<svg class="icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"' + aria + '><path d="' + d + '"/></svg>';
  }

  // ---- formatting ---------------------------------------------------------
  function esc(v) {
    if (v === null || v === undefined) return "";
    return String(v).replace(/[&<>"']/g, (c) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
  }
  function money(cents, currency) {
    if (cents === null || cents === undefined || cents === "") return '<span class="muted">-</span>';
    const n = Number(cents) / 100;
    if (Number.isNaN(n)) return esc(cents);
    const s = n.toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 2 });
    return (currency ? esc(currency) + " " : "") + s;
  }
  function date(v) { return v ? esc(String(v).slice(0, 10)) : "-"; }
  function datetime(v) { return v ? esc(String(v).replace("T", " ").slice(0, 19)) : "-"; }
  const OK = ["succeeded", "active", "finalized", "completed", "paid", "delivered"];
  const ERR = ["failed", "voided", "terminated", "canceled", "refunded"];
  const WARN = ["pending", "retrying", "draft", "processing", "overdue"];
  function badge(text) {
    const t = text === null || text === undefined || text === "" ? "-" : String(text);
    const cls = OK.includes(t) ? " ok" : ERR.includes(t) ? " err" : WARN.includes(t) ? " warn" : "";
    return '<span class="badge' + cls + '">' + esc(t) + "</span>";
  }
  function centsFromInput(v) {
    if (v === "" || v === null || v === undefined) return undefined;
    const n = Number(String(v).replace(/,/g, ""));
    if (Number.isNaN(n)) return undefined;
    return Math.round(n * 100);
  }

  // ---- API client -----------------------------------------------------------
  function getKey() { return sessionStorage.getItem(KEY_STORE) || ""; }
  function setKey(k) { if (k) sessionStorage.setItem(KEY_STORE, k); else sessionStorage.removeItem(KEY_STORE); }

  class ApiError extends Error {
    constructor(status, message, details) { super(message); this.status = status; this.details = details; }
  }
  function describeError(status, body) {
    if (!body) return "HTTP " + status;
    if (body.error_details) {
      const parts = Object.entries(body.error_details).map(([k, v]) => k + ": " + (Array.isArray(v) ? v.join(", ") : JSON.stringify(v)));
      return parts.join("\n") || body.error || "Validation failed";
    }
    if (body.code) return (body.error || "Error") + " (" + body.code + ")";
    return body.error || "HTTP " + status;
  }
  async function request(method, path, body, raw) {
    const headers = { "Authorization": "Bearer " + getKey(), "Accept": raw ? "*/*" : "application/json" };
    const init = { method, headers };
    if (body !== undefined) { headers["Content-Type"] = "application/json"; init.body = JSON.stringify(body); }
    let res;
    try { res = await fetch(path, init); }
    catch (e) { throw new ApiError(0, "Network error: " + e.message); }
    if (res.status === 401) { onUnauthorized(); throw new ApiError(401, "Unauthorized: check the API key."); }
    if (raw && res.ok) return res;
    let data = null;
    const text = await res.text();
    if (text) { try { data = JSON.parse(text); } catch (_) { data = { error: text.slice(0, 300) }; } }
    if (!res.ok) throw new ApiError(res.status, describeError(res.status, data), data && data.error_details);
    return data || {};
  }
  const api = {
    get: (p) => request("GET", p),
    post: (p, b) => request("POST", p, b === undefined ? {} : b),
    put: (p, b) => request("PUT", p, b === undefined ? {} : b),
    del: (p) => request("DELETE", p),
    async download(path, filename) {
      const res = await request("GET", path, undefined, true);
      const blob = await res.blob();
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url; a.download = filename; document.body.appendChild(a); a.click(); a.remove();
      setTimeout(() => URL.revokeObjectURL(url), 5000);
    },
    async postDownload(path, filename) {
      const headers = { "Authorization": "Bearer " + getKey() };
      const res = await fetch(path, { method: "POST", headers });
      if (!res.ok) throw new ApiError(res.status, "HTTP " + res.status);
      const blob = await res.blob();
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url; a.download = filename; document.body.appendChild(a); a.click(); a.remove();
      setTimeout(() => URL.revokeObjectURL(url), 5000);
    },
    graphql: async (query, variables) => {
      const data = await request("POST", "/graphql", { query, variables: variables || {} });
      if (data.errors && data.errors.length) throw new ApiError(422, data.errors.map((e) => e.message).join("\n"));
      return data.data;
    },
  };
  const enc = encodeURIComponent;

  // ---- toasts / status --------------------------------------------------------
  function toast(msg, kind) {
    const t = document.createElement("div");
    t.className = "toast " + (kind || "");
    t.innerHTML = icon(kind === "err" ? "alert" : "check") + "<div>" + esc(msg).replace(/\n/g, "<br>") + "</div>";
    $("#toasts").appendChild(t);
    setTimeout(() => t.remove(), kind === "err" ? 8000 : 4000);
  }
  function setStatus(msg, kind) { const s = $("#status"); s.textContent = msg; s.className = kind || ""; }

  // ---- modal forms -------------------------------------------------------------
  // fields: [{ name, label, type, required, options:[[value,label]], placeholder, help, value }]
  function fieldHtml(f) {
    const id = "f_" + f.name;
    const req = f.required ? " required" : "";
    const ph = f.placeholder ? ' placeholder="' + esc(f.placeholder) + '"' : "";
    const val = f.value === undefined || f.value === null ? "" : f.value;
    let input;
    if (f.type === "select") {
      input = '<select id="' + id + '" name="' + esc(f.name) + '"' + req + ">" +
        (f.options || []).map(([v, l]) => '<option value="' + esc(v) + '"' + (String(v) === String(val) ? " selected" : "") + ">" + esc(l) + "</option>").join("") + "</select>";
    } else if (f.type === "textarea" || f.type === "json") {
      input = '<textarea id="' + id + '" name="' + esc(f.name) + '"' + req + ph + ">" + esc(typeof val === "object" ? JSON.stringify(val, null, 2) : val) + "</textarea>";
    } else if (f.type === "checkbox") {
      return '<div class="field check"><label><input type="checkbox" id="' + id + '" name="' + esc(f.name) + '"' + (val ? " checked" : "") + " />" + esc(f.label) + "</label>" + (f.help ? '<div class="help">' + esc(f.help) + "</div>" : "") + "</div>";
    } else {
      const type = f.type === "money" ? "text" : (f.type || "text");
      const mode = f.type === "money" || f.type === "number" ? ' inputmode="decimal"' : "";
      input = '<input id="' + id + '" name="' + esc(f.name) + '" type="' + esc(type) + '"' + req + ph + mode + ' value="' + esc(val) + '" autocomplete="off" />';
    }
    return '<div class="field"><label for="' + id + '">' + esc(f.label) + (f.required ? " *" : "") + "</label>" + input +
      (f.help ? '<div class="help">' + esc(f.help) + "</div>" : "") + "</div>";
  }
  function readForm(form, fields) {
    const out = {};
    for (const f of fields) {
      const node = form.elements[f.name];
      if (!node) continue;
      let v;
      if (f.type === "checkbox") v = node.checked;
      else v = node.value.trim();
      if (f.type === "money") { v = v === "" ? undefined : centsFromInput(v); if (v === undefined && node.value.trim() !== "") throw new Error(f.label + ": not a valid amount"); }
      else if (f.type === "number") { v = v === "" ? undefined : Number(v); if (Number.isNaN(v)) throw new Error(f.label + ": not a number"); }
      else if (f.type === "json") { if (v === "") v = undefined; else { try { v = JSON.parse(v); } catch (e) { throw new Error(f.label + ": invalid JSON (" + e.message + ")"); } } }
      else if (v === "" && f.type !== "checkbox") v = undefined;
      if (f.required && (v === undefined || v === "")) throw new Error(f.label + " is required");
      out[f.name] = v;
    }
    return out;
  }
  function closeOverlay() { $("#overlay-root").innerHTML = ""; }
  function openForm(opts) {
    const root = $("#overlay-root");
    root.innerHTML =
      '<div class="overlay" role="dialog" aria-modal="true" aria-labelledby="modal-title"><form class="modal" novalidate>' +
      '<header><h3 id="modal-title">' + esc(opts.title) + '</h3><button type="button" class="ghost" data-close aria-label="Close">' + icon("close") + "</button></header>" +
      '<div class="content">' + (opts.intro ? '<div class="help">' + esc(opts.intro) + "</div>" : "") + opts.fields.map(fieldHtml).join("") +
      '<div class="form-error" role="alert"></div></div>' +
      '<footer><button type="button" data-close>Cancel</button><button type="submit" class="' + (opts.danger ? "danger" : "primary") + '">' + esc(opts.submitLabel || "Save") + "</button></footer>" +
      "</form></div>";
    const form = $("form", root);
    const errBox = $(".form-error", root);
    $$("[data-close]", root).forEach((b) => (b.onclick = closeOverlay));
    $(".overlay", root).addEventListener("keydown", (e) => { if (e.key === "Escape") closeOverlay(); });
    const first = $("input, select, textarea", form);
    if (first) first.focus();
    form.onsubmit = async (e) => {
      e.preventDefault();
      errBox.textContent = "";
      let values;
      try { values = readForm(form, opts.fields); } catch (err) { errBox.textContent = err.message; return; }
      const btn = $('button[type="submit"]', form);
      btn.disabled = true;
      try {
        const msg = await opts.onSubmit(values);
        closeOverlay();
        if (msg) toast(msg, "ok");
        if (opts.after) await opts.after();
      } catch (err) {
        errBox.textContent = err.message;
      } finally { btn.disabled = false; }
    };
  }
  function confirmAction(title, text, label, fn) {
    openForm({ title, intro: text, fields: [], submitLabel: label, danger: true, onSubmit: fn, after: () => render() });
  }
  function showJson(title, obj) {
    const root = $("#overlay-root");
    root.innerHTML = '<div class="overlay" role="dialog" aria-modal="true"><div class="modal"><header><h3>' + esc(title) +
      '</h3><button type="button" class="ghost" data-close aria-label="Close">' + icon("close") + '</button></header><div class="content"><pre class="json">' +
      esc(JSON.stringify(obj, null, 2)) + "</pre></div></div></div>";
    $$("[data-close]", root).forEach((b) => (b.onclick = closeOverlay));
  }

  // ---- tables / pager / cards -----------------------------------------------------
  // cols: [{ h, f(row) -> html, cls }]
  function table(cols, rows, opts) {
    opts = opts || {};
    if (!rows || rows.length === 0) return '<div class="empty">' + esc(opts.empty || "Nothing here yet.") + "</div>";
    const head = cols.map((c) => "<th" + (c.cls ? ' class="' + c.cls + '"' : "") + ">" + esc(c.h) + "</th>").join("");
    const body = rows.map((r, i) =>
      "<tr" + (opts.href ? ' class="clickable" data-href="' + esc(opts.href(r)) + '" tabindex="0"' : "") + ' data-idx="' + i + '">' +
      cols.map((c) => "<td" + (c.cls ? ' class="' + c.cls + '"' : "") + ">" + c.f(r) + "</td>").join("") + "</tr>").join("");
    return '<div class="scroll"><table><thead><tr>' + head + "</tr></thead><tbody>" + body + "</tbody></table></div>";
  }
  function bindRowLinks(root) {
    $$("tr[data-href]", root).forEach((tr) => {
      const go = (e) => { if (e.target.closest("button, a")) return; location.hash = tr.dataset.href; };
      tr.addEventListener("click", go);
      tr.addEventListener("keydown", (e) => { if (e.key === "Enter") go(e); });
    });
  }
  function pager(meta, page) {
    if (!meta || !meta.total_pages) return "";
    return '<div class="pager"><span>' + esc(meta.total_count) + " total</span><div class=\"spacer\"></div>" +
      '<button type="button" data-page="' + (page - 1) + '"' + (meta.prev_page ? "" : " disabled") + ">Previous</button>" +
      "<span>Page " + esc(meta.current_page || page) + " of " + esc(meta.total_pages) + "</span>" +
      '<button type="button" data-page="' + (page + 1) + '"' + (meta.next_page ? "" : " disabled") + ">Next</button></div>";
  }
  function bindPager(root, onPage) { $$("[data-page]", root).forEach((b) => (b.onclick = () => onPage(Number(b.dataset.page)))); }
  function kv(pairs) {
    return '<dl class="kv">' + pairs.map(([k, v]) => "<dt>" + esc(k) + "</dt><dd>" + (v === undefined || v === null || v === "" ? "-" : v) + "</dd>").join("") + "</dl>";
  }
  function btn(action, label, iconName, cls) {
    return '<button type="button" data-action="' + esc(action) + '" class="' + (cls || "") + '">' + (iconName ? icon(iconName) : "") + esc(label) + "</button>";
  }
  function bindActions(root, handlers) {
    $$("[data-action]", root).forEach((b) => {
      const h = handlers[b.dataset.action];
      if (!h) return;
      b.onclick = async (e) => {
        e.stopPropagation();
        b.disabled = true;
        try { await h(b); } catch (err) { toast(err.message, "err"); } finally { b.disabled = false; }
      };
    });
  }
  function bars(points, fmt) {
    if (!points.length) return '<div class="empty">No data yet.</div>';
    const max = Math.max(...points.map((p) => Math.abs(p.value)), 1);
    return '<div class="bars" role="img" aria-label="Bar chart">' + points.map((p) =>
      '<div class="bar" title="' + esc(p.label + ": " + fmt(p.value)) + '"><div class="fill" style="height:' + Math.max(1, Math.round((Math.abs(p.value) / max) * 130)) + 'px"></div><div class="lbl">' + esc(p.short || p.label) + "</div></div>").join("") + "</div>";
  }

  // ---- router ------------------------------------------------------------------------
  const routes = [];
  const nav = [];
  function route(pattern, handler) {
    const keys = [];
    const re = new RegExp("^" + pattern.replace(/:[a-z_]+/g, (m) => { keys.push(m.slice(1)); return "([^/]+)"; }) + "$");
    routes.push({ re, keys, handler });
  }
  function navItem(group, hash, label, iconName) { nav.push({ group, hash, label, iconName }); }
  function renderNav() {
    let html = "";
    let group = null;
    for (const n of nav) {
      if (n.group !== group) { group = n.group; html += '<div class="group">' + esc(group) + "</div>"; }
      html += '<a class="nav" href="#' + esc(n.hash) + '" data-hash="' + esc(n.hash) + '">' + icon(n.iconName) + esc(n.label) + "</a>";
    }
    $("#nav").innerHTML = html;
  }
  function highlightNav(path) {
    $$("#nav a.nav").forEach((a) => {
      const h = a.dataset.hash;
      a.classList.toggle("active", path === h || path.startsWith(h + "/"));
      if (a.classList.contains("active")) a.setAttribute("aria-current", "page"); else a.removeAttribute("aria-current");
    });
  }
  function currentPath() { return (location.hash || "#/overview").slice(1).split("?")[0]; }
  function query() {
    const q = (location.hash.split("?")[1]) || "";
    return Object.fromEntries(new URLSearchParams(q));
  }
  let renderSeq = 0;
  async function render() {
    if (!getKey()) return;
    const path = currentPath();
    highlightNav(path);
    const view = $("#view");
    const seq = ++renderSeq;
    for (const r of routes) {
      const m = path.match(r.re);
      if (!m) continue;
      const params = {};
      r.keys.forEach((k, i) => (params[k] = decodeURIComponent(m[i + 1])));
      view.innerHTML = '<div class="loading">Loading...</div>';
      try {
        const html = await r.handler(params, query());
        if (seq !== renderSeq) return;
        if (typeof html === "string") view.innerHTML = html;
        setStatus("Connected", "ok");
      } catch (err) {
        if (seq !== renderSeq) return;
        view.innerHTML = '<div class="card"><div class="error-box">' + icon("alert") + " " + esc(err.message).replace(/\n/g, "<br>") + "</div></div>";
        setStatus(err.message.split("\n")[0], "err");
      }
      return;
    }
    location.hash = "#/overview";
  }
  function setQuery(obj) {
    const base = location.hash.split("?")[0];
    const q = new URLSearchParams(Object.entries(obj).filter(([, v]) => v !== undefined && v !== null && v !== "")).toString();
    location.hash = base + (q ? "?" + q : "");
  }

  // ---- auth gate ----------------------------------------------------------------------
  function onUnauthorized() { setKey(""); showGate("Your session or API key was rejected. Sign in again."); }
  function showGate(msg) {
    $("#app").style.display = "none";
    $("#gate").style.display = "";
    $("#gate-error").textContent = msg || "";
    $("#key").value = "";
    $("#key").focus();
  }
  async function showApp() {
    $("#gate").style.display = "none";
    $("#app").style.display = "";
    try {
      const org = await api.get("/api/v1/organizations");
      const o = org.organization || {};
      $("#org-name").textContent = o.name ? o.name : "";
    } catch (_) { /* rendered views surface errors */ }
    render();
  }

  function boot() {
    $("#gate-title").innerHTML = icon("logo") + " Rago Billing";
    $("#brand").innerHTML = icon("logo") + " Rago Billing";
    $("#logout").innerHTML = icon("logout") + " Sign out";
    renderNav();
    $("#gate-form").onsubmit = (e) => {
      e.preventDefault();
      const k = $("#key").value.trim();
      if (!k) { $("#gate-error").textContent = "Enter an API key."; return; }
      setKey(k);
      showApp();
    };
    $("#logout").onclick = () => { setKey(""); showGate(""); };
    window.addEventListener("hashchange", render);
    if (getKey()) showApp(); else showGate("");
  }

  window.Rago = {
    $, $$, icon, esc, money, date, datetime, badge, centsFromInput, api, enc, toast,
    openForm, confirmAction, showJson, closeOverlay, table, bindRowLinks, pager, bindPager,
    kv, btn, bindActions, bars, route, navItem, render, query, setQuery, boot, getKey, setKey,
  };
})();
