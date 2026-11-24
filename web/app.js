"use strict";
/* tgvault admin panel. No framework, no innerHTML with data: every user-controlled
   string goes through text nodes, so object keys cannot inject markup. */

// ---------- tiny DOM helpers ----------
function h(tag, attrs, ...kids) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v === null || v === undefined || v === false) continue;
    if (k === "class") el.className = v;
    else if (k === "style") for (const [p, val] of Object.entries(v)) el.style.setProperty(p, val);
    else if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else if (k === "value") el.value = v;
    else if (v === true) el.setAttribute(k, "");
    else el.setAttribute(k, String(v));
  }
  for (const kid of kids.flat(Infinity)) {
    if (kid === null || kid === undefined || kid === false) continue;
    el.append(kid instanceof Node ? kid : document.createTextNode(String(kid)));
  }
  return el;
}
const NS = "http://www.w3.org/2000/svg";
function icon(name, size = 18) {
  const s = document.createElementNS(NS, "svg");
  for (const [k, v] of Object.entries({ viewBox: "0 0 24 24", width: size, height: size, fill: "none", stroke: "currentColor", "stroke-width": "1.8", "stroke-linecap": "round", "stroke-linejoin": "round", "aria-hidden": "true" })) s.setAttribute(k, v);
  for (const d of ICON[name] || ICON.file) { const p = document.createElementNS(NS, "path"); p.setAttribute("d", d); s.append(p); }
  return s;
}
const ICON = {
  overview: ["M4 4h7v7H4z", "M13 4h7v4h-7z", "M13 10h7v10h-7z", "M4 13h7v7H4z"],
  files: ["M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"],
  links: ["M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1", "M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1"],
  keys: ["M15 7a4 4 0 1 1-3.9 4.9L4 19v2h3v-2h2v-2h2l1.1-1.1A4 4 0 0 1 15 7z", "M16 9h.01"],
  activity: ["M3 12h4l3-8 4 16 3-8h4"],
  maintenance: ["M14.7 6.3a4 4 0 0 0-5.4 5.4L3 18l3 3 6.3-6.3a4 4 0 0 0 5.4-5.4l-2.6 2.6-2.4-.6-.6-2.4z"],
  folder: ["M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"],
  file: ["M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z", "M14 3v5h5"],
  video: ["M4 6h11a1 1 0 0 1 1 1v10a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1z", "M16 10l5-3v10l-5-3"],
  image: ["M4 5h16v14H4z", "M4 16l5-5 4 4 3-3 4 4", "M15 9h.01"],
  audio: ["M9 18V6l10-2v12", "M9 18a3 3 0 1 1-3-3 3 3 0 0 1 3 3z", "M19 16a3 3 0 1 1-3-3 3 3 0 0 1 3 3z"],
  upload: ["M12 16V4", "M7 9l5-5 5 5", "M4 20h16"],
  download: ["M12 4v12", "M7 11l5 5 5-5", "M4 20h16"],
  search: ["M11 18a7 7 0 1 1 0-14 7 7 0 0 1 0 14z", "M20 20l-4-4"],
  settings: ["M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6z", "M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z"],
  share: ["M18 8a3 3 0 1 0-3-3", "M6 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6z", "M18 22a3 3 0 1 0 0-6 3 3 0 0 0 0 6z", "M8.6 13.5l6.8 4", "M15.4 6.5l-6.8 4"],
  trash: ["M4 7h16", "M10 11v6", "M14 11v6", "M6 7l1 13h10l1-13", "M9 7V4h6v3"],
  plus: ["M12 5v14", "M5 12h14"],
  x: ["M6 6l12 12", "M18 6L6 18"],
  moon: ["M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z"],
  logout: ["M15 4h4a1 1 0 0 1 1 1v14a1 1 0 0 1-1 1h-4", "M10 16l-4-4 4-4", "M6 12h11"],
  book: ["M4 5a2 2 0 0 1 2-2h13v16H6a2 2 0 0 0-2 2z", "M4 19V5", "M8 7h7"],
  copy: ["M9 9h11v11H9z", "M5 15V4h11"],
  lock: ["M6 11h12v9H6z", "M8 11V8a4 4 0 0 1 8 0v3"],
  globe: ["M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18z", "M3 12h18", "M12 3a14 14 0 0 1 0 18", "M12 3a14 14 0 0 0 0 18"],
};
function brandMark(size = 24) {
  const s = document.createElementNS(NS, "svg");
  s.setAttribute("viewBox", "0 0 24 24"); s.setAttribute("width", size); s.setAttribute("height", size); s.setAttribute("aria-hidden", "true");
  // a 3x3 chunk map: the product in one glyph
  const cells = [["0", "var(--b0)"], ["1", "var(--b1)"], ["2", "var(--b0)"], ["3", "var(--b2)"], ["4", "var(--ink)"], ["5", "var(--b0)"], ["6", "var(--b1)"], ["7", "var(--b2)"], ["8", "var(--ink)"]];
  for (const [i, c] of cells) {
    const r = document.createElementNS(NS, "rect");
    const n = Number(i);
    r.setAttribute("x", 2 + (n % 3) * 7); r.setAttribute("y", 2 + Math.floor(n / 3) * 7); r.setAttribute("width", 6); r.setAttribute("height", 6); r.setAttribute("rx", 1.4);
    r.style.fill = c; s.append(r);
  }
  return s;
}

// ---------- formatting ----------
const bytes = (n) => {
  if (n === null || n === undefined) return "—";
  const u = ["B", "KB", "MB", "GB", "TB", "PB"]; let i = 0; let v = Number(n);
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return i === 0 ? `${v} B` : `${v < 10 ? v.toFixed(1) : Math.round(v)} ${u[i]}`;
};
const plural = (n, one, many) => `${Number(n).toLocaleString()} ${n === 1 ? one : many}`;
const when = (ts) => {
  if (!ts) return "—";
  const d = Date.now() / 1000 - ts; const a = Math.abs(d);
  if (a < 60) return "just now";
  const f = a < 3600 ? `${Math.round(a / 60)} min` : a < 86400 ? `${Math.round(a / 3600)} h` : a < 86400 * 30 ? `${Math.round(a / 86400)} days` : null;
  if (!f) return new Date(ts * 1000).toLocaleDateString();
  return d < 0 ? `in ${f}` : `${f} ago`;
};
const fullDate = (ts) => (ts ? new Date(ts * 1000).toLocaleString() : "—");
const encKey = (key) => key.split("/").map(encodeURIComponent).join("/");
const kindOf = (ct) => (ct || "").startsWith("video/") ? "video" : (ct || "").startsWith("audio/") ? "audio" : (ct || "").startsWith("image/") && ct !== "image/svg+xml" ? "image" : "file";
const chip = (kind, size = 16) => h("span", { class: `chip ${kind}` }, icon(kind, size));

// ---------- chunk map (the signature element) ----------
const MAX_CELLS = 480;
function chunkMap(groups, { animate = true, cls = "" } = {}) {
  // groups: [{ color, count, title }] -> squares, scaled down when there are many chunks
  const total = groups.reduce((a, g) => a + g.count, 0);
  const per = Math.max(1, Math.ceil(total / MAX_CELLS));
  const size = total <= 48 ? "lg" : total <= 160 ? "md" : "";
  const el = h("div", { class: `field ${size} ${animate ? "animate" : ""} ${cls}`, role: "img", "aria-label": `${plural(total, "chunk", "chunks")}${per > 1 ? `, each square is ${per} chunks` : ""}` });
  let i = 0;
  for (const g of groups) {
    const n = Math.max(g.count ? 1 : 0, Math.round(g.count / per));
    for (let k = 0; k < n; k++) el.append(h("i", { title: g.title || null, style: { "--c": g.color, "--i": Math.min(i++, 300) } }));
  }
  return { el, per };
}
function decorativeField(n, palette) {
  const el = h("div", { class: "field animate", "aria-hidden": "true" });
  let seed = 7;
  const rnd = () => ((seed = (seed * 16807) % 2147483647) / 2147483647);
  for (let i = 0; i < n; i++) {
    const r = rnd();
    const c = r < 0.08 ? "transparent" : palette[Math.floor(rnd() * palette.length)];
    el.append(h("i", { style: { "--c": c, "--i": i } }));
  }
  return el;
}

// ---------- API client ----------
const api = {
  csrf: null,
  async req(method, path, body, extra = {}) {
    const headers = { ...(extra.headers || {}) };
    if (method !== "GET" && method !== "HEAD" && this.csrf) headers["x-csrf-token"] = this.csrf;
    let payload = body;
    if (body !== undefined && !(body instanceof Blob) && typeof body !== "string") { headers["content-type"] = "application/json"; payload = JSON.stringify(body); }
    const res = await fetch(path, { method, headers, body: payload, credentials: "same-origin" });
    if (res.status === 204) return null;
    const isJson = (res.headers.get("content-type") || "").includes("json");
    const data = isJson ? await res.json() : await res.text();
    if (!res.ok) {
      if (res.status === 401 && !extra.quiet) showLogin();
      const msg = (data && data.error && data.error.message) || `Request failed with status ${res.status}`;
      throw Object.assign(new Error(msg), { status: res.status });
    }
    return data;
  },
  get(p, o) { return this.req("GET", p, undefined, o); },
  post(p, b) { return this.req("POST", p, b === undefined ? {} : b); },
  patch(p, b) { return this.req("PATCH", p, b); },
  del(p) { return this.req("DELETE", p); },
};

// ---------- toasts & dialogs ----------
function toast(msg, isErr = false) {
  const t = h("div", { class: "toast" + (isErr ? " err" : ""), role: isErr ? "alert" : "status" }, msg);
  document.body.append(t);
  setTimeout(() => t.remove(), isErr ? 6000 : 2600);
}
const fail = (e) => toast(e.message || String(e), true);
function dialog(title, body, actions) {
  const d = h("dialog", { "aria-labelledby": "dlg-title" }, h("h2", { id: "dlg-title" }, title), body, h("div", { class: "actions" }, actions));
  d.addEventListener("close", () => d.remove());
  document.body.append(d);
  d.showModal();
  return d;
}
function confirmDialog(title, text, verb, danger = true) {
  return new Promise((resolve) => {
    let ok = false;
    const d = dialog(title, h("p", { class: "muted" }, text), [
      h("button", { class: "btn", onclick: () => d.close() }, "Cancel"),
      h("button", { class: "btn " + (danger ? "danger" : "primary"), onclick: () => { ok = true; d.close(); } }, verb),
    ]);
    d.addEventListener("close", () => resolve(ok));
  });
}
async function copy(text, what = "Link") {
  try { await navigator.clipboard.writeText(text); toast(`${what} copied`); } catch { toast("Copy failed: select the text and copy it manually", true); }
}

// ---------- app state ----------
const S = { me: null, stats: null, buckets: [], bucket: null, prefix: "", mode: "files", filter: "", selected: null, botColors: new Map(), uploads: [] };
const root = document.getElementById("root");
const NAV = [["overview", "Overview"], ["files", "Files"], ["links", "Share links"], ["keys", "API keys"], ["activity", "Activity"], ["maintenance", "Maintenance"]];
function botColor(id) {
  if (id === null || id === undefined) return "var(--b0)";
  if (!S.botColors.has(id)) S.botColors.set(id, `var(--b${S.botColors.size % 7})`);
  return S.botColors.get(id);
}

// ---------- login ----------
function showLogin(errText) {
  api.csrf = null;
  const pw = h("input", { type: "password", id: "pw", autocomplete: "current-password", required: true });
  const err = h("p", { class: "err-line", role: "alert", hidden: !errText }, errText || "");
  const btn = h("button", { class: "btn primary", type: "submit" }, "Sign in");
  const form = h("form", {
    onsubmit: async (ev) => {
      ev.preventDefault(); btn.disabled = true; err.hidden = true;
      try {
        const r = await api.req("POST", "/api/v1/auth/login", { password: pw.value }, { quiet: true });
        api.csrf = r.csrf; await boot();
      } catch (e) {
        err.textContent = e.status === 401 ? "That password is not right." : e.message; err.hidden = false; btn.disabled = false; pw.select();
      }
    },
  },
    h("label", { class: "field", for: "pw" }, "Admin password", pw), err, btn);
  const side = h("div", { class: "side" },
    h("a", { class: "brand", href: "/", style: { padding: "0" } }, brandMark(28), "tgvault"),
    h("h1", {}, "Sign in to your vault"),
    h("p", { style: { margin: "8px 0 26px" } }, "Use the admin password set in TGV_ADMIN_PASSWORD."),
    form,
    h("p", { class: "fine" }, "Building an app? ", h("a", { href: "/docs" }, "Read the API reference"), "."));
  const art = h("div", { class: "art" },
    decorativeField(260, ["var(--b0)", "var(--b0)", "var(--b1)", "var(--b2)", "var(--b5)", "#2b3530", "#2b3530", "#2b3530"]),
    h("blockquote", {}, h("b", {}, "Every square is one piece of a file."), " Pieces are sealed with AES-256-GCM on this server, then posted to your private Telegram channel by whichever bot is free."));
  root.replaceChildren(h("div", { class: "login" }, side, art));
  pw.focus();
}

// ---------- shell & router ----------
function shell(active, content) {
  const nav = h("nav", { "aria-label": "Sections" }, NAV.map(([id, label]) => h("a", { href: `#/${id}`, "aria-current": id === active ? "page" : null }, icon(id), h("span", {}, label))));
  const meter = h("div", { class: "meter", id: "meter" });
  const foot = h("div", { class: "foot" },
    h("a", { class: "iconbtn", href: "/docs", title: "API reference", "aria-label": "API reference" }, icon("book")),
    h("button", { class: "iconbtn", title: "Switch theme", "aria-label": "Switch theme", onclick: toggleTheme }, icon("moon")),
    h("button", { class: "iconbtn", title: "Sign out", "aria-label": "Sign out", onclick: async () => { try { await api.post("/api/v1/auth/logout"); } catch {} showLogin(); } }, icon("logout")));
  const rail = h("aside", { class: "rail" }, h("a", { class: "brand", href: "#/overview" }, brandMark(), "tgvault", h("small", {}, "v0.1")), nav, meter, foot);
  root.replaceChildren(h("div", { class: "shell" }, rail, h("main", { class: "view", id: "view" }, content)));
  drawMeter();
}
function drawMeter() {
  const m = document.getElementById("meter"); const st = S.stats; if (!m || !st) return;
  const groups = (st.chunk_bots || []).map((b) => ({ color: botColor(b.bot), count: b.chunks }));
  const total = groups.reduce((a, g) => a + g.count, 0);
  const per = Math.max(1, Math.ceil(total / 60));
  const mini = h("div", { class: "mini-field", "aria-hidden": "true" });
  for (const g of groups) for (let k = 0; k < Math.max(1, Math.round(g.count / per)); k++) mini.append(h("i", { style: { "--c": g.color } }));
  m.replaceChildren(h("b", {}, bytes(st.totals.logical_bytes)), h("span", { class: "faint" }, `stored in ${plural(st.totals.chunks, "chunk", "chunks")}`), total ? mini : null);
}
function toggleTheme() {
  const cur = document.documentElement.dataset.theme || (matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light");
  const next = cur === "dark" ? "light" : "dark";
  document.documentElement.dataset.theme = next;
  try { localStorage.setItem("tgv-theme", next); } catch {}
}
try { const t = localStorage.getItem("tgv-theme"); if (t) document.documentElement.dataset.theme = t; } catch {}

const $view = () => document.getElementById("view");
async function refreshStats() { try { S.stats = await api.get("/api/v1/admin/stats"); (S.stats.backend.bots || []).forEach((b) => botColor(b.id)); } catch {} }
async function route() {
  if (!api.csrf) return;
  const [page, ...rest] = location.hash.replace(/^#\/?/, "").split("/");
  const pages = { overview: pageOverview, files: pageFiles, links: pageLinks, keys: pageKeys, activity: pageActivity, maintenance: pageMaintenance };
  const fn = pages[page] || pageOverview;
  shell(pages[page] ? page : "overview", h("p", { class: "muted" }, "Loading…"));
  try { await fn(rest.map(decodeURIComponent)); } catch (e) { if (e.status !== 401) $view().replaceChildren(h("div", { class: "empty-state" }, h("strong", {}, "This page could not load"), h("p", {}, e.message))); }
}
window.addEventListener("hashchange", route);
async function boot() {
  try {
    S.me = await api.get("/api/v1/auth/me", { quiet: true });
    api.csrf = S.me.csrf;
    if (!api.csrf) throw new Error("no session");
  } catch { return showLogin(); }
  await refreshStats();
  await route();
}

// ---------- overview ----------
const ACTION_COLOR = { "object.put": "var(--b0)", "import.done": "var(--b0)", "import.failed": "var(--seal)", "object.put_presigned": "var(--b0)", "upload.complete": "var(--b0)", "object.trash": "var(--b6)", "object.delete": "var(--b6)", "trash.empty": "var(--b6)",
  "share.create": "var(--b2)", "presign": "var(--b5)", "key.create": "var(--b4)", "key.revoke": "var(--b6)", "login": "var(--b3)", "login.failed": "var(--seal)", "share.unlock_failed": "var(--seal)" };
const ACTION_TEXT = { "object.put": "Uploaded", "object.put_presigned": "Uploaded by link", "upload.complete": "Uploaded", "upload.init": "Started upload", "object.trash": "Moved to trash", "object.delete": "Deleted", "object.restore": "Restored",
  "object.copy": "Copied", "object.link_hash": "Linked by hash", "trash.empty": "Emptied trash of", "share.create": "Shared", "share.revoke": "Turned off link", "presign": "Signed link for", "key.create": "Created key", "key.revoke": "Revoked key",
  "bucket.create": "Created bucket", "bucket.update": "Changed settings of", "bucket.delete": "Deleted bucket", "login": "Signed in", "login.failed": "Failed sign-in", "share.unlock_failed": "Wrong share password",
  "import.start": "Started import of", "import.done": "Imported", "import.failed": "Import failed for", "backup.create": "Backed up", "gc.run": "Cleaned up", "keys.rewrap": "Re-wrapped keys", "upload.abort": "Cancelled upload" };

async function pageOverview() {
  const [st, au] = await Promise.all([api.get("/api/v1/admin/stats"), api.get("/api/v1/admin/audit?limit=8").catch(() => ({ events: [] }))]);
  S.stats = st; drawMeter();
  const t = st.totals; const be = st.backend;
  (be.bots || []).forEach((b) => botColor(b.id));
  const isTg = be.kind === "telegram";
  const where = isTg ? plural(be.bots.length, "Telegram bot", "Telegram bots") : "this machine's disk";

  const statement = t.objects
    ? h("h1", { class: "statement" }, bytes(t.logical_bytes), " in ", plural(t.objects, "file", "files"), ", sealed into ", h("em", {}, plural(t.chunks, "chunk", "chunks")), ` across ${where}.`)
    : h("h1", { class: "statement" }, "Your vault is empty. Create a bucket and upload the first file.");
  const sub = h("p", { class: "statement-sub" }, `Each chunk is up to ${bytes(st.chunk_size)} and encrypted with its own file key before it leaves this server.`);

  const groups = (st.chunk_bots || []).map((b) => ({ color: botColor(b.bot), count: b.chunks, title: b.bot ? `bot ${b.bot}: ${plural(b.chunks, "chunk", "chunks")}` : `${plural(b.chunks, "chunk", "chunks")}` }));
  const map = groups.length ? chunkMap(groups) : { el: decorativeField(120, ["var(--rule-soft)"]), per: 1 };
  const botName = (id) => { const b = (be.bots || []).find((x) => x.id === id); return b && b.username ? `@${b.username}` : id ? `bot ${id}` : "local disk"; };
  const legend = h("div", { class: "legend" }, (st.chunk_bots || []).map((b) => h("div", { class: "item" }, h("span", { class: "sw", style: { "--c": botColor(b.bot) } }), h("span", { class: "name" }, botName(b.bot)), h("span", { class: "v" }, bytes(b.bytes)))));
  const mapPanel = h("section", { class: "panel span" },
    h("header", {}, h("h2", {}, "Chunk map"), h("span", { class: "muted" }, map.per > 1 ? `Each square is ${map.per} chunks` : "Each square is one chunk")),
    h("div", { class: "mapgrid" }, map.el, groups.length ? legend : h("p", { class: "muted", style: { margin: 0 } }, "Chunks appear here as you upload.")));

  const maxB = Math.max(1, ...st.buckets.map((b) => b.bytes));
  const bucketsPanel = h("section", { class: "panel" },
    h("header", {}, h("h2", {}, "Buckets"), h("a", { href: "#/files" }, "Open files")),
    st.buckets.length ? h("div", { class: "bars" }, st.buckets.map((b, i) => h("div", { class: "bar-row" },
      h("a", { href: `#/files/${encodeURIComponent(b.name)}` }, b.name),
      h("span", { class: "v" }, `${bytes(b.bytes)}, ${plural(b.objects, "file", "files")}`),
      h("div", { class: "track" }, h("i", { style: { "--p": `${(b.bytes / maxB) * 100}%`, "--c": `var(--b${i % 7})` } })))))
      : h("p", { class: "muted", style: { margin: 0 } }, "No buckets yet."));

  const feed = h("section", { class: "panel" },
    h("header", {}, h("h2", {}, "Recent activity"), h("a", { href: "#/activity" }, "See all")),
    au.events.length ? h("ul", { class: "feed" }, au.events.map((e) => h("li", {},
      h("span", { class: "dot", style: { "--c": ACTION_COLOR[e.action] || "var(--ink-3)" } }),
      h("span", { class: "what" }, `${ACTION_TEXT[e.action] || e.action} `, h("span", {}, e.target)),
      h("time", { datetime: new Date(e.ts * 1000).toISOString(), title: fullDate(e.ts) }, when(e.ts)))))
      : h("p", { class: "muted", style: { margin: 0 } }, "Nothing has happened yet."));

  const hit = st.cache.hits + st.cache.misses ? Math.round((st.cache.hits / (st.cache.hits + st.cache.misses)) * 100) : null;
  const facts = h("section", { class: "panel" }, h("header", {}, h("h2", {}, "Health")), h("dl", { class: "facts" },
    fact("Cache hit rate", hit === null ? "—" : `${hit}%`, `${bytes(st.cache.bytes)} of ${bytes(st.cache.max_bytes)} used`),
    fact("Last backup", t.last_backup_at ? when(t.last_backup_at) : "Never", t.last_backup_at ? "Sealed and pinned" : "Run one under Maintenance"),
    fact("In the trash", plural(t.trashed, "file", "files"), "Restorable for a while"),
    fact("Waiting to delete", plural(t.pending_chunk_deletes, "chunk", "chunks"), "Removed by the next clean-up"),
    fact("Encryption key", `Version ${st.active_key}`, `${plural(st.key_versions.length, "version", "versions")} configured`)));

  const bots = isTg ? h("section", { class: "panel flush span" }, h("header", {}, h("h2", {}, "Bots")), botTable(be.bots)) : null;
  $view().replaceChildren(statement, sub, h("div", { class: "ov" }, mapPanel, h("div", { class: "stack" }, bucketsPanel, facts), feed, bots));
}
const fact = (k, v, sub) => h("div", {}, h("dt", {}, k), h("dd", {}, v, sub ? h("small", {}, sub) : null));
function botTable(bots) {
  return h("div", { class: "table-wrap" }, h("table", {},
    h("thead", {}, h("tr", {}, h("th", {}, "Bot"), h("th", {}, "Status"), h("th", { class: "num" }, "Uploads"), h("th", { class: "num" }, "Downloads"), h("th", { class: "num" }, "Errors"), h("th", { class: "hide-sm" }, "Last error"))),
    h("tbody", {}, bots.map((b) => h("tr", {},
      h("td", {}, h("span", { class: "name" }, h("span", { class: "swatch", style: { "--c": botColor(b.id) } }), b.username ? `@${b.username}` : String(b.id))),
      h("td", {}, b.cooldown_ms > 0 ? h("span", { class: "tag warn" }, `Cooling down ${Math.ceil(b.cooldown_ms / 1000)} s`) : h("span", { class: "tag ok" }, "Ready")),
      h("td", { class: "num" }, b.uploads), h("td", { class: "num" }, b.downloads), h("td", { class: "num" }, b.errors),
      h("td", { class: "hide-sm muted" }, b.last_error || "—"))))));
}

// ---------- files ----------
async function loadBuckets() { S.buckets = (await api.get("/api/v1/buckets")).buckets; }

async function pageFiles(args) {
  await loadBuckets();
  if (args[0] && S.buckets.some((b) => b.name === args[0])) { if (S.bucket !== args[0]) { S.prefix = ""; S.selected = null; S.filter = ""; } S.bucket = args[0]; }
  if (!S.bucket || !S.buckets.some((b) => b.name === S.bucket)) S.bucket = S.buckets[0] ? S.buckets[0].name : null;
  const head = h("div", { class: "head" },
    h("div", {}, h("h1", {}, "Files"), h("p", { class: "lede" }, "Drop files anywhere on this page to upload them to the open folder. Everything is encrypted before it leaves the server.")),
    S.bucket && h("div", { class: "row" },
      h("button", { class: "btn", onclick: importDialog }, icon("download", 16), "Import from URL"),
      h("button", { class: "btn primary", onclick: pickFiles }, icon("upload", 16), "Upload files")));
  if (!S.bucket) {
    $view().replaceChildren(head, h("div", { class: "panel" }, h("div", { class: "empty-state" },
      decorativeField(60, ["var(--rule)", "var(--rule-soft)", "var(--b0)"]),
      h("strong", {}, "Create a bucket to start"),
      h("p", {}, "A bucket groups files and carries its own settings: public access, websites allowed to embed, quota and automatic expiry."),
      h("button", { class: "btn primary", onclick: newBucketDialog }, icon("plus", 16), "Create bucket"))));
    return;
  }
  const tabs = h("div", { class: "tabs", role: "tablist", "aria-label": "Buckets" },
    S.buckets.map((b) => h("button", { role: "tab", "aria-selected": String(b.name === S.bucket), onclick: () => { location.hash = `#/files/${encodeURIComponent(b.name)}`; } },
      b.public ? icon("globe", 14) : icon("lock", 14), b.name, h("span", { class: "count" }, bytes(b.bytes)))),
    h("button", { class: "add", onclick: newBucketDialog }, icon("plus", 14), "New bucket"));
  $view().replaceChildren(head, tabs, h("div", { id: "files" }));
  await renderFiles();
}

async function renderFiles() {
  const box = document.getElementById("files"); if (!box) return;
  const b = encodeURIComponent(S.bucket);
  const seg = h("div", { class: "seg", role: "group", "aria-label": "View" },
    h("button", { "aria-pressed": String(S.mode === "files"), onclick: () => { S.mode = "files"; S.selected = null; renderFiles(); } }, "Files"),
    h("button", { "aria-pressed": String(S.mode === "trash"), onclick: () => { S.mode = "trash"; S.selected = null; renderFiles(); } }, "Trash"));
  const settings = h("button", { class: "iconbtn bordered", title: "Bucket settings", "aria-label": "Bucket settings", onclick: bucketSettingsDialog }, icon("settings", 17));

  if (S.mode === "trash") {
    const r = await api.get(`/api/v1/buckets/${b}/trash`);
    const body = r.objects.length ? h("div", { class: "table-wrap" }, h("table", {},
      h("thead", {}, h("tr", {}, h("th", {}, "Name"), h("th", { class: "num" }, "Size"), h("th", {}, "Deleted"), h("th", {}))),
      h("tbody", {}, r.objects.map((o) => h("tr", {},
        h("td", {}, h("span", { class: "name" }, chip(kindOf(o.content_type)), h("span", { class: "label" }, o.key))),
        h("td", { class: "num" }, bytes(o.size)), h("td", { title: fullDate(o.deleted_at) }, when(o.deleted_at)),
        h("td", { class: "num" }, h("button", { class: "btn small", onclick: async () => { try { await api.post(`/api/v1/objects/${o.id}/restore`); toast("Restored"); renderFiles(); } catch (e) { fail(e); } } }, "Restore")))))))
      : h("div", { class: "empty-state" }, h("strong", {}, "The trash is empty"), h("p", {}, `Deleted and overwritten files stay here for ${r.purge_after_days} days before they are removed from storage.`));
    const empty = r.objects.length ? h("button", { class: "btn danger small", onclick: async () => {
      if (!(await confirmDialog("Empty the trash?", `${plural(r.objects.length, "file", "files")} in ${S.bucket} will be deleted for good. This cannot be undone.`, "Empty trash"))) return;
      try { const x = await api.del(`/api/v1/buckets/${b}/trash`); toast(`Deleted ${x ? plural(x.deleted, "file", "files") : "files"}`); renderFiles(); } catch (e) { fail(e); }
    } }, icon("trash", 14), "Empty trash") : null;
    box.replaceChildren(h("div", { class: "toolbar" }, seg, h("span", { class: "spacer" }), empty, settings), h("div", { class: "panel flush" }, body));
    return;
  }

  const r = await api.get(`/api/v1/buckets/${b}/objects?delimiter=/&limit=1000&prefix=${encodeURIComponent(S.prefix)}`);
  const crumbs = h("nav", { class: "crumbs", "aria-label": "Folder path" }, h("button", { onclick: () => openPrefix("") }, S.bucket));
  let acc = "";
  for (const part of S.prefix.split("/").filter(Boolean)) { acc += part + "/"; const p = acc; crumbs.append(h("span", { class: "sep" }, "/"), h("button", { onclick: () => openPrefix(p) }, part)); }
  const filterInput = h("input", { type: "search", placeholder: "Filter this folder", value: S.filter, "aria-label": "Filter files in this folder", oninput: (e) => { S.filter = e.target.value; drawRows(); } });
  const tbody = h("tbody", {});
  const tableBox = h("div", {});
  function drawRows() {
    const q = S.filter.trim().toLowerCase();
    const folders = r.common_prefixes.filter((p) => !q || p.slice(S.prefix.length).toLowerCase().includes(q));
    const objs = r.objects.filter((o) => !q || o.key.slice(S.prefix.length).toLowerCase().includes(q));
    if (!folders.length && !objs.length) {
      tableBox.replaceChildren(h("div", { class: "empty-state" },
        q ? [h("strong", {}, "Nothing matches"), h("p", {}, `No file or folder here contains “${S.filter}”.`)]
          : [decorativeField(40, ["var(--rule)", "var(--rule-soft)"]), h("strong", {}, S.prefix ? "This folder is empty" : "No files in this bucket yet"), h("p", {}, "Drag files onto the page or use Upload files. A key that contains '/' creates folders."),
            h("button", { class: "btn primary", onclick: pickFiles }, icon("upload", 16), "Upload files")]));
      return;
    }
    tbody.replaceChildren(
      ...folders.map((p) => h("tr", { class: "clickable", tabindex: 0, onclick: () => openPrefix(p), onkeydown: (e) => e.key === "Enter" && openPrefix(p) },
        h("td", {}, h("span", { class: "name" }, chip("folder"), h("span", { class: "label" }, p.slice(S.prefix.length).replace(/\/$/, "")))), h("td", { class: "num faint" }, "—"), h("td", { class: "hide-sm faint" }, "Folder"), h("td", { class: "faint" }, ""))),
      ...objs.map((o) => h("tr", { class: "clickable" + (S.selected && S.selected.id === o.id ? " selected" : ""), tabindex: 0, onclick: () => selectObject(o), onkeydown: (e) => e.key === "Enter" && selectObject(o) },
        h("td", {}, h("span", { class: "name" }, chip(kindOf(o.content_type)), h("span", { class: "label" }, o.key.slice(S.prefix.length)))),
        h("td", { class: "num" }, bytes(o.size)), h("td", { class: "hide-sm muted" }, o.content_type), h("td", { class: "muted", title: fullDate(o.created_at) }, when(o.created_at)))));
    tableBox.replaceChildren(h("div", { class: "table-wrap" }, h("table", {}, h("thead", {}, h("tr", {}, h("th", {}, "Name"), h("th", { class: "num" }, "Size"), h("th", { class: "hide-sm" }, "Type"), h("th", {}, "Added"))), tbody)));
  }
  drawRows();
  const toolbar = h("div", { class: "toolbar" }, crumbs, h("span", { class: "spacer" }), h("div", { class: "search" }, icon("search", 15), filterInput), seg, settings);
  const layout = h("div", { class: "files-layout" + (S.selected ? " with-detail" : "") }, h("div", { class: "panel flush" }, tableBox), S.selected ? h("aside", { class: "panel detail", id: "detail", "aria-label": "File details" }) : null);
  box.replaceChildren(toolbar, layout);
  if (S.selected) renderDetail();
}
function openPrefix(p) { S.prefix = p; S.selected = null; S.filter = ""; renderFiles(); }
function selectObject(o) { S.selected = o; renderFiles(); }

async function renderDetail() {
  const o = S.selected; const box = document.getElementById("detail"); if (!box) return;
  const src = `/api/v1/buckets/${encodeURIComponent(o.bucket)}/objects/${encKey(o.key)}`;
  const kind = kindOf(o.content_type);
  const media = kind === "video" ? h("video", { src, controls: true, preload: "metadata", playsinline: true })
    : kind === "audio" ? h("audio", { src, controls: true, preload: "metadata" })
    : kind === "image" ? h("img", { src, alt: o.key })
    : chip(kind, 28);
  const close = h("button", { class: "iconbtn close", "aria-label": "Close details", onclick: () => { S.selected = null; renderFiles(); } }, icon("x", 16));
  const mapBox = h("div", {}, h("p", {}, "Loading chunk map…"));
  const linkOut = h("div", {});
  const ttl = h("select", { "aria-label": "Link lifetime" }, [["3600", "1 hour"], ["86400", "1 day"], ["604800", "7 days"]].map(([v, l]) => h("option", { value: v }, l)));
  const meta = Object.entries(o.meta || {});
  box.replaceChildren(
    h("div", { class: "stage" }, media, close),
    h("div", { class: "body" },
      h("h3", {}, o.key.split("/").pop()), h("div", { class: "keyline" }, `${o.bucket}/${o.key}`),
      h("div", { class: "actions" },
        h("a", { class: "btn small", href: src + "?download=1" }, icon("download", 15), "Download"),
        h("button", { class: "btn small", onclick: () => shareDialog(o) }, icon("share", 15), "Share"),
        h("button", { class: "btn small danger", "aria-label": "Move to trash", title: "Move to trash", onclick: () => deleteObject(o) }, icon("trash", 15))),
      h("dl", {},
        h("dt", {}, "Size"), h("dd", {}, `${bytes(o.size)} (${o.size.toLocaleString()} bytes)`),
        h("dt", {}, "Type"), h("dd", {}, o.content_type),
        h("dt", {}, "Added"), h("dd", {}, fullDate(o.created_at)),
        h("dt", {}, "Expires"), h("dd", {}, o.expires_at ? fullDate(o.expires_at) : "Never"),
        h("dt", {}, "ETag"), h("dd", { class: "mono" }, o.etag),
        ...meta.flatMap(([k, v]) => [h("dt", {}, k), h("dd", {}, String(v))])),
      h("section", {}, h("h4", {}, "Where the bytes live", h("span", { id: "chunkcount" }, "")), mapBox),
      h("section", {}, h("h4", {}, "Temporary link"),
        h("p", {}, "A signed URL that works without a key, for example as the src of a <video> tag on another website."),
        h("div", { class: "row" }, ttl, h("button", { class: "btn small", onclick: async () => {
          try {
            const r = await api.post("/api/v1/presign", { bucket: o.bucket, key: o.key, expires_in: Number(ttl.value) });
            const inp = h("input", { type: "text", readonly: true, value: r.url, "aria-label": "Signed URL", onfocus: (e) => e.target.select() });
            linkOut.replaceChildren(h("div", { class: "copybox" }, inp, h("button", { class: "btn small", onclick: () => copy(r.url) }, icon("copy", 14), "Copy")));
          } catch (e) { fail(e); }
        } }, "Create link")), linkOut)));
  try {
    const info = await api.get(`/api/v1/objects/${o.id}`);
    const chunks = info.chunks || [];
    const field = h("div", { class: "field animate", role: "img", "aria-label": `${plural(chunks.length, "encrypted chunk", "encrypted chunks")}` },
      chunks.map((c, i) => h("i", { title: `Chunk ${c.index + 1}: ${bytes(c.size)}${c.bot ? `, held by bot ${c.bot}` : ""}`, style: { "--c": botColor(c.bot), "--i": i } })));
    const bots = [...new Set(chunks.map((c) => c.bot).filter((x) => x !== null && x !== undefined))];
    document.getElementById("chunkcount").textContent = plural(chunks.length, "chunk", "chunks");
    mapBox.replaceChildren(chunks.length ? field : h("p", {}, "This file is empty."),
      h("p", { class: "caption" }, `AES-256-GCM, ${bots.length ? `spread over ${plural(bots.length, "bot", "bots")}` : (chunks[0] && chunks[0].backend === "local" ? "stored on the local disk" : "stored in Telegram")}${info.shared_by > 1 ? `. Same bytes as ${plural(info.shared_by - 1, "other file", "other files")}, stored once.` : "."}`));
  } catch (e) { mapBox.replaceChildren(h("p", { class: "err-line" }, e.message)); }
}

async function deleteObject(o) {
  if (!(await confirmDialog("Move to trash?", `${o.key} moves to the trash. You can restore it until the trash is purged.`, "Move to trash"))) return;
  try { await api.del(`/api/v1/buckets/${encodeURIComponent(o.bucket)}/objects/${encKey(o.key)}`); S.selected = null; toast("Moved to trash"); renderFiles(); } catch (e) { fail(e); }
}

function shareDialog(o) {
  const pw = h("input", { type: "text", placeholder: "No password", autocomplete: "off" });
  const exp = h("select", {}, [["", "Never"], ["3600", "1 hour"], ["86400", "1 day"], ["604800", "7 days"], ["2592000", "30 days"]].map(([v, l]) => h("option", { value: v, selected: v === "604800" }, l)));
  const max = h("input", { type: "number", min: 1, placeholder: "Unlimited" });
  const out = h("div", {});
  const create = h("button", { class: "btn primary", onclick: async () => {
    try {
      const body = { bucket: o.bucket, key: o.key };
      if (pw.value) body.password = pw.value;
      if (exp.value) body.expires_in = Number(exp.value);
      if (max.value) body.max_downloads = Number(max.value);
      const r = await api.post("/api/v1/shares", body);
      out.replaceChildren(h("div", { class: "secret" }, r.url), h("div", { class: "row" }, h("button", { class: "btn small", onclick: () => copy(r.url) }, icon("copy", 14), "Copy link"), h("a", { class: "btn small ghost", href: r.url, target: "_blank", rel: "noopener" }, "Open page")));
      create.disabled = true;
    } catch (e) { fail(e); }
  } }, "Create link");
  const d = dialog(`Share ${o.key.split("/").pop()}`, h("div", {},
    h("p", { class: "muted" }, "Anyone with the link can open this file. Turn it off any time under Share links."),
    h("div", { class: "formgrid", style: { "margin-top": "6px" } },
      h("label", { class: "field" }, "Password", pw), h("label", { class: "field" }, "Expires", exp), h("label", { class: "field" }, "Download limit", max)), out),
    [h("button", { class: "btn", onclick: () => d.close() }, "Close"), create]);
}

function newBucketDialog() {
  const name = h("input", { type: "text", required: true, placeholder: "media", autocomplete: "off" });
  const pub = h("input", { type: "checkbox" });
  const d = dialog("Create a bucket", h("div", { style: { display: "grid", gap: "16px" } },
    h("label", { class: "field" }, "Name", name, h("span", { class: "hint" }, "Lowercase letters, digits and dashes, 3 to 63 characters.")),
    h("div", { class: "checks" }, h("label", {}, pub, "Public: anyone can read files without a key"))),
    [h("button", { class: "btn", onclick: () => d.close() }, "Cancel"), h("button", { class: "btn primary", onclick: async () => {
      try { await api.post("/api/v1/buckets", { name: name.value.trim(), public: pub.checked }); d.close(); toast("Bucket created"); location.hash = `#/files/${encodeURIComponent(name.value.trim())}`; } catch (e) { fail(e); }
    } }, "Create bucket")]);
  name.focus();
}

function bucketSettingsDialog() {
  const b = S.buckets.find((x) => x.name === S.bucket); if (!b) return;
  const pub = h("input", { type: "checkbox", checked: b.public });
  const cors = h("textarea", { rows: 3, placeholder: "https://example.com" }, (b.cors_origins || []).join("\n"));
  const quota = h("input", { type: "number", min: 0, value: b.quota_bytes ? Math.round(b.quota_bytes / 1048576) : "", placeholder: "No limit" });
  const ttl = h("input", { type: "number", min: 0, value: b.default_ttl ? Math.round(b.default_ttl / 86400) : "", placeholder: "Keep forever" });
  const d = dialog(`Settings for ${b.name}`, h("div", { style: { display: "grid", gap: "16px" } },
    h("div", { class: "checks" }, h("label", {}, pub, `Public at /pub/${b.name}/…`)),
    h("label", { class: "field" }, "Websites allowed to embed or upload", cors, h("span", { class: "hint" }, "One origin per line, like https://example.com. Used for CORS.")),
    h("div", { style: { display: "grid", "grid-template-columns": "1fr 1fr", gap: "16px" } },
      h("label", { class: "field" }, "Quota in MB", quota), h("label", { class: "field" }, "Delete new files after (days)", ttl))),
    [h("button", { class: "btn danger", style: { "margin-right": "auto" }, onclick: async () => {
      if (!(await confirmDialog(`Delete bucket ${b.name}?`, "Every file in it, including the trash, is deleted from storage. This cannot be undone.", "Delete bucket"))) return;
      try { await api.del(`/api/v1/buckets/${encodeURIComponent(b.name)}?force=true`); d.close(); S.bucket = null; toast("Bucket deleted"); location.hash = "#/files"; route(); } catch (e) { fail(e); }
    } }, "Delete bucket"),
    h("button", { class: "btn", onclick: () => d.close() }, "Cancel"),
    h("button", { class: "btn primary", onclick: async () => {
      try {
        await api.patch(`/api/v1/buckets/${encodeURIComponent(b.name)}`, {
          public: pub.checked,
          cors_origins: cors.value.split("\n").map((s) => s.trim()).filter(Boolean),
          quota_bytes: quota.value ? Number(quota.value) * 1048576 : 0,
          default_ttl: ttl.value ? Number(ttl.value) * 86400 : 0,
        });
        d.close(); toast("Settings saved"); route();
      } catch (e) { fail(e); }
    } }, "Save settings")]);
}

// ---------- uploads (single PUT up to 64 MB, multipart above) ----------
const SINGLE_MAX = 64 * 1024 * 1024;
const PART_SIZE = 32 * 1024 * 1024;
let queueEl = null;
function pickFiles() {
  const inp = h("input", { type: "file", multiple: true, style: { display: "none" }, onchange: () => { enqueue([...inp.files]); inp.remove(); } });
  document.body.append(inp); inp.click();
}
function enqueue(files) {
  if (!S.bucket) { toast("Create a bucket first", true); return; }
  for (const f of files) S.uploads.push({ file: f, size: f.size, bucket: S.bucket, key: S.prefix + f.name, sent: 0, state: "waiting" });
  drawQueue(); pump();
}
function drawQueue() {
  if (!queueEl) { queueEl = h("div", { class: "queue", role: "status", "aria-live": "polite" }); document.body.append(queueEl); }
  const active = S.uploads.filter((u) => u.state === "waiting" || u.state === "sending").length;
  queueEl.replaceChildren(
    h("header", {}, active ? `Uploading ${plural(active, "file", "files")}` : "Uploads finished", !active && h("button", { class: "iconbtn", "aria-label": "Dismiss", onclick: () => { S.uploads = []; queueEl.remove(); queueEl = null; } }, icon("x", 16))),
    h("ul", {}, S.uploads.map((u) => {
      const pct = u.size ? Math.min(100, Math.round((u.sent / u.size) * 100)) : null;
      return h("li", { class: u.state === "done" ? "done" : u.state === "failed" ? "fail" : "" },
        h("div", { class: "row", style: { "justify-content": "space-between", "flex-wrap": "nowrap" } }, h("span", { class: "label" }, u.key), h("span", { class: "pct" }, u.state === "failed" ? u.error : u.state === "done" ? bytes(u.size || u.sent) : pct === null ? `${bytes(u.sent)} fetched` : `${pct}%`)),
        h("div", { class: "progress" }, h("i", { style: { "--p": `${u.state === "done" ? 100 : pct === null ? 8 : pct}%` } })));
    })));
}
let pumping = false;
async function pump() {
  if (pumping) return; pumping = true;
  try {
    for (;;) {
      const u = S.uploads.find((x) => x.state === "waiting" && !x.importId); if (!u) break;
      u.state = "sending"; drawQueue();
      try { u.file.size > SINGLE_MAX ? await multipart(u) : await xhrPut(`/api/v1/buckets/${encodeURIComponent(u.bucket)}/objects/${encKey(u.key)}`, u.file, (n) => { u.sent = n; drawQueue(); }, u.file.type); u.state = "done"; }
      catch (e) { u.state = "failed"; u.error = e.message; }
      drawQueue();
      if (u.bucket === S.bucket && document.getElementById("files")) { await refreshStats(); drawMeter(); renderFiles(); }
    }
  } finally { pumping = false; }
}
function xhrPut(url, blob, onProgress, ctype) {
  return new Promise((resolve, reject) => {
    const x = new XMLHttpRequest();
    x.open("PUT", url); x.withCredentials = true;
    if (api.csrf) x.setRequestHeader("x-csrf-token", api.csrf);
    if (ctype) x.setRequestHeader("content-type", ctype);
    x.upload.onprogress = (e) => onProgress(e.loaded);
    x.onload = () => { if (x.status >= 200 && x.status < 300) resolve(JSON.parse(x.responseText || "null")); else { let m = `Upload failed (${x.status})`; try { m = JSON.parse(x.responseText).error.message; } catch {} reject(new Error(m)); } };
    x.onerror = () => reject(new Error("Network error: check the connection and retry"));
    x.send(blob);
  });
}
async function multipart(u) {
  const init = await api.post(`/api/v1/buckets/${encodeURIComponent(u.bucket)}/uploads`, { key: u.key, content_type: u.file.type || undefined });
  const parts = Math.ceil(u.file.size / PART_SIZE);
  try {
    for (let i = 0; i < parts; i++) {
      const start = i * PART_SIZE; const blob = u.file.slice(start, Math.min(u.file.size, start + PART_SIZE));
      for (let attempt = 1; ; attempt++) {
        try { await xhrPut(`/api/v1/uploads/${init.upload_id}/parts/${i + 1}`, blob, (n) => { u.sent = start + n; drawQueue(); }); break; }
        catch (e) { if (attempt >= 3) throw e; await new Promise((r) => setTimeout(r, 1500 * attempt)); }
      }
    }
    await api.post(`/api/v1/uploads/${init.upload_id}/complete`);
  } catch (e) { api.del(`/api/v1/uploads/${init.upload_id}`).catch(() => {}); throw e; }
}
// ---------- import from URL (the server downloads it) ----------
function importDialog() {
  const url = h("input", { type: "text", placeholder: "https://example.com/video.mp4", autocomplete: "off", spellcheck: "false" });
  const name = h("input", { type: "text", placeholder: "Taken from the URL", autocomplete: "off" });
  const hdrs = h("textarea", { rows: 2, placeholder: "Referer: https://example.com/", spellcheck: "false" });
  url.addEventListener("input", () => {
    try { const last = decodeURIComponent(new URL(url.value).pathname.split("/").pop() || ""); if (last && !name.dataset.touched) name.value = last; } catch {}
  });
  name.addEventListener("input", () => { name.dataset.touched = "1"; });
  const go = h("button", { class: "btn primary", onclick: async () => {
    const headers = {};
    for (const line of hdrs.value.split("\n")) { const i = line.indexOf(":"); if (i > 0) headers[line.slice(0, i).trim()] = line.slice(i + 1).trim(); }
    const body = { url: url.value.trim(), headers };
    if (name.value.trim()) body.key = S.prefix + name.value.trim();
    go.disabled = true;
    try {
      const job = await api.post(`/api/v1/buckets/${encodeURIComponent(S.bucket)}/import`, body);
      d.close();
      const item = { importId: job.id, bucket: job.bucket, key: job.key, size: 0, sent: 0, state: "sending" };
      S.uploads.push(item); drawQueue(); pollImport(item);
    } catch (e) { fail(e); go.disabled = false; }
  } }, "Start import");
  const d = dialog("Import from a URL", h("div", { style: { display: "grid", gap: "16px" } },
    h("p", { class: "muted" }, `This server downloads the file and streams it, encrypted, into ${S.bucket}/${S.prefix}. You can close this page while it runs.`),
    h("label", { class: "field" }, "File URL", url),
    h("label", { class: "field" }, "Save as", name),
    h("label", { class: "field" }, "Extra request headers", hdrs, h("span", { class: "hint" }, "Optional, one per line. Some servers want a Referer or a cookie."))),
    [h("button", { class: "btn", onclick: () => d.close() }, "Cancel"), go]);
  url.focus();
}
async function pollImport(item) {
  for (;;) {
    await new Promise((r) => setTimeout(r, 1000));
    let j;
    try { j = await api.get(`/api/v1/imports/${item.importId}`); } catch (e) { item.state = "failed"; item.error = e.message; drawQueue(); return; }
    item.sent = j.received_bytes; item.size = j.total_bytes || 0;
    if (j.state === "done") { item.state = "done"; item.size = j.object ? j.object.size : item.sent; }
    if (j.state === "failed") { item.state = "failed"; item.error = j.error; }
    drawQueue();
    if (item.state !== "sending") {
      if (item.state === "done" && item.bucket === S.bucket && document.getElementById("files")) { await refreshStats(); drawMeter(); renderFiles(); }
      return;
    }
  }
}

let dragDepth = 0; let dropEl = null;
window.addEventListener("dragenter", (e) => { if (!e.dataTransfer || ![...e.dataTransfer.types].includes("Files") || !document.getElementById("files")) return; dragDepth++; if (!dropEl) { dropEl = h("div", { class: "drop" }, `Drop to upload to ${S.bucket}/${S.prefix}`); document.body.append(dropEl); } });
window.addEventListener("dragleave", () => { dragDepth = Math.max(0, dragDepth - 1); if (!dragDepth && dropEl) { dropEl.remove(); dropEl = null; } });
window.addEventListener("dragover", (e) => { if (document.getElementById("files")) e.preventDefault(); });
window.addEventListener("drop", (e) => { if (!document.getElementById("files")) return; e.preventDefault(); dragDepth = 0; if (dropEl) { dropEl.remove(); dropEl = null; } if (e.dataTransfer.files.length) enqueue([...e.dataTransfer.files]); });

// ---------- share links ----------
async function pageLinks() {
  const r = await api.get("/api/v1/shares");
  const status = (s) => s.revoked_at ? h("span", { class: "tag" }, "Turned off") : s.expires_at && s.expires_at < Date.now() / 1000 ? h("span", { class: "tag" }, "Expired")
    : s.max_downloads && s.downloads >= s.max_downloads ? h("span", { class: "tag warn" }, "Used up") : h("span", { class: "tag ok" }, "Live");
  $view().replaceChildren(
    h("div", { class: "head" }, h("div", {}, h("h1", {}, "Share links"), h("p", { class: "lede" }, "Links you handed out. Create new ones from a file's details on the Files page."))),
    h("div", { class: "panel flush" }, r.shares.length ? h("div", { class: "table-wrap" }, h("table", {},
      h("thead", {}, h("tr", {}, h("th", {}, "File"), h("th", {}, "Status"), h("th", { class: "num" }, "Downloads"), h("th", { class: "hide-sm" }, "Protection"), h("th", { class: "hide-sm" }, "Expires"), h("th", {}))),
      h("tbody", {}, r.shares.map((s) => h("tr", {},
        h("td", {}, h("span", { class: "name" }, chip("file"), h("span", { class: "label" }, `${s.bucket}/${s.key}`))), h("td", {}, status(s)),
        h("td", { class: "num" }, s.max_downloads ? `${s.downloads} of ${s.max_downloads}` : s.downloads),
        h("td", { class: "hide-sm" }, s.password ? h("span", { class: "row", style: { gap: "6px" } }, icon("lock", 14), "Password") : h("span", { class: "faint" }, "Open")),
        h("td", { class: "hide-sm muted" }, s.expires_at ? when(s.expires_at) : "Never"),
        h("td", { class: "num" }, !s.revoked_at && h("button", { class: "btn small danger", onclick: async () => {
          if (!(await confirmDialog("Turn off this link?", "People who have it will see that the link is unavailable.", "Turn off"))) return;
          try { await api.del(`/api/v1/shares/${s.id}`); toast("Link turned off"); route(); } catch (e) { fail(e); }
        } }, "Turn off")))))))
      : h("div", { class: "empty-state" }, h("strong", {}, "No share links yet"), h("p", {}, "Open a file on the Files page and choose Share."))));
}

// ---------- API keys ----------
async function pageKeys() {
  const r = await api.get("/api/v1/keys");
  await loadBuckets();
  const scopes = [["read", "Read files"], ["list", "List files"], ["write", "Upload"], ["delete", "Delete"], ["share", "Create links"], ["admin", "Admin"]];
  const name = h("input", { type: "text", placeholder: "my-website", required: true });
  const checks = scopes.map(([v, l]) => h("label", {}, h("input", { type: "checkbox", value: v, checked: v === "read" || v === "list" }), l));
  const bucketChecks = S.buckets.map((b) => h("label", {}, h("input", { type: "checkbox", value: b.name }), b.name));
  const ips = h("input", { type: "text", placeholder: "Any address" });
  const rpm = h("input", { type: "number", min: 0, placeholder: "Server default" });
  const exp = h("select", {}, [["", "Never"], ["86400", "1 day"], ["2592000", "30 days"], ["7776000", "90 days"], ["31536000", "1 year"]].map(([v, l]) => h("option", { value: v }, l)));
  const form = h("form", { class: "panel", onsubmit: async (e) => {
    e.preventDefault();
    const body = { name: name.value.trim(), scopes: checks.map((c) => c.firstChild).filter((c) => c.checked).map((c) => c.value) };
    const bs = bucketChecks.map((c) => c.firstChild).filter((c) => c.checked).map((c) => c.value); if (bs.length) body.buckets = bs;
    const ipl = ips.value.split(",").map((s) => s.trim()).filter(Boolean); if (ipl.length) body.allowed_ips = ipl;
    if (rpm.value !== "") body.rpm = Number(rpm.value);
    if (exp.value) body.expires_in = Number(exp.value);
    try {
      const k = await api.post("/api/v1/keys", body);
      const d = dialog("Copy your new key", h("div", {}, h("p", { class: "muted" }, "This is the only time the full key is shown. The server keeps only a hash of it."), h("div", { class: "secret" }, k.key),
        h("pre", { class: "secret", style: { "font-size": "12px" } }, `curl -H "Authorization: Bearer ${k.key.slice(0, 20)}…" \\\n  ${location.origin}/api/v1/buckets`)),
        [h("button", { class: "btn", onclick: () => copy(k.key, "Key") }, icon("copy", 15), "Copy key"), h("button", { class: "btn primary", onclick: () => { d.close(); route(); } }, "Done")]);
    } catch (e) { fail(e); }
  } },
    h("header", {}, h("h2", {}, "Create a key")),
    h("div", { class: "formgrid" },
      h("label", { class: "field" }, "Name", name), h("label", { class: "field" }, "Allowed IPs or CIDR", ips, h("span", { class: "hint" }, "Comma separated")),
      h("label", { class: "field" }, "Requests per minute", rpm), h("label", { class: "field" }, "Expires", exp)),
    h("p", { class: "sublabel" }, "What the key can do"), h("div", { class: "checks" }, checks),
    S.buckets.length ? [h("p", { class: "sublabel" }, "Limit to buckets. Leave all unticked for every bucket."), h("div", { class: "checks" }, bucketChecks)] : null,
    h("div", { class: "row", style: { "margin-top": "20px" } }, h("button", { class: "btn primary", type: "submit" }, icon("plus", 16), "Create key")));
  $view().replaceChildren(
    h("div", { class: "head" }, h("div", {}, h("h1", {}, "API keys"), h("p", { class: "lede" }, "Give each website or script its own key with only the access it needs. Revoking a key also breaks every signed URL it created."))),
    h("div", { class: "stack" },
      h("div", { class: "panel flush" }, r.keys.length ? h("div", { class: "table-wrap" }, h("table", {},
        h("thead", {}, h("tr", {}, h("th", {}, "Name"), h("th", {}, "Key"), h("th", {}, "Access"), h("th", { class: "hide-sm" }, "Buckets"), h("th", { class: "hide-sm" }, "Last used"), h("th", {}))),
        h("tbody", {}, r.keys.map((k) => {
          const dead = k.revoked_at ? "Revoked" : k.expires_at && k.expires_at < Date.now() / 1000 ? "Expired" : null;
          return h("tr", {},
            h("td", {}, h("span", { class: "name" }, h("span", { class: "chip" }, icon("keys", 15)), h("span", { class: "label" }, k.name)), dead ? [" ", h("span", { class: "tag" }, dead)] : null),
            h("td", { class: "mono faint" }, `tgv_${k.id}_…`), h("td", {}, (k.scopes || []).join(", ")), h("td", { class: "hide-sm" }, k.buckets ? k.buckets.join(", ") : "All"),
            h("td", { class: "hide-sm muted" }, when(k.last_used_at)),
            h("td", { class: "num" }, !k.revoked_at && h("button", { class: "btn small danger", onclick: async () => {
              if (!(await confirmDialog(`Revoke ${k.name}?`, "Apps using this key stop working at once, and its signed URLs stop opening.", "Revoke key"))) return;
              try { await api.del(`/api/v1/keys/${k.id}`); toast("Key revoked"); route(); } catch (e) { fail(e); }
            } }, "Revoke")));
        }))))
        : h("div", { class: "empty-state" }, h("strong", {}, "No keys yet"), h("p", {}, "Create one below for each app that should reach the API."))),
      form));
}

// ---------- activity ----------
async function pageActivity() {
  const tbody = h("tbody", {});
  const more = h("button", { class: "btn small" }, "Show older");
  let before = null;
  const load = async () => {
    const r = await api.get(`/api/v1/admin/audit?limit=100${before ? `&before=${before}` : ""}`);
    for (const e of r.events) tbody.append(h("tr", {},
      h("td", { class: "muted", title: fullDate(e.ts) }, when(e.ts)),
      h("td", {}, h("span", { class: "name" }, h("span", { class: "swatch", style: { "--c": ACTION_COLOR[e.action] || "var(--ink-3)", "border-radius": "50%" } }), ACTION_TEXT[e.action] || e.action)),
      h("td", {}, e.target), h("td", { class: "mono faint" }, e.actor), h("td", { class: "hide-sm mono faint" }, e.ip), h("td", { class: "hide-sm muted" }, e.detail || "")));
    if (r.events.length) before = r.events[r.events.length - 1].id;
    more.hidden = r.events.length < 100;
  };
  more.addEventListener("click", () => load().catch(fail));
  $view().replaceChildren(
    h("div", { class: "head" }, h("div", {}, h("h1", {}, "Activity"), h("p", { class: "lede" }, "Every change and sign-in, newest first. Failed share passwords and logins show up here too."))),
    h("div", { class: "panel flush" }, h("div", { class: "table-wrap" }, h("table", {}, h("thead", {}, h("tr", {}, h("th", {}, "When"), h("th", {}, "What"), h("th", {}, "Target"), h("th", {}, "Who"), h("th", { class: "hide-sm" }, "IP"), h("th", { class: "hide-sm" }, "Detail"))), tbody))),
    h("div", { class: "row", style: { "margin-top": "14px" } }, more));
  await load();
}

// ---------- maintenance ----------
async function pageMaintenance() {
  const [st, bk] = await Promise.all([api.get("/api/v1/admin/stats"), api.get("/api/v1/admin/backups")]);
  const out = h("pre", { class: "secret", hidden: true });
  const task = (title, text, label, path, done) => h("div", { class: "panel" },
    h("header", {}, h("h2", {}, title)), h("p", { class: "muted", style: { margin: "0 0 16px", "font-size": "14px" } }, text),
    h("button", { class: "btn", onclick: async (e) => {
      const b = e.currentTarget; b.disabled = true;
      try { const r = await api.post(path); out.hidden = false; out.textContent = JSON.stringify(r, null, 2); toast(done); } catch (err) { fail(err); } finally { b.disabled = false; }
    } }, label));
  $view().replaceChildren(
    h("div", { class: "head" }, h("div", {}, h("h1", {}, "Maintenance"), h("p", { class: "lede" }, "Housekeeping runs on its own every five minutes. Use these to run it now."))),
    h("div", { class: "ov", style: { "grid-template-columns": "repeat(auto-fit, minmax(240px, 1fr))" } },
      task("Back up the file index", `Seals the database with your master key and ${st.backend.kind === "telegram" ? "pins it in the channel" : "stores it next to the data"}. Automatic: ${bk.interval_hours ? `every ${bk.interval_hours} h` : "off"}.`, "Back up now", "/api/v1/admin/backups", "Backup stored"),
      task("Clean up storage", "Deletes expired files and old trash, aborts stale uploads and removes chunks nothing points to.", "Clean up now", "/api/v1/admin/gc", "Clean-up finished"),
      task("Re-wrap data keys", "After switching TGV_ACTIVE_KEY, re-encrypts every file key with it. Chunks are not touched.", "Re-wrap keys", "/api/v1/admin/rewrap", "Data keys re-wrapped")),
    out,
    h("div", { class: "panel flush", style: { "margin-top": "18px" } }, h("header", {}, h("h2", {}, "Backups")),
      bk.backups.length ? h("div", { class: "table-wrap" }, h("table", {}, h("thead", {}, h("tr", {}, h("th", {}, "Created"), h("th", { class: "num" }, "Size"), h("th", {}, "Stored in"))),
        h("tbody", {}, bk.backups.map((b) => h("tr", {}, h("td", { title: fullDate(b.created_at) }, when(b.created_at)), h("td", { class: "num" }, bytes(b.size)), h("td", {}, b.backend))))))
        : h("div", { class: "empty-state" }, h("strong", {}, "No backups yet"), h("p", {}, "Run one now so the file index can be recovered with tgvault restore."))),
    st.backend.kind === "telegram" ? h("div", { class: "panel flush", style: { "margin-top": "18px" } }, h("header", {}, h("h2", {}, "Bots")), botTable(st.backend.bots)) : null);
}

boot();
