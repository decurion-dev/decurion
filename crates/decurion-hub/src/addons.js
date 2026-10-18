// Settings, Add-ons: extra tools for agents (ADR 0001, v1: tools only),
// shared by the board and the phone. settings.js puts this card in its own
// tab (#settings/addons) and calls load() when Settings opens.
//
// What must stay true:
// - Everything is built with D.el, so add-on names, descriptions and the
//   hub's sentences go in as text, never as HTML. Add-on text comes from
//   whoever wrote the add-on.
// - One approval: pasting a link only fetches and shows the card. Nothing
//   is added until the owner taps Add on that card, which sends back the
//   exact files' hash the card was drawn from.
// - The card's "It will be able to" lines are the hub's fixed sentences;
//   this page never words a permission itself.
// - Turning an add-on on or off for an app is the owner's tap, and the hub
//   checks it again (the app has the test key it needs, no tool clashes).
// - Add, Update, on or off and Remove go with D.send, so each carries a
//   receipt and a resend after a dropped connection gets the first answer.
"use strict";
(() => {
  const { el, icon } = D;
  let box = null, list = null, form = null, cardBox = null, logBox = null;
  let opts = { onSignedOut: () => {} };
  let busy = false, data = null;

  const signedOut = () => opts.onSignedOut?.();
  const fill = (b, ...kids) => b.replaceChildren(...kids.filter((k) => k != null && k !== false));
  const short = (rev) => String(rev ?? "").slice(0, 7);

  function failed(e, what) {
    if (e.signedOut) { signedOut(); return; }
    D.toast(`${what} ${e.message}`, "alert");
  }

  // ---------- Add one from its link ----------
  function addForm() {
    const link = el("input", { id: "addon-link", type: "text", autocomplete: "off", autocapitalize: "off", inputMode: "url", maxLength: 500,
      placeholder: "https://github.com/example/words", "aria-describedby": "addon-link-hint addon-err", dataset: { key: "addon-link" } });
    link.spellcheck = false;
    const folder = el("input", { id: "addon-folder", type: "text", autocomplete: "off", autocapitalize: "off", maxLength: 200,
      placeholder: "addons/words", "aria-describedby": "addon-folder-hint", dataset: { key: "addon-folder" } });
    folder.spellcheck = false;
    const err = el("span", { className: "field-error", id: "addon-err", role: "alert" });
    const go = el("button", { className: "btn primary", type: "submit", id: "addon-check" }, "Check it");
    const f = el("form", { className: "form-grid", id: "addon-add", noValidate: true },
      el("div", { className: "field full" }, el("label", { htmlFor: "addon-link" }, "Add an add-on"), link,
        el("span", { className: "hint", id: "addon-link-hint" }, "Paste the add-on's link. Decurion fetches it and shows you what it can do before anything is added.")),
      el("details", { className: "fold full" }, el("summary", {}, icon("right"), "It is in a folder of that repo"),
        el("div", { className: "field" }, el("label", { htmlFor: "addon-folder" }, "Folder"), folder,
          el("span", { className: "hint", id: "addon-folder-hint" }, "Where its decurion-addon.toml is, inside the repo."))),
      el("div", { className: "full actions" }, go, err));
    f.onsubmit = async (e) => {
      e.preventDefault();
      if (busy) return;
      err.textContent = "";
      if (D.offline) { err.textContent = "Your PC isn't answering. Try again when it's back."; return; }
      const source = link.value.trim();
      if (!source) { err.textContent = "Paste the add-on's link first."; link.focus(); return; }
      busy = true; go.disabled = true; go.textContent = "Checking…";
      try {
        const card = await D.api("/addons/preview", { method: "POST", body: JSON.stringify({ source, folder: folder.value.trim() }) });
        showCard(card);
      } catch (x) {
        if (x.signedOut) { signedOut(); return; }
        err.textContent = x.message;
      } finally {
        busy = false; go.disabled = false; go.textContent = "Check it";
      }
    };
    return f;
  }

  // ---------- The one approval ----------
  function showCard(c) {
    const update = c.update_from != null;
    const picks = [];
    const apps = update ? null : el("fieldset", { className: "addon-apps" },
      el("legend", { className: "label" }, "Use it in"),
      ...(c.apps.length ? c.apps.map((a) => {
        const id = `addon-app-${a.id}`;
        const cb = el("input", { type: "checkbox", id, disabled: a.missing.length > 0 });
        picks.push([a.id, cb]);
        return el("div", { className: "check" }, cb, el("label", { htmlFor: id }, a.name),
          a.missing.length ? el("span", { className: "hint" }, ` needs the test key ${a.missing.join(" and ")} first`) : null);
      }) : [el("p", { className: "hint" }, "Add an app first, then turn the add-on on for it.")]),
      el("p", { className: "hint" }, "You can change this any time."));
    const err = el("span", { className: "field-error", role: "alert" });
    const yes = el("button", { type: "button", className: "btn primary", id: "addon-yes" }, update ? "Update" : "Add");
    const no = el("button", { type: "button", className: "btn" }, "Cancel");
    no.onclick = () => { cardBox.hidden = true; fill(cardBox); };
    yes.onclick = async () => {
      if (busy) return;
      err.textContent = "";
      busy = true; yes.disabled = true;
      const chosen = picks.filter(([, cb]) => cb.checked).map(([id]) => id);
      try {
        const r = await D.send("/addons/approve", { hash: c.hash, apps: chosen });
        D.toast(update ? `Updated ${r.name} to version ${r.version}.` : `Added ${r.name}. Its tools reach agents from their next piece of work.`);
        cardBox.hidden = true; fill(cardBox);
        form.reset();
        await load();
      } catch (x) {
        if (x.signedOut) { signedOut(); return; }
        err.textContent = x.message;
      } finally { busy = false; yes.disabled = false; }
    };
    const about = [c.description, c.authors?.length ? `By ${c.authors.join(", ")}.` : null].filter(Boolean).join(" ");
    fill(cardBox,
      el("h3", { id: "addon-card-h", tabIndex: -1 }, c.title),
      about ? el("p", {}, about) : null,
      el("p", { className: "hint" }, `From ${c.from}, commit ${short(c.rev)}.`),
      el("p", { className: "label" }, update ? "What it would do that it could not before:" : "It will be able to:"),
      c.can.length ? el("ul", { className: "addon-can" }, ...c.can.map((l) => el("li", {}, l))) : el("p", { className: "hint" }, "Nothing new."),
      el("p", {}, c.never),
      c.warnings?.length ? el("div", { className: "notice" }, icon("alert"),
        el("div", { className: "grow" }, el("b", {}, "Good to know"), el("ul", {}, ...c.warnings.map((w) => el("li", {}, w))))) : null,
      update ? el("p", { className: "hint" }, `Version ${c.update_from} keeps working until you tap Update.`) : apps,
      el("div", { className: "actions" }, yes, no, err));
    cardBox.hidden = false;
    cardBox.querySelector("#addon-card-h")?.focus({ preventScroll: false });
  }

  // ---------- What is added ----------
  function appSwitch(x, a) {
    const id = `addon-${x.id}-app-${a.id}`;
    const b = el("button", { type: "button", className: "switch", role: "switch", "aria-checked": a.on ? "true" : "false",
      "aria-labelledby": `${id}-l`, dataset: { key: id }, disabled: D.offline || (!a.on && a.missing.length > 0) },
      el("span", { className: "track" }), el("span", { id: `${id}-l` }, a.name));
    b.onclick = async () => {
      if (busy) return;
      busy = true; b.disabled = true;
      try {
        await D.send(`/addons/${encodeURIComponent(x.id)}/apps/${a.id}`, { on: !a.on });
        D.toast(!a.on ? `${x.name} is on for ${a.name} from its next piece of work.` : `${x.name} is off for ${a.name} from its next piece of work.`);
        await load();
      } catch (e) { failed(e, `Couldn't change ${x.name} for ${a.name}.`); }
      finally { busy = false; b.disabled = false; }
    };
    const why = !a.on && a.missing.length ? el("span", { className: "hint" }, `needs the test key ${a.missing.join(" and ")}`) : null;
    return el("div", { className: "check" }, b, why);
  }

  function row(x) {
    const upd = el("button", { type: "button", className: "btn sm", dataset: { key: `addon-upd-${x.id}` }, disabled: D.offline }, icon("retry"), "Check for an update");
    upd.onclick = async () => {
      if (busy) return;
      busy = true; upd.disabled = true;
      try {
        const r = await D.send(`/addons/${encodeURIComponent(x.id)}/update`, {});
        if (r.card) { showCard(r.card); D.toast(`A new version of ${x.name} asks for more. See what is new below.`, "alert"); }
        else if (r.updated) { D.toast(`Updated ${x.name} to version ${r.version}. It asks for nothing new.`); await load(); }
        else D.toast(`${x.name} is up to date.`);
      } catch (e) { failed(e, `Couldn't check ${x.name} for an update.`); }
      finally { busy = false; upd.disabled = false; }
    };
    const rm = el("button", { type: "button", className: "btn quiet danger-text sm", dataset: { key: `addon-rm-${x.id}` }, disabled: D.offline }, icon("trash"), "Remove");
    rm.onclick = async () => {
      const ok = await D.confirm([`Remove ${x.name}?`, "New work stops getting its tools at once. Work already running finishes with it. What it did stays in the log.", "Remove add-on", "Keep it"]);
      if (!ok) return;
      try { await D.send(`/addons/${encodeURIComponent(x.id)}/remove`, {}); D.toast(`Removed ${x.name}.`); await load(); }
      catch (e) { failed(e, `Couldn't remove ${x.name}.`); }
    };
    return el("li", { className: "addon-row" },
      el("span", { className: "ic" }, icon("tool")),
      el("div", { className: "who" },
        el("b", {}, `${x.name} · version ${x.version}`),
        x.description ? el("span", { className: "hint" }, x.description) : null,
        el("span", { className: "hint" }, `From ${x.from}, commit ${short(x.rev)}. Added ${D.ago(x.approved_at)}.`),
        el("details", { className: "fold" }, el("summary", {}, icon("right"), "What it can do"),
          el("ul", { className: "addon-can" }, ...x.can.map((l) => el("li", {}, l))), el("p", { className: "hint" }, data.never)),
        el("div", { className: "addon-on", role: "group", "aria-label": `Apps ${x.name} is on for` },
          el("span", { className: "label" }, "On for"),
          ...(x.apps.length ? x.apps.map((a) => appSwitch(x, a)) : [el("span", { className: "hint" }, "No apps yet.")]))),
      el("div", { className: "actions" }, upd, rm));
  }

  // The log in plain words, newest first, each line under the add-on's
  // name (its id only once it is removed and the name is no longer known).
  function logLine(l, names, addonNames) {
    const app = l.app_id != null ? names[l.app_id] ?? `app ${l.app_id}` : "";
    const work = l.thread_id != null ? `piece of work ${l.thread_id}` : "";
    const what = {
      installed: `Added. ${l.detail}`,
      updated: `Updated, ${l.detail}`,
      removed: "Removed",
      enabled: `Turned on for ${app}`,
      disabled: `Turned off for ${app}`,
      used: `Used by ${work} (${app}), ${l.detail}`,
      key_used: `Gave ${work} (${app}) its ${l.detail}`,
      refused: `Not used by ${work}: its files changed after you approved them`,
    }[l.event] ?? l.event;
    return el("li", {}, el("div", { className: "who" }, el("b", {}, `${addonNames[l.addon_id] ?? l.addon_id}: ${what}`),
      el("span", { className: "hint" }, `${D.ago(l.at)}${l.actor === "owner" ? ", by you" : ""}`)));
  }

  async function load() {
    if (!box) return;
    try { data = await D.api("/addons"); } catch (e) {
      if (e.signedOut) { signedOut(); return; }
      fill(list, el("li", { className: "empty" }, `Couldn't load add-ons. ${e.message}`));
      return;
    }
    fill(list, ...(data.addons.length ? data.addons.map(row) : [el("li", { className: "empty" }, "No add-ons yet. Paste a link below to add one.")]));
    const names = {}, addonNames = {};
    for (const x of data.addons) {
      addonNames[x.id] = x.name;
      for (const a of x.apps) names[a.id] = a.name;
    }
    fill(logBox, data.log.length
      ? el("ul", { className: "rows" }, ...data.log.map((l) => logLine(l, names, addonNames)))
      : el("p", { className: "hint" }, "Nothing yet."));
  }

  // The card settings.js puts in its Add-ons tab.
  function card(o = {}) {
    opts = { ...opts, ...o };
    list = el("ul", { className: "rows", id: "addon-rows" }, el("li", { className: "empty" }, "Loading…"));
    form = addForm();
    cardBox = el("div", { className: "addon-card per-app", id: "addon-card", role: "region", "aria-labelledby": "addon-card-h", hidden: true });
    logBox = el("div", { id: "addon-log" });
    box = el("section", { className: "scard wide live", id: "set-addons", "aria-labelledby": "h-addons" },
      el("header", {}, el("span", { className: "ic" }, icon("tool", "l")), el("div", {}, el("h2", { id: "h-addons" }, "Add-ons"),
        el("p", {}, "Add-ons give your agents extra tools. You add one from its link and approve it once; it never asks again unless an update wants to do more. Each tool runs in a sealed box of its own, never sees your Claude sign-in, and the rules that always stop for you still do."))),
      list, cardBox, form,
      el("details", { className: "fold" }, el("summary", {}, icon("right"), "What add-ons did lately"), logBox));
    return box;
  }

  window.DAddons = { card, load };
})();
