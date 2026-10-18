// Files the owner attaches for agents: a brief, a spec, a price list, a photo
// of a sketch. Drawn in the board's Settings (one panel per app) and in a
// goal's Files tab on the PC and the phone. It is self-contained because the
// goals page does not load ui.js: it brings its own helpers, look and confirm,
// and uses only the colour names both pages define, with fallbacks.
//
// What must stay true:
// - Names come from the owner and notes from the hub. They go in as text,
//   never as HTML.
// - Files go up one at a time, each with its own receipt, so a resend after
//   a dropped connection never adds the same file twice.
// - Size is checked here first (25 MB a file) to spare a slow upload; the hub
//   checks again and its word is final.
// - There is one panel per app and goal. A screen that redraws gets the same
//   element back, so an upload in progress or an error shown is never lost.
// - Agents get copies at their next start; the words on screen say so and
//   never promise that running work sees a change.
"use strict";
(() => {
  const MB = 1024 * 1024;
  const MAX_FILE = 25 * MB, MAX_APP = 250 * MB, MAX_COUNT = 100;
  const ACCEPT = [
    ".pdf", ".doc", ".docx", ".xls", ".xlsx", ".csv", ".ppt", ".pptx", ".txt", ".md", ".markdown",
    "image/*", "application/pdf", "text/plain", "text/markdown", "text/csv",
  ].join(",");
  const BADGE = { pdf: "PDF", word: "DOC", sheet: "XLS", slides: "PPT", image: "IMG", text: "TXT", other: "FILE" };
  const SIGNED_OUT = "Not signed in. Open the sign-in link the hub printed when it started.";
  const OFFLINE = "Can't reach your PC right now. Try again when it's back.";
  const TOO_BIG = "is larger than 25 MB. Make it smaller and try again.";
  const EMPTY = "Add a brief, a spec, a price list or a photo of a sketch. Agents read them while they work.";
  const panels = new Map();

  // ---------- Small helpers ----------
  // el("div", { className: "x" }, "text", node). Strings become text, never HTML.
  function el(tag, attrs, ...kids) {
    const e = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v == null || v === false) continue;
      if (k === "className") e.className = v;
      else if (k === "hidden" || k === "disabled" || k === "multiple") e[k] = true;
      else if (k.startsWith("on") && typeof v === "function") e.addEventListener(k.slice(2), v);
      else e.setAttribute(k, v === true ? "" : String(v));
    }
    for (const kid of kids.flat()) if (kid != null && kid !== false) e.append(kid instanceof Node ? kid : String(kid));
    return e;
  }

  const SVG = "http://www.w3.org/2000/svg";
  const ICONS = {
    clip: ["m21.4 11.1-9.2 9.2a6 6 0 0 1-8.5-8.5l9.2-9.2a4 4 0 0 1 5.7 5.7l-9.2 9.2a2 2 0 0 1-2.8-2.8l8.5-8.5"],
    open: ["M12 15V3", "m7 10 5 5 5-5", "M5 21h14"],
    trash: ["M3 6h18", "M8 6V4h8v2", "M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6"],
    shield: ["M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"],
    alert: ["M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z", "M12 9v4", "M12 17h.01"],
    done: ["M20 6 9 17l-5-5"],
  };
  function icon(name) {
    const s = document.createElementNS(SVG, "svg");
    s.setAttribute("viewBox", "0 0 24 24");
    s.setAttribute("class", "df-i");
    s.setAttribute("aria-hidden", "true");
    for (const d of ICONS[name] ?? []) {
      const p = document.createElementNS(SVG, "path");
      p.setAttribute("d", d);
      s.append(p);
    }
    return s;
  }

  function receipt() {
    if (window.crypto?.randomUUID) return crypto.randomUUID();
    const b = new Uint8Array(16);
    if (window.crypto?.getRandomValues) crypto.getRandomValues(b);
    else for (let i = 0; i < b.length; i++) b[i] = Math.floor(Math.random() * 256);
    return Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
  }

  const size = (n) => (n < MB ? `${Math.max(1, Math.round(n / 1024))} KB` : `${(n / MB).toFixed(n < 10 * MB ? 1 : 0)} MB`);
  const plural = (n, one, many = one + "s") => `${n} ${n === 1 ? one : many}`;
  // "Quarterly-price-list-final-version-3.xlsx" keeps its start and its
  // ending, so the kind of file still shows. The full name is in the tooltip.
  const shortName = (s) => (s.length > 48 ? `${s.slice(0, 30)}…${s.slice(-15)}` : s);
  // The hub may send "2026-10-18 09:30:00" (no zone, meaning UTC) or ISO.
  function when(s) {
    if (!s) return null;
    let d = new Date(s);
    if (isNaN(d)) d = new Date(String(s).replace(" ", "T") + "Z");
    return isNaN(d) ? null : d;
  }
  function words(chars) {
    const w = chars / 6;
    if (w < 20) return "a few words";
    const step = w < 1000 ? 10 : w < 10000 ? 100 : 1000;
    return `about ${(Math.round(w / step) * step).toLocaleString()} words`;
  }

  // What agents get from a file, in a few words. `warn` marks a file agents
  // may not be able to read, so the owner knows to change it.
  function canRead(f) {
    if (f.kind === "pdf") return { text: "Agents read it directly" };
    if (f.kind === "image") return { text: "Agents can look at it" };
    if (f.kind === "text") return { text: "Agents read it as it is" };
    if (f.text_chars > 0) return { text: `Agents can read the text (${words(f.text_chars)})` };
    return { text: f.note || "Agents get a copy, but may not be able to read it.", warn: true, usedNote: true };
  }

  // The hub's errors are plain sentences for the owner. Anything else (an
  // empty body, a web page, a framework message) is not shown as it is.
  const friendly = (t) => {
    const s = String(t ?? "").trim();
    return s.length > 0 && s.length <= 400 && !s.startsWith("<") && !/length limit|payload|failed to buffer|request body/i.test(s) ? s : "";
  };

  // Sends one file as the request body. Uses XMLHttpRequest for the upload
  // progress, which fetch does not give.
  function put(url, file, onProgress) {
    return new Promise((resolve, reject) => {
      const x = new XMLHttpRequest();
      x.open("POST", url);
      x.withCredentials = true;
      x.setRequestHeader("content-type", file.type || "application/octet-stream");
      x.upload.onprogress = (e) => { if (e.lengthComputable && e.total) onProgress(e.loaded / e.total); };
      x.onload = () => resolve({ status: x.status, text: x.responseText });
      x.onerror = x.ontimeout = () => reject(new Error(OFFLINE));
      x.onabort = () => reject(new Error("The upload was stopped."));
      x.send(file);
    });
  }

  async function api(path, opts = {}) {
    let r;
    try {
      r = await fetch("/api" + path, { credentials: "same-origin", ...opts });
    } catch (_) {
      throw Object.assign(new Error(OFFLINE), { offline: true });
    }
    if (r.status === 401) throw Object.assign(new Error(SIGNED_OUT), { signedOut: true });
    const text = await r.text();
    if (!r.ok) throw new Error(friendly(text) || `The hub said ${r.status}.`);
    if (!text) return null;
    try { return JSON.parse(text); } catch (_) { return null; }
  }

  // ---------- Asking first ----------
  // The board has its own confirm (ui.js); the goals page does not.
  function confirmAsk([title, text, yes, no]) {
    if (window.D?.confirm) return window.D.confirm([title, text, yes, no]);
    return new Promise((resolve) => {
      const id = "df-cf-" + receipt().slice(0, 8);
      const keep = el("button", { type: "button", className: "df-btn" }, no);
      const go = el("button", { type: "button", className: "df-btn danger" }, yes);
      const d = el("dialog", { className: "df-dialog", "aria-labelledby": id },
        el("h2", { id }, title), el("p", {}, text), el("div", { className: "df-dialog-row" }, keep, go));
      let answer = false;
      keep.onclick = () => d.close();
      go.onclick = () => { answer = true; d.close(); };
      d.addEventListener("close", () => { d.remove(); resolve(answer); });
      document.body.append(d);
      if (d.showModal) { d.showModal(); keep.focus(); } else { d.remove(); resolve(window.confirm(`${title}\n\n${text}`)); }
    });
  }

  // ---------- The look ----------
  const CSS = `
.df-panel { --df-card: var(--card, var(--surface, #fff)); --df-line: var(--line, #e1e4ea); --df-strong: var(--line-strong, var(--field-line, #7d8695));
  --df-soft: var(--neutral-soft, var(--surface-2, #eef0f4)); --df-ink: var(--ink, #14171c); --df-muted: var(--muted, #545d6b);
  --df-accent: var(--accent, #2553d6); --df-accent-ink: var(--accent-ink, #1f4fd1); --df-accent-soft: var(--accent-soft, #e8eefc);
  --df-bad: var(--bad-ink, var(--red, #b42318)); --df-bad-soft: var(--bad-soft, var(--red-soft, #fdeceb));
  --df-warn: var(--warn-ink, var(--amber, #8a5300)); --df-ok: var(--ok-ink, var(--green, #18703a));
  --df-radius: var(--radius, 12px); --df-h: 36px;
  container-type: inline-size; display: grid; gap: 10px; min-width: 0; color: var(--df-ink); font-size: 14px; line-height: 1.45; }
.df-panel h3.df-title { margin: 0; font-size: 14px; font-weight: 600; }
.df-i { width: 16px; height: 16px; flex: none; fill: none; stroke: currentColor; stroke-width: 2; stroke-linecap: round; stroke-linejoin: round; }
.df-group { display: grid; gap: 6px; min-width: 0; }
.df-group-h { margin: 0; font-size: 12.5px; font-weight: 600; color: var(--df-muted); }
.df-list { list-style: none; margin: 0; padding: 0; border: 1px solid var(--df-line); border-radius: var(--df-radius); background: var(--df-card); }
.df-panel .df-list > li { display: grid; grid-template-columns: 40px minmax(0, 1fr) auto; align-items: center; gap: 4px 12px; padding: 10px 12px; }
.df-panel .df-list > li + li { border-top: 1px solid var(--df-line); }
.df-badge { width: 40px; height: 44px; border-radius: 8px; display: grid; place-items: center; font: 700 11px/1 ui-sans-serif, system-ui, sans-serif; letter-spacing: .04em;
  background: var(--df-soft); color: var(--df-muted); border: 1px solid var(--df-line); }
.df-badge[data-kind="pdf"], .df-badge[data-kind="word"], .df-badge[data-kind="sheet"], .df-badge[data-kind="slides"] { background: var(--df-accent-soft); color: var(--df-accent-ink); border-color: transparent; }
.df-main { min-width: 0; display: grid; gap: 1px; }
.df-name { font-weight: 600; overflow-wrap: anywhere; display: -webkit-box; -webkit-line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; }
.df-meta, .df-can, .df-note { font-size: 12.5px; color: var(--df-muted); overflow-wrap: anywhere; }
.df-can { display: flex; align-items: center; gap: 5px; }
.df-can .df-i { width: 13px; height: 13px; color: var(--df-ok); }
.df-can.warn { color: var(--df-warn); }
.df-can.warn .df-i { color: var(--df-warn); }
.df-acts { display: flex; gap: 4px; flex-wrap: wrap; justify-content: flex-end; }
.df-btn { font: inherit; font-size: 13.5px; font-weight: 600; min-height: var(--df-h); padding: 0 12px; border-radius: 10px; border: 1px solid var(--df-strong);
  background: var(--df-card); color: var(--df-ink); cursor: pointer; display: inline-flex; align-items: center; justify-content: center; gap: 6px; text-decoration: none; white-space: nowrap; }
.df-btn:hover { background: var(--df-soft); }
.df-btn:disabled { opacity: .55; cursor: not-allowed; }
.df-btn.primary { background: var(--df-accent); border-color: var(--df-accent); color: var(--on-accent, #fff); }
.df-btn.primary:hover { filter: brightness(1.07); }
.df-btn.quiet { background: transparent; border-color: transparent; color: var(--df-accent-ink); padding: 0 8px; }
.df-btn.quiet:hover { background: var(--df-accent-soft); }
.df-btn.quiet.bad { color: var(--df-bad); }
.df-btn.quiet.bad:hover { background: var(--df-bad-soft); }
.df-btn.danger { background: var(--red-solid, var(--bad, #c0261b)); border-color: var(--red-solid, var(--bad, #c0261b)); color: #fff; }
.df-empty { margin: 0; padding: 14px; border: 1px dashed var(--df-strong); border-radius: var(--df-radius); color: var(--df-muted); font-size: 13.5px; }
.df-drop { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 12px; padding: 10px 12px; border: 1px dashed var(--df-strong); border-radius: var(--df-radius); }
.df-drop .df-hint { flex: 1 1 180px; min-width: 0; }
.df-panel.df-over .df-drop { border-color: var(--df-accent); background: var(--df-accent-soft); box-shadow: inset 0 0 0 1px var(--df-accent); }
.df-hint { margin: 0; font-size: 12.5px; color: var(--df-muted); }
.df-dropword { display: none; }
@media (hover: hover) and (pointer: fine) { .df-dropword { display: inline; } }
.df-status { margin: 0; font-size: 13px; color: var(--df-ink); display: flex; align-items: center; gap: 8px; }
.df-status:empty, .df-err:empty { display: none; }
.df-status .df-i { color: var(--df-ok); }
.df-bar { flex: 0 0 80px; height: 6px; border-radius: 99px; background: var(--df-soft); overflow: hidden; }
.df-bar span { display: block; height: 100%; background: var(--df-accent); border-radius: 99px; transition: width .2s ease; }
.df-err { margin: 0; padding: 8px 10px; border-radius: 10px; background: var(--df-bad-soft); color: var(--df-bad); font-size: 13px; overflow-wrap: anywhere; white-space: pre-line; }
.df-note { display: flex; align-items: flex-start; gap: 6px; margin: 0; }
.df-note .df-i { width: 14px; height: 14px; margin-top: 2px; }
.df-dialog { color: var(--ink, #14171c); background: var(--card, var(--surface, #fff)); border: 1px solid var(--line, #e1e4ea); border-radius: 16px;
  padding: 20px; width: min(420px, calc(100vw - 32px)); box-shadow: 0 12px 32px rgba(15, 18, 24, .2); }
.df-dialog::backdrop { background: var(--scrim, rgba(15, 18, 24, .45)); }
.df-dialog h2 { margin: 0 0 8px; font-size: 18px; overflow-wrap: anywhere; }
.df-dialog p { margin: 0; color: var(--muted, #545d6b); }
.df-dialog-row { display: flex; gap: 8px; justify-content: flex-end; flex-wrap: wrap; margin-top: 18px; }
@container (max-width: 440px) {
  .df-panel .df-list > li { grid-template-columns: 40px minmax(0, 1fr); }
  .df-acts { grid-column: 2; justify-content: flex-start; margin-left: -8px; }
}
@media (pointer: coarse), (max-width: 640px) {
  .df-panel { --df-h: 44px; }
  .df-dialog-row { flex-direction: column-reverse; }
  .df-dialog-row .df-btn { width: 100%; min-height: 44px; }
}
@media (prefers-reduced-motion: reduce) { .df-bar span { transition: none; } }
`;
  function addStyle() {
    if (document.getElementById("df-style")) return;
    const s = document.createElement("style");
    s.id = "df-style";
    s.textContent = CSS;
    document.head.append(s);
  }

  // ---------- The panel ----------
  // DFiles.panel({ appId, goalId, title, onChange }) returns the element.
  //   appId     the app the files belong to
  //   goalId    a goal of that app: shows its files, plus the app's own
  //             (read-only here). Leave it out for the app's own files.
  //   title     an optional heading; most callers give their own
  //   onChange  called with { all, own, shared } after each load or change
  function panel(opts) {
    const appId = Number(opts.appId), goalId = opts.goalId == null ? null : Number(opts.goalId);
    const key = `${appId}:${goalId ?? ""}`;
    const had = panels.get(key);
    if (had) {
      had.opts = opts;
      had.setTitle(opts.title);
      // The caller hears the counts again, so a redrawn screen can show them.
      if (had.files && opts.onChange) setTimeout(had.notify, 0);
      if (!had.busy && Date.now() - had.loadedAt > 15000) had.load();
      return had.root;
    }
    addStyle();
    const p = { opts, busy: false, loadedAt: 0, files: null, loadErr: "" };
    panels.set(key, p);
    const forGoal = goalId != null;
    const base = `/apps/${appId}/files`;
    const uid = `df-${appId}-${goalId ?? "app"}`;

    const title = el("h3", { className: "df-title", id: `${uid}-t` });
    const body = el("div", { className: "df-group" }, el("p", { className: "df-hint" }, "Loading files…"));
    const input = el("input", { type: "file", multiple: true, accept: ACCEPT, hidden: true, tabindex: "-1", "aria-hidden": "true" });
    const addBtn = el("button", { type: "button", className: "df-btn primary", "data-key": `${uid}-add`, "aria-describedby": `${uid}-lim` },
      icon("clip"), forGoal ? "Add files to this goal" : "Add files");
    const limits = el("p", { className: "df-hint", id: `${uid}-lim` }, "Up to 25 MB each.",
      el("span", { className: "df-dropword" }, " You can also drop files here."));
    const status = el("p", { className: "df-status", role: "status", "aria-live": "polite" });
    const err = el("p", { className: "df-err", role: "alert", id: `${uid}-err` });
    const room = el("p", { className: "df-hint", hidden: true });
    const root = el("div", { className: "df-panel", role: "group", "data-df": key },
      title, body,
      el("div", { className: "df-drop" }, addBtn, limits, input),
      status, err, room,
      el("p", { className: "df-note" }, icon("shield"),
        "Agents get copies. The files stay on your PC. Please don't attach patient or personal information."));
    p.root = root;

    p.setTitle = (t) => {
      title.textContent = t || "";
      title.hidden = !t;
      if (t) { root.setAttribute("aria-labelledby", title.id); root.removeAttribute("aria-label"); }
      else { root.removeAttribute("aria-labelledby"); root.setAttribute("aria-label", forGoal ? "Files for this goal" : "Files for agents"); }
    };
    p.setTitle(opts.title);

    // Which files this panel shows as its own, and which come from the app.
    const split = (all) => ({
      all,
      own: all.filter((f) => (forGoal ? Number(f.goal_id) === goalId : f.goal_id == null)),
      shared: forGoal ? all.filter((f) => f.goal_id == null) : [],
    });

    function row(f, canRemove) {
      const can = canRead(f);
      const added = when(f.added_at);
      const meta = [size(f.size ?? 0)];
      if (added) meta.push(`added ${added.toLocaleDateString([], { month: "short", day: "numeric" })}`);
      const open = el("a", { className: "df-btn quiet", href: `/api${base}/${encodeURIComponent(f.id)}`, download: f.name,
        "aria-label": `Open ${f.name}`, "data-key": `${uid}-open-${f.id}` }, icon("open"), "Open");
      let rm = null;
      if (canRemove) {
        rm = el("button", { type: "button", className: "df-btn quiet bad", "aria-label": `Remove ${f.name}`, "data-key": `${uid}-rm-${f.id}` },
          icon("trash"), "Remove");
        rm.onclick = () => remove(f, rm);
      }
      return el("li", {},
        el("span", { className: "df-badge", "data-kind": f.kind, "aria-hidden": "true" }, BADGE[f.kind] ?? "FILE"),
        el("div", { className: "df-main" },
          el("span", { className: "df-name", title: f.name }, shortName(f.name)),
          el("span", { className: "df-meta", title: added ? added.toLocaleString() : null }, meta.join(" · ")),
          el("span", { className: `df-can${can.warn ? " warn" : ""}` }, icon(can.warn ? "alert" : "done"), can.text),
          f.note && !can.usedNote ? el("span", { className: "df-meta" }, f.note) : null),
        el("div", { className: "df-acts" }, open, rm));
    }

    function draw() {
      if (!p.files) {
        if (p.loadErr) {
          const again = el("button", { type: "button", className: "df-btn", "data-key": `${uid}-again` }, "Try again");
          again.onclick = () => load();
          body.replaceChildren(el("p", { className: "df-err", role: "alert" }, `Couldn't load the files. ${p.loadErr}`), el("div", {}, again));
        }
        return;
      }
      const { all, own, shared } = split(p.files);
      const kids = [];
      if (forGoal) kids.push(el("h4", { className: "df-group-h" }, own.length ? `For this goal (${own.length})` : "For this goal"));
      kids.push(own.length
        ? el("ul", { className: "df-list", "aria-label": forGoal ? "Files for this goal" : "Files for the whole app" }, own.map((f) => row(f, true)))
        : el("p", { className: "df-empty" }, forGoal ? `No files for this goal yet. ${EMPTY}` : EMPTY));
      if (forGoal && shared.length) {
        kids.push(el("h4", { className: "df-group-h" }, `For the whole app (${shared.length})`),
          el("ul", { className: "df-list", "aria-label": "Files for the whole app" }, shared.map((f) => row(f, false))),
          el("p", { className: "df-hint" }, "Agents on every goal of this app get these too. Add or remove them in Settings on the board."));
      }
      if (!forGoal) {
        const n = all.length - own.length;
        if (n) kids.push(el("p", { className: "df-hint" }, `Goals of this app have ${plural(n, "more file")} of their own. You'll find them in each goal's Files tab.`));
      }
      body.replaceChildren(...kids);
      const used = all.reduce((s, f) => s + (f.size ?? 0), 0);
      room.hidden = !all.length;
      room.textContent = `This app has ${plural(all.length, "file")} (${size(used)}). It can hold up to ${MAX_COUNT} files and 250 MB.`;
    }

    p.notify = () => {
      if (p.files) try { p.opts.onChange?.(split(p.files)); } catch (_) { /* the caller's trouble, not ours */ }
    };
    function changed() {
      draw();
      p.notify();
    }

    async function load() {
      try {
        const list = await api(base);
        p.files = Array.isArray(list) ? list : [];
        p.loadErr = "";
        p.loadedAt = Date.now();
      } catch (e) {
        p.loadErr = e.message;
        p.loadedAt = Date.now();
      }
      changed();
    }
    p.load = load;

    let clearTimer = 0;
    function say(nodes, sticky) {
      clearTimeout(clearTimer);
      status.replaceChildren(...[nodes].flat());
      if (!sticky) clearTimer = setTimeout(() => status.replaceChildren(), 8000);
    }

    // Why a file can't go up, before sending it, or "".
    function refuse(f, all) {
      if (f.size > MAX_FILE) return `${f.name} ${TOO_BIG}`;
      if (all.length >= MAX_COUNT) return `${f.name} was not added. This app already has ${MAX_COUNT} files. Remove one first.`;
      const used = all.reduce((s, x) => s + (x.size ?? 0), 0);
      if (used + f.size > MAX_APP) return `${f.name} was not added. This app's files can add up to 250 MB, and there isn't room. Remove some first.`;
      return "";
    }

    async function sendOne(f, n, of) {
      const r = receipt();
      let url = `/api${base}?name=${encodeURIComponent(f.name)}&receipt=${encodeURIComponent(r)}`;
      if (forGoal) url += `&goal=${encodeURIComponent(goalId)}`;
      const bar = el("span", { style: "width:0%" });
      const label = el("span", {}, `Adding ${shortName(f.name)}…${of > 1 ? ` (${n} of ${of})` : ""}`);
      say([el("span", { className: "df-bar", "aria-hidden": "true" }, bar), label], true);
      const progress = (x) => { bar.style.width = `${Math.round(x * 100)}%`; };
      let res;
      try {
        res = await put(url, f, progress);
      } catch (_) {
        // No answer at all: send once more with the same receipt, so the hub
        // adds it at most once.
        await new Promise((ok) => setTimeout(ok, 1000));
        res = await put(url, f, progress);
      }
      if (res.status === 401) throw Object.assign(new Error(SIGNED_OUT), { signedOut: true });
      if (res.status === 413) throw new Error(friendly(res.text) || `${f.name} ${TOO_BIG}`);
      if (res.status < 200 || res.status >= 300) throw new Error(friendly(res.text) || `The hub said ${res.status}.`);
      let made = null;
      try { made = JSON.parse(res.text); } catch (_) { /* the list reload shows it */ }
      return made;
    }

    // Adds the files one at a time, in the order given.
    const queue = [];
    async function add(list) {
      const files = Array.from(list || []).filter((f) => f && f.name);
      if (!files.length) return;
      queue.push(...files);
      if (p.busy) return;
      p.busy = true;
      err.textContent = "";
      const errors = [], added = [];
      if (!p.files) await load();
      let n = 0;
      while (queue.length) {
        const f = queue.shift();
        n++;
        const why = refuse(f, p.files ?? []);
        if (why) { errors.push(why); continue; }
        try {
          const made = await sendOne(f, n, n + queue.length);
          added.push(f.name);
          if (made && p.files && !p.files.some((x) => x.id === made.id)) { p.files.unshift(made); changed(); }
        } catch (e) {
          if (e.signedOut) { errors.push(SIGNED_OUT); queue.length = 0; break; }
          errors.push(`Couldn't add ${f.name}. ${e.message}`);
        }
      }
      p.busy = false;
      await load();
      if (added.length === 1) say([icon("done"), `${shortName(added[0])} added. Agents get it from their next start.`]);
      else if (added.length) say([icon("done"), `${plural(added.length, "file")} added. Agents get them from their next start.`]);
      else status.replaceChildren();
      err.textContent = errors.join("\n");
    }
    p.add = add;

    async function remove(f, btn) {
      const ok = await confirmAsk([`Remove ${f.name}?`,
        "Agents will no longer get it. Work already running keeps its copy until it next starts.", "Remove", "Keep it"]);
      if (!ok) return;
      btn.disabled = true;
      err.textContent = "";
      try {
        await api(`${base}/${encodeURIComponent(f.id)}/remove`, {
          method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ receipt: receipt() }),
        });
        if (p.files) p.files = p.files.filter((x) => x.id !== f.id);
        changed();
        say([icon("done"), `${shortName(f.name)} removed. Agents won't get it from their next start.`]);
        addBtn.focus({ preventScroll: true });
        load();
      } catch (e) {
        btn.disabled = false;
        err.textContent = `Couldn't remove ${f.name}. ${e.message}`;
      }
    }

    // The picker. On a phone it offers the camera, photos and files.
    p.pick = () => input.click();
    addBtn.onclick = p.pick;
    input.addEventListener("change", () => {
      const list = Array.from(input.files || []);
      input.value = "";
      add(list);
    });

    // Drag and drop, on a computer. Only files count, not dragged text.
    const hasFiles = (e) => Array.from(e.dataTransfer?.types || []).includes("Files");
    let depth = 0;
    root.addEventListener("dragenter", (e) => { if (!hasFiles(e)) return; e.preventDefault(); depth++; root.classList.add("df-over"); });
    root.addEventListener("dragover", (e) => { if (!hasFiles(e)) return; e.preventDefault(); e.dataTransfer.dropEffect = "copy"; });
    root.addEventListener("dragleave", () => { depth = Math.max(0, depth - 1); if (!depth) root.classList.remove("df-over"); });
    root.addEventListener("drop", (e) => {
      if (!hasFiles(e)) return;
      e.preventDefault();
      depth = 0;
      root.classList.remove("df-over");
      add(e.dataTransfer.files);
    });

    load();
    return root;
  }

  // DFiles.pick(opts) opens the file picker for that app or goal, adding to
  // the same panel DFiles.panel(opts) returns. Call it straight from a tap:
  // browsers open a picker only then.
  function pick(opts) {
    panel(opts);
    const key = `${Number(opts.appId)}:${opts.goalId == null ? "" : Number(opts.goalId)}`;
    panels.get(key)?.pick();
  }

  window.DFiles = { panel, pick };
})();
