"use strict";
/* Renders /api/v1/openapi.json into a reference page. Text only via text nodes. */
function h(tag, attrs, ...kids) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v === null || v === undefined || v === false) continue;
    if (k === "class") el.className = v; else if (k.startsWith("on")) el.addEventListener(k.slice(2), v); else el.setAttribute(k, v === true ? "" : String(v));
  }
  for (const kid of kids.flat(Infinity)) if (kid !== null && kid !== undefined && kid !== false) el.append(kid instanceof Node ? kid : document.createTextNode(String(kid)));
  return el;
}
// `code` and **bold** only; everything else stays literal text.
function rich(text) {
  const out = [];
  const re = /(`[^`]+`|\*\*[^*]+\*\*)/g;
  let last = 0; let m;
  for (const para of String(text || "").split(/\n\n+/)) {
    const p = h("p", {});
    last = 0; re.lastIndex = 0;
    while ((m = re.exec(para))) {
      p.append(para.slice(last, m.index));
      p.append(m[0].startsWith("`") ? h("code", {}, m[0].slice(1, -1)) : h("strong", {}, m[0].slice(2, -2)));
      last = m.index + m[0].length;
    }
    p.append(para.slice(last));
    out.push(p);
  }
  return out;
}
const slug = (m, p) => `${m}-${p}`.replace(/[^a-z0-9]+/gi, "-").replace(/^-|-$/g, "").toLowerCase();
const METHOD_ORDER = ["get", "head", "post", "put", "patch", "delete"];

function schemaText(s, spec, depth = 0) {
  if (!s) return "";
  if (s.$ref) return schemaText(spec.components.schemas[s.$ref.split("/").pop()], spec, depth);
  if (s.type === "object" && s.properties && depth < 2) {
    const req = new Set(s.required || []);
    return "{\n" + Object.entries(s.properties).map(([k, v]) => `${"  ".repeat(depth + 1)}"${k}"${req.has(k) ? "" : "?"}: ${schemaText(v, spec, depth + 1)}`).join(",\n") + `\n${"  ".repeat(depth)}}`;
  }
  if (s.type === "array") return `[${schemaText(s.items, spec, depth)}]`;
  if (s.enum) return s.enum.map((e) => JSON.stringify(e)).join(" | ");
  return Array.isArray(s.type) ? s.type.join(" | ") : s.type || "any";
}

async function main() {
  const spec = await (await fetch("/api/v1/openapi.json")).json();
  const base = (spec.servers && spec.servers[0] && spec.servers[0].url) || location.origin;
  const byTag = new Map();
  for (const [path, ops] of Object.entries(spec.paths)) {
    for (const m of METHOD_ORDER) if (ops[m]) { const t = ops[m].tags[0]; if (!byTag.has(t)) byTag.set(t, []); byTag.get(t).push({ path, method: m, op: ops[m] }); }
  }
  const order = spec.tags.map((t) => t.name).filter((n) => byTag.has(n));

  const toc = document.getElementById("toc");
  toc.append(h("a", { class: "brand plain", href: "/" }, "tgvault API"));
  const nav = h("nav", {});
  nav.append(h("a", { href: "#start" }, "Getting started"));
  for (const t of order) {
    nav.append(h("a", { href: `#tag-${slug("t", t)}`, class: "toc-tag" }, t));
  }
  toc.append(nav, h("div", { class: "foot" }, h("a", { href: "/api/v1/openapi.json" }, "openapi.json"), h("a", { href: "/" }, "Open the panel")));

  const doc = document.getElementById("doc");
  const pre = (text) => {
    const code = h("pre", { class: "code" }, text);
    const btn = h("button", { class: "btn small copy", onclick: async () => { try { await navigator.clipboard.writeText(text); btn.textContent = "Copied"; setTimeout(() => (btn.textContent = "Copy"), 1500); } catch {} } }, "Copy");
    return h("div", { class: "codebox" }, code, btn);
  };
  const start = h("section", { id: "start", class: "doc-intro" },
    h("h1", {}, spec.info.title), rich(spec.info.description),
    h("h2", {}, "Base URL"), pre(base),
    h("h2", {}, "Put a video on your website"),
    h("ol", { class: "steps" },
      h("li", {}, "Create a key with the read scope, limited to one bucket, under API keys in the panel."),
      h("li", {}, "Upload the file, or let your backend do it:", pre(`curl -H "Authorization: Bearer $TGV_KEY" -H "Content-Type: video/mp4" \\\n  -T trailer.mp4 ${base}/api/v1/buckets/media/objects/trailer.mp4`)),
      h("li", {}, "From your backend, ask for a signed link and hand it to the page. Players seek with Range requests, which the link supports. Add your site's origin to the bucket's CORS list.",
        pre(`curl -H "Authorization: Bearer $TGV_KEY" -H "Content-Type: application/json" \\\n  -d '{"bucket":"media","key":"trailer.mp4","expires_in":3600}' ${base}/api/v1/presign\n\n<video src="{url from the response}" controls></video>`))),
    h("p", { class: "muted" }, "For files anyone may see, make the bucket public and use ", h("code", {}, `${base}/pub/<bucket>/<key>`), " directly. Never put an API key in browser code."));
  doc.replaceChildren(start);

  for (const t of order) {
    const sec = h("section", { id: `tag-${slug("t", t)}`, class: "doc-tag" }, h("h2", { class: "tag-title" }, t));
    for (const { path, method, op } of byTag.get(t)) {
      const id = slug(method, path);
      const art = h("article", { id, class: "endpoint" });
      art.append(h("div", { class: "sig" }, h("span", { class: `verb v-${method}` }, method.toUpperCase()), h("code", { class: "path" }, path)));
      art.append(h("h3", {}, op.summary));
      art.append(...rich(op.description));
      const auth = op.security && op.security.length === 0 ? "No key needed" : "Bearer API key or panel session";
      art.append(h("p", { class: "muted small" }, auth));
      if (op.parameters && op.parameters.length) {
        art.append(h("h4", {}, "Parameters"), h("div", { class: "table-wrap" }, h("table", {},
          h("thead", {}, h("tr", {}, h("th", {}, "Name"), h("th", {}, "In"), h("th", {}, "Type"), h("th", {}, "Description"))),
          h("tbody", {}, op.parameters.map((p) => h("tr", {}, h("td", {}, h("code", {}, p.name), p.required ? " (required)" : ""), h("td", {}, p.in), h("td", {}, schemaText(p.schema, spec)), h("td", {}, rich(p.description))))))));
      }
      if (op.requestBody) {
        const [ct, media] = Object.entries(op.requestBody.content)[0];
        art.append(h("h4", {}, `Request body (${ct})`));
        art.append(pre(media.example ? JSON.stringify(media.example, null, 2) : schemaText(media.schema, spec)));
      }
      art.append(h("h4", {}, "Responses"), h("ul", { class: "responses" }, Object.entries(op.responses).map(([code, r]) => {
        const resolved = r.$ref ? spec.components.responses[r.$ref.split("/").pop()] : r;
        return h("li", {}, h("code", { class: `code-${code[0]}` }, code), " ", resolved.description);
      })));
      if (op["x-curl"]) art.append(h("h4", {}, "Example"), pre(op["x-curl"].replaceAll("$BASE", base)));
      sec.append(art);
    }
    doc.append(sec);
  }
  if (location.hash) { const el = document.getElementById(location.hash.slice(1)); if (el) el.scrollIntoView(); }
}
main().catch((e) => { document.getElementById("doc").replaceChildren(h("p", {}, "Could not load the API description: ", e.message)); });
