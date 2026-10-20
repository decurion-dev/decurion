// Decurion's settings, shared by the board and the phone: one Sign-ins list
// with a light for every account (and "Sign in on this PC" for Claude), apps
// (a short list, then one page per app with its rules, plan and money,
// websites, files, name and folder), computers, accounts and caps, test keys,
// the GitHub token, website sign-ins and paired phones, plus the usage
// summary the board shows in its sidebar.
//
// What must stay true:
// - Everything is built with D.el, so names, paths and hub text go in as
//   text, never as HTML. The one exception is the pairing QR code, which the
//   hub draws itself from the pairing link.
// - Links from the hub (such as Tailscale's page to turn on HTTPS) are
//   shown only when they are https, and built with D.el as links.
// - Nothing the owner is typing is wiped by the timed refresh: lists with a
//   focused field are not redrawn, and redraws give focus back by data-key.
// - Test-key values and the GitHub token go to the hub once, in a password
//   field with autofill and spellcheck off, are cleared after saving, and
//   never come back except as their last four characters. They are never
//   kept in the page's storage, put in a toast or logged.
// - An app's Max plan changes only when the owner taps Save beside it on the
//   app's page; nothing here switches plans by itself.
// - An app's rules (merge, deploy, checked work going up by itself, patient
//   data) change only on the owner's tap. The hub checks every change again,
//   refuses Deploy without valid steps, and its words go next to the field.
//
// - Claude's sign-in never reaches this page: "Sign in on this PC" only
//   starts it, shows Claude's own link if its page did not open, and sends
//   the short code that page gives. Lights come from the hub as they are.
//
// Each area is its own tab (#settings/<tab>): a side list on the PC, a list
// to tap into on the phone. Each app has its own page inside the Apps tab,
// #settings/apps/<id>.
//
// Use: D.settings.mount(box, { refresh, onSignedOut, place, top }) once, then
// D.settings.update({ apps, spend, accounts, workers }) after each load,
// D.settings.show(part) when the settings view opens or its address changes
// (part is what follows "#settings/": "apps" or "apps/3"),
// D.settings.hide() when it closes, and D.settings.tick() on the page's
// timer. top is shown above the phone's list of tabs.
// D.settings.usage(el) draws the pay-per-use and Max plan summary into el.
// D.settings.setFirst(app, on) stars an app to go first today, or unstars it.
"use strict";
(() => {
  const { el, icon } = D;

  // ---------- State ----------
  let root = null, opts = { refresh: async () => {}, onSignedOut: () => {}, place: "pc" };
  let shown = false, loaded = false;
  let apps = [], spend = null, accounts = null, workers = [], hasCloud = false;
  const siteDraft = {}, siteErr = {}, siteBusy = {}; // app id -> typed text, error, saving
  const sitesSaved = new Map();  // app id -> list the hub just stored
  const rulesSaved = new Map();  // app id -> rules the hub just stored
  // An app's name and folder form: app id -> typed values (only once the
  // owner types), errors by field, saving.
  const editing = {}, editErr = {}, editBusy = {}, starBusy = {};
  // An app's rules: app id -> typed deploy steps, live site, reviewer and
  // plan; errors by field; saving; the deploy steps opened before Deploy is on.
  // saving holds what is on its way to the hub: "rules-3", "reviewer-3", "profile-3".
  const rulesDraft = {}, rulesErr = {}, deployOpen = {};
  const saving = new Set();
  let sitesSeq = 0, builtIn = null, devices = null;
  let conn = null;                // GET /phone/connection: the address phones use
  const sigs = {};
  const parts = {};               // elements this module draws into, by name

  const phone = () => opts.place === "phone";
  const here = () => (phone() ? "your PC" : "this PC");

  // ---------- Small helpers ----------
  const isMax = (a) => a.account === "A" || a.account === "B";
  const pct = (n) => `${Math.round(n)}%`;
  function changed(name, value) {
    const s = JSON.stringify(value);
    if (sigs[name] === s) return false;
    sigs[name] = s;
    return true;
  }
  // Nothing in a list is redrawn while the owner is typing in it.
  const typed = (e) => e?.tagName === "INPUT" || e?.tagName === "TEXTAREA";
  const typingIn = (box) => { const a = document.activeElement; return typed(a) && box.contains(a); };
  // Re-renders without losing keyboard focus: focus is restored by data-key.
  function keepFocus(fn) {
    const was = document.activeElement;
    const key = was?.dataset?.key;
    let caret = null;
    try { if (typed(was) && was.selectionStart != null) caret = [was.selectionStart, was.selectionEnd]; } catch (_) { /* no caret */ }
    fn();
    if (key && document.activeElement?.dataset?.key !== key) {
      const now = document.querySelector(`[data-key="${CSS.escape(key)}"]`);
      now?.focus({ preventScroll: true });
      if (caret && typed(now)) try { now.setSelectionRange(...caret); } catch (_) { /* no caret */ }
    }
  }
  const focusKey = (key) => document.querySelector(`[data-key="${CSS.escape(key)}"]`)?.focus({ preventScroll: true });
  function untilText(iso) {
    const d = new Date(iso);
    if (isNaN(d)) return "later";
    return d.toDateString() === new Date().toDateString()
      ? d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })
      : d.toLocaleString([], { weekday: "long", hour: "numeric", minute: "2-digit" });
  }
  function meterEl(frac, near, label, now, max) {
    return el("div", { className: `meter${near ? " near" : ""}`, role: "meter", "aria-valuemin": "0", "aria-valuemax": String(max), "aria-valuenow": String(now), "aria-label": label },
      el("span", { style: `width:${(Math.max(0, Math.min(1, frac)) * 100).toFixed(1)}%` }));
  }
  // Pay-per-use money across Claude, ChatGPT and Grok.
  function paidTotals() {
    const paid = (accounts ?? []).filter((a) => !isMax(a));
    const spent = paid.reduce((s, a) => s + (a.spent_usd ?? 0), 0);
    const cap = paid.reduce((s, a) => s + (a.cap_usd ?? 0), 0);
    const near = paid.filter((a) => (a.cap_usd > 0 ? a.spent_usd >= 0.8 * a.cap_usd : a.spent_usd > 0));
    return { paid, spent, cap, near };
  }
  // A Max plan's fullest allowance, 0 to 100, or null before any reading.
  const planUsed = (a) => (a.week_pct == null && a.five_hour_pct == null ? null : Math.max(a.week_pct ?? 0, a.five_hour_pct ?? 0));
  const planNear = (a) => a.limited || (planUsed(a) ?? 0) >= 80;
  const appsOn = (a) => apps.filter((x) => !x.pay_per_use && x.profile === a.account).map((x) => x.name);
  // A password field for a secret: no autofill, no spellcheck, no capitals.
  function secretInput(attrs) {
    const i = el("input", { type: "password", autocomplete: "new-password", autocapitalize: "off", ...attrs });
    i.spellcheck = false;
    i.setAttribute("autocorrect", "off");
    i.setAttribute("data-lpignore", "true");
    return i;
  }
  function showButton(input) {
    const show = el("button", { type: "button", className: "btn quiet sm", "aria-controls": input.id, "aria-pressed": "false" }, "Show");
    show.onclick = () => {
      const hidden = input.type === "password";
      input.type = hidden ? "text" : "password";
      show.textContent = hidden ? "Hide" : "Show";
      show.setAttribute("aria-pressed", hidden ? "true" : "false");
    };
    return show;
  }
  const signedOut = () => opts.onSignedOut?.();
  // replaceChildren would print a left-out piece as the word "null".
  const fill = (box, ...kids) => box.replaceChildren(...kids.filter((k) => k != null && k !== false));

  // ---------- Usage summary: the board's sidebar and the top of phone Settings ----------
  function usage(u) {
    if (!u) return;
    if (!spend) { u.hidden = true; return; }
    if (!changed(`usage-${u.id || "x"}`, [spend, accounts, apps.map((a) => [a.profile, a.pay_per_use]), Math.floor(Date.now() / 60000)]) && !u.hidden) return;
    u.hidden = false;
    if (!accounts) {
      // No account figures: the Claude pay-per-use total alone.
      const { pay_per_use_usd: used, cap_usd: cap, max_equivalent_usd: max } = spend;
      const near = cap > 0 ? used >= 0.8 * cap : used > 0;
      u.classList.toggle("near", near);
      u.removeAttribute("title");
      u.replaceChildren(
        el("span", { className: "lbl" }, "Pay-per-use this month"),
        el("span", { className: "num" }, el("b", {}, D.money(used)), el("span", {}, ` of ${D.money(cap)}`)),
        meterEl(cap > 0 ? used / cap : (used > 0 ? 1 : 0), near, "Pay-per-use spent this month", used, cap),
        el("span", { className: "hint" }, `Max plans used ${D.money(max)} worth`));
      return;
    }
    const { paid, spent, cap, near } = paidTotals();
    u.classList.toggle("near", near.length > 0);
    u.title = paid.map((a) => `${a.label}: ${D.money(a.spent_usd)} of ${D.money(a.cap_usd)}`).join("\n");
    // Each Max plan in use or with a reading, under the money.
    const plans = accounts.filter((a) => isMax(a) && (a.seen_at || appsOn(a).length));
    fill(u,
      el("span", { className: "lbl" }, "Pay-per-use this month"),
      el("span", { className: "num" }, el("b", {}, D.money(spent)), el("span", {}, ` of ${D.money(cap)}`)),
      meterEl(cap > 0 ? spent / cap : (spent > 0 ? 1 : 0), near.length > 0, "Pay-per-use spent this month", spent, cap),
      near.length ? el("span", { className: "hint" }, `${near.map((a) => a.label).join(" and ")} near ${near.length === 1 ? "its" : "their"} cap`) : null,
      ...plans.flatMap((a) => {
        const used = planUsed(a);
        // The fuller allowance is the one that runs out first.
        const text = a.limited ? "used up" : used == null ? "no reading yet"
          : (a.five_hour_pct ?? -1) > (a.week_pct ?? -1) ? `${pct(a.five_hour_pct)} of 5 hours` : `${pct(a.week_pct)} of week`;
        const line = el("span", { className: `plan${planNear(a) ? " near" : ""}` }, el("span", {}, a.label), el("span", {}, text));
        return used == null ? [line] : [line, meterEl(used / 100, planNear(a), `${a.label} allowance used`, Math.round(used), 100)];
      }));
  }

  // ---------- The section markup ----------
  function card(id, iconName, title, about, ...body) {
    const h = `h-${id}`;
    return el("section", { className: "scard wide live", id: `set-${id}`, "aria-labelledby": h },
      el("header", {}, el("span", { className: "ic" }, icon(iconName, "l")), el("div", {}, el("h2", { id: h }, title), el("p", {}, about))),
      ...body);
  }
  const loading = (tag = "li") => (tag === "li" ? el("li", { className: "empty" }, "Loading…") : el("p", { className: "hint" }, "Loading…"));

  function build() {
    // Apps: a short list, and one page per app (#settings/apps/<id>).
    parts.appRows = el("ul", { className: "rows app-list", id: "app-rows" }, loading());
    parts.builtIn = el("details", { className: "fold", id: "sites-builtin", hidden: true },
      el("summary"), el("ul", { className: "builtin", "aria-label": "Always allowed websites" }));
    parts.sitesNote = el("div", { className: "sites-note", id: "sites-note", hidden: true },
      el("p", { className: "hint", id: "sites-hint" }, "Claude and the package stores (npm, PyPI, crates.io) are always allowed. An entry like stripe.com also covers its subdomains. Changes apply to the next piece of work."),
      parts.builtIn);
    parts.addApp = addAppForm();
    parts.appPage = el("div", { className: "app-page", id: "app-page", hidden: true });
    // Computers
    parts.workerRows = el("ul", { className: "rows", id: "worker-rows" }, loading());
    parts.addCloud = el("a", { className: "btn", id: "add-cloud", href: "/cloud" });
    // Accounts
    parts.spendBig = el("div", { className: "spend-big", id: "spend-big" });
    parts.accountRows = el("ul", { className: "rows", id: "account-rows" }, loading());
    // Keys, GitHub, sign-ins
    parts.keys = el("div", { id: "key-apps" }, loading("p"));
    parts.github = el("div", { id: "github-token" }, loading("p"));
    parts.signIns = el("div", { id: "sign-in-apps" }, loading("p"));
    // Sign-ins
    parts.lights = el("ul", { className: "rows", id: "sign-in-lights" }, loading());
    parts.lightsNote = el("div", { id: "sign-in-note" });
    parts.claude = el("div", { id: "claude-sign-in", hidden: true });
    // Phones
    parts.pair = el("button", { type: "button", className: "btn", id: "pair" });
    parts.pair.onclick = pairPhone;
    pairLabel();
    parts.qr = el("div", { className: "qr", id: "qr", hidden: true });
    parts.devices = el("ul", { className: "rows", id: "devices" }, loading());
    // Which Decurion this PC runs
    parts.version = el("div", { id: "decurion-version" }, loading("p"));

    const phones = el("section", { className: `scard live${phone() ? " wide" : ""}`, id: "set-phones", "aria-labelledby": "h-phones" },
      el("header", {}, el("span", { className: "ic" }, icon("phone", "l")), el("div", {}, el("h2", { id: "h-phones" }, "Phones"),
        el("p", {}, phone()
          ? "Pair another phone to get the inbox there too. The code works once, for a few minutes."
          : "Pair your phone to get the inbox there. The code works once, for a few minutes."))),
      el("div", {}, parts.pair),
      parts.qr,
      el("div", {}, el("h3", { className: "sub-h" }, "Paired phones and browsers"), parts.devices));

    const cards = {
      "sign-ins": card("sign-ins", "shield", "Sign-ins",
        "Every account Decurion signs in with, in one place. Green is fine, amber needs you soon, red needs you now. You get a reminder on your phone a week before a sign-in runs out.",
        parts.lightsNote, parts.claude, parts.lights),
      // The list and the open app's page share the Apps tab, so the tab stays
      // selected while an app's page is open.
      apps: el("div", { className: "set-apps" },
        parts.appList = card("apps", "app", "Apps",
          `Tap an app to see and change its rules. Each app is a git checkout on ${here()}. Agents work on their own copy and never touch yours.`,
          parts.appRows, parts.addApp),
        parts.appPage),
      computers: card("computers", "cloud", "Computers",
        "Where agents work. Each piece of work runs on your PC or on a cloud worker, under the same rules, and can move between them at any time.",
        parts.workerRows, el("div", {}, parts.addCloud)),
      accounts: card("accounts", "money", "Accounts",
        "Claude A and Claude B are flat-rate Max plans. Their bars show how much of this week's allowance and of the current five-hour allowance is used, as Claude last said while working. The other accounts are billed by use: work on them stops and asks you when the month's total reaches that account's cap. Each run can spend at most $10 of what is left.",
        parts.spendBig, parts.accountRows),
      keys: card("keys", "key", "Test keys",
        "Test keys reach this app's agents while they work. They are kept in Windows' secure store and shown again only as their last four characters. Live keys are refused.",
        parts.keys),
      github: card("github", "branch", "GitHub",
        `When you accept work, Decurion sends it to the app's GitHub page as a new branch, using ${here()}'s own GitHub sign-in. With a GitHub token saved here, it also opens the pull request for you. Without one, you get a button that opens it in one tap. Decurion merges only when you tap Merge, for apps whose rules allow it. Otherwise you merge on GitHub.`,
        parts.github),
      "site-sign-ins": card("site-sign-ins", "globe", "Website sign-ins",
        "Let this app's agents check pages that need a sign-in. You sign in once, in a sign-in window on your PC or phone; the sign-in is kept on your PC and your password is never saved. Use test accounts only: an agent may be able to read part of what a website keeps.",
        parts.signIns),
      // Add-ons: drawn by addons.js (window.DAddons), in a tab of their own.
      addons: window.DAddons?.card({ onSignedOut: signedOut }) ?? null,
      phones,
      voice: voiceCard(),
      decurion: card("decurion", "pc", "Decurion",
        "Which Decurion your PC runs, and the changes merged on GitHub that it isn't running yet. Decurion never updates itself: you choose when.",
        parts.version),
    };
    return tabs(cards);
  }

  // ---------- Tabs ----------
  // One tab per area, so the owner sees one thing at a time instead of a long
  // page. On the PC the tabs are a list beside the open one; on the phone they
  // are a list to tap into, with a way back. Each tab is a plain link,
  // #settings/<id>, so the back button works and anything (a reminder on the
  // phone, a button elsewhere) can open one directly. Every card stays built
  // while hidden, so moving between tabs never loses what the owner typed.
  const TABS = [
    { id: "sign-ins", icon: "shield", name: "Sign-ins", about: "Every account Decurion uses" },
    { id: "apps", icon: "app", name: "Apps", about: "Your apps and the rules for each" },
    { id: "computers", icon: "cloud", name: "Computers", about: "Your PC and cloud workers" },
    { id: "accounts", icon: "money", name: "Accounts", about: "Max plans and pay-per-use money" },
    { id: "keys", icon: "key", name: "Test keys", about: "Keys agents use while testing" },
    { id: "github", icon: "branch", name: "GitHub", about: "Where accepted work goes" },
    { id: "site-sign-ins", icon: "globe", name: "Website sign-ins", about: "Pages agents may sign in to" },
    { id: "addons", icon: "tool", name: "Add-ons", about: "Extra tools for your agents" },
    { id: "phones", icon: "phone", name: "Phones", about: "Pair a phone, see paired ones" },
    { id: "voice", icon: "mic", name: "Voice", about: "Talk and hear replies on this device" },
    { id: "decurion", icon: "pc", name: "Decurion", about: "Which Decurion your PC runs" },
  ];
  // Older links used other names for some tabs.
  const OLD_NAMES = { spending: "accounts" };
  // appOpen is the id of the app whose page is open (#settings/apps/<id>),
  // as typed in the address; fromApps is true when the owner tapped into it
  // from the list, so its way back steps back in history.
  let tabList = [], current = null, fromList = false, appOpen = null, fromApps = false;
  // From an app's page back to the list it was opened from, past the tabs
  // stepped through on the way (pickTab counts them), or from a tab back to
  // Settings' list on the phone.
  const backToList = () => history.go(-1 - (appOpen != null ? history.state?.appDepth ?? 0 : 0));
  const tabIds = () => tabList.map((t) => t.id);

  function tabs(cards) {
    tabList = TABS.filter((t) => cards[t.id]);
    parts.cards = cards;
    parts.notes = {};
    parts.links = {};
    const links = tabList.map((t) => {
      parts.notes[t.id] = el("span", { className: "note" }, t.about);
      const a = el("a", { className: "set-tab", href: `#settings/${t.id}`, dataset: { key: `tab-${t.id}` } },
        el("span", { className: "ic" }, icon(t.icon)),
        el("span", { className: "nm" }, el("b", {}, t.name), parts.notes[t.id]),
        phone() ? icon("right") : null);
      parts.links[t.id] = a;
      return el("li", {}, a);
    });
    parts.index = el("div", { className: "set-index" }, phone() ? opts.top : null,
      el("nav", { "aria-label": "Settings" }, el("ul", { className: "set-tabs" }, ...links)));
    // The phone's way back to the list. Coming from the list, it steps back
    // in history, so the back button and this link agree.
    // Inside an app's page it leads back to the list of apps instead.
    const back = phone() ? el("a", { className: "set-back", href: "#settings" }, icon("left"), el("span", {}, "Settings")) : null;
    if (back) back.onclick = (e) => { if (appOpen != null ? fromApps : fromList) { e.preventDefault(); backToList(); } };
    parts.back = back;
    parts.panel = el("div", { className: "set-panel" }, back, ...tabList.map((t) => cards[t.id]));
    const box = el("div", { className: `settings${phone() ? " on-phone" : ""}` }, parts.index, parts.panel);
    parts.box = box;
    select(current);
    return box;
  }
  // Opens one tab, or on the phone the list when id is null. The PC always
  // has a tab open: the first one when none is asked for. stepped is true
  // when the owner moved here from inside Settings, not from a link elsewhere.
  // want is what follows "#settings/": a tab's id, "apps/<id>" for one
  // app's page, or "apps/<id>/<tab>" for one tab of it.
  function select(want, stepped = false) {
    let [id, sub, part] = typeof want === "string" ? want.split("/") : [want];
    id = OLD_NAMES[id] ?? id;
    if (!tabIds().includes(id)) { id = phone() ? null : tabList[0]?.id ?? null; sub = null; }
    const app = id === "apps" && sub ? sub : null;
    // Its tab: #settings/apps/<id> opens Rules.
    if (app != null) {
      appTab = appTabOf(part);
      if (page && String(page.id) === app) page.tabs.select(appTab);
    }
    const wasApp = appOpen;
    if (id !== current) fromList = stepped && phone() && current === null && id !== null;
    if (app !== appOpen) fromApps = stepped && current === "apps" && appOpen === null && app !== null;
    current = id;
    appOpen = app;
    for (const t of tabList) {
      parts.cards[t.id].hidden = t.id !== id;
      if (t.id === id) parts.links[t.id].setAttribute("aria-current", "page");
      else parts.links[t.id].removeAttribute("aria-current");
    }
    parts.box?.classList.toggle("tab-open", id !== null);
    if (parts.appList) { parts.appList.hidden = app != null; parts.appPage.hidden = app == null; }
    if (parts.back) {
      parts.back.href = app != null ? "#settings/apps" : "#settings";
      parts.back.lastChild.textContent = app != null ? "Apps" : "Settings";
    }
    if (app !== wasApp) {
      render();
      // Focus follows the owner: to the app's name on its page, or back to
      // its row in the list.
      if (stepped && app != null) parts.appPage.querySelector("h2")?.focus({ preventScroll: true });
      else if (stepped && wasApp != null) focusKey(`app-go-${wasApp}`);
    }
    return id;
  }
  // A short status beside a tab's name, such as "2 need you" on Sign-ins.
  // Without one, the tab shows its plain description (on the phone only).
  function tabNote(id, text, tone) {
    const n = parts.notes?.[id];
    if (!n) return;
    const t = TABS.find((x) => x.id === id);
    n.className = `note${tone ? ` status ${tone}` : ""}`;
    fill(n, tone ? icon("alert") : null, text ?? t.about);
  }

  // Voice choices are kept on each phone or PC, not on the hub, so this card
  // only appears where the browser can listen or read aloud (voice.js).
  function voiceCard() {
    const V = window.Voice;
    if (!V || !(V.canListen() || V.canSpeak())) return null;
    return el("section", { className: "scard live", id: "set-voice", "aria-labelledby": "h-voice" },
      el("header", {}, el("span", { className: "ic" }, V.icon("mic")), el("div", {}, el("h2", { id: "h-voice" }, "Voice"),
        el("p", {}, "Talk instead of typing, and hear replies read aloud. These choices are kept on this device only."))),
      V.settingsPanel());
  }

  // ---------- Add an app ----------
  function addAppForm() {
    const field = (...kids) => el("div", { className: "field" }, ...kids);
    const name = el("input", { id: "app-name", type: "text", autocomplete: "off", autocapitalize: "off", placeholder: "pv-medispa", "aria-describedby": "app-name-hint app-name-err" });
    const path = el("input", { id: "app-path", type: "text", autocomplete: "off", autocapitalize: "off", placeholder: "C:\\path\\to\\repo", "aria-describedby": "app-path-hint app-path-err" });
    path.spellcheck = false;
    const nameErr = el("span", { className: "field-error", id: "app-name-err" });
    const pathErr = el("span", { className: "field-error", id: "app-path-err" });
    const profile = el("select", { id: "app-profile", "aria-describedby": "app-profile-hint" },
      el("option", { value: "A" }, "Max plan A"), el("option", { value: "B" }, "Max plan B"));
    const ppu = el("input", { type: "checkbox", id: "app-ppu" });
    const btn = el("button", { className: "btn primary", type: "submit", id: "add-app-btn" }, "Add app");
    const err = el("span", { className: "field-error", id: "app-err", role: "alert" });
    // A folder picker on the PC, when this hub has one. Typing still works.
    const pick = el("button", { type: "button", className: "btn", id: "app-path-pick", hidden: typeof D.pickFolder !== "function" }, icon("app"), "Choose folder");
    pick.onclick = async () => {
      if (typeof D.pickFolder !== "function") return;
      pick.disabled = true;
      try {
        const chosen = await D.pickFolder({ title: "Choose your app's folder", start: path.value.trim() || undefined });
        if (chosen) { path.value = chosen; fieldErr(path, pathErr, ""); }
      } catch (x) { fieldErr(path, pathErr, `Couldn't open the folder picker. ${x.message}`); }
      finally { pick.disabled = false; path.focus(); }
    };
    function fieldErr(input, errEl, msg) {
      errEl.textContent = msg || "";
      if (msg) input.setAttribute("aria-invalid", "true"); else input.removeAttribute("aria-invalid");
    }
    name.addEventListener("input", () => fieldErr(name, nameErr, ""));
    path.addEventListener("input", () => fieldErr(path, pathErr, ""));

    const form = el("form", { id: "add-app", className: "form-grid", noValidate: true },
      el("h3", { className: "full" }, "Add an app"),
      field(el("label", { htmlFor: "app-name" }, "Name"), name,
        el("span", { className: "hint", id: "app-name-hint" }, "1 to 40 letters, digits, - or _. No spaces."), nameErr),
      field(el("label", { htmlFor: "app-path" }, "Repo path"),
        el("div", { className: "path-row" }, path, pick),
        el("span", { className: "hint", id: "app-path-hint" }, `The full path to the app's git folder on ${here()}.`), pathErr),
      field(el("label", { htmlFor: "app-profile" }, "Claude account"), profile,
        el("span", { className: "hint", id: "app-profile-hint" }, "Which signed-in Claude Max plan the agents use.")),
      field(el("span", { className: "label" }, "Billing"),
        el("label", { className: "check" }, ppu, " Pay-per-use instead of a Max plan"),
        el("span", { className: "hint" }, "Billed by use, under the Claude pay-per-use cap. Switch any time.")),
      el("div", { className: "full actions" }, btn, el("span", { className: "hint" }, "New apps start at PR only."), err));
    form.onsubmit = async (e) => {
      e.preventDefault();
      const n = name.value.trim(), p = path.value.trim();
      err.textContent = "";
      let bad = false;
      if (!/^[A-Za-z0-9_-]{1,40}$/.test(n)) { fieldErr(name, nameErr, n ? "Use only letters, digits, - or _, up to 40." : "Give the app a name."); bad = true; } else fieldErr(name, nameErr, "");
      if (!/^([A-Za-z]:[\\/]|\/)/.test(p) || /[,"]/.test(p)) { fieldErr(path, pathErr, p ? "Use the full path, like C:\\path\\to\\repo, without commas or quotes." : "Enter the repo path."); bad = true; } else fieldErr(path, pathErr, "");
      if (bad) { form.querySelector("[aria-invalid]")?.focus(); return; }
      btn.disabled = true;
      try {
        await D.api("/apps", { method: "POST", body: JSON.stringify({ name: n, repo_path: p, profile: profile.value, pay_per_use: ppu.checked }) });
        form.reset();
        D.toast(`Added ${n} at PR only. You can start work on it now.`);
        await opts.refresh();
        loadKeys();
        loadSignIns();
      } catch (x) {
        if (x.signedOut) { signedOut(); return; }
        if (/name/i.test(x.message)) fieldErr(name, nameErr, x.message.replace(/^an app/, "An app").replace(/^app name/, "The name"));
        else if (/repo_path/.test(x.message)) fieldErr(path, pathErr, "Use the full path, without commas or quotes.");
        else err.textContent = `Couldn't add the app. ${x.message}`;
      } finally { btn.disabled = false; }
    };
    return form;
  }

  // ---------- Go first today ----------
  // Only orders the queue: caps, plan limits and where work runs still
  // apply. The star ends by itself when the day does.
  async function setFirst(a, on) {
    if (starBusy[a.id]) return false;
    starBusy[a.id] = true;
    render();
    try {
      await D.send(`/apps/${a.id}/first`, { on });
      D.toast(on ? `${a.name} goes first today. Its waiting work starts before other apps' work until the day ends.`
        : `${a.name} takes turns with the other apps again.`);
      await opts.refresh();
      return true;
    } catch (e) {
      if (e.signedOut) { signedOut(); return false; }
      D.toast(`Couldn't change which app goes first. ${e.message}`, "alert");
      return false;
    } finally {
      delete starBusy[a.id];
      render();
    }
  }

  function starButton(a) {
    const on = !!a.first_today;
    const b = el("button", { type: "button", className: "btn sm star", "aria-pressed": on ? "true" : "false", dataset: { key: `first-${a.id}` },
      title: on ? "Its waiting work starts before other apps' work today. Tap to let it take turns again." : "Let its waiting work start before other apps' work today" },
      icon("star"), on ? "Going first today" : "Go first today");
    b.disabled = D.offline || !!starBusy[a.id];
    b.onclick = () => setFirst(a, !on);
    return b;
  }

  // ---------- An app's name and folder ----------
  // Only what changed is sent. The hub checks everything again and says in
  // plain words what to fix; its answer goes next to the field it is about.
  // What the owner types is kept in editing, so a redraw never loses it.
  function editForm(a) {
    const d = editing[a.id] ?? { name: a.name, repo_path: a.repo_path };
    const errs = editErr[a.id] ?? {};
    const busy = !!editBusy[a.id] || D.offline;
    const key = (k) => `edit-${k}-${a.id}`;
    const errEl = (k) => el("span", { className: "field-error", id: `${key(k)}-err`, role: "alert" }, errs[k] ?? "");
    const cleared = (k, input, err) => () => {
      editing[a.id] = { ...(editing[a.id] ?? d), [k]: input.value };
      if (errs[k]) { delete errs[k]; err.textContent = ""; input.removeAttribute("aria-invalid"); }
    };
    const text = (k, attrs) => {
      const i = el("input", { id: key(k), type: "text", autocomplete: "off", autocapitalize: "off", value: d[k] ?? "",
        dataset: { key: key(k) }, "aria-describedby": `${key(k)}-hint ${key(k)}-err`, "aria-invalid": errs[k] ? "true" : null, ...attrs });
      i.spellcheck = false;
      return i;
    };
    const field = (...kids) => el("div", { className: "field" }, ...kids);
    const hint = (k, words) => el("span", { className: "hint", id: `${key(k)}-hint` }, words);

    const name = text("name", { maxLength: 40 }), nameErr = errEl("name");
    name.oninput = cleared("name", name, nameErr);
    const path = text("repo_path", {}), pathErr = errEl("repo_path");
    path.oninput = cleared("repo_path", path, pathErr);
    const pick = el("button", { type: "button", className: "btn", dataset: { key: key("pick") }, hidden: typeof D.pickFolder !== "function" }, icon("app"), "Choose folder");
    pick.onclick = async () => {
      if (typeof D.pickFolder !== "function") return;
      pick.disabled = true;
      try {
        const chosen = await D.pickFolder({ title: `Choose ${a.name}'s folder`, start: path.value.trim() || undefined });
        if (chosen) { path.value = chosen; cleared("repo_path", path, pathErr)(); }
      } catch (x) { errs.repo_path = `Couldn't open the folder picker. ${x.message}`; editErr[a.id] = errs; render(); }
      finally { pick.disabled = false; focusKey(key("repo_path")); }
    };
    const save = el("button", { type: "submit", className: "btn primary", disabled: busy, dataset: { key: key("save") } }, busy && !D.offline ? "Saving…" : "Save");
    const cancel = el("button", { type: "button", className: "btn", dataset: { key: key("cancel") } }, "Cancel");
    cancel.onclick = () => { delete editing[a.id]; delete editErr[a.id]; sigs["app-name"] = ""; render(); focusKey(key("name")); };

    const form = el("form", { className: "form-grid", id: `app-edit-${a.id}`, noValidate: true, "aria-label": `Change ${a.name}'s name and folder` },
      field(el("label", { htmlFor: key("name") }, "Name"), name, hint("name", "1 to 40 letters, digits, - or _. No spaces."), nameErr),
      field(el("label", { htmlFor: key("repo_path") }, "Folder"), el("div", { className: "path-row" }, path, pick),
        hint("repo_path", `The full path to the app's git folder on ${here()}.`), pathErr),
      el("div", { className: "full actions" }, save, cancel, errs.all ? el("span", { className: "field-error", role: "alert" }, errs.all) : null));

    form.onsubmit = async (ev) => {
      ev.preventDefault();
      if (editBusy[a.id]) return;
      if (D.offline) { editErr[a.id] = { all: "Your PC isn't answering. Try again when it's back." }; render(); return; }
      const n = name.value.trim(), p = path.value.trim();
      const bad = {};
      if (!/^[A-Za-z0-9_-]{1,40}$/.test(n)) bad.name = n ? "Use only letters, digits, - or _, up to 40." : "Give the app a name.";
      if (!/^([A-Za-z]:[\\/]|\/)/.test(p) || /[,"]/.test(p)) bad.repo_path = p ? "Use the full path, like C:\\path\\to\\repo, without commas or quotes." : "Enter the folder's full path.";
      const first = Object.keys(bad)[0];
      if (first) { editErr[a.id] = bad; render(); focusKey(key(first)); return; }
      const body = {};
      if (n !== a.name) body.name = n;
      if (p !== a.repo_path) body.repo_path = p;
      if (!Object.keys(body).length) {
        delete editing[a.id]; delete editErr[a.id]; render();
        D.toast(`Nothing to save. ${a.name} is as it was.`);
        return;
      }
      editBusy[a.id] = true;
      delete editErr[a.id];
      render();
      try {
        const saved = await D.send(`/apps/${a.id}/edit`, body);
        delete editing[a.id];
        keepSaved(a, saved);
        D.toast(savedText(a, saved ?? { ...a, ...body }, body));
        await opts.refresh();
        // Test keys and sign-ins list apps by name.
        if (body.name) { loadKeys(); loadSignIns(); }
      } catch (e) {
        if (e.signedOut) { signedOut(); return; }
        const m = e.message;
        const k = /already called|rename/i.test(m) ? "name"
          : /folder|repo_path/i.test(m) ? "repo_path" : /name/i.test(m) ? "name" : "all";
        const words = k === "all" ? `Couldn't save ${a.name}. ${m}`
          : k === "repo_path" && /^repo_path/.test(m) ? "Use the full path, without commas or quotes."
          : m.replace(/^app name/, "The name");
        editErr[a.id] = { [k]: words };
      } finally {
        delete editBusy[a.id];
        render();
        const err = editErr[a.id] && Object.keys(editErr[a.id])[0];
        if (err && err !== "all") focusKey(key(err));
      }
    };
    return form;
  }

  // The hub answers a change with the whole app: show it straight away,
  // before the next load.
  function keepSaved(a, saved) {
    if (!saved || saved.id !== a.id) return;
    for (const x of apps) if (x.id === a.id) Object.assign(x, saved);
  }

  // What the owner gets from a saved change, in their words.
  function savedText(before, after, body) {
    const nm = after.name, said = [];
    if (body.name) said.push(`${before.name} is now called ${nm}.`);
    if (body.repo_path) said.push(`${nm} now uses the folder ${after.repo_path}.`);
    if (body.profile) said.push(before.pay_per_use
      ? `When ${nm} goes back to a Max plan, it uses Max plan ${after.profile}.`
      : `New work in ${nm} uses Max plan ${after.profile}. Running work keeps its plan.`);
    if ("reviewer" in body) said.push(after.reviewer
      ? `${after.reviewer} is asked to review ${nm}'s pull requests.`
      : `No one is asked to review ${nm}'s pull requests now.`);
    return `Saved. ${said.join(" ")}`;
  }

  // The reviewer and the Max plan, saved from the app's page through the
  // same change as the name and folder (POST /apps/{id}/edit).
  async function saveField(a, k, body) {
    const tag = `${k}-${a.id}`;
    if (saving.has(tag)) return;
    const errs = (rulesErr[a.id] ??= {});
    if (D.offline) { errs[k] = "Your PC isn't answering. Try again when it's back."; render(); return; }
    saving.add(tag);
    delete errs[k];
    render();
    try {
      const saved = await D.send(`/apps/${a.id}/edit`, body);
      if (rulesDraft[a.id]) delete rulesDraft[a.id][k];
      keepSaved(a, saved);
      D.toast(savedText(a, saved ?? { ...a, ...body }, body));
      await opts.refresh();
    } catch (e) {
      if (e.signedOut) { signedOut(); return; }
      errs[k] = k === "reviewer" ? e.message : `Couldn't change the plan. ${e.message}`;
    } finally {
      saving.delete(tag);
      render();
      if (errs[k]) focusKey(`rule-${k}-${a.id}`);
    }
  }

  // ---------- Rules for each app ----------
  // What Decurion may do for one app, set only by the owner's tap. PR only
  // is where every app starts: Decurion pushes and opens pull requests,
  // nothing more. Full control adds a Merge button on its pull requests and
  // a Deploy button after a merge, each one tap at a time. The hub checks
  // every change again; Deploy needs saved steps it accepts.
  const RULES_START = { merge: false, deploy: false, deploy_steps: "", deploy_site: null, send_checked: true, patient_data: true };
  const rulesOf = (a) => ({ ...RULES_START, ...(a.rules ?? {}) });
  const modeOf = (r) => (r.merge && r.deploy ? "full" : !r.merge && !r.deploy ? "pr" : "mix");
  const MODE = { pr: ["branch", "PR only"], full: ["done", "Full control"], mix: ["settings", "Your own mix"] };
  const ruleChip = (r) => { const [ic, words] = MODE[modeOf(r)]; return el("span", { className: `pill rule-${modeOf(r)}` }, icon(ic), words); };
  // Every app starts with this rule on, to be safe (migration 0095), so the
  // chip says what it does rather than claiming the app holds such data.
  const dataChip = () => el("span", { className: "pill", title: "Agents use made-up data only. Every app starts this way to be safe. If this app's live version holds no patient data, turn it off under the app's Rules." },
    icon("shield"), "Made-up data only");
  const hasSteps = (r) => /\S/.test(r.deploy_steps ?? "");

  // Sends a change to the app's rules. The hub answers with the whole app;
  // its rules are kept until a load newer than the change arrives.
  async function setRules(a, body, words) {
    const tag = `rules-${a.id}`;
    if (saving.has(tag)) return false;
    if (D.offline) { D.toast("Your PC isn't answering. Try again when it's back.", "alert"); return false; }
    const errs = (rulesErr[a.id] ??= {});
    const deploying = "deploy_steps" in body || "deploy_site" in body;
    saving.add(tag);
    if (deploying) { delete errs.steps; delete errs.site; }
    render();
    try {
      const saved = await D.send(`/apps/${a.id}/rules`, body);
      const rules = saved?.rules ?? { ...rulesOf(a), ...body };
      sitesSeq++;
      rulesSaved.set(a.id, rules);
      for (const x of apps) if (x.id === a.id) x.rules = rules;
      D.toast(words(rules));
      opts.refresh();
      return true;
    } catch (e) {
      if (e.signedOut) { signedOut(); return false; }
      if (deploying) errs[/site|https|address/i.test(e.message) && !/step|line/i.test(e.message) ? "site" : "steps"] = e.message;
      else D.toast(`Couldn't change ${a.name}'s rules. ${e.message}`, "alert");
      return false;
    } finally {
      saving.delete(tag);
      render();
    }
  }

  function prOnly(a) {
    delete deployOpen[a.id];
    return setRules(a, { merge: false, deploy: false },
      () => `${a.name} is at PR only. Decurion opens pull requests, and you or your reviewer merge on GitHub.`);
  }
  // Full control needs deploy steps first: without saved ones, Merge goes
  // on and the steps open, asking for them.
  async function fullControl(a, r) {
    if (hasSteps(r)) {
      return setRules(a, { merge: true, deploy: true },
        () => `${a.name} has full control. Its finished pull requests get a Merge button, and merged work a Deploy button.`);
    }
    deployOpen[a.id] = true;
    if (!r.merge) await setRules(a, { merge: true }, () => `Merge is on for ${a.name}. Add its deploy steps below to turn Deploy on too.`);
    else render();
    focusKey(`rule-steps-${a.id}`);
  }
  function tapDeploy(a, r) {
    if (r.deploy) {
      delete deployOpen[a.id];
      return setRules(a, { deploy: false }, () => `${a.name} has no Deploy button now. Its steps are kept.`);
    }
    if (deployOpen[a.id]) { delete deployOpen[a.id]; delete rulesErr[a.id]?.steps; delete rulesErr[a.id]?.site; render(); return; }
    if (hasSteps(r)) return setRules(a, { deploy: true }, () => `${a.name} can be deployed from Decurion now. After a merge, one tap runs its steps.`);
    deployOpen[a.id] = true;
    render();
    focusKey(`rule-steps-${a.id}`);
  }

  // One rule: its name, why it matters, and a switch.
  function ruleSwitch(a, k, title, why, on, busy, onTap) {
    const id = `rule-${k}-${a.id}`;
    const b = el("button", { type: "button", className: "switch rule-sw", role: "switch", "aria-checked": on ? "true" : "false",
      "aria-labelledby": `${id}-l`, "aria-describedby": `${id}-d`, dataset: { key: id }, disabled: busy }, el("span", { className: "track" }));
    b.onclick = onTap;
    return el("div", { className: "rule-row" },
      el("div", { className: "rule-txt" }, el("b", { id: `${id}-l` }, title), el("span", { className: "hint", id: `${id}-d` }, why)),
      b);
  }

  function rulesCard(a) {
    const r = rulesOf(a);
    const mode = modeOf(r);
    const busy = saving.has(`rules-${a.id}`) || D.offline;
    const errs = rulesErr[a.id] ?? {};
    const draft = (rulesDraft[a.id] ??= {});
    const opt = (m) => {
      const b = el("button", { type: "button", className: "rules-opt", "aria-pressed": mode === m ? "true" : "false",
        dataset: { key: `rule-mode-${m}-${a.id}` }, disabled: busy }, MODE[m][1]);
      b.onclick = () => (m === "pr" ? prOnly(a) : fullControl(a, r));
      return b;
    };
    const top = el("div", { className: "rules-top" },
      el("div", { className: "rules-seg", role: "group", "aria-label": `Rules for ${a.name}` }, opt("pr"), opt("full")),
      el("span", { className: mode === "mix" ? "rules-mix" : "hint" }, mode === "mix" ? "Your own mix"
        : mode === "pr" ? "Decurion pushes and opens pull requests. Nothing more."
          : "Decurion may also merge and deploy, one tap each."));

    const merge = ruleSwitch(a, "merge", "Merge from Decurion", r.merge
      ? "Finished pull requests get a Merge button. One tap merges, only once GitHub says the checks passed and nothing conflicts."
      : "Off: you or your reviewer merge on GitHub. On: finished pull requests get a Merge button.", r.merge, busy,
    () => setRules(a, { merge: !r.merge }, () => (r.merge
      ? `You or your reviewer merge ${a.name}'s pull requests on GitHub now.`
      : `${a.name}'s finished pull requests get a Merge button now.`)));

    const open = r.deploy || !!deployOpen[a.id];
    const deploy = ruleSwitch(a, "deploy", "Deploy from Decurion", r.deploy
      ? `After a merge you get a Ready to deploy card. One tap runs the steps below on ${here()}.`
      : "Off: no Deploy button. On: after a merge, one tap runs the steps you save here.", r.deploy, busy, () => tapDeploy(a, r));
    let steps = null;
    if (open) {
      const sk = `rule-steps-${a.id}`, wk = `rule-site-${a.id}`;
      const ta = el("textarea", { id: sk, className: "mono steps", rows: 4, autocomplete: "off", autocapitalize: "off", maxLength: 2000,
        value: draft.steps ?? r.deploy_steps ?? "", placeholder: "npm ci\nnpm run deploy", dataset: { key: sk },
        "aria-describedby": `${sk}-hint ${sk}-err`, "aria-invalid": errs.steps ? "true" : null });
      ta.spellcheck = false;
      const site = el("input", { id: wk, type: "url", inputMode: "url", autocomplete: "off", autocapitalize: "off", maxLength: 300,
        value: draft.site ?? r.deploy_site ?? "", placeholder: "https://app.example.com", dataset: { key: wk },
        "aria-describedby": `${wk}-hint ${wk}-err`, "aria-invalid": errs.site ? "true" : null });
      site.spellcheck = false;
      const stepsErr = el("span", { className: "field-error", id: `${sk}-err`, role: "alert" }, errs.steps ?? "");
      const siteErrEl = el("span", { className: "field-error", id: `${wk}-err`, role: "alert" }, errs.site ?? "");
      const clear = (k, input, err) => () => {
        draft[k] = input.value;
        if (errs[k]) { delete errs[k]; err.textContent = ""; input.removeAttribute("aria-invalid"); }
      };
      ta.oninput = clear("steps", ta, stepsErr);
      site.oninput = clear("site", site, siteErrEl);
      const save = el("button", { type: "submit", className: "btn primary", dataset: { key: `rule-deploy-save-${a.id}` }, disabled: busy },
        saving.has(`rules-${a.id}`) ? "Saving…" : r.deploy ? "Save" : "Save and turn on");
      let cancel = null;
      if (!r.deploy) {
        cancel = el("button", { type: "button", className: "btn", dataset: { key: `rule-deploy-cancel-${a.id}` } }, "Cancel");
        cancel.onclick = () => { delete deployOpen[a.id]; delete draft.steps; delete draft.site; delete errs.steps; delete errs.site; render(); focusKey(`rule-deploy-${a.id}`); };
      }
      steps = el("form", { className: "deploy-steps", noValidate: true, "aria-label": `Deploy steps for ${a.name}` },
        r.deploy ? null : el("div", { className: "notice" }, icon("tool"),
          el("span", { className: "grow" }, "Type the steps that deploy this app, then tap Save and turn on. Decurion can't deploy without them.")),
        el("div", { className: "field" }, el("label", { htmlFor: sk }, "Deploy steps"), ta,
          el("span", { className: "hint", id: `${sk}-hint` }, "One command per line, the same ones you'd type in the app's folder. Up to 8."), stepsErr),
        el("div", { className: "field" }, el("label", { htmlFor: wk }, "Live site"), site,
          el("span", { className: "hint", id: `${wk}-hint` }, "Where Open site goes when a deploy finishes. Optional."), siteErrEl),
        el("div", { className: "actions" }, save, cancel));
      steps.onsubmit = async (ev) => {
        ev.preventDefault();
        const text = ta.value.replace(/\r/g, "").replace(/\s+$/, "");
        const live = site.value.trim();
        if (!/\S/.test(text)) { errs.steps = "Type at least one step, such as npm run deploy."; rulesErr[a.id] = errs; render(); focusKey(sk); return; }
        if (live && !/^https:\/\//i.test(live)) { errs.site = "The live site must start with https://, like https://app.example.com."; rulesErr[a.id] = errs; render(); focusKey(wk); return; }
        const was = r.deploy;
        const ok = await setRules(a, { deploy: true, deploy_steps: text, deploy_site: live || null }, () => (was
          ? `Saved ${a.name}'s deploy steps.`
          : `${a.name} can be deployed from Decurion now. After a merge, one tap runs these steps.`));
        if (ok) { delete deployOpen[a.id]; delete draft.steps; delete draft.site; render(); }
        else focusKey(rulesErr[a.id]?.site ? wk : sk);
      };
    }

    const sent = ruleSwitch(a, "send", "Checked work goes to GitHub by itself",
      "A piece of a goal you approved goes up as a pull request once the checker passes it. Off: it waits for your Accept.", r.send_checked, busy,
      () => setRules(a, { send_checked: !r.send_checked }, () => (r.send_checked
        ? `Checked pieces of ${a.name}'s goals wait for your Accept now.`
        : `Checked pieces of ${a.name}'s goals go to GitHub by themselves now.`)));
    const data = ruleSwitch(a, "data", "Its live version holds patient data",
      "Agents are told to use made-up data only and never real records. Decurion never sees patient data either way.", r.patient_data, busy,
      () => setRules(a, { patient_data: !r.patient_data }, () => (r.patient_data
        ? `${a.name} is marked as holding no patient data.`
        : `${a.name}'s agents are told to use made-up data only.`)));

    return tabCard("What Decurion may do for this app. Agents can never change these.",
      top, merge, deploy, steps, sent, data, reviewerField(a), alwaysOn());
  }

  // Who reviews the app's pull requests: asked to review each one.
  function reviewerField(a) {
    const k = `rule-reviewer-${a.id}`, tag = `reviewer-${a.id}`;
    const errs = rulesErr[a.id] ?? {};
    const draft = (rulesDraft[a.id] ??= {});
    const busy = saving.has(tag) || D.offline;
    const input = el("input", { id: k, type: "text", autocomplete: "off", autocapitalize: "off", maxLength: 40,
      placeholder: "GitHub name, like param-k", value: draft.reviewer ?? a.reviewer ?? "", dataset: { key: k },
      "aria-describedby": `${k}-hint ${k}-err`, "aria-invalid": errs.reviewer ? "true" : null });
    input.spellcheck = false;
    const err = el("span", { className: "field-error", id: `${k}-err`, role: "alert" }, errs.reviewer ?? "");
    const same = () => input.value.trim().replace(/^@/, "") === (a.reviewer ?? "");
    const save = el("button", { type: "submit", className: "btn", dataset: { key: `${k}-save` }, disabled: busy || same() },
      saving.has(tag) ? "Saving…" : "Save");
    input.oninput = () => {
      draft.reviewer = input.value;
      save.disabled = D.offline || saving.has(tag) || same();
      if (errs.reviewer) { delete errs.reviewer; err.textContent = ""; input.removeAttribute("aria-invalid"); }
    };
    const form = el("form", { className: "rule-field", noValidate: true },
      el("div", { className: "field" }, el("label", { htmlFor: k }, "Who reviews its pull requests"),
        el("div", { className: "cap-row" }, input, save),
        el("span", { className: "hint", id: `${k}-hint` }, "Optional. They are asked to review each pull request. Leave it empty for no one."), err));
    form.onsubmit = (ev) => {
      ev.preventDefault();
      const rv = input.value.trim().replace(/^@/, "");
      if (rv && !(rv.length <= 39 && /^[A-Za-z0-9](-?[A-Za-z0-9])*$/.test(rv))) {
        (rulesErr[a.id] ??= {}).reviewer = "Type their GitHub name, like param-k. Only letters, digits and single dashes.";
        render(); focusKey(k); return;
      }
      if (rv === (a.reviewer ?? "")) return;
      saveField(a, "reviewer", { reviewer: rv || null });
    };
    return form;
  }

  // ---------- An app's page ----------
  // The style guide's rule: one thing at a time. Under the app's name sits a
  // row of tabs (D.tabs), one per part, Rules first; each tab has its own
  // link, #settings/apps/<id>/<tab>. The page is built once while the app is
  // open: switching tabs only hides and shows panels, and new data redraws
  // only the panel it changes, so nothing the owner typed is lost.
  const APP_TABS = [
    { id: "rules", label: "Rules" },
    { id: "money", label: "Plan and money" },
    { id: "websites", label: "Websites" },
    { id: "files", label: "Files" },
    { id: "name", label: "Name and folder" },
  ];
  const appTabOf = (t) => (APP_TABS.some((x) => x.id === t) ? t : "rules");
  let page = null;        // the open app's { id, head, panels, tabs }
  let appTab = "rules";   // the open tab, as the address says

  // One tab's box, with a line saying what it holds.
  function tabCard(about, ...body) {
    return el("section", { className: "scard live tab-card" }, about ? el("p", { className: "tab-about" }, about) : null, ...body);
  }

  function pageFor(id) {
    if (page?.id === id) return page;
    const panels = Object.fromEntries(APP_TABS.map((t) => [t.id, el("div", { className: "app-tab" })]));
    const head = el("div", { className: "app-head-box" });
    const tabs = D.tabs(APP_TABS.map((t) => ({ id: t.id, label: t.label, panel: panels[t.id] })),
      { label: "Parts of this app", selected: appTab, onPick: pickTab });
    page = { id, head, panels, tabs };
    for (const k of Object.keys(sigs)) if (k.startsWith("app-")) delete sigs[k];
    fill(parts.appPage, head, tabs.el, ...Object.values(panels));
    return page;
  }

  // The owner tapped a tab: give it its own address, so the back button
  // comes back to it. appDepth counts the tabs stepped through since the
  // app's page opened, so "‹ Apps" can step back past them to the list.
  function pickTab(tab) {
    if (tab === appTab) return;
    appTab = tab;
    const at = `#settings/apps/${appOpen}/${tab}`;
    try { history.pushState({ appDepth: (history.state?.appDepth ?? 0) + 1 }, "", at); } catch (_) { location.hash = at; }
    // A tab opens at its top.
    window.scrollTo(0, 0);
  }

  function appHead(a) {
    let back = null;
    // The phone has its own way back above the page.
    if (!phone()) {
      back = el("a", { className: "set-back", href: "#settings/apps", dataset: { key: "app-back" } }, icon("left"), "Apps");
      back.onclick = (e) => { if (fromApps) { e.preventDefault(); backToList(); } };
    }
    const r = rulesOf(a);
    return el("header", { className: "app-head" }, back,
      el("div", { className: "app-title" }, el("h2", { id: "h-app-page", tabIndex: -1 }, a.name),
        el("span", { className: "rule-chips" }, ruleChip(r), r.patient_data ? dataChip() : null)),
      el("span", { className: "mono app-path" }, a.repo_path));
  }

  // The Max plan, pay-per-use, the app's own cap and going first today.
  function moneyCard(a, sp) {
    const k = `rule-profile-${a.id}`, tag = `profile-${a.id}`;
    const errs = rulesErr[a.id] ?? {};
    const draft = (rulesDraft[a.id] ??= {});
    const busy = saving.has(tag) || D.offline;
    const plan = el("select", { id: k, dataset: { key: k }, "aria-describedby": `${k}-hint ${k}-err` },
      el("option", { value: "A" }, "Max plan A"), el("option", { value: "B" }, "Max plan B"));
    plan.value = draft.profile ?? a.profile;
    const save = el("button", { type: "button", className: "btn", dataset: { key: `${k}-save` }, disabled: busy || plan.value === a.profile },
      saving.has(tag) ? "Saving…" : "Save plan");
    plan.onchange = () => { draft.profile = plan.value; save.disabled = D.offline || saving.has(tag) || plan.value === a.profile; };
    save.onclick = () => { if (plan.value !== a.profile) saveField(a, "profile", { profile: plan.value }); };

    const ppu = el("button", { type: "button", className: "btn", dataset: { key: `ppu-${a.id}` } },
      icon(a.pay_per_use ? "ai" : "money"), a.pay_per_use ? `Switch to Max plan ${a.profile}` : "Switch to pay-per-use");
    ppu.disabled = D.offline;
    ppu.onclick = async () => {
      ppu.disabled = true;
      try {
        await D.api(`/apps/${a.id}/pay-per-use`, { method: "POST", body: JSON.stringify({ on: !a.pay_per_use }) });
        D.toast(a.pay_per_use ? `${a.name} now uses Max plan ${a.profile}.` : `${a.name} now uses pay-per-use.`);
        await opts.refresh();
      } catch (e) { if (e.signedOut) { signedOut(); return; } D.toast(`Couldn't switch ${a.name}. ${e.message}`, "alert"); ppu.disabled = false; }
    };
    const spent = sp && (sp.spent_usd > 0 || sp.cap_usd != null)
      ? `Pay-per-use this month: ${D.money(sp.spent_usd)}${sp.cap_usd != null ? ` of its own ${D.money(sp.cap_usd)} cap` : ""}.` : null;
    return tabCard(a.pay_per_use
      ? `Uses Claude pay-per-use now, under its caps. An app can have its own monthly cap on pay-per-use money, across Claude, ChatGPT and Grok.`
      : `Uses Max plan ${a.profile} now. An app can have its own monthly cap on pay-per-use money, across Claude, ChatGPT and Grok.`,
      el("div", { className: "plan-grid" },
        el("div", { className: "field" }, el("label", { htmlFor: k }, "Claude Max plan"),
          el("div", { className: "cap-row" }, plan, save),
          el("span", { className: "hint", id: `${k}-hint` }, a.pay_per_use
            ? "This app uses pay-per-use now. This is the plan it goes back to when you switch it."
            : "New work uses this plan. Work already running keeps its plan."),
          el("span", { className: "field-error", id: `${k}-err`, role: "alert" }, errs.profile ?? "")),
        appCap(a)),
      spent ? el("p", { className: "hint" }, spent) : null,
      el("div", { className: "app-acts" }, ppu, starButton(a)));
  }

  function websitesCard(a) {
    parts.sitesNote.hidden = false;
    return tabCard("Agents on this app reach only the built-in websites and the ones you add here.",
      sitesBlock(a), parts.sitesNote);
  }

  // Files for agents, drawn by /files.js when the page loads it. Without it
  // the card is left out. The panel is the same element on every redraw.
  function filesCard(a) {
    if (!window.DFiles?.panel) return tabCard("Files can't be added from this screen. Open Settings on your PC.");
    return tabCard("Briefs, specs and pictures agents read while they work on this app.",
      window.DFiles.panel({ appId: a.id }));
  }

  // Walls that hold for every app, whatever its rules. Read only.
  const ALWAYS = [
    "Each piece of work runs in its own sealed box.",
    "Agents never hold your GitHub, AWS or deploy sign-ins.",
    "Agents reach only Claude, the package stores and this app's websites.",
    "Nothing merges or deploys without your tap, and agents can never do either.",
    "Nothing is ever pushed straight to your main branch.",
    "Pay-per-use spending stops at your caps.",
  ];
  function alwaysOn() {
    return el("div", { className: "always-on", role: "group", "aria-labelledby": "h-always-on" },
      el("h3", { id: "h-always-on" }, "Always on, for every app"),
      el("p", { className: "hint" }, "These can't be switched off."),
      el("ul", { className: "locks" }, ...ALWAYS.map((t) => el("li", {}, icon("lock"), el("span", {}, t)))));
  }

  // The list of apps: one row each, a link to its page.
  function appRow(a) {
    const r = rulesOf(a);
    return el("li", { className: "go-row" },
      el("a", { href: `#settings/apps/${a.id}`, dataset: { key: `app-go-${a.id}` } },
        el("span", { className: "ic" }, icon("app")),
        el("span", { className: "who" }, el("b", {}, a.name),
          el("span", { className: "rule-chips" },
            el("span", { className: "pill queued" }, icon(a.pay_per_use ? "money" : "ai"), a.pay_per_use ? "Pay-per-use" : `Max plan ${a.profile}`),
            ruleChip(r), r.patient_data ? dataChip() : null,
            a.reviewer ? el("span", { className: "pill queued" }, `${a.reviewer} reviews`) : null)),
        icon("right")));
  }

  function drawAppPage() {
    const box = parts.appPage;
    const a = apps.find((x) => String(x.id) === appOpen) ?? null;
    if (!a) {
      page = null;
      if (changed("app-gone", [appOpen])) {
        fill(box, el("section", { className: "scard wide app-gone" },
          el("h2", { tabIndex: -1 }, "This app isn't here any more."),
          el("p", {}, el("a", { className: "btn", href: "#settings/apps" }, icon("left"), "Back to Apps"))));
      }
      return;
    }
    delete sigs["app-gone"];
    const pg = pageFor(a.id);
    const id = a.id;
    const sp = (spend?.apps ?? []).find((x) => x.app_id === id) ?? null;
    // Each part redraws only when its own data changed. A cap being typed
    // would be lost on redraw, so its tab waits; anything else being typed
    // is kept in a draft, and its field gets focus back.
    const draw = (name, sig, build) => {
      const box = name === "head" ? pg.head : pg.panels[name];
      const key = document.activeElement?.dataset?.key ?? "";
      if (typingIn(box) && !/^(site-|edit-|rule-)/.test(key)) return;
      if (!changed(`app-${name}`, [id, D.offline, ...sig])) return;
      keepFocus(() => fill(box, build()));
    };
    draw("head", [a.name, a.rules, a.repo_path], () => appHead(a));
    draw("rules", [a.name, a.rules, a.reviewer, rulesErr[id], [...saving], deployOpen[id]], () => rulesCard(a));
    draw("money", [a.name, a.profile, a.pay_per_use, a.monthly_cap_usd, a.first_today, sp, starBusy[id], rulesErr[id]?.profile,
      saving.has(`profile-${id}`)], () => moneyCard(a, sp));
    draw("websites", [a.name, a.sites, siteErr[id], siteBusy[id]], () => websitesCard(a));
    draw("files", [!!window.DFiles], () => filesCard(a));
    draw("name", [a.name, a.repo_path, !!editing[id], editErr[id], editBusy[id]], () => tabCard(null, editForm(a)));
  }

  // ---------- Drawing from the page's data ----------
  function render() {
    if (!root || !shown || !loaded) return;
    if (!builtIn) loadBuiltIn();
    parts.addApp.querySelector("#app-path-pick").hidden = typeof D.pickFolder !== "function";
    if (changed("set-apps", [apps, D.offline])) {
      keepFocus(() => parts.appRows.replaceChildren(...(apps.length ? apps.map(appRow)
        : [el("li", { className: "empty" }, "No apps yet. Add one below.")])));
    }
    parts.appList.hidden = appOpen != null;
    parts.appPage.hidden = appOpen == null;
    if (appOpen != null) drawAppPage();
    if (changed("set-computers", [workers, D.offline])) {
      parts.addCloud.replaceChildren(icon("cloud"), hasCloud ? "Add another cloud worker" : "Add cloud worker");
      keepFocus(() => {
        parts.workerRows.replaceChildren(...(workers.length ? workers.map((w) => {
          const cloudOne = w.place === "cloud";
          let b = null;
          if (cloudOne && w.id != null) {
            b = el("button", { type: "button", className: "btn quiet danger-text sm", dataset: { key: `worker-${w.id}` } }, icon("trash"), "Remove");
            b.disabled = D.offline;
            b.onclick = async () => {
              const ok = await D.confirm([`Remove the cloud worker "${w.name}"?`,
                "Its work comes back to your PC and carries on there. To use the cloud again, add a new cloud worker.", "Remove worker", "Keep it"]);
              if (!ok) return;
              try { await D.api(`/workers/${w.id}/remove`, { method: "POST" }); D.toast(`Removed ${w.name}. Its work is back on your PC.`); await opts.refresh(); }
              catch (x) { if (x.signedOut) { signedOut(); return; } D.toast(`Couldn't remove ${w.name}. ${x.message}`, "alert"); }
            };
          }
          return el("li", {},
            el("span", { className: "ic" }, icon(cloudOne ? "cloud" : "pc")),
            el("div", { className: "who" }, el("b", {}, cloudOne ? `Cloud worker ${w.name}` : "Your PC"),
              el("span", { className: "hint" }, w.online ? (cloudOne ? "Runs work set to the cloud" : "Runs work set to your PC") : w.seconds_ago != null ? `Offline, last heard from ${D.ago(new Date(Date.now() - w.seconds_ago * 1000).toISOString())}` : "Offline, not heard from yet")),
            el("span", { className: `acct${w.online ? " on" : ""}` }, el("span", { className: "dot" }), w.online ? "Online" : "Offline"),
            b);
        }) : [el("li", { className: "empty" }, "No computers heard from yet.")]));
      });
    }
    if (spend && !typingIn(parts.accountRows) && changed("set-accounts", [spend, accounts, apps.map((a) => [a.name, a.profile, a.pay_per_use]), D.offline, Math.floor(Date.now() / 60000)])) {
      const { spent, cap, near } = paidTotals();
      const max = spend.max_equivalent_usd;
      const box = parts.spendBig;
      box.classList.toggle("near", near.length > 0);
      box.replaceChildren(
        el("span", { className: "hint" }, "Pay-per-use this month, all accounts"),
        el("div", { className: "num" }, D.money(spent), el("span", {}, `of ${D.money(cap)} in caps`)),
        meterEl(cap > 0 ? spent / cap : (spent > 0 ? 1 : 0), near.length > 0, "Pay-per-use spent this month", spent, cap),
        el("span", { className: "hint" }, near.length
          ? `${near.map((a) => a.label).join(" and ")} ${near.length === 1 ? "is" : "are"} near the cap. Raise it below if you need more.`
          : `This month's work on the Max plans would have cost ${D.money(max)} on pay-per-use. You aren't billed for it.`));
      keepFocus(() => parts.accountRows.replaceChildren(...(accounts
        ? accounts.map((a) => (isMax(a) ? maxRow(a) : paidRow(a)))
        : [el("li", { className: "empty" }, "Couldn't load the accounts. They show up again on the next refresh.")])));
    }
    parts.pair.disabled = D.offline;
  }

  // ---------- Websites an app's agents may use ----------
  // Every name goes in as text, never as HTML.
  async function loadBuiltIn() {
    builtIn = [];
    let list;
    try { list = await D.api("/sites/built-in"); } catch (_) { builtIn = null; return; }
    if (!Array.isArray(list) || !list.length) return;
    builtIn = list;
    const det = parts.builtIn;
    det.querySelector("summary").replaceChildren(icon("right"), `See the ${D.plural(list.length, "website")} always allowed`);
    det.querySelector("ul").replaceChildren(...list.map((h) => el("li", {}, String(h))));
    det.hidden = false;
  }

  // Saves the whole list and shows what the hub stored, which is cleaned up:
  // "https://Stripe.com/docs" comes back as stripe.com, built-in sites drop out.
  async function saveSites(a, next, { added, removed }) {
    siteBusy[a.id] = true;
    delete siteErr[a.id];
    render();
    const before = a.sites ?? [];
    try {
      const stored = await D.api(`/apps/${a.id}/sites`, { method: "POST", body: JSON.stringify({ sites: next }) });
      const list = Array.isArray(stored) ? stored : next;
      sitesSeq++;
      sitesSaved.set(a.id, list);
      for (const x of apps) if (x.id === a.id) x.sites = list;
      if (added) {
        siteDraft[a.id] = "";
        const fresh = list.filter((h) => !before.includes(h));
        if (fresh.length) D.toast(`${a.name} can now use ${fresh.join(", ")}. This applies to its next piece of work.`);
        else D.toast(`${added.join(", ")} ${added.length === 1 ? "is" : "are"} already allowed, so nothing changed.`);
      } else {
        D.toast(`${a.name} can no longer use ${removed}. This applies to its next piece of work.`);
      }
    } catch (e) {
      if (e.signedOut) { signedOut(); return; }
      if (added) siteErr[a.id] = e.message;
      else D.toast(`Couldn't remove ${removed}. ${e.message}`, "alert");
    } finally {
      siteBusy[a.id] = false;
      render();
    }
  }

  function sitesBlock(a) {
    const sites = a.sites ?? [];
    const busy = !!siteBusy[a.id] || D.offline;
    const inKey = `site-in-${a.id}`, errId = `site-err-${a.id}`;
    const err = el("span", { className: "field-error", id: errId, role: "alert" }, siteErr[a.id] ?? "");
    const input = el("input", { type: "text", id: inKey, autocomplete: "off", autocapitalize: "off", inputMode: "url", maxLength: 300,
      placeholder: "Add a website, like stripe.com", value: siteDraft[a.id] ?? "", dataset: { key: inKey },
      "aria-describedby": `sites-hint ${errId}`, "aria-invalid": siteErr[a.id] ? "true" : null });
    input.spellcheck = false;
    input.oninput = () => {
      siteDraft[a.id] = input.value;
      if (siteErr[a.id]) { delete siteErr[a.id]; err.textContent = ""; input.removeAttribute("aria-invalid"); }
    };
    const add = el("button", { type: "submit", className: "btn", dataset: { key: `site-add-${a.id}` }, disabled: busy }, icon("plus"), "Add");
    const form = el("form", { className: "site-add", noValidate: true },
      el("label", { className: "sr-only", for: inKey }, `Add a website for ${a.name}`), input, add);
    const fail = (msg) => { siteErr[a.id] = msg; render(); focusKey(inKey); };
    form.onsubmit = async (ev) => {
      ev.preventDefault();
      if (siteBusy[a.id]) return;
      if (D.offline) return fail("Your PC isn't answering. Try again when it's back.");
      // Several can go in at once, split by spaces or commas.
      const entries = input.value.split(/[\s,]+/).filter(Boolean);
      if (!entries.length) return fail("Type a website name, like stripe.com.");
      const fresh = entries.filter((h) => !sites.includes(h.toLowerCase()));
      if (!fresh.length) return fail(`${entries.join(", ")} ${entries.length === 1 ? "is" : "are"} already on the list.`);
      siteDraft[a.id] = input.value;
      await saveSites(a, [...sites, ...fresh], { added: fresh });
      focusKey(inKey);
    };
    const list = sites.length ? el("ul", { className: "site-chips", "aria-label": `Websites ${a.name} can use` }, ...sites.map((h) => {
      const x = el("button", { type: "button", className: "btn icon sm quiet", "aria-label": `Remove ${h} from ${a.name}`, title: "Remove",
        dataset: { key: `site-rm-${a.id}-${h}` }, disabled: busy }, icon("close"));
      x.onclick = async () => {
        if (siteBusy[a.id] || D.offline) return;
        await saveSites(a, sites.filter((s) => s !== h), { removed: h });
        focusKey(inKey);
      };
      return el("li", {}, el("span", {}, h), x);
    })) : null;
    return el("div", { className: "sites", role: "group", "aria-label": `Websites for ${a.name}` },
      el("div", { className: "sites-h" },
        el("span", { className: "label" }, sites.length ? "Added for this app:" : "None added yet.")),
      list, form, err);
  }

  // ---------- Accounts ----------
  // A Claude Max plan: this week's and the current five hours' allowance.
  function maxRow(a) {
    const users = appsOn(a);
    const about = [`Max plan ${a.account}, flat rate`, users.length ? `used by ${users.join(", ")}` : "no app uses it now"];
    if (a.seen_at) about.push(`last reported ${D.ago(a.seen_at)}`);
    const meters = el("div", { className: "acct-meters" });
    if (!a.seen_at) meters.append(el("span", { className: "hint" }, "No reading yet. It shows up after work runs on this plan."));
    for (const [name, used, renews] of [["This week", a.week_pct, a.week_resets_at], ["Current five hours", a.five_hour_pct, a.five_hour_resets_at]]) {
      if (used == null) continue;
      meters.append(el("div", { className: "acct-meter" },
        el("div", { className: "line" }, el("span", {}, name),
          el("span", {}, el("b", {}, `${pct(used)} used`), renews ? ` · renews ${untilText(renews)}` : "")),
        meterEl(used / 100, used >= 80 || (a.limited && used >= 100), `${a.label}, ${name.toLowerCase()}, used`, Math.round(used), 100)));
    }
    if (a.limited) meters.append(el("p", { className: "acct-out" }, icon("alert"), `Used up for now${a.resets_at ? `, back ${untilText(a.resets_at)}` : ""}.`));
    return el("li", {},
      el("span", { className: "ic" }, icon("ai")),
      el("div", { className: "who" }, el("b", {}, a.label), el("span", { className: "hint" }, about.join(" · "))),
      meters);
  }

  // A pay-per-use account: this month's money against its cap, which the owner can change.
  function paidRow(a) {
    const used = a.spent_usd ?? 0, cap = a.cap_usd ?? 0;
    const near = cap > 0 ? used >= 0.8 * cap : used > 0;
    const id = `acct-cap-${a.account}`;
    const input = el("input", { id, type: "number", min: "0", step: "1", inputMode: "decimal", value: String(cap), dataset: { key: id }, "aria-describedby": `${id}-err` });
    const err = el("span", { className: "field-error", id: `${id}-err`, role: "alert" });
    const save = el("button", { type: "button", className: "btn sm", dataset: { key: `${id}-save` } }, "Save cap");
    save.disabled = D.offline;
    save.onclick = async () => {
      const v = input.value.trim();
      if (v === "" || !(+v >= 0)) { err.textContent = `Enter a monthly cap in dollars for ${a.label}, 0 or more.`; input.setAttribute("aria-invalid", "true"); input.focus(); return; }
      save.disabled = true;
      try {
        await D.api("/spend/cap", { method: "POST", body: JSON.stringify({ account: a.account, cap_usd: +v }) });
        input.blur();
        D.toast(`${a.label} cap set to ${D.money(+v)} a month.`);
        await opts.refresh();
      } catch (x) { if (x.signedOut) { signedOut(); return; } err.textContent = `Couldn't save the cap. ${x.message}`; save.disabled = false; }
    };
    input.addEventListener("input", () => { err.textContent = ""; input.removeAttribute("aria-invalid"); });
    input.addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); save.click(); } });
    const about = cap > 0 ? "Billed by use" : "Billed by use · the cap is $0, so set one to use it";
    return el("li", {},
      el("span", { className: "ic" }, icon("money")),
      el("div", { className: "who" }, el("b", {}, a.label), el("span", { className: "hint" }, about)),
      el("div", { className: "acct-meters" },
        el("div", { className: "acct-meter" },
          el("div", { className: "line" }, el("span", {}, "This month"), el("span", {}, el("b", {}, D.money(used)), ` of ${D.money(cap)}`)),
          meterEl(cap > 0 ? used / cap : (used > 0 ? 1 : 0), near, `${a.label} spent this month`, used, cap))),
      el("div", { className: "row-cap" },
        el("label", { className: "label", htmlFor: id }, "Monthly cap"),
        el("div", { className: "cap-row" }, el("div", { className: "money-in" }, el("span", {}, "$"), input), save),
        err));
  }

  // An app's own monthly cap on pay-per-use money, across every AI. Empty means none.
  function appCap(a) {
    const id = `app-cap-${a.id}`;
    const input = el("input", { id, type: "number", min: "0", step: "1", inputMode: "decimal", placeholder: "No cap",
      value: a.monthly_cap_usd == null ? "" : String(a.monthly_cap_usd), dataset: { key: id }, "aria-describedby": `${id}-err` });
    const err = el("span", { className: "field-error", id: `${id}-err`, role: "alert" });
    const setCap = async (cap_usd, btn) => {
      btn.disabled = true;
      err.textContent = "";
      try {
        await D.api(`/apps/${a.id}/cap`, { method: "POST", body: JSON.stringify({ cap_usd }) });
        input.blur();
        D.toast(cap_usd == null ? `${a.name} has no cap of its own now.` : `${a.name} can spend up to ${D.money(cap_usd)} a month.`);
        await opts.refresh();
      } catch (x) { if (x.signedOut) { signedOut(); return; } err.textContent = `Couldn't save the cap. ${x.message}`; btn.disabled = false; }
    };
    const save = el("button", { type: "button", className: "btn sm", dataset: { key: `${id}-save` } }, "Save cap");
    save.disabled = D.offline;
    save.onclick = () => {
      const v = input.value.trim();
      if (v === "") return setCap(null, save);
      if (!(+v >= 0)) { err.textContent = "Enter dollars, 0 or more, or leave it empty for no cap."; input.setAttribute("aria-invalid", "true"); input.focus(); return; }
      setCap(+v, save);
    };
    input.addEventListener("input", () => { err.textContent = ""; input.removeAttribute("aria-invalid"); });
    input.addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); save.click(); } });
    let clear = null;
    if (a.monthly_cap_usd != null) {
      clear = el("button", { type: "button", className: "btn quiet sm", dataset: { key: `${id}-clear` } }, "Remove cap");
      clear.disabled = D.offline;
      clear.onclick = () => setCap(null, clear);
    }
    return el("div", { className: "row-cap" },
      el("label", { className: "label", htmlFor: id }, "Own monthly cap"),
      el("div", { className: "cap-row" }, el("div", { className: "money-in" }, el("span", {}, "$"), input), save, clear),
      err);
  }

  // ---------- Paired phones and browsers ----------
  async function loadDevices() {
    try { devices = await D.api("/devices"); } catch (e) {
      if (e.signedOut) { signedOut(); return; }
      if (!devices) parts.devices.replaceChildren(el("li", { className: "empty" }, `Couldn't load phones. ${e.message}`));
      return;
    }
    if (!changed("devices", [devices, D.offline])) return;
    const when = (iso) => new Date(iso).toLocaleString([], { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });
    keepFocus(() => {
      parts.devices.replaceChildren(...(devices.length ? devices.map((d) => {
        const b = el("button", { type: "button", className: "btn quiet danger-text sm", dataset: { key: `dev-${d.id}` } }, icon("trash"), "Remove");
        b.disabled = D.offline;
        b.onclick = async () => {
          const ok = await D.confirm(d.this_device
            ? [`Remove "${d.name}"?`, `This is the ${phone() ? "phone" : "browser"} you are using. It is signed out right away and will need a new pairing code to get back in.`, "Remove and sign out", "Keep it"]
            : [`Remove "${d.name}"?`, "It is signed out and will need a new pairing code to get back in.", "Remove phone", "Keep it"]);
          if (!ok) return;
          try { await D.api(`/devices/${d.id}/remove`, { method: "POST" }); D.toast(`Removed ${d.name}.`); loadDevices(); }
          catch (x) { if (x.signedOut) { signedOut(); return; } D.toast(`Couldn't remove ${d.name}. ${x.message}`, "alert"); }
        };
        return el("li", {},
          el("span", { className: "ic" }, icon(/phone|iPad/.test(d.name) ? "phone" : "pc")),
          el("div", { className: "who" },
            el("b", {}, d.name, d.this_device ? el("span", { className: "this-tag" }, phone() ? "This phone" : "This browser") : null),
            el("span", { className: "hint" }, [d.has_push ? "Notifications on" : "Notifications off", d.last_seen ? `Last used ${when(d.last_seen)}` : "Not used yet"].join(" · "))),
          b);
      }) : [el("li", { className: "empty" }, "No phones paired yet.")]));
    });
  }

  // Before the hub has an https address, the button sets Tailscale up first,
  // so it says so.
  function pairLabel() {
    const text = conn && !conn.https ? "Connect your phone" : phone() ? "Pair another phone" : "Pair a phone";
    if (!parts.pair || parts.pair.dataset.label === text) return;
    parts.pair.dataset.label = text;
    parts.pair.replaceChildren(icon("phone"), text);
  }

  async function loadConnection() {
    try { conn = await D.api("/phone/connection"); } catch (e) {
      if (e.signedOut) { signedOut(); return; }
      conn = null;
    }
    pairLabel();
  }

  // "Connect your phone" fails with {error, link}; anything else is plain text.
  function connectFailure(e) {
    try {
      const v = JSON.parse(e.message);
      if (v && typeof v.error === "string") return { error: v.error, link: typeof v.link === "string" ? v.link : null };
    } catch (_) { /* plain text */ }
    return { error: e.message, link: null };
  }

  function showConnectFailure(f) {
    const link = f.link && f.link.startsWith("https://")
      ? el("a", { href: f.link, target: "_blank", rel: "noopener noreferrer" },
        f.link.startsWith("https://tailscale.com/download") ? "Get Tailscale" : "Open the Tailscale page")
      : null;
    parts.qr.hidden = false;
    fill(parts.qr, el("div", { className: "notice warn", role: "alert" }, icon("alert"),
      el("div", { className: "grow" }, el("p", {}, f.error), link ? el("p", {}, link) : null)));
  }

  // Asks the hub to set up Tailscale's https address when it has none yet.
  // True when the hub has one now.
  async function connect() {
    if (conn && conn.https) return true;
    parts.pair.dataset.label = "";
    parts.pair.replaceChildren(icon("phone"), "Connecting…");
    try {
      const c = await D.api("/phone/connect", { method: "POST" });
      conn = { public_url: c.url, https: true };
      return true;
    } catch (e) {
      if (e.signedOut) { signedOut(); return false; }
      if (D.offline) { D.toast(`Couldn't connect your phone. ${e.message}`, "alert"); return false; }
      showConnectFailure(connectFailure(e));
      return false;
    } finally { pairLabel(); }
  }

  async function pairPhone() {
    const b = parts.pair;
    b.disabled = true;
    try {
      if (!(await connect())) return;
      const p = await D.api("/pairing", { method: "POST" });
      const box = el("div", { className: "qr-box" });
      box.innerHTML = p.qr_svg; // drawn by the hub itself from the pairing link
      parts.qr.hidden = false;
      fill(parts.qr, box,
        el("p", { className: "hint" }, phone()
          ? `Scan it with the other phone's camera. It works once, for ${D.plural(p.minutes, "minute")}.`
          : `Scan it with the phone's camera. It works once, for ${D.plural(p.minutes, "minute")}.`),
        el("a", { href: p.url }, phone() ? "Or open this link on the other phone" : "Or open this link on the phone"),
        p.warning ? el("div", { className: "notice warn" }, icon("alert"), el("span", { className: "grow" }, p.warning)) : null);
      setTimeout(loadDevices, 15000);
    } catch (e) { if (e.signedOut) { signedOut(); return; } D.toast(`Couldn't make a pairing code. ${e.message}`, "alert"); }
    finally { b.disabled = D.offline; }
  }

  // ---------- Sign-ins ----------
  // The hub works out each light; this draws them and the buttons that fix them.
  let lights = null, lightsAt = 0, claudePoll = null;
  const longDay = (iso) => new Date(iso).toLocaleDateString([], { year: "numeric", month: "short", day: "numeric" });
  const LAMP = { green: "Fine", amber: "Needs you soon", red: "Needs you now", grey: "Not set up" };
  function whenText(i) {
    if (!i.expires_at || i.days_left == null) return null;
    const d = i.days_left;
    if (d < 0) return `Ran out ${longDay(i.expires_at)}`;
    if (d === 0) return "Runs out today";
    if (d === 1) return "Runs out tomorrow";
    if (d <= 30) return `Runs out in ${d} days`;
    return `Good until ${longDay(i.expires_at)}`;
  }
  async function loadLights(force) {
    if (!force && Date.now() - lightsAt < 60000) return;
    lightsAt = Date.now();
    try { lights = await D.api("/sign-in-lights"); } catch (e) {
      if (e.signedOut) { signedOut(); return; }
      if (!lights) parts.lights.replaceChildren(el("li", { className: "empty" }, `Couldn't load sign-ins. ${e.message}`));
      return;
    }
    if (!changed("lights", [lights, D.offline])) return;
    const need = lights.red + lights.amber;
    fill(parts.lightsNote, need
      ? el("div", { className: `notice ${lights.red ? "bad" : "warn"}`, role: "status" }, icon("alert"),
        el("span", { className: "grow" }, need === 1 ? "1 sign-in needs you." : `${need} sign-ins need you.`))
      : el("p", { className: "hint" }, "Everything Decurion needs is signed in."));
    tabNote("sign-ins", need ? `${need} need${need === 1 ? "s" : ""} you` : null, need ? (lights.red ? "bad" : "warn") : null);
    keepFocus(() => parts.lights.replaceChildren(...lights.items.map(lightRow)));
  }
  function lightRow(i) {
    const when = whenText(i);
    return el("li", { className: `light-row ${i.light}` },
      el("span", { className: `lamp ${i.light}`, role: "img", "aria-label": LAMP[i.light] ?? "", title: LAMP[i.light] ?? "" }),
      el("div", { className: "who" }, el("b", {}, i.label),
        el("span", { className: "hint" }, when ? `${i.text} · ${when}` : i.text)),
      lightButton(i), lightSteps(i));
  }
  // What to do on another site (GitHub's token page), step by step, with the
  // one link that starts it. Only GitHub pages are ever linked.
  function lightSteps(i) {
    if (!Array.isArray(i.steps) || !i.steps.length) return null;
    const go = typeof i.link?.url === "string" && i.link.url.startsWith("https://github.com/")
      ? el("a", { className: "btn sm", href: i.link.url, target: "_blank", rel: "noopener noreferrer" }, icon("branch"), i.link.label || "Open GitHub")
      : null;
    return el("div", { className: "light-steps" },
      el("ol", {}, ...i.steps.map((t) => el("li", {}, t))), go);
  }
  function lightButton(i) {
    const btn = (text, onclick, primary) => {
      const b = el("button", { type: "button", className: `btn sm${primary ? " primary" : ""}`, dataset: { key: `light-${i.key}` } }, text);
      b.disabled = D.offline;
      b.onclick = () => onclick(b);
      return b;
    };
    const urgent = i.light === "red" || i.light === "amber";
    switch (i.action) {
      case "claude_sign_in":
        return btn(i.light === "green" ? "Sign in again" : "Sign in on this PC", (b) => startClaude(i.profile, b), urgent || i.light === "grey");
      case "github_token":
        return el("a", { className: `btn sm${urgent ? " primary" : ""}`, href: "#settings/github" }, i.light === "grey" ? "Add a token" : "Change");
      case "connect_phone":
        return btn("Connect your phone", async () => {
          location.hash = "#settings/phones";
          await pairPhone();
          loadLights(true);
        }, true);
      case "site_sign_in":
        return btn(i.light === "grey" ? "Sign in" : "Sign in again", (b) => startSignIn({ id: i.id }, b).then(() => loadLights(true)), urgent);
      case "add_cloud":
        return el("a", { className: "btn sm", href: "/cloud" }, "Add one");
      default:
        return null;
    }
  }

  // "Sign in on this PC": Claude Code opens its sign-in page in the PC's
  // browser and Decurion keeps what it gives back. This panel follows along.
  async function startClaude(profile, b) {
    b.disabled = true;
    try {
      const s = await D.api("/claude/sign-in", { method: "POST", body: JSON.stringify({ profile }) });
      showClaude(s);
    } catch (e) {
      if (e.signedOut) { signedOut(); return; }
      D.toast(`Couldn't start the Claude sign-in. ${e.message}`, "alert");
      // One may already be open (from another tab, or before a reload):
      // show it, with its Cancel button.
      await pollClaude();
    } finally { b.disabled = D.offline; }
    // The panel sits above a long list; bring it to the owner.
    if (!parts.claude.hidden) parts.claude.scrollIntoView({ behavior: "smooth", block: "nearest" });
  }
  async function pollClaude() {
    clearTimeout(claudePoll);
    claudePoll = null;
    let s;
    try { s = await D.api("/claude/sign-in"); } catch (e) {
      if (e.signedOut) { signedOut(); return; }
      claudePoll = setTimeout(pollClaude, 3000);
      return;
    }
    showClaude(s);
  }
  async function cancelClaude() {
    try { showClaude(await D.api("/claude/sign-in/cancel", { method: "POST" })); }
    catch (e) { if (e.signedOut) { signedOut(); return; } D.toast(`Couldn't stop the sign-in. ${e.message}`, "alert"); }
  }
  function showClaude(s) {
    clearTimeout(claudePoll);
    claudePoll = null;
    const box = parts.claude;
    if (!s || s.phase === "idle") { box.hidden = true; box.replaceChildren(); box.dataset.state = ""; return; }
    const plan = `Claude ${s.profile ?? ""}`.trim();
    if (s.phase === "saved" || s.phase === "failed") {
      box.hidden = false;
      box.dataset.state = "";
      const close = el("button", { type: "button", className: "btn quiet sm" }, "OK");
      close.onclick = cancelClaude;
      const retry = s.phase === "failed" && s.profile ? el("button", { type: "button", className: "btn sm" }, "Try again") : null;
      if (retry) retry.onclick = () => startClaude(s.profile, retry);
      fill(box, el("div", { className: `notice ${s.phase === "saved" ? "" : "bad"}`, role: "status" },
        icon(s.phase === "saved" ? "done" : "alert"),
        el("span", { className: "grow" }, s.message ?? (s.phase === "saved" ? `Signed in to ${plan}.` : "The sign-in did not finish.")),
        retry, close));
      if (s.phase === "saved") D.toast(`Signed in to ${plan}.`);
      loadLights(true);
      return;
    }
    // Waiting: keep a half-typed code, only redraw when something changed.
    claudePoll = setTimeout(pollClaude, 1500);
    const key = JSON.stringify([s.profile, s.link, s.wants_code]);
    if (box.dataset.state === key) return;
    box.dataset.state = key;
    box.hidden = false;
    const cancel = el("button", { type: "button", className: "btn quiet sm" }, "Cancel");
    cancel.onclick = cancelClaude;
    const steps = el("div", { className: "grow" },
      el("p", {}, el("b", {}, `Signing in to ${plan}`)),
      el("p", { className: "hint" }, phone()
        ? `Claude's sign-in page opens in the browser on your PC. Sign in there with the account for ${plan}. This updates by itself.`
        : `Claude's sign-in page opens in this PC's browser. Sign in with the account for ${plan}, then come back here. This updates by itself.`),
      s.profile === "B" ? el("p", { className: "hint" }, "If the page signs you in to your other Claude account, sign out of Claude in that browser first.") : null);
    let more = null;
    if (s.link && s.link.startsWith("https://")) {
      const code = el("input", { id: "claude-code", type: "text", autocomplete: "off", spellcheck: false, autocapitalize: "off", placeholder: "Paste the code from Claude's page", dataset: { key: "claude-code" } });
      const err = el("span", { className: "field-error", role: "alert" });
      const form = el("form", { className: "form-grid", noValidate: true, autocomplete: "off" },
        el("div", { className: "field" }, el("label", { htmlFor: "claude-code" }, "Code from Claude's page"), code),
        el("div", { className: "full actions" }, el("button", { className: "btn primary", type: "submit" }, "Send code"), err));
      form.onsubmit = async (e) => {
        e.preventDefault();
        err.textContent = "";
        if (!code.value.trim()) { err.textContent = "Paste the code first."; code.focus(); return; }
        try { showClaude(await D.api("/claude/sign-in/code", { method: "POST", body: JSON.stringify({ code: code.value.trim() }) })); code.value = ""; }
        catch (x) { if (x.signedOut) { signedOut(); return; } err.textContent = x.message; }
      };
      more = el("details", { className: "fold" }, el("summary", {}, "The page didn't open?"),
        el("p", { className: "hint" }, "Open Claude's page yourself, sign in, and copy the code it shows. Then paste it here."),
        el("p", {}, el("a", { href: s.link, target: "_blank", rel: "noopener noreferrer" }, "Open Claude's sign-in page")),
        form);
    }
    keepFocus(() => fill(box, el("div", { className: "notice", role: "status" }, icon("key"), steps, cancel), more));
  }

  // ---------- Test keys ----------
  // Drawn when Settings opens and after each change, not on the timer, so a
  // half-typed key is never wiped. Values go to the hub once and never come back.
  async function loadKeys() {
    let sections;
    try {
      const list = await D.api("/apps");
      sections = await Promise.all(list.map(keySection));
    } catch (e) {
      if (e.signedOut) { signedOut(); return; }
      parts.keys.replaceChildren(el("p", { className: "field-error" }, `Couldn't load test keys. ${e.message}`));
      return;
    }
    parts.keys.replaceChildren(...(sections.length ? sections : [el("p", { className: "hint" }, "Add an app first.")]));
  }
  async function keySection(a) {
    const keys = await D.api(`/apps/${a.id}/test-keys`);
    const id = (s) => `key-${a.id}-${s}`;
    const rows = el("ul", { className: "rows" }, ...(keys.length ? keys.map((k) => {
      const b = el("button", { type: "button", className: "btn quiet danger-text sm" }, icon("trash"), "Remove");
      b.onclick = async () => {
        const ok = await D.confirm([`Remove ${k.name} from ${a.name}?`, "Its agents stop getting it from their next run.", "Remove key", "Keep it"]);
        if (!ok) return;
        try { await D.api(`/apps/${a.id}/test-keys/${encodeURIComponent(k.name)}/remove`, { method: "POST" }); D.toast(`Removed ${k.name}.`); await loadKeys(); }
        catch (x) { if (x.signedOut) { signedOut(); return; } D.toast(`Couldn't remove ${k.name}. ${x.message}`, "alert"); }
      };
      return el("li", {},
        el("span", { className: "ic" }, icon("key")),
        el("div", { className: "who" }, el("b", {}, k.name), el("span", { className: "hint" }, `••••${k.last_four}`)),
        b);
    }) : [el("li", { className: "empty" }, "No test keys yet.")]));
    const name = el("input", { id: id("name"), type: "text", placeholder: "STRIPE_SECRET_KEY", autocomplete: "off", autocapitalize: "characters" });
    name.spellcheck = false;
    const value = secretInput({ id: id("value"), placeholder: "sk_test_…", "aria-describedby": id("err") });
    const err = el("span", { className: "field-error", id: id("err"), role: "alert" });
    const form = el("form", { className: "form-grid", noValidate: true, autocomplete: "off" },
      el("div", { className: "field" }, el("label", { htmlFor: id("name") }, "Name"), name,
        el("span", { className: "hint" }, "The setting your app reads it from.")),
      el("div", { className: "field" }, el("label", { htmlFor: id("value") }, "Key"),
        el("div", { className: "secret-row" }, value, showButton(value))),
      el("div", { className: "full actions" }, el("button", { className: "btn primary", type: "submit" }, "Save key"), err));
    form.onsubmit = async (e) => {
      e.preventDefault();
      err.textContent = "";
      const keyName = name.value.trim();
      try {
        await D.api(`/apps/${a.id}/test-keys`, { method: "POST", body: JSON.stringify({ name: keyName, value: value.value }) });
        value.value = "";
        value.type = "password";
        D.toast(`Saved ${keyName} for ${a.name}.`);
        await loadKeys();
      } catch (x) { if (x.signedOut) { signedOut(); return; } err.textContent = x.message; }
    };
    return el("div", { className: "per-app" }, el("h3", { className: "sub-h" }, a.name), rows, form);
  }

  // ---------- GitHub ----------
  // The owner's GitHub token, for opening pull requests. Like test keys, it
  // goes to the hub once and only its last four characters come back.
  async function loadGitHub() {
    let s;
    try { s = await D.api("/github"); } catch (e) {
      if (e.signedOut) { signedOut(); return; }
      parts.github.replaceChildren(el("p", { className: "field-error" }, `Couldn't load the GitHub settings. ${e.message}`));
      return;
    }
    const rows = el("ul", { className: "rows" });
    if (s.token) {
      const b = el("button", { type: "button", className: "btn quiet danger-text sm" }, icon("trash"), "Remove");
      b.onclick = async () => {
        const ok = await D.confirm(["Remove the GitHub token?", "Accepted work still goes to GitHub, and you open each pull request with one tap.", "Remove token", "Keep it"]);
        if (!ok) return;
        try { await D.api("/github/token/remove", { method: "POST" }); D.toast("Removed the GitHub token."); await loadGitHub(); }
        catch (x) { if (x.signedOut) { signedOut(); return; } D.toast(`Couldn't remove the GitHub token. ${x.message}`, "alert"); }
      };
      rows.append(el("li", {}, el("span", { className: "ic" }, icon("key")),
        el("div", { className: "who" }, el("b", {}, "GitHub token"), el("span", { className: "hint" }, `••••${s.token.last_four} · Decurion opens pull requests for you`)), b));
    } else {
      rows.append(el("li", { className: "empty" }, "No token saved. You open each pull request yourself, with one tap."));
    }
    const value = secretInput({ id: "gh-token", placeholder: "github_pat_…", "aria-describedby": "gh-token-hint gh-token-err" });
    const err = el("span", { className: "field-error", id: "gh-token-err", role: "alert" });
    const form = el("form", { className: "form-grid", noValidate: true, autocomplete: "off" },
      el("div", { className: "field" }, el("label", { htmlFor: "gh-token" }, s.token ? "Replace the token" : "GitHub token"),
        el("div", { className: "secret-row" }, value, showButton(value))),
      el("p", { className: "hint", id: "gh-token-hint", style: "align-self:center" }, "Make a fine-grained token on GitHub with Pull requests: Read and write for your apps' repos. To merge from Decurion, it also needs Contents: Read and write. See Help, Review and merge work."),
      el("div", { className: "full actions" }, el("button", { className: "btn primary", type: "submit" }, "Save token"), err));
    form.onsubmit = async (e) => {
      e.preventDefault();
      err.textContent = "";
      if (!value.value.trim()) { err.textContent = "Paste the token first."; value.focus(); return; }
      try {
        await D.api("/github/token", { method: "POST", body: JSON.stringify({ value: value.value.trim() }) });
        value.value = "";
        value.type = "password";
        D.toast("Saved the GitHub token. Decurion opens pull requests for you from now on.");
        await loadGitHub();
      } catch (x) { if (x.signedOut) { signedOut(); return; } err.textContent = x.message.replace(/^a GitHub/, "A GitHub").replace(/^this does/, "This does"); }
    };
    parts.github.replaceChildren(rows, form);
  }

  // ---------- Which Decurion this PC runs ----------
  // GET /api/decurion/version (version.rs): its version, the hub's words on
  // whether it is the newest, the changes waiting and how to update, drawn
  // as given. The hub keeps its answer for a few minutes, so opening this
  // tab never waits on GitHub for long. The tab says "Update waiting" in
  // amber while this PC is behind.
  const VERSION_LOOK = { current: ["done", "done", "Newest"], behind: ["needs_you", "alert", "Update waiting"], unknown: ["queued", "queued", "Not known"] };
  async function loadVersion() {
    let v;
    try { v = await D.api("/decurion/version"); } catch (e) {
      if (e.signedOut) { signedOut(); return; }
      if (changed("set-version", ["err", e.message])) {
        parts.version.replaceChildren(el("p", { className: "field-error" }, `Couldn't check which Decurion runs here. ${e.message}`));
      }
      return;
    }
    if (!changed("set-version", [v, Math.floor(Date.now() / 60000)])) return;
    const behind = v?.state === "behind";
    const [cls, ic, word] = VERSION_LOOK[v?.state] ?? VERSION_LOOK.unknown;
    tabNote("decurion", behind ? "Update waiting" : null, behind ? "warn" : null);
    const how = Array.isArray(v?.how) ? v.how : [];
    fill(parts.version,
      el("ul", { className: "rows" }, el("li", {},
        el("span", { className: "ic" }, icon("pc")),
        el("div", { className: "who" }, el("b", {}, "This PC runs"),
          el("span", { className: "hint" }, v?.short ? `Version ${v.short}` : "A version Decurion can't name",
            v?.checked_at ? ` · checked ${D.ago(v.checked_at)}` : "")),
        el("span", { className: `pill ${cls}` }, icon(ic), word))),
      v?.words ? el("p", { className: "ver-words" }, v.words) : null,
      v?.changes?.length ? el("h3", { className: "sub-h" }, "Waiting changes") : null,
      D.versionChanges(v?.changes),
      how.length ? el("h3", { className: "sub-h" }, "How to update") : null,
      how.length ? el("ul", { className: "ver-how-list" }, ...how.map((l) => el("li", {}, l))) : null,
      how.length || behind ? el("p", { className: "hint" },
        el("a", { href: D.UPDATE_HELP(), target: "_blank", rel: "noopener noreferrer" }, "How to update Decurion")) : null);
  }

  // ---------- Website sign-ins ----------
  // Drawn like test keys: when Settings opens and after each change, so a
  // half-typed address is never wiped.
  const day = (iso) => new Date(iso).toLocaleDateString([], { month: "short", day: "numeric" });
  function signInState(s) {
    if (s.status === "none") return "Not signed in yet";
    if (s.status === "expired") return "Stopped working. Sign in again.";
    if (s.status === "expiring") return s.expires_at ? `Runs out ${day(s.expires_at)}. Sign in again soon.` : "Runs out soon. Sign in again soon.";
    return s.expires_at ? `Signed in, until ${day(s.expires_at)}` : "Signed in";
  }
  async function loadSignIns() {
    let sections;
    try {
      const list = await D.api("/apps");
      sections = await Promise.all(list.map(signInSection));
    } catch (e) {
      if (e.signedOut) { signedOut(); return; }
      parts.signIns.replaceChildren(el("p", { className: "field-error" }, `Couldn't load website sign-ins. ${e.message}`));
      return;
    }
    parts.signIns.replaceChildren(...(sections.length ? sections : [el("p", { className: "hint" }, "Add an app first.")]));
  }
  // Opens the sign-in window: a fresh browser on the PC, streamed here.
  async function startSignIn(s, btn) {
    btn.disabled = true;
    try {
      const { session_id } = await D.api(`/sign-ins/${s.id}/sign-in`, { method: "POST" });
      if (D.openBrowserViewer) {
        const shownNow = D.openBrowserViewer(session_id, { saveButton: true });
        if (shownNow && typeof shownNow.then === "function") shownNow.then(loadSignIns, loadSignIns);
      } else {
        D.toast("The sign-in window is not available on this hub yet.", "alert");
      }
    } catch (x) { if (x.signedOut) { signedOut(); return; } D.toast(`Couldn't open the sign-in window. ${x.message}`, "alert"); }
    finally { btn.disabled = false; }
  }
  async function signInSection(a) {
    const list = await D.api(`/apps/${a.id}/sign-ins`);
    const id = (s) => `signin-${a.id}-${s}`;
    const rows = el("ul", { className: "rows" }, ...(list.length ? list.map((s) => {
      const go = el("button", { type: "button", className: `btn sm${s.status === "ok" ? "" : " primary"}` }, s.status === "none" ? "Sign in" : "Sign in again");
      go.disabled = D.offline;
      go.onclick = () => startSignIn(s, go);
      const rm = el("button", { type: "button", className: "btn quiet danger-text sm" }, icon("trash"), "Remove");
      rm.onclick = async () => {
        const ok = await D.confirm([`Remove the ${s.label} sign-in from ${a.name}?`, "Its agents stop being signed in to that website from their next run, and the saved sign-in is deleted from your PC.", "Remove sign-in", "Keep it"]);
        if (!ok) return;
        try { await D.api(`/sign-ins/${s.id}/remove`, { method: "POST" }); D.toast(`Removed ${s.label}.`); await loadSignIns(); }
        catch (x) { if (x.signedOut) { signedOut(); return; } D.toast(`Couldn't remove ${s.label}. ${x.message}`, "alert"); }
      };
      const covers = s.domains.length ? `Covers ${s.domains.join(", ")}` : s.start_url;
      return el("li", {},
        el("span", { className: "ic" }, icon(s.status === "expired" || s.status === "expiring" ? "alert" : "key")),
        el("div", { className: "who" }, el("b", {}, s.label),
          el("span", { className: "hint" }, signInState(s)),
          el("span", { className: "hint" }, covers)),
        el("div", { className: "cap-row" }, go, rm));
    }) : [el("li", { className: "empty" }, "No website sign-ins yet.")]));
    const label = el("input", { id: id("label"), type: "text", placeholder: "Staging admin", autocomplete: "off", maxLength: 60 });
    const start = el("input", { id: id("start"), type: "url", placeholder: "https://staging.example.com/login", autocomplete: "off", autocapitalize: "off", inputMode: "url" });
    const check = el("input", { id: id("check"), type: "url", placeholder: "https://staging.example.com/account", autocomplete: "off", autocapitalize: "off", inputMode: "url" });
    start.spellcheck = false; check.spellcheck = false;
    const err = el("span", { className: "field-error", id: id("err"), role: "alert" });
    for (const i of [label, start, check]) { i.setAttribute("aria-describedby", id("err")); i.addEventListener("input", () => { err.textContent = ""; }); }
    const form = el("form", { className: "form-grid", noValidate: true },
      el("div", { className: "field" }, el("label", { htmlFor: id("label") }, "Name"), label,
        el("span", { className: "hint" }, "Your own name for it, such as the account it signs in as.")),
      el("div", { className: "field" }, el("label", { htmlFor: id("start") }, "Sign-in page"), start,
        el("span", { className: "hint" }, "Starts with https://.")),
      el("div", { className: "field" }, el("label", { htmlFor: id("check") }, "Page to check (optional)"), check,
        el("span", { className: "hint" }, "A page that only works when signed in. Checked before each run.")),
      el("div", { className: "full actions" }, el("button", { className: "btn primary", type: "submit" }, "Add sign-in"), err));
    form.onsubmit = async (e) => {
      e.preventDefault();
      err.textContent = "";
      const name = label.value.trim(), addr = start.value.trim();
      if (!name) { err.textContent = "Give the sign-in a name."; label.focus(); return; }
      if (!/^https:\/\//i.test(addr)) { err.textContent = "Enter the sign-in page's address, starting with https://."; start.focus(); return; }
      try {
        await D.api(`/apps/${a.id}/sign-ins`, { method: "POST", body: JSON.stringify({ label: name, start_url: addr, check_url: check.value.trim() || null }) });
        D.toast(`Added ${name} for ${a.name}. Tap Sign in next to it to sign in.`);
        await loadSignIns();
      } catch (x) { if (x.signedOut) { signedOut(); return; } err.textContent = x.message; }
    };
    return el("div", { className: "per-app" }, el("h3", { className: "sub-h" }, a.name), rows, form);
  }

  // ---------- The module's face ----------
  D.settings = {
    // Builds every section into box. place is "pc" for the board, "phone" for the inbox.
    mount(box, o = {}) {
      opts = { ...opts, ...o };
      root = box;
      for (const k of Object.keys(sigs)) delete sigs[k];
      box.replaceChildren(build());
      render();
    },
    // Taken before loading, and passed back to update, so a website list or
    // rules saved while the load was on its way are not overwritten by older data.
    seq: () => sitesSeq,
    // The page's latest data. Any field left out keeps its last value.
    update(data = {}) {
      if (Array.isArray(data.apps)) {
        const a = data.apps;
        const newer = data.seq == null ? sitesSaved.size + rulesSaved.size > 0 : data.seq !== sitesSeq;
        if (newer) {
          for (const x of a) {
            if (sitesSaved.has(x.id)) x.sites = sitesSaved.get(x.id);
            if (rulesSaved.has(x.id)) x.rules = rulesSaved.get(x.id);
          }
        } else { sitesSaved.clear(); rulesSaved.clear(); }
        apps = a;
        loaded = true;
      }
      if ("spend" in data) spend = data.spend ?? null;
      if ("accounts" in data) accounts = Array.isArray(data.accounts) ? data.accounts : null;
      if ("workers" in data) {
        workers = Array.isArray(data.workers) ? data.workers : [];
        hasCloud = workers.some((w) => w.place === "cloud");
      }
      render();
    },
    // The settings view opened, or its address changed: open the tab asked
    // for ("accounts", "apps", "apps/3" for one app's page, ...), and load
    // what only settings needs.
    // Gives back the open tab's id, or null for the phone's list.
    show(part) {
      const was = shown;
      shown = true;
      const id = select(part, was);
      if (was) { if (id === "decurion") loadVersion(); return id; }
      render();
      loadDevices();
      loadConnection();
      loadLights(true);
      pollClaude();
      loadKeys();
      loadGitHub();
      loadSignIns();
      loadVersion();
      window.DAddons?.load();
      return id;
    },
    hide() { shown = false; },
    get shown() { return shown; },
    // On the page's timer: phones paired from another screen show up.
    // The open Decurion tab asks again too: the hub answers from its cache.
    tick() { if (shown) { loadDevices(); loadLights(); if (current === "decurion") loadVersion(); } },
    render,
    usage,
    setFirst,
  };
  D.onOffline(() => { render(); if (shown && devices) { sigs.devices = ""; loadDevices(); } });
})();
