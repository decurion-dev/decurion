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
    trash: ["M3 6h18", "M8 6V4h8v2", "M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6"],
    menu: ["M4 6h16", "M4 12h16", "M4 18h16"],
    sun: ["c12,12,4", "M12 2v2", "M12 20v2", "m4.9 4.9 1.4 1.4", "m17.7 17.7 1.4 1.4", "M2 12h2", "M20 12h2", "m6.3 17.7-1.4 1.4", "m19.1 4.9-1.4 1.4"],
    moon: ["M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9z"],
    shield: ["M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"],
    globe: ["c12,12,10", "M2 12h20", "M12 2a15 15 0 0 1 0 20 15 15 0 0 1 0-20z"],
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
      { to: "done", label: "Accept", kind: "primary", done: "Accepted. Marked as done." },
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
  D.lastWords = (ev) => {
    const last = [...ev].reverse().find((r) => r.event.kind === "finished" || r.event.kind === "message");
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
  D.aiLabel = (app) => (!app ? "" : app.pay_per_use ? "Claude pay-per-use" : `Claude ${app.profile}`);
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
  D.toast = (text, icon = "done") => {
    let box = document.querySelector(".toasts");
    if (!box) { box = D.el("div", { className: "toasts", role: "status", "aria-live": "polite" }); document.body.append(box); }
    const t = D.el("div", { className: "toast" }, D.icon(icon), D.el("span", {}, text));
    box.append(t);
    setTimeout(() => t.remove(), 6000);
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
        item(e.ok ? "ok" : "bad", e.ok ? "done" : "failed", el("b", {}, e.ok ? "Finished" : "Could not finish"), when(r), el("div", {}, e.summary), cost && el("div", { className: "hint" }, cost));
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
  const HOW_IDLE = "The agent isn't working on this right now. Change course now or Do this next sends the work back to the agent with your message. A note waits for the next time it works.";
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
      "aria-describedby": "wk-msg-how wk-msg-err", dataset: { key: "wk-msg" } });
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
          await D.send(`/threads/${id}/messages`, { kind, text });
          delete drafts[id];
          ta.value = "";
          D.toast(sentText(t, kind), "send");
          c.sig = "";
          c.changed();
          c.load();
        } catch (e) {
          err.textContent = `"${label}" did not go through. ${e.message}`;
        } finally {
          sending = false;
          btns.forEach((x) => (x.disabled = D.offline));
        }
      };
      return b;
    });
    const box = el("section", { className: "talk" },
      el("label", { className: "section-title", for: "wk-msg" }, "Tell the agent something"),
      ta, how, el("div", { className: "row" }, ...btns), waiting, err);
    return {
      box, ta,
      update(t, n) {
        box.hidden = t.state === "done";
        how.textContent = HOW[t.state] ?? HOW_IDLE;
        waiting.hidden = !n;
        waiting.replaceChildren(icon("queued"), `Waiting to reach the agent: ${D.plural(n, "message")}`);
        if (!sending) btns.forEach((x) => (x.disabled = D.offline));
      },
    };
  }

  function draw(c) {
    const t = c.t, ev = c.ev, app = apps[t.app_id];
    const kids = kidsOf(t.id), parent = t.parent_id != null ? all.find((p) => p.id === t.parent_id) : null;
    const waiting = c.msgs ? c.msgs.filter((m) => m.delivered_at == null).length : 0;
    const sig = JSON.stringify([t.state, t.place, cloud, t.updated_at, t.title, ev.length, D.pro, D.offline, waiting, parent?.title,
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
      };
      p.body = el("div", { className: "work-body live" }, p.err, p.upper, p.talk.box, p.lower);
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
        app && el("span", { className: "tag dot-sep" }, icon("ai"), D.aiLabel(app)),
        el("span", { className: "tag dot-sep" }, icon(D.PLACES[t.place]?.[0] ?? "pc"), D.PLACES[t.place]?.[1] ?? "Your PC"),
        el("span", { className: "tag dot-sep updated" }, `Updated ${D.ago(t.updated_at)}`)));
    p.head.replaceWith(head);
    p.head = head;

    const said = D.lastWords(ev);
    const upper = [];
    if (said && t.state !== "running") {
      const label = { needs_you: "The agent is asking", in_review: "The agent's summary", failed: "What went wrong", done: "The agent's summary", queued: "Last thing it said" }[t.state];
      upper.push(el("section", {}, el("div", { className: "section-title" }, label), el("div", { className: `callout ${t.state}` }, said)));
    }
    p.upper.replaceChildren(...upper);
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
    moves.forEach((b, i) => { b.disabled = D.offline; b.dataset.key = `wk-move-${i}`; });
    // On a computer the main button sits on the right.
    const foot = el("footer", { className: "work-foot", hidden: !moves.length && !D.offline }, ...moves.reverse());
    if (D.offline) foot.prepend(el("p", { className: "hint", style: "width:100%" }, "Answer when your PC is back."));
    p.foot.replaceWith(foot);
    p.foot = foot;

    if (focusKey && document.activeElement?.dataset?.key !== focusKey) panel.querySelector(`[data-key="${CSS.escape(focusKey)}"]`)?.focus({ preventScroll: true });
  }

  // D.openWork(thread, changed, { talk: true }) opens the panel; talk puts the cursor in the message box.
  D.openWork = async (t, changed, opts = {}) => {
    ensurePanel();
    clearInterval(timer);
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
    if (opts.talk && c.parts && !c.parts.talk.box.hidden) c.parts.talk.ta.focus();
    timer = setInterval(c.load, 3000);
  };
  D.onPro(() => { if (cur?.parts) { cur.sig = ""; draw(cur); } });
  D.onOffline(() => { if (cur?.parts) { cur.sig = ""; draw(cur); } });
})();
