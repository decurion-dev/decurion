// The new-work form, shared by the board, the phone inbox and the Goals page:
// the one place the owner asks for something. One box says what they want; by default it
// goes to the project lead as a goal, which asks, plans and waits for the
// owner's approval before anything starts. "Just do it" is the owner's own
// explicit choice to skip the plan for a small change: one agent, one piece,
// straight away. Nothing here ever approves a plan.
//
// D.composer(opts) builds the form once and returns
//   { el, update({ apps, threads, hasCloud, filter, loaded }), setApp(id), focus(), reset() }
// update() only changes the choices around the typed text, so a refresh never
// takes away what the owner is typing. Built with D.el: names and titles go in
// as text, never as HTML.
"use strict";
(() => {
  const { el, icon } = D;
  const AI_NAMES = { chatgpt: "ChatGPT", grok: "Grok" };
  const ACTIVE = new Set(["queued", "running", "needs_you", "in_review"]);
  const isMac = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
  const LAST_APP = "decurion.board.lastApp", LAST_AI = "decurion.board.ai";
  const MODES = {
    lead: { label: "Project lead", button: "Send to the project lead",
      line: "The project lead asks what it needs, shows you a plan, and starts after you approve. A checker reviews each piece. Just do it skips the plan and the checker, for small changes." },
    direct: { label: "Just do it", button: "Start work",
      line: "One agent does it right away. No plan and no checker. Best for small changes." },
  };

  // The mic for "What do you want?" (voice.js fills .c-voice when the browser
  // can listen; empty, it takes no room). It sits at the end of the row under
  // the box. On the phone sheet that row's pieces stand in one ordered column,
  // so order 2 keeps the mic right under the box there too, inside the margin.
  document.head.append(el("style", {}, `
    .composer .c-voice { flex: none; order: 2; display: flex; margin-left: auto; margin-right: var(--side, 0px); }
    .composer .c-voice:empty { display: none; }`));

  // The account an app's Claude work runs on.
  const claudeAcct = (app) => (!app ? "" : app.pay_per_use ? "pay-per-use" : `Max plan ${app.profile}`);

  // A short title from the first line of what the owner typed.
  D.deriveTitle = (prompt) => {
    const first = (prompt.split(/\r?\n/).find((l) => l.trim()) ?? "").trim().replace(/\s+/g, " ");
    if (first.length <= 60) return first.replace(/[.\s]+$/, "") || first;
    const cut = first.slice(0, 60);
    const sp = cut.lastIndexOf(" ");
    return (sp > 30 ? cut.slice(0, sp) : cut).replace(/[\s,.;:-]+$/, "") + "…";
  };

  // opts:
  //   root       element to fill (else a new <section>)
  //   prefix     for element ids, so two forms never share one ("" on the board)
  //   labels     true shows a visible label over each choice (the phone)
  //   keyHint    false hides the "Ctrl Enter to start" hint
  //   draftKey   D.store key that keeps the typed text if the page is closed
  //   noApps     { text, href } for the "add your first app" state
  //   onStarted(thread, { title, ai })  after "Just do it" made a piece of work
  //   onGoal(goal)                       after the project lead got a goal (default: open it on Goals)
  D.composer = (opts = {}) => {
    const p = opts.prefix ?? "new-";
    const id = (n) => `${p}${n}`;
    const sigs = {};
    const changed = (name, v) => { const s = JSON.stringify(v); if (sigs[name] === s) return false; sigs[name] = s; return true; };
    let st = { apps: [], threads: [], hasCloud: false, filter: "all", loaded: false };
    let mode = "lead", sending = false, wantApp = null, lastFilter = null;
    // One receipt per tap: kept only while the same piece of work is unanswered,
    // so pressing Start again after a dropped connection never makes it twice.
    let pending = null;

    const prompt = el("textarea", { id: id("prompt"), className: "c-prompt", rows: 3, "aria-describedby": id("add-err"),
      dataset: { voice: "", voiceLabel: "Say what you want" },
      placeholder: "For example: add a page where clients book a treatment, pick a time and get a confirmation email" });
    const app = el("select", { id: id("app"), className: "c-app", "aria-label": "App" });
    const ai = el("select", { id: id("ai"), className: "c-ai", "aria-label": "Which AI does the work" },
      el("option", { value: "claude" }, "Claude"), el("option", { value: "chatgpt" }, "ChatGPT"), el("option", { value: "grok" }, "Grok"));
    const place = el("select", { id: id("place"), className: "c-place", "aria-label": "Where it runs" },
      el("option", { value: "pc" }, "On your PC"), el("option", { value: "cloud" }, "In the cloud"));
    const title = el("input", { id: id("title"), className: "c-title", type: "text", placeholder: "Title (optional)", "aria-label": "Title (optional)", maxLength: 120, autocomplete: "off" });
    const parent = el("select", { id: id("parent"), className: "c-parent", "aria-label": "Part of other work" });
    const bg = el("input", { type: "checkbox", id: id("background") });
    const bgWrap = el("label", { className: "check c-bg", id: id("bg-wrap") }, bg, " Keep going if the parent stops");
    const start = el("button", { className: "btn primary c-start", id: id("start"), type: "submit" });
    const err = el("div", { className: "field-error", id: id("add-err"), role: "alert" });
    const modeLine = el("p", { className: "c-mode-line hint", id: id("mode-line"), "aria-live": "polite" });
    const modeBtns = Object.entries(MODES).map(([k, m]) => {
      const b = el("button", { type: "button", className: "seg", role: "radio", dataset: { mode: k }, title: m.line }, m.label);
      b.onclick = () => setMode(k, true);
      return b;
    });
    const seg = el("div", { className: "c-mode", role: "radiogroup", "aria-label": "Who does it", "aria-describedby": id("mode-line") }, ...modeBtns);
    // Arrow keys move between the two choices, like any radio group.
    seg.addEventListener("keydown", (e) => {
      if (!["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(e.key)) return;
      e.preventDefault();
      setMode(mode === "lead" ? "direct" : "lead", true);
    });

    // A choice with a visible label on the phone, or bare as on the board.
    const field = (label, control, extra = "") => (opts.labels
      ? el("div", { className: `c-field ${extra}`.trim() }, el("label", { className: "label", htmlFor: control.id }, label), control)
      : control);
    const fAi = field("Which AI", ai, "c-direct");
    const fPlace = field("Where it runs", place, "c-direct");
    const fTitle = field("Title", title, "c-direct");
    const fParent = field("Part of other work", parent, "c-direct");
    const hint = opts.keyHint === false ? null
      : el("span", { className: "kbd-hint" }, el("kbd", {}, isMac ? "Cmd" : "Ctrl"), " ", el("kbd", {}, "Enter"), " to start");
    const form = el("form", { id: id("add-thread"), noValidate: true },
      el("label", { className: "lead", htmlFor: id("prompt") }, "What do you want?"),
      prompt,
      el("div", { className: "c-modes" }, seg, modeLine, el("div", { className: "c-voice", dataset: { voiceSlot: id("prompt") } })),
      el("div", { className: "bar" },
        field("App", app),
        fAi, fPlace, fTitle, fParent, bgWrap,
        el("span", { className: "grow" }), hint, start),
      err);
    const na = opts.noApps ?? {};
    const noApps = el("div", { className: "empty-state", id: id("no-apps"), hidden: true },
      el("div", { className: "mark" }, icon("app", "l")),
      el("h3", {}, "Add your first app to start"),
      el("p", {}, na.text ?? "An app is a git folder on this PC. Once it's added, say what you want here."),
      el("a", { className: "btn primary", href: na.href ?? "#settings/apps" }, icon("plus"), "Add an app"));
    const root = opts.root ?? el("section", { "aria-label": "New work" });
    root.classList.add("composer", "live");
    root.replaceChildren(form, noApps);

    function setMode(m, focus = false) {
      mode = m;
      for (const b of modeBtns) {
        const on = b.dataset.mode === m;
        b.setAttribute("aria-checked", on ? "true" : "false");
        b.tabIndex = on ? 0 : -1;
        if (on && focus) b.focus();
      }
      root.dataset.mode = m;
      modeLine.textContent = MODES[m].line;
      paintStart();
      fillParents();
    }
    function paintStart() {
      if (sending) return;
      start.replaceChildren(MODES[mode].button, icon("send"));
      start.disabled = D.offline;
    }

    // The Claude choice names the account the chosen app's Claude work runs on.
    function showClaudeChoice() {
      const a = st.apps.find((x) => x.id === +app.value);
      ai.options[0].textContent = a ? `Claude (${claudeAcct(a)})` : "Claude";
    }
    {
      const last = D.store.get(LAST_AI);
      if (last && [...ai.options].some((o) => o.value === last)) ai.value = last;
    }
    ai.onchange = () => D.store.set(LAST_AI, ai.value);

    // "Part of": open work in the same app. Kept while the owner is choosing.
    function fillParents() {
      const appId = +app.value;
      const open = st.threads.filter((t) => t.app_id === appId && ACTIVE.has(t.state));
      if (document.activeElement !== parent && changed("parents", [appId, open.map((t) => [t.id, t.title])])) {
        const keep = parent.value;
        parent.replaceChildren(el("option", { value: "" }, "Not part of other work"),
          ...open.map((t) => el("option", { value: String(t.id) }, `Part of: ${t.title}`)));
        parent.value = open.some((t) => String(t.id) === keep) ? keep : "";
      }
      const direct = mode === "direct";
      fAi.hidden = !direct;
      fPlace.hidden = !direct || !st.hasCloud;
      fTitle.hidden = !direct;
      fParent.hidden = !direct || (!open.length && !parent.value);
      bgWrap.hidden = !direct || !parent.value;
      if (!parent.value) bg.checked = false;
    }
    parent.onchange = fillParents;

    function grow() { prompt.style.height = "auto"; prompt.style.height = Math.min(prompt.scrollHeight + 2, 300) + "px"; }
    const saveDraft = () => { if (opts.draftKey) D.store.set(opts.draftKey, JSON.stringify({ prompt: prompt.value, title: title.value })); };
    const showTitleHint = () => { title.placeholder = prompt.value.trim() ? `Title: ${D.deriveTitle(prompt.value)}` : "Title (optional)"; };
    prompt.addEventListener("input", () => {
      grow();
      showTitleHint();
      if (prompt.value.trim()) err.textContent = "";
      saveDraft();
    });
    title.addEventListener("input", saveDraft);
    for (const f of [prompt, title]) {
      f.addEventListener("keydown", (e) => {
        if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) { e.preventDefault(); form.requestSubmit(); }
      });
    }
    app.onchange = () => { D.store.set(LAST_APP, app.value); fillParents(); showClaudeChoice(); };
    if (opts.draftKey) {
      try {
        const d = JSON.parse(D.store.get(opts.draftKey) || "null");
        if (d) { prompt.value = d.prompt ?? ""; title.value = d.title ?? ""; showTitleHint(); }
      } catch (_) { /* no draft */ }
    }

    function reset() {
      prompt.value = ""; title.value = ""; title.placeholder = "Title (optional)";
      bg.checked = false; err.textContent = "";
      saveDraft();
      setMode("lead");
      requestAnimationFrame(grow);
    }

    // Sends a piece of work with its receipt, once more if the PC did not answer.
    async function sendWork(body) {
      const key = JSON.stringify(body);
      if (!pending || pending.key !== key) pending = { key, receipt: D.receipt() };
      const req = { method: "POST", body: JSON.stringify({ ...body, receipt: pending.receipt }) };
      try {
        try { return await D.api("/threads", req); } catch (e) {
          if (!D.offline) throw e;
          await new Promise((ok) => setTimeout(ok, 1000));
          return await D.api("/threads", req);
        }
      } catch (e) {
        // The hub answered no: nothing was made, so the next tap is a new one.
        if (!D.offline) pending = null;
        throw e;
      }
    }

    form.onsubmit = async (e) => {
      e.preventDefault();
      if (sending) return;
      err.textContent = "";
      if (D.offline) { err.textContent = "Your PC isn't answering. Try again when it's back."; return; }
      const text = prompt.value.trim();
      if (!text) { err.textContent = "Say what you want first."; prompt.focus(); return; }
      const app_id = +app.value;
      if (!app_id) { err.textContent = "Pick an app."; app.focus(); return; }
      sending = true;
      start.disabled = true;
      start.replaceChildren(icon("running"), mode === "lead" ? "Sending" : "Starting");
      try {
        if (mode === "lead") {
          // Goals take no receipt yet, so this is sent once and never resent on its own.
          let goal;
          try { goal = await D.api("/goals", { method: "POST", body: JSON.stringify({ app_id, text }) }); } catch (x) {
            err.textContent = D.offline
              ? "Your PC didn't answer. Look on Goals before you send it again: it may have got there."
              : `Couldn't send it to the project lead. ${x.message}`;
            return;
          }
          D.store.set(LAST_APP, String(app_id));
          reset();
          D.toast("Sent to the project lead. It will ask what it needs, then show you a plan.", "send");
          if (opts.onGoal) opts.onGoal(goal);
          else if (goal?.id != null) location.href = `/goals#goal-${goal.id}`;
          return;
        }
        const t = title.value.trim() || D.deriveTitle(text);
        const pv = parent.value;
        const who = ai.value;
        let thread;
        try {
          thread = await sendWork({ app_id, title: t, prompt: text, ai: who, place: st.hasCloud ? place.value : "pc",
            parent_id: pv ? +pv : null, background: !!pv && bg.checked });
        } catch (x) {
          err.textContent = `Couldn't start the work. ${x.message}`;
          return;
        }
        pending = null;
        D.store.set(LAST_APP, String(app_id));
        D.store.set(LAST_AI, who);
        reset();
        D.toast(`Started "${t}". ${AI_NAMES[who] ?? "Claude"} will pick it up.`);
        opts.onStarted?.(thread, { title: t, ai: who });
      } finally {
        sending = false;
        paintStart();
      }
    };

    function update(next = {}) {
      st = { ...st, ...next, loaded: next.loaded ?? true };
      const has = st.apps.length > 0;
      form.hidden = st.loaded && !has;
      noApps.hidden = !st.loaded || has;
      if (changed("apps", st.apps.map((a) => [a.id, a.name]))) {
        const keep = app.value || (st.filter && st.filter !== "all" ? st.filter : D.store.get(LAST_APP));
        app.replaceChildren(...st.apps.map((a) => el("option", { value: String(a.id) }, a.name)));
        if (keep && st.apps.some((a) => String(a.id) === keep)) app.value = keep;
      }
      // The box starts on the app the owner picked on the board or with the
      // phone's app filter, and follows when they pick another one.
      if (st.filter !== lastFilter) {
        lastFilter = st.filter;
        if (st.filter && st.filter !== "all" && !wantApp) setApp(st.filter);
      }
      // An app asked for by fill() before the apps had loaded.
      if (wantApp && st.apps.some((a) => String(a.id) === wantApp)) { app.value = wantApp; wantApp = null; }
      // The choice of computer only shows once there is a cloud worker.
      if (!st.hasCloud) place.value = "pc";
      fillParents();
      showClaudeChoice();
      paintStart();
    }

    function setApp(v) {
      if (v == null || v === "all" || !st.apps.some((a) => String(a.id) === String(v))) return;
      app.value = String(v);
      // Remembered, so the box on another page starts on this app too.
      D.store.set(LAST_APP, app.value);
      fillParents();
      showClaudeChoice();
    }

    // Fills the box in for the owner, such as "Fix what it found" on finished work.
    // The owner still reads it and presses the button; nothing is sent here.
    function fill({ app_id, text, mode: m } = {}) {
      if (app_id != null) { wantApp = String(app_id); setApp(app_id); }
      if (text != null) { prompt.value = String(text); title.value = ""; showTitleHint(); saveDraft(); }
      err.textContent = "";
      setMode(m === "direct" ? "direct" : "lead");
      requestAnimationFrame(grow);
    }

    // The box to type in, or the "Add an app" button when there are no apps.
    function focus() {
      const target = form.hidden ? noApps.querySelector(".btn") : prompt;
      target?.focus({ preventScroll: true });
      if (target === prompt) { const n = prompt.value.length; try { prompt.setSelectionRange(n, n); } catch (_) { /* no caret */ } }
    }

    setMode("lead");
    requestAnimationFrame(grow);
    return { el: root, update, setApp, fill, focus, reset };
  };

  // D.newWork({ app_id, text, mode }) opens the page's one "What do you want?"
  // box, filled in, with the project lead chosen unless mode is "direct". Each
  // page says how it shows the box with D.setNewWork(fn); fn gets the same
  // object. Any open panel is closed first, so the box is in front.
  let opener = null;
  D.setNewWork = (fn) => { opener = fn; };
  D.newWork = async (fill = {}) => {
    const wasWork = !!history.state?.work;
    for (const d of document.querySelectorAll("dialog[open]")) d.close();
    // The work panel steps back in history as it closes; wait for that first.
    if (wasWork) await new Promise((ok) => { window.addEventListener("popstate", ok, { once: true }); setTimeout(ok, 600); });
    if (opener) opener({ mode: "lead", ...fill });
    else location.href = "/#board";
  };
})();
