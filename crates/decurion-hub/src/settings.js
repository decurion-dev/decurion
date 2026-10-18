// Decurion's settings, shared by the board and the phone: apps and the
// websites their agents may use, computers, accounts and caps, test keys,
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
//
// Use: D.settings.mount(box, { refresh, onSignedOut, place }) once, then
// D.settings.update({ apps, spend, accounts, workers }) after each load,
// D.settings.show(part) when the settings view opens, D.settings.hide()
// when it closes, and D.settings.tick() on the page's timer.
// D.settings.usage(el) draws the pay-per-use and Max plan summary into el.
"use strict";
(() => {
  const { el, icon } = D;

  // ---------- State ----------
  let root = null, opts = { refresh: async () => {}, onSignedOut: () => {}, place: "pc" };
  let shown = false, loaded = false;
  let apps = [], spend = null, accounts = null, workers = [], hasCloud = false;
  const siteDraft = {}, siteErr = {}, siteBusy = {}; // app id -> typed text, error, saving
  const sitesSaved = new Map();  // app id -> list the hub just stored
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
  const typingIn = (box) => { const a = document.activeElement; return a?.tagName === "INPUT" && box.contains(a); };
  // Re-renders without losing keyboard focus: focus is restored by data-key.
  function keepFocus(fn) {
    const was = document.activeElement;
    const key = was?.dataset?.key;
    let caret = null;
    try { if (was?.tagName === "INPUT" && was.selectionStart != null) caret = [was.selectionStart, was.selectionEnd]; } catch (_) { /* no caret */ }
    fn();
    if (key && document.activeElement?.dataset?.key !== key) {
      const now = document.querySelector(`[data-key="${CSS.escape(key)}"]`);
      now?.focus({ preventScroll: true });
      if (caret && now?.tagName === "INPUT") try { now.setSelectionRange(...caret); } catch (_) { /* no caret */ }
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
    // Apps
    parts.appRows = el("ul", { className: "rows", id: "app-rows" }, loading());
    parts.builtIn = el("details", { className: "fold", id: "sites-builtin", hidden: true },
      el("summary"), el("ul", { className: "builtin", "aria-label": "Always allowed websites" }));
    parts.sitesNote = el("div", { className: "sites-note", id: "sites-note", hidden: true },
      el("p", { className: "hint", id: "sites-hint" }, "Claude and the package stores (npm, PyPI, crates.io) are always allowed. An entry like stripe.com also covers its subdomains. Changes apply to the next piece of work."),
      parts.builtIn);
    parts.addApp = addAppForm();
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
    // Phones
    parts.pair = el("button", { type: "button", className: "btn", id: "pair" });
    parts.pair.onclick = pairPhone;
    pairLabel();
    parts.qr = el("div", { className: "qr", id: "qr", hidden: true });
    parts.devices = el("ul", { className: "rows", id: "devices" }, loading());

    const phones = el("section", { className: `scard live${phone() ? " wide" : ""}`, id: "set-phones", "aria-labelledby": "h-phones" },
      el("header", {}, el("span", { className: "ic" }, icon("phone", "l")), el("div", {}, el("h2", { id: "h-phones" }, "Phones"),
        el("p", {}, phone()
          ? "Pair another phone to get the inbox there too. The code works once, for a few minutes."
          : "Pair your phone to get the inbox there. The code works once, for a few minutes."))),
      el("div", {}, parts.pair),
      parts.qr,
      el("div", {}, el("h3", { className: "sub-h" }, "Paired phones and browsers"), parts.devices));

    return el("div", { className: `settings${phone() ? " on-phone" : ""}` },
      card("apps", "app", "Apps",
        `Each app is a git checkout on ${here()}. Agents work on their own copy and never touch yours. An app can also have its own monthly cap on pay-per-use money, across Claude, ChatGPT and Grok.`,
        parts.appRows, parts.sitesNote, parts.addApp),
      card("computers", "cloud", "Computers",
        "Where agents work. Each piece of work runs on your PC or on a cloud worker, under the same rules, and can move between them at any time.",
        parts.workerRows, el("div", {}, parts.addCloud)),
      card("accounts", "money", "Accounts",
        "Claude A and Claude B are flat-rate Max plans. Their bars show how much of this week's allowance and of the current five-hour allowance is used, as Claude last said while working. The other accounts are billed by use: work on them stops and asks you when the month's total reaches that account's cap. Each run can spend at most $10 of what is left.",
        parts.spendBig, parts.accountRows),
      card("keys", "key", "Test keys",
        "Test keys reach this app's agents while they work. They are kept in Windows' secure store and shown again only as their last four characters. Live keys are refused.",
        parts.keys),
      card("github", "branch", "GitHub",
        `When you accept work, Decurion sends it to the app's GitHub page as a new branch, using ${here()}'s own GitHub sign-in. With a GitHub token saved here, it also opens the pull request for you. Without one, you get a button that opens it in one tap. Decurion never merges: you do that on GitHub.`,
        parts.github),
      card("sign-ins", "key", "Website sign-ins",
        "Let this app's agents check pages that need a sign-in. You sign in once, in a sign-in window on your PC or phone; the sign-in is kept on your PC and your password is never saved. Use test accounts only: an agent may be able to read part of what a website keeps.",
        parts.signIns),
      phones,
      voiceCard());
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
      el("div", { className: "full actions" }, btn, err));
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
        D.toast(`Added ${n}. You can start work on it now.`);
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

  // ---------- Drawing from the page's data ----------
  function render() {
    if (!root || !shown || !loaded) return;
    if (!builtIn) loadBuiltIn();
    parts.sitesNote.hidden = !apps.length;
    parts.addApp.querySelector("#app-path-pick").hidden = typeof D.pickFolder !== "function";
    const appSpend = Object.fromEntries((spend?.apps ?? []).map((x) => [x.app_id, x]));
    // A half-typed cap would be lost on redraw; a website being typed is kept
    // in siteDraft and its field gets focus back.
    const typingCap = typingIn(parts.appRows) && !document.activeElement.dataset.key?.startsWith("site-");
    if (!typingCap && changed("set-apps", [apps, spend?.apps, D.offline, siteErr, siteBusy])) {
      keepFocus(() => {
        parts.appRows.replaceChildren(...(apps.length ? apps.map((a) => {
          const b = el("button", { type: "button", className: "btn sm", dataset: { key: `ppu-${a.id}` } },
            a.pay_per_use ? `Switch to Max plan ${a.profile}` : "Switch to pay-per-use");
          b.disabled = D.offline;
          b.onclick = async () => {
            b.disabled = true;
            try {
              await D.api(`/apps/${a.id}/pay-per-use`, { method: "POST", body: JSON.stringify({ on: !a.pay_per_use }) });
              D.toast(a.pay_per_use ? `${a.name} now uses Max plan ${a.profile}.` : `${a.name} now uses pay-per-use.`);
              await opts.refresh();
            } catch (e) { if (e.signedOut) { signedOut(); return; } D.toast(`Couldn't switch ${a.name}. ${e.message}`, "alert"); b.disabled = false; }
          };
          const sp = appSpend[a.id];
          const spent = sp && (sp.spent_usd > 0 || sp.cap_usd != null)
            ? `Pay-per-use this month: ${D.money(sp.spent_usd)}${sp.cap_usd != null ? ` of its own ${D.money(sp.cap_usd)} cap` : ""}` : "";
          return el("li", {},
            el("span", { className: "ic" }, icon("app")),
            el("div", { className: "who" }, el("b", {}, a.name), el("span", { className: "mono" }, a.repo_path),
              spent ? el("span", { className: "hint" }, spent) : null),
            el("span", { className: "acct" }, icon(a.pay_per_use ? "money" : "ai"), a.pay_per_use ? "Claude pay-per-use" : `Claude Max plan ${a.profile}`),
            b,
            appCap(a),
            sitesBlock(a),
            filesBlock(a));
        }) : [el("li", { className: "empty" }, "No apps yet. Add one below.")]));
      });
    }
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
      el("div", { className: "sites-h" }, icon("globe"), el("span", { className: "label" }, "Websites"),
        el("span", { className: "hint" }, sites.length ? "Built-in, plus these:" : "Built-in only")),
      list, form, err);
  }

  // Files for agents, drawn by /files.js when the page loads it. Without it
  // the slot is left out.
  function filesBlock(a) {
    if (!window.DFiles?.panel) return null;
    return el("div", { className: "sites", role: "group", "aria-label": `Files for ${a.name}` },
      el("div", { className: "sites-h" }, icon("app"), el("span", { className: "label" }, "Files for agents")),
      window.DFiles.panel({ appId: a.id }));
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
      el("p", { className: "hint", id: "gh-token-hint", style: "align-self:center" }, "Make a fine-grained token on GitHub with Pull requests: Read and write for your apps' repos. See Help, Review and merge work."),
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
    // Taken before loading, and passed back to update, so a website list
    // saved while the load was on its way is not overwritten by older data.
    seq: () => sitesSeq,
    // The page's latest data. Any field left out keeps its last value.
    update(data = {}) {
      if (Array.isArray(data.apps)) {
        const a = data.apps;
        const newer = data.seq == null ? sitesSaved.size > 0 : data.seq !== sitesSeq;
        if (newer) for (const x of a) { if (sitesSaved.has(x.id)) x.sites = sitesSaved.get(x.id); }
        else sitesSaved.clear();
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
    // The settings view opened: load what only settings needs, and scroll to
    // a part ("accounts", "apps", ...) when one is asked for.
    show(part) {
      shown = true;
      render();
      loadDevices();
      loadConnection();
      loadKeys();
      loadGitHub();
      loadSignIns();
      const target = part && root?.querySelector(`#set-${CSS.escape(part === "spending" ? "accounts" : part)}`); // Spending became Accounts
      return target || null;
    },
    hide() { shown = false; },
    get shown() { return shown; },
    // On the page's timer: phones paired from another screen show up.
    tick() { if (shown) loadDevices(); },
    render,
    usage,
  };
  D.onOffline(() => { render(); if (shown && devices) { sigs.devices = ""; loadDevices(); } });
})();
