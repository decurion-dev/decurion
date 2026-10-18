// Decurion's shared screen code: talking to the hub, icons, status labels,
// the confirm and toast pieces, and the work panel. Used by the board and
// the phone inbox so both look and behave the same.
"use strict";
(() => {
  const D = {};
  window.D = D;

  // ---------- Small helpers ----------
  D.$ = (id) => document.getElementById(id);
  // el("div", { className: "x" }, "text", childNode, ...). Strings become text, never HTML.
  D.el = (tag, attrs = {}, ...kids) => {
    const e = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs ?? {})) {
      if (v == null || v === false) continue;
      if (k === "dataset") Object.assign(e.dataset, v);
      else if (k.startsWith("aria-") || k === "role" || k === "for") e.setAttribute(k, v === true ? "true" : v);
      else e[k] = v;
    }
    for (const k of kids.flat()) if (k != null && k !== false) e.append(k instanceof Node ? k : String(k));
    return e;
  };

  const ICONS = {
    queued: ["c12,12,8"],
    running: ["M21 12a9 9 0 1 1-6.22-8.56"],
    needs_you: ["M18 11V6a2 2 0 0 0-4 0v5", "M14 10V4a2 2 0 0 0-4 0v2", "M10 10.5V6a2 2 0 0 0-4 0v8", "M18 8a2 2 0 1 1 4 0v6a8 8 0 0 1-8 8h-2c-2.8 0-4.5-.86-6-2.34l-3.6-3.6a2 2 0 0 1 2.83-2.82L7 15"],
    question: ["M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"],
    in_review: ["M2 12s3-7 10-7 10 7 10 7-3 7-10 7-10-7-10-7z", "c12,12,3"],
    done: ["M20 6 9 17l-5-5"],
    failed: ["M18 6 6 18", "M6 6l12 12"],
    close: ["M18 6 6 18", "M6 6l12 12"],
    right: ["m9 18 6-6-6-6"],
    left: ["m15 18-6-6 6-6"],
    plus: ["M12 5v14", "M5 12h14"],
    alert: ["M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z", "M12 9v4", "M12 17h.01"],
    offline: ["M2 2l20 20", "M8.5 16.5a5 5 0 0 1 7 0", "M2 8.8a15 15 0 0 1 4.2-2.6", "M10.7 5c4-.4 8.1.9 11.3 3.8", "M16.9 11.3a10 10 0 0 1 2.2 1.7", "M5 13a10 10 0 0 1 5.2-2.8", "M12 20h.01"],
    bell: ["M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9", "M10.3 21a1.9 1.9 0 0 0 3.4 0"],
    inbox: ["M22 12h-6l-2 3h-4l-2-3H2", "M5.5 5.1 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.5-6.9A2 2 0 0 0 16.8 4H7.2a2 2 0 0 0-1.7 1.1z"],
    board: ["M5 3h14a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2z", "M9 3v18", "M15 3v18"],
    settings: ["M4 21v-7", "M4 10V3", "M12 21v-9", "M12 8V3", "M20 21v-5", "M20 12V3", "M1 14h6", "M9 8h6", "M17 16h6"],
    app: ["M4 20h16a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.7-.9l-.8-1.2A2 2 0 0 0 7.9 3H4a2 2 0 0 0-2 2v13c0 1.1.9 2 2 2z"],
    ai: ["M12 3l1.9 5.1L19 10l-5.1 1.9L12 17l-1.9-5.1L5 10l5.1-1.9z", "M19 17v4", "M17 19h4"],
    cloud: ["M17.5 19H9a7 7 0 1 1 6.71-9h1.79a4.5 4.5 0 1 1 0 9z"],
    pc: ["M4 4h16a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2z", "M8 21h8", "M12 17v4"],
    phone: ["M7 2h10a2 2 0 0 1 2 2v16a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2z", "M12 18h.01"],
    key: ["m15.5 7.5 2.3 2.3a1 1 0 0 0 1.4 0l2.1-2.1a1 1 0 0 0 0-1.4L19 4", "m21 2-9.6 9.6", "c7.5,15.5,5.5"],
    money: ["M12 2v20", "M17 5H9.5a3.5 3.5 0 0 0 0 7h5a3.5 3.5 0 0 1 0 7H6"],
    start: ["m6 3 14 9-14 9z"],
    tool: ["M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.8-3.8a6 6 0 0 1-7.9 7.9l-6.9 6.9a2.1 2.1 0 0 1-3-3l6.9-6.9a6 6 0 0 1 7.9-7.9z"],
    retry: ["M3 12a9 9 0 1 0 3-6.7L3 8", "M3 3v5h5"],
    branch: ["M6 3v12", "c18,6,3", "c6,18,3", "M18 9a9 9 0 0 1-9 9"],
    send: ["M12 19V5", "m5 12 7-7 7 7"],
    archive: ["M3 4h18v4H3z", "M5 8v11a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8", "M10 12h4"],
    trash: ["M3 6h18", "M8 6V4h8v2", "M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6"],
    menu: ["M4 6h16", "M4 12h16", "M4 18h16"],
    sun: ["c12,12,4", "M12 2v2", "M12 20v2", "m4.9 4.9 1.4 1.4", "m17.7 17.7 1.4 1.4", "M2 12h2", "M20 12h2", "m6.3 17.7-1.4 1.4", "m19.1 4.9-1.4 1.4"],
    moon: ["M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9z"],
    shield: ["M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"],
    globe: ["c12,12,10", "M2 12h20", "M12 2a15 15 0 0 1 0 20 15 15 0 0 1 0-20z"],
    expand: ["M15 3h6v6", "M9 21H3v-6", "M21 3l-7 7", "M3 21l7-7"],
    plan: ["M11 6h10", "M11 12h10", "M11 18h10", "m3 6 1.5 1.5L7 5", "m3 12 1.5 1.5L7 11", "m3 18 1.5 1.5L7 17"],
    mic: ["M12 3a3 3 0 0 0-3 3v5a3 3 0 0 0 6 0V6a3 3 0 0 0-3-3z", "M5 11a7 7 0 0 0 14 0", "M12 18v3"],
    star: ["M12 2.5l2.9 6 6.6.9-4.8 4.6 1.2 6.5L12 17.4l-5.9 3.1 1.2-6.5-4.8-4.6 6.6-.9z"],
  };
  const SVG = "http://www.w3.org/2000/svg";
  D.icon = (name, cls = "") => {
    const s = document.createElementNS(SVG, "svg");
    s.setAttribute("viewBox", "0 0 24 24");
    s.setAttribute("class", `i ${cls}`.trim());
    s.setAttribute("aria-hidden", "true");
    for (const d of ICONS[name] ?? []) {
      let p;
      if (d.startsWith("c")) {
        const [cx, cy, r] = d.slice(1).split(",");
        p = document.createElementNS(SVG, "circle");
        p.setAttribute("cx", cx); p.setAttribute("cy", cy); p.setAttribute("r", r);
      } else {
        p = document.createElementNS(SVG, "path");
        p.setAttribute("d", d);
      }
      s.append(p);
    }
    if (name === "running") s.classList.add("spin");
    return s;
  };

  // The Decurion mark: a leader's chevron above a squad of ten.
  D.mark = (size = 28) => {
    const s = document.createElementNS(SVG, "svg");
    s.setAttribute("viewBox", "0 0 32 32");
    s.setAttribute("width", size); s.setAttribute("height", size);
    s.setAttribute("aria-hidden", "true");
    s.innerHTML = '<rect width="32" height="32" rx="8" fill="var(--accent)"/>' +
      '<path d="M9 13.5 16 7l7 6.5" fill="none" stroke="#fff" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round"/>' +
      [7, 11.5, 16, 20.5, 25].map((x) => `<circle cx="${x}" cy="19" r="1.55" fill="#fff"/><circle cx="${x}" cy="24" r="1.55" fill="#fff"/>`).join("");
    return s;
  };

  // The phone's bottom tabs, the same on the inbox and the goals page so the
  // phone feels like one app. Inbox, Board and Settings are hash routes of
  // /inbox (a link to another hash of the page you are on does not reload
  // it); Goals and Plan (the master plan) are pages of their own. Each page
  // styles .tabbar in its own look.
  // The badges start hidden; the page fills them in.
  D.phoneTabs = (current) => D.el("nav", { className: "tabbar", "aria-label": "Sections" },
    D.el("div", { className: "tabs" }, [
      ["inbox", "/inbox#inbox", "Inbox", "inbox", "badge"],
      ["board", "/inbox#board", "Board", "board"],
      ["goals", "/goals", "Goals", "send", "goals-badge"],
      ["plan", "/plan", "Plan", "plan"],
      ["settings", "/inbox#settings", "Settings", "settings"],
    ].map(([tab, href, label, icon, badge]) => D.el("a", { className: "tab", id: `tab-${tab}`, href, "aria-current": tab === current ? "page" : null },
      D.el("span", { className: "ind" }, D.icon(icon), badge && D.el("span", { className: "badge", id: badge, hidden: true })),
      D.el("span", {}, label)))));

  // ---------- States ----------
  // One word, one symbol and one color per state, everywhere.
  D.STATES = {
    queued: { word: "Queued", col: "Queued" },
    running: { word: "Running", col: "Running" },
    needs_you: { word: "Needs you", col: "Needs you" },
    in_review: { word: "In review", col: "In review" },
    done: { word: "Done", col: "Done" },
    failed: { word: "Failed", col: "Failed" },
  };
  D.ORDER = ["needs_you", "running", "in_review", "queued", "done", "failed"];
  D.pill = (state) => D.el("span", { className: `pill ${state}` }, D.icon(state), D.STATES[state]?.word ?? state);

  // What the owner can do from each state. `confirm` marks the ones that
  // stop work, which always ask first.
  D.MOVES = {
    in_review: [
      { to: "done", label: "Accept", kind: "primary", done: "Accepted. Decurion is sending it to GitHub for you to merge there." },
      { to: "queued", label: "Send back", kind: "", done: "Sent back. The agent will pick up where it left off." },
      { to: "failed", label: "Reject", kind: "danger", done: "Rejected.",
        confirm: (t) => [`Reject "${t.title}"?`, "Its changes stay on its branch, but the work is closed.", "Reject work", "Keep it"] },
    ],
    needs_you: [
      { to: "queued", label: "Continue", kind: "primary", done: "Continuing. The agent will pick up where it left off." },
      { to: "failed", label: "Stop", kind: "danger", done: "Stopped.",
        confirm: (t) => [`Stop "${t.title}"?`, "Changes so far are kept on its branch.", "Stop work", "Keep it"] },
    ],
    failed: [{ to: "queued", label: "Retry", kind: "primary", done: "Queued again. The agent will pick up where it left off." }],
    queued: [
      { to: "failed", label: "Cancel", kind: "danger", done: "Cancelled.",
        confirm: (t) => [`Cancel "${t.title}"?`, "It has not finished. Anything it saved stays on its branch.", "Cancel work", "Keep it"] },
    ],
  };

  // ---------- Talking to the hub ----------
  D.signedOutText = "Not signed in. Open the login link the hub printed when it started.";
  const offlineHooks = [];
  D.offline = false;
  D.onOffline = (fn) => offlineHooks.push(fn);
  function setOffline(v) {
    if (v === D.offline) return;
    D.offline = v;
    if (v) D.offlineSince = new Date();
    document.body.classList.toggle("offline", v);
    for (const fn of offlineHooks) fn(v);
  }
  D.api = async (path, opts = {}) => {
    let r;
    try {
      r = await fetch("/api" + path, { headers: { "content-type": "application/json" }, ...opts });
    } catch (_) {
      setOffline(true);
      throw new Error("Can't reach your PC right now.");
    }
    setOffline(false);
    if (r.status === 401) { const e = new Error(D.signedOutText); e.signedOut = true; throw e; }
    if (!r.ok) throw new Error((await r.text()) || `The hub said ${r.status}.`);
    return r.status === 204 ? null : r.json();
  };

  // Events come in pages of 500; fetch them all so the outcome is never cut off.
  D.events = async (id, after = 0) => {
    let ev = [], page;
    do {
      page = await D.api(`/threads/${id}/events?after=${ev.length ? ev[ev.length - 1].id : after}`);
      ev = ev.concat(page);
    } while (page.length === 500);
    return ev;
  };
  D.tail = (id, n = 20) => D.api(`/threads/${id}/events?tail=${n}`);

  // The newest thing the agent said, whether its final summary or a message.
  // Other events, such as a blocked website visit, are never its words.
  // A block of questions for the owner reads as just its questions here;
  // D.rawWords keeps the block for the screens that draw its buttons.
  D.lastWords = (ev) => D.questions(D.rawWords(ev)).plain;
  D.rawWords = (ev) => {
    const last = [...(ev ?? [])].reverse().find((r) => r.event.kind === "finished" || r.event.kind === "message");
    return last ? (last.event.summary ?? last.event.text ?? "") : "";
  };

  // One line for a card: what the work is doing now, or asking.
  D.cardLine = (t, ev) => {
    if (t.state === "queued") return t.session_id ? "Waiting to pick up where it left off" : "Waiting to start";
    if (!ev?.length) return t.state === "running" ? "Starting up" : "";
    if (t.state === "running") {
      // A blocked website visit is a side note, not what the work is doing.
      const e = [...ev].reverse().find((r) => r.event.kind !== "blocked")?.event;
      if (!e) return "Working";
      if (e.kind === "started") return "Starting up its box";
      if (e.kind === "limit_hit") return "Paused: the Claude Max account ran out of usage";
      if (e.kind === "tool") return e.summary ? `${e.name}: ${e.summary}` : `Using ${e.name}`;
      if (e.kind === "message") return e.text;
    }
    return D.lastWords(ev);
  };

  // ---------- Words and numbers ----------
  D.money = (n) => "$" + (n ?? 0).toFixed(2);
  D.clock = (iso) => new Date(iso).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  D.ago = (iso) => {
    const s = Math.max(0, (Date.now() - new Date(iso).getTime()) / 1000);
    if (s < 45) return "just now";
    if (s < 3600) return `${Math.round(s / 60)} min ago`;
    if (s < 86400) return `${Math.round(s / 3600)} h ago`;
    const d = new Date(iso);
    return d.toLocaleDateString([], { month: "short", day: "numeric" });
  };
  // Who does a piece of work: ChatGPT or Grok, or Claude on the app's account.
  D.aiLabel = (app, t) => (t?.ai === "chatgpt" ? "ChatGPT" : t?.ai === "grok" ? "Grok"
    : !app ? "" : app.pay_per_use ? "Claude pay-per-use" : `Claude ${app.profile}`);
  D.plural = (n, one, many = one + "s") => `${n} ${n === 1 ? one : many}`;
  // "Blocked a visit to evil.net (not on this app's list)". The reason comes
  // from the gate in plain words; a leading "is" reads badly in brackets.
  D.blockedWhy = (e) => String(e.reason ?? "").trim().replace(/^is /, "");
  D.blockedText = (e) => {
    const why = D.blockedWhy(e);
    return `Blocked a visit to ${e.host || "a website"}${why ? ` (${why})` : ""}`;
  };

  // ---------- Simple and Pro view ----------
  const store = {
    get(k) { try { return localStorage.getItem(k); } catch (_) { return null; } },
    set(k, v) { try { localStorage.setItem(k, v); } catch (_) { /* private window */ } },
  };
  D.store = store;
  const proHooks = [];
  D.pro = store.get("decurion.pro") === "1";
  D.setPro = (v) => { D.pro = v; store.set("decurion.pro", v ? "1" : "0"); document.body.classList.toggle("pro", v); for (const fn of proHooks) fn(v); };
  D.onPro = (fn) => proHooks.push(fn);
  D.switchEl = (label, get, set, onChange) => {
    const b = D.el("button", { type: "button", className: "switch", role: "switch" },
      D.el("span", { className: "track" }), D.el("span", {}, label), D.el("span", { className: "state" }));
    const paint = () => { b.setAttribute("aria-checked", get() ? "true" : "false"); b.lastChild.textContent = get() ? "On" : "Off"; };
    b.onclick = () => { set(!get()); paint(); };
    if (onChange) onChange(paint);
    paint();
    return b;
  };

  // ---------- Toasts and confirmations ----------
  // `undo`, when given, adds an Undo button that runs it and closes the toast.
  D.toast = (text, icon = "done", undo = null) => {
    let box = document.querySelector(".toasts");
    if (!box) { box = D.el("div", { className: "toasts", role: "status", "aria-live": "polite" }); document.body.append(box); }
    const t = D.el("div", { className: "toast" }, D.icon(icon), D.el("span", { className: "grow" }, text));
    if (undo) {
      const b = D.el("button", { type: "button", className: "toast-undo" }, "Undo");
      b.onclick = () => { t.remove(); undo(); };
      t.append(b);
    }
    box.append(t);
    setTimeout(() => t.remove(), undo ? 9000 : 6000);
  };

  // Resolves true only when the owner presses the yes button.
  D.confirm = ([title, text, yes, no]) => new Promise((resolve) => {
    const d = D.el("dialog", { className: "confirm", "aria-labelledby": "cf-t" },
      D.el("div", { className: "body" }, D.el("h2", { id: "cf-t" }, title), D.el("p", {}, text)));
    const keep = D.el("button", { type: "button", className: "btn" }, no);
    const go = D.el("button", { type: "button", className: "btn danger" }, yes);
    d.append(D.el("div", { className: "row" }, keep, go));
    let answer = false;
    keep.onclick = () => d.close();
    go.onclick = () => { answer = true; d.close(); };
    d.addEventListener("close", () => { d.remove(); resolve(answer); });
    document.body.append(d);
    d.showModal();
    keep.focus();
  });

  D.moveButtons = (t, { size = "", onMoved, onError, quiet = false } = {}) => (D.MOVES[t.state] ?? []).map((m) => {
    const cls = quiet ? (m.kind === "danger" ? "btn quiet danger-text" : "btn quiet") : m.kind === "danger" ? "btn quiet danger-text" : `btn ${m.kind}`;
    const b = D.el("button", { type: "button", className: `${cls} ${size}`.trim() }, m.label);
    b.onclick = async (ev) => {
      ev.stopPropagation();
      if (D.offline) return;
      b.disabled = true;
      const ok = await D.move(t, m, onError);
      b.disabled = false;
      if (ok && onMoved) onMoved(m);
    };
    return b;
  });

  let apps = {};
  D.setApps = (list) => { apps = Object.fromEntries(list.map((a) => [a.id, a])); };
  D.appName = (id) => apps[id]?.name ?? "";

  document.body.classList.toggle("pro", D.pro);
})();

// ---------- The agent's words: light formatting, its questions, next steps ----------
// Agents write their summaries in a little markdown. D.richText draws the
// parts they use (paragraphs, lists, headings, bold, code, quotes, simple
// tables) with D.el and text nodes only: nothing an agent writes ever
// becomes HTML, and its links stay plain text. D.questions pulls out what
// the agent asks the owner, so screens can put it first, and D.askBox draws
// it with one-tap answers. An answer goes the same way as the message box's
// "Do this next": the work goes back to its agent with it. Nothing here
// moves work any other way.
(() => {
  const { el, icon } = D;
  const FENCE = /^\s*(`{3,}|~{3,})\s*([\w-]*)\s*$/;
  const ROW = /^\s*\|.*\|\s*$/;
  const RULE = /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$/;
  const cells = (line) => line.trim().replace(/^\|/, "").replace(/\|$/, "").split("|").map((c) => c.trim());
  const spaces = (s) => s.replace(/\t/g, "    ").length;

  // Lines into blocks: h, p, li (indent, ord, num), quote, code, table, hr.
  function blocks(text) {
    const lines = String(text ?? "").replace(/\r\n?/g, "\n").split("\n");
    const out = [];
    let open = null; // the block that a next plain line joins
    for (let i = 0; i < lines.length; i++) {
      const line = lines[i];
      let m = line.match(FENCE);
      if (m) {
        const body = [];
        let j = i + 1;
        for (; j < lines.length; j++) {
          const e = lines[j].match(/^\s*(`{3,}|~{3,})\s*$/);
          if (e && e[1][0] === m[1][0] && e[1].length >= m[1].length) break;
          body.push(lines[j]);
        }
        out.push({ k: "code", lang: m[2].toLowerCase(), text: body.join("\n") });
        i = j;
        open = null;
        continue;
      }
      if (!line.trim()) { open = null; continue; }
      if ((m = line.match(/^\s{0,3}#{1,6}\s+(.*?)[\s#]*$/))) { out.push({ k: "h", text: m[1] }); open = null; continue; }
      if (/^\s{0,3}([-*_])(\s*\1){2,}\s*$/.test(line)) { out.push({ k: "hr" }); open = null; continue; }
      if (ROW.test(line) && i + 1 < lines.length && RULE.test(lines[i + 1])) {
        const rows = [];
        let j = i + 2;
        while (j < lines.length && ROW.test(lines[j])) rows.push(cells(lines[j++]));
        out.push({ k: "table", head: cells(line), rows });
        i = j - 1;
        open = null;
        continue;
      }
      if ((m = line.match(/^(\s*)([-*+]|\d{1,9}[.)])\s+(.*)$/))) {
        const ord = /\d/.test(m[2]);
        out.push(open = { k: "li", indent: spaces(m[1]), ord, num: ord ? parseInt(m[2], 10) : 0, text: m[3] });
        continue;
      }
      if ((m = line.match(/^\s*>\s?(.*)$/))) {
        if (open?.k === "quote") open.text += "\n" + m[1];
        else out.push(open = { k: "quote", text: m[1] });
        continue;
      }
      if (open) open.text += "\n" + line.trim();
      else out.push(open = { k: "p", text: line.trim() });
    }
    return out;
  }

  // `code`, **bold**, __bold__, [text](address) as plain text, *italic*.
  // No lookbehind: older phone browsers would refuse the whole file.
  const INLINE = /`([^`]+)`|\*\*(\S(?:.*?\S)?)\*\*|__(\S(?:.*?\S)?)__|\[([^\]]+)\]\(([^)\s]+)\)|(^|[^\w*])\*([^\s*](?:[^*]*?[^\s*])?)\*(?![\w*])/g;
  function inline(s, depth = 0) {
    const out = [];
    let last = 0;
    for (const m of s.matchAll(INLINE)) {
      const at = m.index + (m[7] !== undefined ? m[6].length : 0);
      if (at > last) out.push(s.slice(last, at));
      if (m[1] !== undefined) out.push(el("code", {}, m[1]));
      else if (m[2] !== undefined || m[3] !== undefined) {
        const inner = m[2] ?? m[3];
        out.push(el("strong", {}, ...(depth < 2 ? inline(inner, depth + 1) : [inner])));
      } else if (m[4] !== undefined) out.push(...inline(m[4], depth + 1), m[4] === m[5] ? "" : ` (${m[5]})`);
      else out.push(el("em", {}, m[7]));
      last = m.index + m[0].length;
    }
    if (last < s.length) out.push(s.slice(last));
    return out;
  }
  const lines = (text) => text.split("\n").flatMap((l, i) => (i ? [el("br"), ...inline(l)] : inline(l)));

  // D.richText(text) → a <div class="rich"> that is safe to put anywhere.
  D.richText = (text) => {
    const root = el("div", { className: "rich" });
    let stack = []; // open lists: { indent, list, ord }
    for (const b of blocks(text)) {
      if (b.k !== "li") stack = [];
      if (b.k === "h") root.append(el("p", { className: "rh" }, el("strong", {}, ...inline(b.text))));
      else if (b.k === "p") root.append(el("p", {}, ...lines(b.text)));
      else if (b.k === "quote") root.append(el("blockquote", {}, ...lines(b.text)));
      else if (b.k === "hr") root.append(el("hr"));
      else if (b.k === "code") root.append(el("pre", {}, el("code", {}, b.text)));
      else if (b.k === "table") {
        const n = Math.max(b.head.length, ...b.rows.map((r) => r.length));
        const tr = (r, tag) => el("tr", {}, ...Array.from({ length: n }, (_, i) => el(tag, {}, ...inline(r[i] ?? ""))));
        root.append(el("div", { className: "rtable" }, el("table", {},
          el("thead", {}, tr(b.head, "th")), el("tbody", {}, ...b.rows.map((r) => tr(r, "td"))))));
      } else {
        while (stack.length && b.indent < stack[stack.length - 1].indent) stack.pop();
        let top = stack[stack.length - 1];
        if (top && b.indent === top.indent && top.ord !== b.ord) { stack.pop(); top = stack[stack.length - 1]; }
        if (!top || b.indent > top.indent) {
          const list = b.ord ? el("ol", b.num > 1 ? { start: b.num } : {}) : el("ul");
          const host = top?.list.lastElementChild;
          if (host && stack.length < 4) host.append(list); else root.append(list);
          stack.push(top = { indent: b.indent, list, ord: b.ord });
        }
        top.list.append(el("li", {}, ...lines(b.text)));
      }
    }
    return root;
  };

  // ---------- What the agent asks ----------
  // Agents are told (RULES in the runner) to end with a ```questions block
  // of JSON: [{ "text": "...", "choices": ["...", "..."] }]. Only the last
  // block counts and it is checked strictly; a bad one gives no questions
  // and leaves the text as it was. Without a block, sentences that ask
  // something are picked out, with no choices.
  const MAX_Q = 3, MAX_CHOICES = 4, Q_LEN = 300, C_LEN = 120;
  const clip = (s, n) => {
    const t = String(s).replace(/\s+/g, " ").trim();
    return t.length > n ? t.slice(0, n - 1).trimEnd() + "…" : t;
  };
  const plain = (s) => s.replace(/\*\*|__|`/g, "").replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, "$1 ($2)");
  const BLOCK = /^[ \t]*`{3}[ \t]*questions[ \t]*\n([\s\S]*?)\n[ \t]*`{3}[ \t]*$/gm;
  function parseBlock(src) {
    let v;
    try { v = JSON.parse(src); } catch (_) { return null; }
    if (!Array.isArray(v) || !v.length || v.length > MAX_Q) return null;
    const out = [];
    for (const q of v) {
      if (!q || typeof q !== "object" || Array.isArray(q) || typeof q.text !== "string" || !q.text.trim()) return null;
      const ch = q.choices ?? [];
      if (!Array.isArray(ch) || ch.length === 1 || ch.length > MAX_CHOICES) return null;
      if (ch.some((c) => typeof c !== "string" || !c.trim())) return null;
      out.push({ text: clip(q.text, Q_LEN), choices: [...new Set(ch.map((c) => clip(c, C_LEN)))] });
    }
    return out;
  }
  // Words that mean the agent wants something from the owner. "No
  // questions" is the opposite, so it never counts.
  const ASKS = /\b(questions?|should I|shall I|do you want|would you like|worth checking|let me know|please confirm|your call|up to you|decide|which (one|do you))\b/i;
  const NOT_ASKS = /\bno (open |other |further )?questions\b/i;
  const ASKS_HEAD = /\b(questions?|decisions?|for you|to decide|open items?)\b/i;
  const sentences = (t) => t.match(/[^.?!]+(?:[.?!]+["')\]]*|$)/g)?.map((x) => x.trim()).filter(Boolean) ?? [];
  function guess(text) {
    const under = [], loose = [];
    let heading = false;
    for (const b of blocks(text)) {
      if (b.k === "code" || b.k === "hr" || b.k === "table") continue;
      if (b.k === "h") { heading = ASKS_HEAD.test(b.text) && !NOT_ASKS.test(b.text); continue; }
      const t = plain(b.text).replace(/\s+/g, " ").trim();
      if (!t) continue;
      if (heading) { under.push(t); continue; }
      const asked = sentences(t).filter((x) => /\?["')\]]*$/.test(x));
      if (asked.length) loose.push(...asked);
      else if (ASKS.test(t) && !NOT_ASKS.test(t)) loose.push(t);
    }
    // Real questions beat sentences that only mention one.
    const real = loose.filter((t) => /\?["')\]]*$/.test(t));
    const pick = under.length ? under.slice(0, MAX_Q) : (real.length ? real : loose).slice(-MAX_Q);
    // A section label such as "(e)" in front means nothing on its own.
    return [...new Set(pick.map((t) => clip(t.replace(/^\(?[a-z0-9]{1,2}\)\s+/i, ""), Q_LEN)))].map((text) => ({ text, choices: [] }));
  }
  // D.questions(text) → { questions: [{ text, choices }], rest, fromBlock, plain }.
  // `rest` is the text without the block; `plain` is for one-line uses.
  D.questions = (text) => {
    const s = String(text ?? "");
    let last = null;
    for (const m of s.matchAll(BLOCK)) last = m;
    if (last) {
      const qs = parseBlock(last[1]);
      if (!qs) return { questions: [], rest: s, fromBlock: false, plain: s };
      const rest = (s.slice(0, last.index) + s.slice(last.index + last[0].length)).replace(/\n{3,}/g, "\n\n").trim();
      return { questions: qs, rest, fromBlock: true, plain: [rest, ...qs.map((q) => q.text)].filter(Boolean).join("\n\n") };
    }
    return { questions: guess(s), rest: s, fromBlock: false, plain: s };
  };

  // D.askBox(thread, questions, { onSent, onTalk(prefill), guessed }) → the "The
  // agent asks" box. With one question a tap on a choice is the answer; with
  // more, the owner picks for each and sends once. Every button stays off
  // while an answer is on its way and after it went, and D.send carries a
  // receipt, so an answer is never sent twice.
  //
  // Voice (voice.js, when the page loads it and the browser can) adds a
  // speaker that reads the questions and their choices, and "Say your
  // answer". A spoken answer only ever picks or shows a choice: the owner
  // still taps the choice, "Send this answer" or "Send answers". It never
  // sends by itself, because a misheard word must not become an answer.
  const picks = new Map();
  const ended = (s) => (/[.?!]$/.test(s.trim()) ? s.trim() : `${s.trim()}.`);
  // The look of the voice pieces here and in the work panel. voice.js styles
  // its own buttons; this only places them.
  document.head.append(el("style", {}, `
    .asks-title .voice-speak { margin-left: auto; flex: none; white-space: nowrap; }
    .asks-heard { display: grid; gap: 8px; justify-items: start; font-size: 14px; overflow-wrap: anywhere; }
    .asks-heard[hidden] { display: none; }
    .asks-heard .btn { min-height: 44px; }
    .asks .btn[aria-pressed="true"] svg.i { color: var(--red); }
    .section-title.with-voice { display: flex; align-items: center; gap: 8px; }
    .section-title.with-voice .voice-speak { margin-left: auto; min-height: 28px; padding-top: 2px; padding-bottom: 2px; }
    .talk .row .talk-voice { order: 1; margin-left: auto; display: flex; }
    .talk .row .talk-voice:empty { display: none; }
    @media (max-width: 640px) { .talk .row .talk-voice { order: 0; flex: 1 1 100%; justify-content: flex-end; } }`));
  D.askBox = (t, qs, opts = {}) => {
    const key = `${t.id}|${t.updated_at}`;
    const mine = picks.get(key) ?? {};
    picks.set(key, mine);
    const multi = qs.length > 1 && qs.some((q) => q.choices.length);
    const all = [];
    let sending = false, sent = false;
    // Voice is read when the box is drawn, never at load: voice.js loads after this file.
    const V = window.Voice;
    const canHear = !!V?.canListen?.() && qs.some((q) => q.choices.length);
    let hearing = null, said = null; // the live listen, and the one choice a spoken answer matched
    const err = el("p", { className: "field-error", role: "alert" });
    const spoken = () => qs.map((q, i) => (qs.length > 1 ? `Question ${i + 1}. ` : "") + ended(q.text) +
      (q.choices.length ? ` ${canHear ? "You can say" : "The choices are"}: ${q.choices.join(", or ")}.` : "")).join(" ");
    const speaker = V?.canSpeak?.() ? V.speaker(spoken, { label: "Read aloud" }) : null;
    const box = el("section", { className: `asks${t.state === "needs_you" ? " needs_you" : ""}`, "aria-label": "The agent asks" },
      el("div", { className: "asks-title" }, icon("question"), opts.guessed ? "The agent seems to ask"
        : qs.length > 1 ? `The agent asks ${qs.length} things` : "The agent asks", speaker));
    const answer = async (text) => {
      if (sending || sent) return;
      hearing?.stop();
      err.textContent = "";
      if (D.offline) { err.textContent = "Your PC isn't answering. Answer when it's back."; return; }
      sending = true;
      paint();
      try {
        await D.send(`/threads/${t.id}/messages`, { kind: "follow_up", text });
        sent = true;
        picks.delete(key);
        D.toast(`Sent. "${t.title}" went back to the agent with your answer.`, "send");
        opts.onSent?.();
      } catch (e) {
        err.textContent = `Your answer did not go through. ${e.message}`;
      } finally {
        sending = false;
        paint();
      }
    };
    const choiceBtns = [];
    qs.forEach((q, i) => {
      const row = el("div", { className: "aq" }, el("p", { className: "aq-t" }, qs.length > 1 ? `${i + 1}. ${q.text}` : q.text));
      if (q.choices.length) {
        const btns = q.choices.map((c) => {
          const b = el("button", { type: "button", className: "btn choice" }, c);
          if (multi) b.setAttribute("aria-pressed", "false");
          b.onclick = (ev) => {
            ev.stopPropagation();
            if (!multi) { answer(`${q.text} — ${c}`); return; }
            mine[i] = mine[i] === c ? undefined : c;
            paint();
          };
          choiceBtns.push([b, i, c]);
          all.push(b);
          return b;
        });
        row.append(el("div", { className: "aq-choices", role: "group", "aria-label": `Answers to: ${q.text}` }, ...btns));
      }
      box.append(row);
    });
    const foot = el("div", { className: "asks-foot" });
    let sendAll = null;
    if (multi) {
      sendAll = el("button", { type: "button", className: "btn primary" }, icon("send"), "Send answers");
      sendAll.onclick = (ev) => {
        ev.stopPropagation();
        const lines = qs.map((q, i) => mine[i] && `${q.text} — ${mine[i]}`).filter(Boolean);
        if (lines.length) answer(lines.join("\n"));
      };
      all.push(sendAll);
      foot.append(sendAll);
    }
    const heardBox = el("div", { className: "asks-heard", role: "status", "aria-live": "polite", hidden: true });
    const showHeard = (...kids) => { kids = kids.filter(Boolean); heardBox.replaceChildren(...kids); heardBox.hidden = !kids.length; };
    const button = (cls, kids, onclick) => {
      const b = el("button", { type: "button", className: `btn ${cls}` }, ...kids);
      b.onclick = (ev) => { ev.stopPropagation(); onclick(); };
      all.push(b);
      return b;
    };
    // The own-words box, started with what was heard, for when no choice fits.
    const talkWith = (words) => opts.onTalk(qs.length === 1 ? `${qs[0].text}\nMy answer: ${words}`
      : `${qs.map((q, i) => `${i + 1}. ${q.text}`).join("\n")}\n\nMy answer: ${words}`);
    // What a spoken answer does: shows what was heard and picks the choice it
    // clearly means, or says to tap one. Nothing is sent from here.
    const heard = (words) => {
      const quote = `Heard: "${words}".`;
      if (!multi) {
        const q = qs[0], i = V.matchChoice(words, q.choices);
        if (i >= 0) {
          said = q.choices[i];
          showHeard(el("p", {}, `${quote} That sounds like "${said}". Tap Send this answer, or tap another choice.`),
            button("primary", [icon("send"), "Send this answer"], () => answer(`${q.text} — ${said}`)));
          paint();
          return;
        }
      } else {
        const hits = qs.map((q, i) => [i, q.choices.length ? V.matchChoice(words, q.choices) : -1]).filter(([, c]) => c >= 0);
        if (hits.length === 1) {
          const [i, c] = hits[0];
          mine[i] = qs[i].choices[c];
          showHeard(el("p", {}, `${quote} Picked "${mine[i]}" for question ${i + 1}. Check your picks, then tap Send answers.`));
          paint();
          return;
        }
      }
      showHeard(el("p", {}, opts.onTalk ? `${quote} Tap a choice, or use Answer in your own words.` : `${quote} Tap a choice.`),
        opts.onTalk && button("", ["Answer with these words"], () => talkWith(words)));
      paint();
    };
    let sayBtn = null;
    if (canHear) {
      sayBtn = button("quiet", [], async () => {
        if (hearing) { hearing.stop(); return; }
        if (sending || sent || D.offline) return;
        said = null;
        showHeard(el("p", {}, "Listening. Say your answer."));
        hearing = window.Voice.listen({ continuous: false,
          onText: (fin, interim) => showHeard(el("p", {}, `Hearing: "${[fin, interim].filter(Boolean).join(" ")}"`)) });
        paint();
        const words = await hearing.done;
        hearing = null;
        if (words && !sending && !sent) heard(words);
        else { showHeard(); paint(); }
      });
      foot.append(sayBtn);
    }
    if (opts.onTalk) {
      const own = el("button", { type: "button", className: "btn quiet" }, "Answer in your own words");
      own.onclick = (ev) => {
        ev.stopPropagation();
        opts.onTalk(qs.map((q) => `${q.text}\nMy answer: `).join("\n\n"));
      };
      all.push(own);
      foot.append(own);
    }
    if (foot.childNodes.length) box.append(foot);
    box.append(heardBox, el("p", { className: "hint" }, "Your answer goes back to the agent, and it picks up where it left off."), err);
    function paint() {
      for (const [b, i, c] of choiceBtns) {
        if (multi) b.setAttribute("aria-pressed", mine[i] === c ? "true" : "false");
        else if (said != null) b.setAttribute("aria-pressed", c === said ? "true" : "false");
        else b.removeAttribute("aria-pressed");
      }
      if (sayBtn) {
        sayBtn.replaceChildren(icon("mic"), hearing ? "Stop listening" : "Say your answer");
        sayBtn.setAttribute("aria-pressed", hearing ? "true" : "false");
      }
      for (const b of all) b.disabled = sending || sent || D.offline;
      if (sendAll && !sending && !sent) sendAll.disabled = D.offline || !qs.some((_, i) => mine[i]);
    }
    paint();
    return box;
  };

  // ---------- Fix what it found ----------
  // New work made from a summary, through the one "What do you want?" box
  // (D.newWork, compose.js). Only offered where that box exists. The text
  // stays well under the 8000 characters a goal may hold.
  const FIX_MAX = 6000;
  D.canFix = () => typeof D.newWork === "function";
  D.fixText = (t, said) => {
    const head = `Fix what "${t.title}" found:\n\n`;
    let body = String(said ?? "").trim();
    if (head.length + body.length > FIX_MAX) {
      const cut = body.slice(0, FIX_MAX - head.length - 80);
      body = cut.slice(0, Math.max(cut.lastIndexOf("\n"), cut.length - 200)).trimEnd() +
        `\n\n(Cut short. The full summary is in "${t.title}".)`;
    }
    return head + body;
  };
  D.fixWork = (t, said) => { if (D.canFix()) D.newWork({ app_id: t.app_id, text: D.fixText(t, said) }); };
})();

// ---------- Crash-safety additions ----------
// A receipt and one retry for every change, moves that name the state you
// saw, Stop for running work, the task tree, messages to the agent, and the
// work panel that shows them.
(() => {
  const { el, icon } = D;
  const VIA = { steer: "change of course", follow_up: "next", note: "note" };

  // ---------- Receipts ----------
  D.receipt = () => {
    if (window.crypto?.randomUUID) return crypto.randomUUID();
    const b = new Uint8Array(16);
    if (window.crypto?.getRandomValues) crypto.getRandomValues(b);
    else for (let i = 0; i < b.length; i++) b[i] = Math.floor(Math.random() * 256);
    return Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
  };
  // Every change carries a receipt. If the PC can't be reached, the same
  // receipt goes once more, so the hub never does the same thing twice.
  D.send = async (path, body) => {
    const opts = { method: "POST", body: JSON.stringify({ ...body, receipt: D.receipt() }) };
    try {
      return await D.api(path, opts);
    } catch (e) {
      // D.api marks the page offline only when the request never got an answer.
      if (!D.offline) throw e;
      await new Promise((ok) => setTimeout(ok, 1000));
      return D.api(path, opts);
    }
  };

  // ---------- What is part of what ----------
  let all = [];
  D.setThreads = (list) => { all = list ?? []; };
  const active = (t) => t.state !== "done" && t.state !== "failed";
  const kidsOf = (id) => all.filter((k) => k.parent_id === id);
  // The smaller tasks that stop too when this one stops (background ones keep going).
  const stopsToo = (id) => kidsOf(id).filter((k) => !k.background).flatMap((k) => [...(active(k) ? [k] : []), ...stopsToo(k.id)]);
  const alsoStops = (t) => {
    const n = stopsToo(t.id).length;
    return n ? ` ${D.plural(n, "smaller task")} inside it ${n === 1 ? "stops" : "stop"} too. Ones set to keep going on their own don't.` : "";
  };
  D.kidsOf = kidsOf;
  D.treeLine = (t) => {
    const notes = [], n = kidsOf(t.id).length;
    if (t.parent_id != null) notes.push(`Part of: ${all.find((p) => p.id === t.parent_id)?.title ?? "other work"}`);
    if (t.parent_id != null && t.background) notes.push("keeps going on its own");
    if (n) notes.push(D.plural(n, "smaller task"));
    return notes.join(" · ");
  };

  // ---------- Moves ----------
  for (const list of Object.values(D.MOVES)) {
    for (const m of list) {
      if (m.to !== "failed" || !m.confirm) continue;
      const ask = m.confirm;
      m.confirm = (t) => { const [title, text, ...rest] = ask(t); return [title, text + alsoStops(t), ...rest]; };
    }
  }
  D.MOVES.running = [{ to: "failed", label: "Stop", kind: "danger", done: "Stopped.",
    confirm: (t) => [`Stop "${t.title}"?`, "The agent stops now. Changes so far are kept on its branch." + alsoStops(t), "Stop work", "Keep it"] }];
  // Moves a thread, asking first when the move stops work, with a receipt and
  // the state you saw: a tap on a screen that
  // has gone stale (Stop on work that has since finished) is refused.
  D.move = async (t, m, onError) => {
    if (m.confirm && !(await D.confirm(m.confirm(t)))) return false;
    try {
      await D.send(`/threads/${t.id}/move`, { to: m.to, expect: t.state });
      D.toast(m.done, m.to === "failed" ? "close" : "done");
      return true;
    } catch (e) {
      const msg = `"${m.label}" did not go through. ${e.message}`;
      if (onError) onError(msg); else D.toast(msg, "alert");
      return false;
    }
  };

  // ---------- Dismissing finished things ----------
  // Done or failed work, and finished goals, can be dismissed: hidden into a
  // Dismissed view, never deleted, and brought back with one tap. The hub
  // refuses anything still going (dismiss.rs).
  D.finished = (t) => t.state === "done" || t.state === "failed";
  // kind is "threads" or "goals"; items are the things shown, with titles.
  D.dismiss = async (kind, items, { dismissed = true, onChanged } = {}) => {
    const list = (items ?? []).filter(Boolean);
    if (!list.length || D.offline) return false;
    const what = kind === "goals" ? ["goal", "goals"] : ["piece of work", "pieces of work"];
    try {
      await D.api(`/${kind}/dismiss`, { method: "POST", body: JSON.stringify({ ids: list.map((t) => t.id), dismissed }) });
    } catch (e) {
      D.toast(`${dismissed ? "Dismiss" : "Bring back"} did not go through. ${e.message}`, "alert");
      return false;
    }
    const one = list.length === 1 ? `"${list[0].title}"` : D.plural(list.length, ...what);
    if (dismissed) {
      D.toast(`Dismissed ${one}. ${list.length === 1 ? "It's" : "They're"} kept under Dismissed if you need ${list.length === 1 ? "it" : "them"} again.`, "archive",
        () => D.dismiss(kind, list, { dismissed: false, onChanged }));
    } else D.toast(`${one[0].toUpperCase()}${one.slice(1)} ${list.length === 1 ? "is" : "are"} back.`, "done");
    if (onChanged) onChanged();
    return true;
  };

  // ---------- Sent to GitHub ----------
  // Accepted work goes to GitHub as a branch, with a pull request or a link
  // that opens one. Only GitHub links are ever drawn as links.
  const ghLink = (u) => typeof u === "string" && u.startsWith("https://github.com/");
  // One short line, for cards that are buttons themselves.
  D.githubText = (g) => {
    if (!g) return "";
    if (g.state === "waiting" || g.state === "sending") return "Sending to GitHub…";
    if (g.state === "failed") return "Not sent to GitHub. Open it to see why and try again.";
    if (g.pull_request) return "Pull request ready on GitHub";
    return ghLink(g.url) ? "On GitHub as a branch. Open it to make the pull request." : `Sent as the branch ${g.branch}`;
  };
  // What happened, with the link or Try again. `small` is for board cards.
  D.github = (t, onChanged, { small = false } = {}) => {
    const g = t.github;
    if (!g) return null;
    const sz = small ? "btn sm quiet" : "btn sm";
    if (g.state === "waiting" || g.state === "sending") {
      return el("p", { className: "gh hint" }, icon("running"), " Sending to GitHub…");
    }
    if (g.state === "sent") {
      const link = ghLink(g.url) && el("a", { className: sz, href: g.url, target: "_blank", rel: "noopener noreferrer" },
        icon("branch"), g.pull_request ? "Open pull request" : "Open pull request on GitHub");
      if (link) link.onclick = (ev) => ev.stopPropagation();
      if (small) return el("p", { className: "gh" }, link || `Sent as the branch ${g.branch}`);
      const words = g.pull_request ? "Sent to GitHub. The pull request is ready for you to look over and merge there."
        : link ? "Sent to GitHub as a branch. Open the pull request in one tap, then merge it there."
        : `Sent to the app's git server as the branch ${g.branch}.`;
      return el("div", { className: "gh notice" }, icon("done"),
        el("div", { className: "grow" }, words, g.error ? el("div", { className: "hint" }, g.error) : null), link || null);
    }
    const again = el("button", { type: "button", className: sz }, icon("retry"), "Try again");
    again.disabled = D.offline;
    again.onclick = async (ev) => {
      ev.stopPropagation();
      if (D.offline) return;
      again.disabled = true;
      try {
        await D.send(`/threads/${t.id}/github`, {});
        D.toast(`Sending "${t.title}" to GitHub again.`, "send");
        if (onChanged) onChanged();
      } catch (e) {
        D.toast(`"Try again" did not go through. ${e.message}`, "alert");
        again.disabled = false;
      }
    };
    if (small) {
      return el("div", { className: "gh-small" },
        el("p", { className: "gh warn" }, icon("alert"), el("b", {}, "Not sent to GitHub")),
        g.error ? el("p", { className: "says", title: g.error }, g.error) : null, again);
    }
    return el("div", { className: "gh notice warn" }, icon("alert"),
      el("div", { className: "grow" }, el("b", {}, "Not sent to GitHub. "), g.error || "",
        el("div", { className: "hint" }, "The work is still accepted. Nothing was lost.")), again);
  };

  // ---------- Log lines ----------
  // Owner and cut-off lines are written by the hub only. Agent messages are
  // always labelled, so an agent can't pass its words off as yours.
  D.ownerLine = (e) => {
    if (e.kind === "owner") return `You (${VIA[e.via] ?? e.via}): ${e.text}`;
    if (e.kind === "interrupted") {
      return `Interrupted while running ${e.name}${e.summary ? `: ${e.summary}` : ""}. ` +
        (e.safe_to_redo ? "Safe to run again." : "Checking what happened before redoing it.");
    }
    return null;
  };

  // ---------- The work panel ----------
  // ui.js's panel, plus the task tree and a message box. The panel's body is
  // kept while it refreshes and the message box is updated in place, so a
  // refresh never takes away what you are typing.
  let apps = {}, panel, cur = null, timer;
  const setApps = D.setApps;
  D.setApps = (list) => { apps = Object.fromEntries(list.map((a) => [a.id, a])); setApps(list); };
  // Whether a cloud worker is set up, so work can move between it and the PC.
  // The board says; elsewhere it is asked once when work is first opened.
  let cloud = null;
  D.setCloud = (v) => { cloud = !!v; };
  D.PLACES = { pc: ["pc", "Your PC"], cloud: ["cloud", "Cloud"] };
  const drafts = {};

  function ensurePanel() {
    if (panel) return panel;
    panel = el("dialog", { className: "panel", "aria-labelledby": "wk-title" });
    document.body.append(panel);
    panel.addEventListener("close", () => {
      clearInterval(timer);
      cur?.view?.close();
      cur = null;
      if (history.state?.work) history.back();
    });
    panel.addEventListener("click", (e) => { if (e.target === panel) panel.close(); });
    window.addEventListener("popstate", () => { if (panel.open && !history.state?.work) panel.close(); });
    return panel;
  }

  function timeline(ev, pro) {
    const ol = el("ol", { className: "timeline" });
    const item = (icCls, ic, ...kids) => ol.append(el("li", { className: icCls === "quiet" ? "quiet" : "" }, el("span", { className: `ic ${icCls}` }, icon(ic)), el("div", { className: "txt" }, ...kids)));
    const when = (r) => el("span", { className: "when" }, D.clock(r.at));
    for (let i = 0; i < ev.length; i++) {
      const r = ev[i], e = r.event;
      if (e.kind === "started") item("", "start", "Started its sealed box", when(r));
      else if (e.kind === "limit_hit") item("bad", "alert", "The Claude Max account ran out of usage", when(r));
      else if (e.kind === "message") item("say", "question", `Agent: ${e.text}`, when(r));
      else if (e.kind === "owner") item("", "send", D.ownerLine(e), when(r));
      else if (e.kind === "interrupted") item("", "alert", D.ownerLine(e), when(r));
      else if (e.kind === "blocked") {
        // The box was turned away from a website. Nothing for the owner to do;
        // the agent carries on. Simple view folds a run of them into one line.
        let j = i;
        while (j + 1 < ev.length && ev[j + 1].event.kind === "blocked") j++;
        const run = ev.slice(i, j + 1);
        if (pro || run.length === 1) {
          for (const b of run) item("quiet", "shield", D.blockedText(b.event), when(b));
        } else {
          const hosts = [...new Set(run.map((b) => b.event.host || "a website"))];
          const names = hosts.length > 3 ? `${hosts.slice(0, 3).join(", ")} and ${D.plural(hosts.length - 3, "other")}` : hosts.join(", ");
          const det = el("details", { className: "fold" },
            el("summary", { className: "tools-sum" }, icon("right"), `Blocked ${D.plural(run.length, "visit")} to ${names}`),
            ...run.map((b) => {
              const why = D.blockedWhy(b.event);
              return el("div", { className: "hint" }, `${b.event.host || "a website"}${why ? ` (${why})` : ""}`, when(b));
            }));
          item("quiet", "shield", det);
        }
        i = j;
      }
      else if (e.kind === "finished") {
        const cost = pro && e.cost_usd != null ? `${D.money(e.cost_usd)} so far` : "";
        item(e.ok ? "ok" : "bad", e.ok ? "done" : "failed", el("b", {}, e.ok ? "Finished" : "Could not finish"), when(r), el("div", {}, D.questions(e.summary).plain), cost && el("div", { className: "hint" }, cost));
      } else if (e.kind === "tool") {
        // Simple view folds a run of tool calls into one line.
        let j = i;
        while (j + 1 < ev.length && ev[j + 1].event.kind === "tool") j++;
        const run = ev.slice(i, j + 1);
        if (pro) {
          for (const t of run) item("", "tool", el("div", { className: "tool" }, el("b", {}, t.event.name), " ", t.event.summary), when(t));
        } else {
          const counts = {};
          for (const t of run) counts[t.event.name] = (counts[t.event.name] ?? 0) + 1;
          const sum = Object.entries(counts).map(([n, c]) => (c > 1 ? `${n} ×${c}` : n)).join(", ");
          item("", "tool", el("details", { className: "fold" },
            el("summary", { className: "tools-sum" }, icon("right"), `Worked: ${D.plural(run.length, "step")} (${sum})`),
            ...run.map((t) => el("div", { className: "tool" }, el("b", {}, t.event.name), " ", t.event.summary))));
        }
        i = j;
      }
    }
    if (!ev.length) ol.append(el("li", {}, el("span", { className: "ic" }, icon("queued")), el("div", { className: "txt hint" }, "No activity yet.")));
    return ol;
  }

  const TALK = [["steer", "Change course now"], ["follow_up", "Do this next"], ["note", "Add a note"]];
  const HOW = {
    running: "Change course now stops its current step and gives it your message. Do this next waits until it finishes what it's doing. A note adds information without stopping it.",
    queued: "This work is waiting for its turn. Whatever you send reaches the agent when it starts.",
  };
  // When the agent is not working, Change course now and Do this next do the
  // same thing (the work goes back to it with your message), so only the
  // second shows, named for what it does from here.
  const IDLE = new Set(["needs_you", "in_review", "failed"]);
  const SEND_IDLE = { needs_you: "Send this answer", in_review: "Send back with this answer", failed: "Retry with this message" };
  const howIdle = (state) => `The agent isn't working on this right now. ${SEND_IDLE[state] ?? "Do this next"} sends the work back to the agent with your message, and it picks up where it left off. A note waits for the next time it works.`;
  function sentText(t, kind) {
    if (kind === "note") return t.state === "running" ? "Note added. The agent sees it at its next step." : "Note added. The agent sees it the next time it works.";
    if (t.state === "queued") return "Sent. The agent gets it when it starts.";
    if (t.state === "running") return kind === "steer" ? "Sent. The agent stops its current step and reads it." : "Sent. The agent gets it when it finishes what it's doing.";
    return `Sent. "${t.title}" went back to the agent with your message.`;
  }

  function talkBox(c) {
    const id = c.id;
    let sending = false;
    const ta = el("textarea", { id: "wk-msg", rows: 3, maxLength: 4000, placeholder: "Your message to the agent",
      "aria-describedby": "wk-msg-how wk-msg-err", dataset: { key: "wk-msg", voice: "", voiceLabel: "Say your message" } });
    ta.value = drafts[id] ?? "";
    const how = el("p", { className: "hint", id: "wk-msg-how" });
    const waiting = el("p", { className: "waiting", hidden: true });
    const err = el("p", { className: "field-error", id: "wk-msg-err", role: "alert" });
    ta.oninput = () => { drafts[id] = ta.value; err.textContent = ""; };
    const btns = TALK.map(([kind, label]) => {
      const b = el("button", { type: "button", className: "btn" }, label);
      b.onclick = async () => {
        const t = c.t, text = ta.value.trim();
        err.textContent = "";
        if (D.offline) { err.textContent = "Your PC isn't answering. Send it when it's back."; return; }
        if (!text) { err.textContent = "Type a message first."; ta.focus(); return; }
        sending = true;
        btns.forEach((x) => (x.disabled = true));
        try {
          // With the Accept on screen and no question from the agent, a
          // plain "yes" is that Accept (the hub decides what counts as yes).
          const res = await D.send(`/threads/${id}/messages`, { kind, text, accept_on_yes: t.state === "in_review" && !c.asked });
          delete drafts[id];
          ta.value = "";
          D.toast(res?.state === "done" ? `Accepted. "${t.title}" is on its way to GitHub.` : sentText(t, kind), "send");
          c.sig = "";
          c.changed();
          c.load();
        } catch (e) {
          err.textContent = `"${b.textContent}" did not go through. ${e.message}`;
        } finally {
          sending = false;
          btns.forEach((x) => (x.disabled = D.offline));
        }
      };
      return b;
    });
    const title = el("label", { className: "section-title", for: "wk-msg" }, "Tell the agent something");
    // voice.js puts the mic for the box here, beside the send buttons (styled with the voice pieces by D.askBox).
    const mic = el("span", { className: "talk-voice", dataset: { voiceSlot: "wk-msg" } });
    const box = el("section", { className: "talk" }, title, ta, how, el("div", { className: "row" }, mic, ...btns), waiting, err);
    return {
      box, ta,
      // "Answer the agent": brings the box into view with the cursor in it,
      // starting it with `prefill` (the agent's questions) when it is empty.
      answer(prefill) {
        if (box.hidden) return;
        if (prefill && !ta.value.trim()) {
          ta.value = prefill;
          drafts[id] = prefill;
        }
        const calm = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
        box.scrollIntoView({ block: "center", behavior: calm ? "auto" : "smooth" });
        ta.focus({ preventScroll: true });
        const at = prefill ? ta.value.indexOf("My answer: ") : -1;
        const end = at >= 0 ? at + "My answer: ".length : ta.value.length;
        ta.setSelectionRange(end, end);
      },
      update(t, n) {
        box.hidden = t.state === "done";
        const idle = IDLE.has(t.state);
        title.textContent = t.state === "in_review" || t.state === "needs_you" ? "Answer the agent" : "Tell the agent something";
        how.textContent = HOW[t.state] ?? howIdle(t.state);
        TALK.forEach(([kind, label], i) => {
          btns[i].hidden = idle && kind === "steer";
          btns[i].textContent = idle && kind === "follow_up" ? SEND_IDLE[t.state] : label;
        });
        waiting.hidden = !n;
        waiting.replaceChildren(icon("queued"), `Waiting to reach the agent: ${D.plural(n, "message")}`);
        if (!sending) btns.forEach((x) => (x.disabled = D.offline));
      },
    };
  }

  // Closes the panel, then runs `fn` once the panel's own step back in the
  // browser history is over, so a box `fn` opens is not closed by it.
  function afterClose(fn) {
    const popping = !!history.state?.work;
    let ran = false;
    const go = () => {
      if (ran) return;
      ran = true;
      window.removeEventListener("popstate", go);
      fn();
    };
    if (popping) { window.addEventListener("popstate", go); setTimeout(go, 400); }
    panel.close();
    if (!popping) go();
  }

  // "What next?": the choices from here in plain words, with shortcuts to
  // the message box and to new work. The moves themselves stay the footer's
  // buttons; this only says what each one does.
  function whatNext(c, asks, said) {
    const t = c.t, s = t.state;
    const rows = [];
    const row = (head, text, btn) => rows.push(el("div", { className: "nx" },
      el("p", { className: "nx-h" }, head), el("p", { className: "nx-t" }, ...[text].flat()), btn || null));
    const b = (w) => el("b", {}, w);
    const talk = (label) => {
      const x = el("button", { type: "button", className: "btn", dataset: { key: "wk-answer" } }, icon("question"), label);
      x.onclick = () => c.parts.talk.answer(asks.length ? asks.map((q) => `${q.text}\nMy answer: `).join("\n\n") : "");
      return x;
    };
    let fix = null;
    if (D.canFix() && said.trim() && (s === "in_review" || s === "done")) {
      fix = el("button", { type: "button", className: "btn", dataset: { key: "wk-fix" } }, icon("tool"), "Fix these");
      fix.onclick = () => afterClose(() => D.fixWork(t, said));
    }
    if (s === "in_review") {
      row("Looks right?", ["Tap ", b("Accept"), " at the bottom. Decurion sends it to GitHub as a pull request, and you merge it there."]);
      row(asks.length ? "Want to answer in your own words, or ask for changes?" : "Has a question, or want changes?",
        "Your message goes back to the agent, and it picks up where it left off.", talk("Answer the agent"));
      if (fix) row("Want what it found fixed?", "Start new work from this summary. This piece stays as it is.", fix);
      row("Don't want it?", [b("Reject"), " closes it. Nothing goes to GitHub. Its changes stay on its branch, and you can retry it later."]);
    } else if (s === "needs_you") {
      row("Answer it", asks.length ? "Tap an answer above, or type your own. The agent carries on with it."
        : "Type your answer. The agent carries on with it.", asks.length ? null : talk("Answer the agent"));
      row("Rather not answer?", [b("Continue"), " lets the agent carry on without an answer. ", b("Stop"), " ends the work; its changes stay on its branch."]);
    } else if (s === "failed") {
      row("Try again?", [b("Retry"), " starts it again, and it picks up where it left off."]);
      row("Want it done differently?", "Tell the agent what to change. The work goes back to it with your message.", talk("Tell the agent"));
    } else if (s === "done") {
      if (t.github?.state === "sent" && t.github.pull_request) row("Merge it on GitHub", "Open the pull request above, look it over, and merge it there. Decurion never merges.");
      if (fix) row("Want what it found fixed?", "Start new work from this summary.", fix);
    }
    if (!rows.length) return null;
    return el("section", {}, el("div", { className: "section-title" }, "What next?"), el("div", { className: "next" }, ...rows));
  }

  function draw(c) {
    const t = c.t, ev = c.ev, app = apps[t.app_id];
    const kids = kidsOf(t.id), parent = t.parent_id != null ? all.find((p) => p.id === t.parent_id) : null;
    const waiting = c.msgs ? c.msgs.filter((m) => m.delivered_at == null).length : 0;
    const sig = JSON.stringify([t.state, t.place, cloud, t.updated_at, t.title, t.github, ev.length, D.pro, D.offline, waiting, parent?.title,
      kids.map((k) => [k.id, k.state, k.title, k.background])]);
    if (sig === c.sig) return;
    c.sig = sig;
    const focusKey = panel.contains(document.activeElement) ? document.activeElement.dataset?.key : null;
    if (!c.parts) {
      const p = c.parts = {
        head: el("header"), foot: el("footer"),
        err: el("div", { className: "notice bad", role: "alert", hidden: true }),
        upper: el("div", { className: "part" }), lower: el("div", { className: "part" }),
        talk: talkBox(c),
        // Kept across refreshes, so a live view of the browser stays open.
        browser: el("div", { className: "part" }),
      };
      p.body = el("div", { className: "work-body live" }, p.err, p.upper, p.browser, p.talk.box, p.lower);
      panel.replaceChildren(p.head, p.body, p.foot);
    }
    const p = c.parts;
    const close = el("button", { type: "button", className: "btn icon quiet close", "aria-label": "Close", dataset: { key: "wk-close" } }, icon("close", "l"));
    close.onclick = () => panel.close();
    const head = el("header", { className: "work-head" },
      el("div", { className: "top" }, D.pill(t.state), el("span", { className: "grow" }), close),
      el("h2", { id: "wk-title" }, t.title),
      el("div", { className: "meta" },
        el("span", { className: "tag" }, icon("app"), D.appName(t.app_id) || "Unknown app"),
        app && el("span", { className: "tag dot-sep" }, icon("ai"), D.aiLabel(app, t)),
        el("span", { className: "tag dot-sep" }, icon(D.PLACES[t.place]?.[0] ?? "pc"), D.PLACES[t.place]?.[1] ?? "Your PC"),
        el("span", { className: "tag dot-sep updated" }, `Updated ${D.ago(t.updated_at)}`)));
    p.head.replaceWith(head);
    p.head = head;

    // What the agent asks comes first, then GitHub, then its words in full,
    // then what the owner can do next.
    const { questions, rest, fromBlock } = D.questions(D.rawWords(ev));
    const asks = IDLE.has(t.state) ? questions : [];
    const upper = [];
    if (asks.length) {
      upper.push(D.askBox(t, asks, {
        onSent: () => { c.sig = ""; c.changed(); c.load(); },
        onTalk: (pre) => p.talk.answer(pre),
        guessed: !fromBlock,
      }));
    }
    if (t.github) {
      upper.push(el("section", {}, el("div", { className: "section-title" }, "GitHub"),
        D.github(t, () => { c.sig = ""; c.changed(); c.load(); })));
    }
    if (rest.trim() && t.state !== "running") {
      const label = { needs_you: asks.length ? "What the agent said" : "The agent is asking", in_review: "The agent's summary", failed: "What went wrong", done: "The agent's summary", queued: "Last thing it said" }[t.state];
      // A small "Read aloud" in the header, so the owner can hear the report.
      const hear = window.Voice?.canSpeak?.() ? window.Voice.speaker(rest, { label: "Read aloud" }) : null;
      upper.push(el("section", {}, el("div", { className: `section-title${hear ? " with-voice" : ""}` }, label, hear),
        el("div", { className: `callout rich-box ${t.state}` }, D.richText(rest))));
    }
    const next = whatNext(c, asks, rest);
    if (next) upper.push(next);
    p.upper.replaceChildren(...upper);
    browserPart(c);
    c.asked = asks.length > 0;
    p.talk.update(t, waiting);

    const open = (k, key) => {
      const b = el("button", { type: "button", className: "btn sm", "aria-label": `Open ${k.title}`, dataset: { key } }, "Open");
      b.onclick = () => D.openWork(k, c.changed);
      return b;
    };
    const lower = [];
    if (t.parent_id != null) {
      lower.push(el("section", {}, el("div", { className: "section-title" }, "Part of"),
        el("div", { className: "tree-row" },
          el("span", { className: "nm" }, parent ? parent.title : "Other work", t.background ? el("span", { className: "hint" }, " · This keeps going on its own if that one stops") : null),
          parent && open(parent, "wk-parent"))));
    }
    if (kids.length) {
      lower.push(el("section", {}, el("div", { className: "section-title" }, `Smaller tasks inside this one (${kids.length})`),
        el("ul", { className: "kids" }, ...kids.map((k) => el("li", {}, D.pill(k.state),
          el("span", { className: "nm" }, k.title, k.background ? el("span", { className: "hint" }, " · keeps going on its own") : null),
          open(k, `wk-kid-${k.id}`))))));
    }
    lower.push(el("section", {},
      el("details", { className: "fold", open: t.state === "queued" || !ev.length },
        el("summary", { className: "section-title" }, icon("right"), "What you asked"),
        el("p", { className: "asked" }, t.prompt))));
    lower.push(el("section", {}, el("div", { className: "section-title" }, "Activity"), timeline(ev, D.pro)));
    if (D.pro) {
      const last = [...ev].reverse().find((r) => r.event.kind === "finished");
      const kv = el("dl", { className: "kv" });
      const row = (k, v) => v != null && v !== "" && kv.append(el("dt", {}, k), el("dd", {}, String(v)));
      row("Work id", t.id);
      row("Part of", t.parent_id != null ? `#${t.parent_id}${t.background ? " (keeps going on its own)" : ""}` : null);
      row("Branch", t.branch);
      row("Session", t.session_id);
      row("Repo", app?.repo_path);
      if (last?.event.cost_usd != null) row("Cost so far", D.money(last.event.cost_usd) + (app?.pay_per_use ? "" : " (Max account, not billed)"));
      if (last?.event.input_tokens != null) row("Tokens", `${last.event.input_tokens.toLocaleString()} in, ${(last.event.output_tokens ?? 0).toLocaleString()} out`);
      row("Last change", new Date(t.updated_at).toLocaleString());
      lower.push(el("section", {}, el("div", { className: "section-title" }, "Details"), kv));
    }
    p.lower.replaceChildren(...lower);

    const moves = D.moveButtons(t, {
      onMoved: () => { panel.close(); c.changed(); },
      onError: (m) => { p.err.hidden = false; p.err.replaceChildren(icon("alert"), el("span", { className: "grow" }, m)); },
    });
    // Work moves between the PC and the cloud worker at any point before it is done.
    if (cloud && t.state !== "done") {
      const to = t.place === "cloud" ? "pc" : "cloud";
      const b = el("button", { type: "button", className: "btn quiet" }, icon(to), to === "cloud" ? "Move to cloud" : "Move to PC");
      b.onclick = async () => {
        if (D.offline) return;
        b.disabled = true;
        try {
          await D.api(`/threads/${t.id}/place`, { method: "POST", body: JSON.stringify({ place: to }) });
          D.toast(to === "cloud" ? `Moving "${t.title}" to the cloud.` : `Moving "${t.title}" to your PC.`, "done");
          panel.close(); c.changed();
        } catch (e) {
          b.disabled = false;
          p.err.hidden = false;
          p.err.replaceChildren(icon("alert"), el("span", { className: "grow" }, `The move did not go through. ${e.message}`));
        }
      };
      moves.push(b);
    }
    // Finished work can be put out of the way, or brought back.
    if (D.finished(t)) {
      const back = !!t.dismissed_at;
      const b = el("button", { type: "button", className: "btn quiet" }, icon(back ? "retry" : "archive"), back ? "Bring back" : "Dismiss");
      b.onclick = async () => {
        b.disabled = true;
        if (await D.dismiss("threads", [t], { dismissed: !back })) { panel.close(); c.changed(); } else b.disabled = D.offline;
      };
      moves.push(b);
    }
    moves.forEach((b, i) => { b.disabled = D.offline; b.dataset.key = `wk-move-${i}`; });
    // On a computer the main button sits on the right.
    const foot = el("footer", { className: "work-foot", hidden: !moves.length && !D.offline }, ...moves.reverse());
    if (D.offline) foot.prepend(el("p", { className: "hint", style: "width:100%" }, "Answer when your PC is back."));
    p.foot.replaceWith(foot);
    p.foot = foot;

    if (focusKey && document.activeElement?.dataset?.key !== focusKey) panel.querySelector(`[data-key="${CSS.escape(focusKey)}"]`)?.focus({ preventScroll: true });
  }

  // ---------- The test browser, for running Claude work ----------
  // Only Claude's runs have a test browser to watch. The live view stays
  // open while the work runs, and closes (handing the browser back) when it
  // stops or the panel closes.
  function browserPart(c) {
    const t = c.t, box = c.parts.browser;
    if (t.state !== "running" || (t.ai ?? "claude") !== "claude") {
      c.view?.close();
      c.view = null;
      c.browserShown = false;
      box.replaceChildren();
      return;
    }
    if (c.browserShown) return;
    c.browserShown = true;
    watchButton(c);
  }

  function watchButton(c, said) {
    const box = c.parts.browser;
    const err = el("p", { className: "field-error", role: "alert" });
    const b = el("button", { type: "button", className: "btn", dataset: { key: "wk-watch" } }, icon("globe"), "Watch the browser");
    b.onclick = async () => {
      if (D.offline) return;
      b.disabled = true;
      err.textContent = "";
      try {
        const s = await D.api(`/threads/${c.id}/browser/watch`, { method: "POST" });
        if (cur !== c) return;
        c.view = D.browserView(s.id, {
          onEnded: (why) => { if (cur !== c) return; c.view = null; watchButton(c, why); },
        });
        box.replaceChildren(el("section", {}, el("div", { className: "section-title" }, "Test browser"), c.view.el));
      } catch (e) {
        b.disabled = false;
        err.textContent = `Couldn't open the browser. ${e.message}`;
      }
    };
    box.replaceChildren(el("section", {},
      el("div", { className: "section-title" }, "Test browser"),
      el("div", { className: "bv-start" },
        el("p", { className: "hint" }, said || "See the agent's test browser live. You can take it over to click and type in it yourself."),
        b),
      err));
  }

  // D.openWork(thread, changed, { talk: true }) opens the panel; talk puts the cursor in the message box.
  D.openWork = async (t, changed, opts = {}) => {
    ensurePanel();
    clearInterval(timer);
    cur?.view?.close();
    const c = cur = { id: t.id, t, ev: [], msgs: null, sig: "", parts: null, changed: changed ?? (() => {}) };
    panel.replaceChildren(el("div", { className: "work-head" }, D.pill(t.state), el("h2", { id: "wk-title" }, t.title)),
      el("div", { className: "work-body" }, ...[0, 1, 2].map(() => el("div", { className: "skeleton", style: "height:64px" }))));
    if (!panel.open) {
      if (!history.state?.work) history.pushState({ work: t.id }, "", `#work-${t.id}`);
      panel.showModal();
    }
    if (cloud === null) {
      try { cloud = (await D.api("/workers")).some((w) => w.place === "cloud"); } catch (_) { cloud = false; }
    }
    c.load = async () => {
      if (cur !== c) return;
      try {
        const [thread, more, msgs] = await Promise.all([D.api(`/threads/${t.id}`), D.events(t.id, c.ev.length ? c.ev[c.ev.length - 1].id : 0),
          D.api(`/threads/${t.id}/messages`).catch(() => null)]);
        if (cur !== c) return;
        c.ev = c.ev.concat(more);
        c.t = thread;
        if (msgs) c.msgs = msgs;
        draw(c);
      } catch (e) {
        if (cur === c && !c.parts) {
          panel.querySelector(".work-body").replaceChildren(el("div", { className: "notice bad" }, icon("alert"),
            el("span", { className: "grow" }, `Couldn't load this work. ${e.message}`)));
        }
      }
    };
    await c.load();
    if (cur !== c) return;
    if (opts.talk && c.parts && !c.parts.talk.box.hidden) {
      if (opts.prefill) c.parts.talk.answer(opts.prefill); else c.parts.talk.ta.focus();
    }
    timer = setInterval(c.load, 3000);
  };
  D.onPro(() => { if (cur?.parts) { cur.sig = ""; draw(cur); } });
  D.onOffline(() => { if (cur?.parts) { cur.sig = ""; draw(cur); } });
})();

// ---------- The live test browser ----------
// A live picture of a test browser, passed along by the hub. Used by the work
// panel for running Claude work (watch, Take over, Hand back) and by the
// sign-in window, which adds its own button. Only the picture, the page's
// real address and who is driving come down; taps, drags and typing go up,
// and only while you have the browser.
(() => {
  const { el, icon } = D;
  // The most one piece of input may weigh (MAX_INPUT_BYTES in the hub).
  const MAX_INPUT = 4096;
  const KEYS = [["Enter", "Enter", "Press Enter"], ["Tab", "Tab", "Press Tab"], ["Backspace", "Backspace", "Delete the letter before the cursor"]];
  const bytes = (s) => new TextEncoder().encode(s).length;

  // D.browserView(sessionId, opts) → { el, close() }. Put `el` on the page
  // straight away; it connects once it is there.
  //   opts.takeOver  false for a window you drive from the start (no Take over / Hand back)
  //   opts.extra     { label, kind, onClick(view) }: one more button, e.g. "I'm signed in, save"
  //   opts.hint      the line under the picture, in place of the usual one
  //   opts.onEnded   called with the reason once the view is over for good
  D.browserView = (sid, opts = {}) => {
    const takeOver = opts.takeOver !== false;
    let ws = null, who = takeOver ? "agent" : "owner", over = false, ended = null, closing = false;
    let frameW = 0, frameH = 0, gotFrame = false, slow = null, handing = false;

    const dot = el("span", { className: "dot", "aria-hidden": "true" });
    const status = el("span", { className: "grow" });
    const takeBtn = el("button", { type: "button", className: "btn primary sm" }, "Take over");
    const backBtn = el("button", { type: "button", className: "btn primary sm" }, "Hand back");
    const bigBtn = el("button", { type: "button", className: "btn quiet sm bv-bigger", "aria-pressed": "false" }, icon("expand"), "Bigger");
    const url = el("span", { className: "addr" }, "Opening…");
    const canvas = el("canvas", { tabIndex: 0, "aria-label": "Live picture of the test browser" });
    const ctx = canvas.getContext("2d");
    const overText = el("p", {}, "Connecting to your PC…");
    const again = el("button", { type: "button", className: "btn sm", hidden: true }, "Try again");
    const veil = el("div", { className: "bv-over" }, overText, again);
    const stage = el("div", { className: "bv-stage" }, canvas, veil);
    const typed = el("input", { type: "text", autocomplete: "off", autocapitalize: "off",
      enterKeyHint: "send", placeholder: "Type here, then Send", "aria-label": "Text to type into the page" });
    typed.setAttribute("autocorrect", "off");
    typed.setAttribute("spellcheck", "false");
    const sendBtn = el("button", { type: "button", className: "btn sm" }, icon("send"), "Send");
    const keyBtns = KEYS.map(([key, label, aria]) => {
      const b = el("button", { type: "button", className: "btn sm", "aria-label": aria }, label);
      b.onclick = () => input({ kind: "key", key });
      return b;
    });
    const back = el("button", { type: "button", className: "btn sm", "aria-label": "Go back a page" }, icon("left"), "Back");
    back.onclick = () => input({ kind: "back" });
    const drive = el("div", { className: "bv-drive" },
      el("div", { className: "bv-type" }, typed, sendBtn),
      el("div", { className: "bv-keys" }, ...keyBtns, back));
    const hint = el("p", { className: "hint" }, opts.hint ?? (takeOver
      ? "Tap to click, drag to scroll. Don't type real passwords here: use a sign-in window for that."
      : "Tap to click, drag to scroll. Use the box above the keys to type."));
    const nudge = el("p", { className: "hint bv-nudge", role: "status", "aria-live": "polite" });
    const extra = opts.extra
      ? el("button", { type: "button", className: `btn ${opts.extra.kind ?? "primary"}` }, opts.extra.label)
      : null;
    const root = el("div", { className: "bv", dataset: { who } },
      el("div", { className: "bv-bar" }, dot, status, takeOver && takeBtn, takeOver && backBtn, bigBtn),
      el("div", { className: "bv-url", title: "The page the browser is on" }, icon("globe"), url),
      stage, drive, hint, nudge, extra && el("div", { className: "bv-extra" }, extra));

    function paint() {
      root.dataset.who = who;
      const mine = who === "owner";
      status.textContent = over ? "The live view has ended."
        : !takeOver ? "You have the browser."
        : mine ? "You have the browser. The agent is waiting."
        : "The agent is using the browser. You're watching.";
      takeBtn.hidden = over || mine;
      backBtn.hidden = over || !mine;
      takeBtn.disabled = backBtn.disabled = handing || D.offline;
      drive.hidden = over || !mine;
      if (mine) nudge.textContent = "";
      for (const b of [sendBtn, back, ...keyBtns]) b.disabled = over;
      if (extra) extra.disabled = over;
    }

    function cover(text, retry = false) {
      overText.textContent = text;
      again.hidden = !retry;
      veil.hidden = false;
    }

    function say(text) {
      nudge.textContent = text;
      clearTimeout(say.t);
      say.t = setTimeout(() => (nudge.textContent = ""), 5000);
    }

    // Sends one piece of input, if you have the browser.
    function input(i) {
      if (who !== "owner") { say("Tap Take over first to use the browser yourself."); return false; }
      if (!ws || ws.readyState !== 1) return false;
      const msg = JSON.stringify({ type: "input", input: i });
      if (bytes(msg) > MAX_INPUT) return false;
      ws.send(msg);
      return true;
    }

    // Typed text goes in pieces small enough for the hub to pass on.
    function sendText(text) {
      const chars = Array.from(text);
      for (let i = 0; i < chars.length;) {
        let n = Math.min(500, chars.length - i);
        while (n > 1 && bytes(JSON.stringify({ type: "input", input: { kind: "text", text: chars.slice(i, i + n).join("") } })) > MAX_INPUT) n = Math.ceil(n / 2);
        if (!input({ kind: "text", text: chars.slice(i, i + n).join("") })) return false;
        i += n;
      }
      return true;
    }
    const sendTyped = () => {
      if (!typed.value) { typed.focus(); return; }
      if (sendText(typed.value)) typed.value = "";
    };
    sendBtn.onclick = sendTyped;
    typed.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && !e.isComposing) { e.preventDefault(); sendTyped(); }
    });

    // Where on the picture, as fractions of it.
    const spot = (x, y) => {
      const r = canvas.getBoundingClientRect();
      const f = (v) => Math.min(1, Math.max(0, v));
      return { x: f((x - r.left) / r.width), y: f((y - r.top) / r.height) };
    };
    // Picture pixels per screen pixel, so a drag moves the page as far as your finger.
    const scale = () => (frameW ? frameW / canvas.getBoundingClientRect().width : 1);

    // Scrolls are gathered, in picture pixels, and sent at most every 50 ms.
    let pend = null;
    function scroll(x, y, dx, dy) {
      if (!pend) { pend = { ...spot(x, y), dx: 0, dy: 0 }; setTimeout(flush, 50); }
      pend.dx += dx; pend.dy += dy;
    }
    function flush() {
      const p = pend;
      pend = null;
      if (!p || !frameW || !frameH || (Math.abs(p.dx) < 1 && Math.abs(p.dy) < 1)) return;
      // Sent as fractions of the picture, as the browser box expects; at
      // most ten pictures' worth at once.
      const f = (v, size) => Math.max(-10, Math.min(10, v / size));
      input({ kind: "scroll", x: p.x, y: p.y, dx: f(p.dx, frameW), dy: f(p.dy, frameH) });
    }

    let press = null;
    canvas.addEventListener("pointerdown", (e) => {
      if (e.button !== 0 || over) return;
      if (who !== "owner") { if (takeOver) say("Tap Take over first to use the browser yourself."); return; }
      press = { x: e.clientX, y: e.clientY, lx: e.clientX, ly: e.clientY, drag: false };
      try { canvas.setPointerCapture(e.pointerId); } catch (_) { /* already gone */ }
    });
    canvas.addEventListener("pointermove", (e) => {
      if (!press) return;
      if (!press.drag && Math.hypot(e.clientX - press.x, e.clientY - press.y) > 8) press.drag = true;
      if (press.drag) {
        const k = scale();
        scroll(e.clientX, e.clientY, -(e.clientX - press.lx) * k, -(e.clientY - press.ly) * k);
      }
      press.lx = e.clientX; press.ly = e.clientY;
    });
    canvas.addEventListener("pointerup", (e) => {
      if (!press) return;
      if (!press.drag) input({ kind: "click", ...spot(e.clientX, e.clientY) });
      press = null;
    });
    canvas.addEventListener("pointercancel", () => { press = null; });
    canvas.addEventListener("wheel", (e) => {
      if (who !== "owner" || over) return;
      e.preventDefault();
      const k = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? 400 : 1;
      scroll(e.clientX, e.clientY, e.deltaX * k, e.deltaY * k);
    }, { passive: false });
    // On a computer, typing with the picture selected goes to the page.
    canvas.addEventListener("keydown", (e) => {
      if (who !== "owner" || e.ctrlKey || e.metaKey || e.altKey) return;
      if (e.key.length === 1) { e.preventDefault(); sendText(e.key); }
      else if (["Enter", "Tab", "Backspace", "Escape", "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"].includes(e.key)) {
        e.preventDefault();
        input({ kind: "key", key: e.key });
      }
    });

    async function show(m) {
      const img = new Image();
      img.src = "data:image/jpeg;base64," + m.jpeg;
      try { await img.decode(); } catch (_) { ack(m.seq); return; }
      if (closing) return;
      const w = m.width || img.naturalWidth, h = m.height || img.naturalHeight;
      if (w !== frameW || h !== frameH) {
        frameW = w; frameH = h;
        canvas.width = w; canvas.height = h;
        stage.style.setProperty("--ar", `${w} / ${h}`);
        stage.style.setProperty("--arn", String(w / h));
      }
      ctx.drawImage(img, 0, 0, w, h);
      if (!gotFrame) { gotFrame = true; clearTimeout(slow); }
      veil.hidden = true;
      ack(m.seq);
    }
    // Lets the next picture come.
    const ack = (seq) => { if (ws?.readyState === 1) ws.send(JSON.stringify({ type: "frame_ack", seq })); };

    function connect() {
      if (closing) return;
      const width = Math.round(Math.min(Math.max(stage.clientWidth || window.innerWidth, 320), 1280) * Math.min(window.devicePixelRatio || 1, 2));
      const proto = location.protocol === "https:" ? "wss" : "ws";
      ended = null;
      cover("Connecting to your PC…");
      ws = new WebSocket(`${proto}://${location.host}/api/browser/${encodeURIComponent(sid)}/ws?max_width=${width}`);
      clearTimeout(slow);
      slow = setTimeout(() => {
        if (!gotFrame && !over) cover("Still waiting for the test browser. It starts just before the agent does, so it may not be open yet.");
      }, 20000);
      ws.onmessage = (e) => {
        let m;
        try { m = JSON.parse(e.data); } catch (_) { return; }
        if (m.type === "frame") show(m);
        else if (m.type === "page") { url.textContent = m.url || "about:blank"; url.title = m.title || ""; }
        else if (m.type === "control") {
          const was = who;
          who = m.who === "owner" ? "owner" : "agent";
          if (takeOver && was === "owner" && who === "agent" && !handing) say("The browser went back to the agent.");
          paint();
        } else if (m.type === "ended") { ended = m.why; cover(m.why); }
      };
      ws.onclose = () => {
        ws = null;
        if (closing) return;
        if (ended != null) finish(ended);
        else cover("Lost the connection to your PC.", true);
      };
    }
    again.onclick = () => connect();

    function finish(why) {
      over = true;
      clearTimeout(slow);
      root.classList.remove("big");
      cover(why || "The live view has ended.");
      paint();
      opts.onEnded?.(why);
    }

    async function hand(path, to) {
      handing = true;
      paint();
      try {
        const s = await D.api(`/browser/${encodeURIComponent(sid)}/${path}`, { method: "POST" });
        who = s.controller === "owner" ? "owner" : "agent";
        if (to === "owner") canvas.focus({ preventScroll: true });
      } catch (e) {
        say(`That didn't go through. ${e.message}`);
      } finally {
        handing = false;
        paint();
      }
    }
    takeBtn.onclick = () => hand("take", "owner");
    backBtn.onclick = () => hand("give-back", "agent");
    bigBtn.onclick = () => {
      const big = root.classList.toggle("big");
      bigBtn.setAttribute("aria-pressed", big ? "true" : "false");
      bigBtn.lastChild.textContent = big ? "Smaller" : "Bigger";
    };
    root.addEventListener("keydown", (e) => { if (e.key === "Escape" && root.classList.contains("big") && document.activeElement !== canvas) bigBtn.click(); });
    const view = {
      el: root,
      sid,
      // Closing the view hands the browser back to the agent.
      close() {
        closing = true;
        clearTimeout(slow);
        if (ws) { try { ws.close(); } catch (_) { /* already closed */ } }
        ws = null;
      },
    };
    if (extra) extra.onclick = () => opts.extra.onClick?.(view);
    paint();
    setTimeout(connect, 0);
    return view;
  };

  // D.openBrowserViewer(sessionId, { saveButton }) opens a sign-in window in
  // a panel of its own: the owner drives a fresh browser on the PC, signs in,
  // and taps "I'm signed in, save". Resolves once the panel closes.
  D.openBrowserViewer = (sid, opts = {}) => new Promise((resolve) => {
    let saved = false;
    const close = el("button", { type: "button", className: "btn icon quiet close", "aria-label": "Close" }, icon("close", "l"));
    const d = el("dialog", { className: "panel", "aria-labelledby": "si-title" });
    const note = el("p", { className: "hint", role: "status", "aria-live": "polite" });
    const view = D.browserView(sid, {
      takeOver: false,
      hint: "Tap to click, drag to scroll, and type in the box above the keys. Use a test account. Decurion saves the sign-in, never your password.",
      extra: opts.saveButton === false ? null : {
        label: "I'm signed in, save",
        onClick: async () => {
          note.textContent = "Saving…";
          try {
            await D.api(`/browser/${sid}/save`, { method: "POST" });
          } catch (e) {
            note.textContent = `Not saved yet. ${e.message}`;
          }
        },
      },
      onEnded: (why) => {
        // The runner says exactly this once the sign-in is kept on the PC.
        saved = why === "The sign-in was saved.";
        note.textContent = saved ? "Saved. Agents working on this app will be signed in from their next run." : (why ?? "");
        if (saved) { D.toast("Sign-in saved"); setTimeout(() => d.open && d.close(), 1200); }
      },
    });
    d.append(
      el("header", { className: "work-head" },
        el("div", { className: "top" }, el("span", { className: "grow" }), close),
        el("h2", { id: "si-title" }, "Sign in")),
      el("div", { className: "si-body" }, view.el, note));
    close.onclick = () => d.close();
    d.addEventListener("close", () => { view.close(); d.remove(); resolve(saved); });
    document.body.append(d);
    d.showModal();
  });
})();

// ---------- The folder picker (D.pickFolder) ----------
// Browses the PC's folders through the hub (GET /api/folders), so it works
// the same on the board and on a paired phone. The hub decides what may be
// shown; this only draws it. Folder names go in as text, never as HTML.
(() => {
  const { el, icon } = D;
  const SVG = "http://www.w3.org/2000/svg";
  // Corner-up-left: "the folder above".
  const upIcon = () => {
    const s = document.createElementNS(SVG, "svg");
    s.setAttribute("viewBox", "0 0 24 24");
    s.setAttribute("class", "i");
    s.setAttribute("aria-hidden", "true");
    for (const d of ["M9 14 4 9l5-5", "M4 9h10.5a5.5 5.5 0 0 1 0 11H11"]) {
      const p = document.createElementNS(SVG, "path");
      p.setAttribute("d", d);
      s.append(p);
    }
    return s;
  };
  const gitBadge = () => el("span", { className: "fp-git" }, icon("branch"), "Git app folder");

  // D.pickFolder({ title, start }) opens the picker, at `start` if the hub
  // can show it. Resolves with the chosen folder's full path, or null.
  D.pickFolder = ({ title = "Choose your app's folder", start = "" } = {}) => new Promise((resolve) => {
    let chosen = null, here = null, seq = 0;
    const close = el("button", { type: "button", className: "btn icon quiet fp-close", "aria-label": "Close" }, icon("close", "l"));
    const up = el("button", { type: "button", className: "btn fp-up", disabled: true }, upIcon(), "Up");
    const crumbs = el("ol", { className: "fp-crumbs" });
    const note = el("div", { className: "fp-note", role: "status", "aria-live": "polite" });
    const list = el("ul", { className: "fp-list", "aria-label": "Folders" });
    const count = el("p", { className: "sr-only", role: "status", "aria-live": "polite" });
    const cancel = el("button", { type: "button", className: "btn" }, "Cancel");
    const use = el("button", { type: "button", className: "btn primary", disabled: true }, "Use this folder");
    const d = el("dialog", { className: "fp", "aria-labelledby": "fp-t" },
      el("header", { className: "fp-head" },
        el("div", { className: "fp-top" }, el("h2", { id: "fp-t", tabIndex: -1 }, title), close),
        el("nav", { className: "fp-nav", "aria-label": "Where you are" }, up, crumbs)),
      note,
      el("div", { className: "fp-body" }, list, count),
      el("footer", { className: "fp-foot" }, cancel, use));

    const crumbButton = (label, path, current, ico) => {
      const b = el("button", { type: "button", className: "fp-crumb", "aria-current": current ? "location" : null }, ico, label);
      if (!current) b.onclick = () => go(path);
      return el("li", {}, b);
    };
    const message = (kind, text, retry) => {
      note.replaceChildren();
      if (!text) return;
      note.append(el("div", { className: `notice ${kind}`.trim() }, icon(kind === "ok" ? "branch" : "alert"),
        el("span", { className: "grow" }, text),
        retry ? el("button", { type: "button", className: "btn sm", onclick: retry }, "Try again") : null));
    };

    const draw = (v, why) => {
      here = v.path;
      const atStart = !v.path;
      up.disabled = atStart;
      up.onclick = () => go(v.up ?? "");
      crumbs.replaceChildren(crumbButton("Your PC", "", atStart, icon("pc")),
        ...v.crumbs.map((c, i) => crumbButton(c.name, c.path, i === v.crumbs.length - 1, null)));
      crumbs.scrollLeft = crumbs.scrollWidth;
      use.disabled = atStart;
      if (why) message("info", why);
      else if (atStart) message("", "");
      else if (v.git) message("ok", "This is a git folder. You can use it.");
      else message("warn", "This folder isn't a git folder yet. Pick the folder that holds your app's .git.");
      list.replaceChildren();
      if (!v.folders.length) {
        list.append(el("li", { className: "fp-empty" }, atStart
          ? "No code folders found on your PC. Close this and type the folder's path instead."
          : "No folders inside this one."));
      }
      for (const f of v.folders) {
        const b = el("button", { type: "button", className: "fp-item", onclick: () => go(f.path) },
          icon("app", "l"),
          el("span", { className: "fp-name" }, el("span", { className: "fp-label" }, f.name),
            atStart ? el("span", { className: "fp-path" }, f.path) : null),
          f.git ? gitBadge() : null,
          icon("right"));
        list.append(el("li", {}, b));
      }
      if (v.cut) list.append(el("li", { className: "fp-empty" }, `Only the first ${v.folders.length} folders are shown.`));
      count.textContent = `${v.name ?? "Your PC"}: ${D.plural(v.folders.length, "folder")}.`;
    };

    async function go(path, why) {
      const mine = ++seq;
      const hadFocus = list.contains(document.activeElement);
      d.setAttribute("aria-busy", "true");
      list.classList.add("fp-loading");
      try {
        const v = await D.api(path ? `/folders?path=${encodeURIComponent(path)}` : "/folders");
        if (mine !== seq) return;
        draw(v, why);
        if (hadFocus) list.querySelector("button")?.focus();
      } catch (e) {
        if (mine !== seq) return;
        // A folder that can't be shown: back to the start, saying why. If
        // the start itself fails, say so with a way to try again.
        if (path) { go("", e.message); return; }
        message("bad", e.message, () => go(""));
        list.replaceChildren();
        use.disabled = true;
        here = null;
      } finally {
        if (mine === seq) { d.removeAttribute("aria-busy"); list.classList.remove("fp-loading"); }
      }
    }

    close.onclick = () => d.close();
    cancel.onclick = () => d.close();
    use.onclick = () => { if (here) { chosen = here; d.close(); } };
    d.addEventListener("close", () => { seq++; d.remove(); resolve(chosen); });
    document.body.append(d);
    d.showModal();
    // Start on the title, so it is read out and no button looks pressed.
    d.querySelector("#fp-t").focus();
    go(String(start ?? "").trim());
  });
})();
