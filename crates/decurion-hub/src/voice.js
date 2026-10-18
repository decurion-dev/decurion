// Decurion's voice: a mic beside typing boxes and a speaker on replies,
// questions and reports, on the board, the phone inbox and Goals.
//
// What must stay true:
// - Only the browser's own speech is used (SpeechRecognition to turn speech
//   into words, speechSynthesis to read aloud). Decurion never records, sends
//   or saves sound. The words land in the box exactly as if typed, and the
//   owner still taps Send: nothing sends by itself unless the owner turned on
//   "Send when I stop talking" for Talk mode.
// - The mic is on only while its button shows red. It stops on a tap, a
//   pause, leaving the page, or when another mic starts.
// - Key, token and password boxes never get a mic (`attachable`).
// - Settings live on each device in localStorage, never on the hub.
//
// Pages opt in with attributes, so this file works beside whatever the page
// already does: `data-voice` on a textarea or text input adds a mic (put it in
// `[data-voice-slot="<field id>"]` if the page has one, otherwise right after
// the field); `data-speak` on an element adds a speaker that reads it aloud
// (the attribute's value if given, otherwise the element's own text).
"use strict";
(() => {
  const SR = window.SpeechRecognition || window.webkitSpeechRecognition;
  const synth = window.speechSynthesis;
  const KEY = "decurion.voice";
  const DEFAULTS = { voice: "", rate: 1, readNew: false, talk: false, autoSend: false, onDevice: true };

  const V = {};
  window.Voice = V;
  if (window.D) window.D.voice = V;

  V.canListen = () => !!SR && window.isSecureContext !== false;
  V.canSpeak = () => !!synth && typeof SpeechSynthesisUtterance !== "undefined";

  // ---------- Settings (this device only) ----------
  V.settings = () => {
    try { return { ...DEFAULTS, ...JSON.parse(localStorage.getItem(KEY) || "{}") }; } catch (_) { return { ...DEFAULTS }; }
  };
  V.save = (patch) => {
    const next = { ...V.settings(), ...patch };
    try { localStorage.setItem(KEY, JSON.stringify(next)); } catch (_) {}
    document.dispatchEvent(new CustomEvent("voice:settings", { detail: next }));
    return next;
  };

  // ---------- Small pieces ----------
  const NS = "http://www.w3.org/2000/svg";
  const svg = (paths, filled) => {
    const s = document.createElementNS(NS, "svg");
    s.setAttribute("viewBox", "0 0 24 24");
    s.setAttribute("width", "18");
    s.setAttribute("height", "18");
    s.setAttribute("aria-hidden", "true");
    s.setAttribute("fill", filled ? "currentColor" : "none");
    s.setAttribute("stroke", filled ? "none" : "currentColor");
    s.setAttribute("stroke-width", "2.2");
    s.setAttribute("stroke-linecap", "round");
    s.setAttribute("stroke-linejoin", "round");
    for (const d of paths) {
      const p = document.createElementNS(NS, "path");
      p.setAttribute("d", d);
      s.append(p);
    }
    return s;
  };
  const MIC = ["M12 3a3 3 0 0 0-3 3v5a3 3 0 0 0 6 0V6a3 3 0 0 0-3-3z", "M5 11a7 7 0 0 0 14 0", "M12 18v3"];
  const STOP = ["M7 7h10v10H7z"];
  const SPEAKER = ["M4 9h4l5-4v14l-5-4H4z", "M16.5 8.5a5 5 0 0 1 0 7"];

  const say = (text) => {
    if (window.D?.toast) return window.D.toast(text, "alert");
    let box = document.querySelector(".voice-note");
    if (!box) {
      box = document.createElement("div");
      box.className = "voice-note";
      box.setAttribute("role", "status");
      box.setAttribute("aria-live", "polite");
      document.body.append(box);
    }
    box.textContent = text;
    clearTimeout(say.t);
    say.t = setTimeout(() => { box.textContent = ""; }, 6000);
  };

  const lang = () => document.documentElement.lang && document.documentElement.lang.length > 1
    ? (navigator.language?.startsWith(document.documentElement.lang) ? navigator.language : document.documentElement.lang)
    : (navigator.language || "en-US");

  // ---------- Reading aloud ----------
  // Turns the page's text into what a person would say: no markdown marks,
  // no long addresses, and short pieces, because Chrome stops a single long
  // utterance after about fifteen seconds.
  V.speakable = (text) => String(text ?? "")
    .replace(/```[\s\S]*?```/g, " There is some code here. ")
    .replace(/`([^`]*)`/g, "$1")
    .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
    .replace(/https?:\/\/\S+/g, " a link ")
    .replace(/[*_#>]+/g, " ")
    .replace(/\s*[\r\n]+\s*/g, ". ")
    .replace(/\.(\s*\.)+/g, ".")
    .replace(/\s{2,}/g, " ")
    .trim();

  V.pieces = (text, max = 220) => {
    const out = [];
    for (const sentence of V.speakable(text).match(/[^.!?]+[.!?]*\s*/g) ?? []) {
      let s = sentence.trim();
      while (s.length > max) {
        let cut = s.lastIndexOf(",", max);
        if (cut < max / 2) cut = s.lastIndexOf(" ", max);
        if (cut < 1) cut = max;
        out.push(s.slice(0, cut + 1).trim());
        s = s.slice(cut + 1).trim();
      }
      if (s) out.push(s);
    }
    return out;
  };

  let voices = [];
  const loadVoices = () => { voices = V.canSpeak() ? synth.getVoices() : []; };
  if (V.canSpeak()) {
    loadVoices();
    synth.addEventListener?.("voiceschanged", loadVoices);
  }
  V.voices = () => { if (!voices.length) loadVoices(); return voices; };
  const pickVoice = () => {
    const want = V.settings().voice;
    const all = V.voices();
    return all.find((v) => v.voiceURI === want)
      ?? all.find((v) => v.default && v.lang?.startsWith(lang().slice(0, 2)))
      ?? all.find((v) => v.lang?.startsWith(lang().slice(0, 2)))
      ?? null;
  };

  let reading = null; // { owner, token }
  const announce = () => document.dispatchEvent(new CustomEvent("voice:reading", { detail: { owner: reading?.owner ?? null } }));

  V.stop = () => {
    if (!V.canSpeak()) return;
    reading = null;
    synth.cancel();
    announce();
  };
  V.reading = (owner) => owner == null ? !!reading : reading?.owner === owner;

  // Reads text aloud and resolves when it finishes or is stopped. `owner` is
  // whatever started it (a button), so that button can show "Stop".
  V.speak = (text, { owner = null } = {}) => new Promise((resolve) => {
    if (!V.canSpeak()) { say("This browser can't read aloud."); return resolve(false); }
    const parts = V.pieces(text);
    if (!parts.length) return resolve(false);
    V.listening && V.listening.stop();
    synth.cancel();
    const token = {};
    reading = { owner, token };
    announce();
    const { rate } = V.settings();
    const voice = pickVoice();
    let i = 0;
    const next = () => {
      if (reading?.token !== token) return resolve(false);
      if (i >= parts.length) { reading = null; announce(); return resolve(true); }
      const u = new SpeechSynthesisUtterance(parts[i++]);
      if (voice) { u.voice = voice; u.lang = voice.lang; } else u.lang = lang();
      u.rate = Math.min(1.6, Math.max(0.6, Number(rate) || 1));
      u.onend = next;
      u.onerror = (e) => {
        if (e.error !== "interrupted" && e.error !== "canceled" && reading?.token === token) {
          reading = null;
          announce();
          if (e.error === "not-allowed") say("Tap the speaker button to hear it. The browser only reads aloud after a tap.");
        }
        resolve(false);
      };
      synth.speak(u);
    };
    next();
  });

  // Reads a new reply if the owner turned that on, the page is in view and
  // they have tapped the page at least once (browsers refuse sound before).
  V.readNew = (text) => {
    const s = V.settings();
    if (!(s.readNew || s.talk) || document.hidden || !V.canSpeak()) return Promise.resolve(false);
    if (navigator.userActivation && !navigator.userActivation.hasBeenActive) return Promise.resolve(false);
    return V.speak(text);
  };

  // A speaker button. `text` is a string or a function that returns one.
  V.speaker = (text, { label = "Read aloud" } = {}) => {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "voice-speak";
    const name = document.createElement("span");
    b.append(svg(SPEAKER), name);
    const paint = () => {
      const on = V.reading(b);
      b.classList.toggle("on", on);
      b.setAttribute("aria-pressed", on ? "true" : "false");
      name.textContent = on ? "Stop" : label;
    };
    paint();
    document.addEventListener("voice:reading", paint);
    b.addEventListener("click", (e) => {
      e.preventDefault();
      e.stopPropagation();
      if (V.reading(b)) return V.stop();
      V.speak(typeof text === "function" ? text() : text, { owner: b });
    });
    if (!V.canSpeak()) b.hidden = true;
    return b;
  };

  // ---------- Listening ----------
  // Chrome on a PC (139 and later) can turn speech into words on the PC
  // itself once a language pack is downloaded; elsewhere the browser's own
  // speech service does it. `local` caches what this device can do.
  let local = null;
  V.onDevice = async () => {
    if (!SR?.available) return "unavailable";
    try { return await SR.available({ langs: [lang()], processLocally: true }); } catch (_) { return "unavailable"; }
  };
  V.installOnDevice = async () => {
    if (!SR?.install) return false;
    try { const ok = await SR.install({ langs: [lang()], processLocally: true }); local = (await V.onDevice()) === "available"; return !!ok; } catch (_) { return false; }
  };

  const ERRORS = {
    "not-allowed": "The microphone is blocked for this page. Allow it in the browser's site settings, then tap the mic again.",
    "service-not-allowed": "This browser won't turn speech into words here. Try Chrome, or type instead.",
    "audio-capture": "No microphone was found. Plug one in or check it is turned on, then try again.",
    "network": "Speech needs the internet on this device. Check the connection, or type instead.",
    "no-speech": "I didn't hear anything. Tap the mic and try again.",
    "language-not-supported": "This browser can't hear this language. Type instead.",
  };

  // Starts listening and returns at once with `{ stop(), done }`; `done`
  // resolves with everything heard once listening stops. `onText(final,
  // interim)` is called as words arrive. Nothing here waits before
  // `start()`, so the browser still sees the owner's tap.
  V.listen = ({ onText = () => {}, continuous = true } = {}) => {
    let stopped = false;
    let rec = null;
    const handle = { stop() { stopped = true; try { rec?.stop(); } catch (_) {} }, done: null };
    handle.done = new Promise((resolve) => {
      if (!V.canListen()) { say("This browser can't hear you. Use Chrome on the phone or PC, or type instead."); return resolve(""); }
      V.listening && V.listening.stop();
      V.stop();
      rec = new SR();
      rec.lang = lang();
      rec.continuous = continuous;
      rec.interimResults = true;
      rec.maxAlternatives = 1;
      if (local && V.settings().onDevice && "processLocally" in rec) rec.processLocally = true;
      let heard = "";
      V.listening = handle;
      document.dispatchEvent(new CustomEvent("voice:listening", { detail: { on: true } }));
      rec.onresult = (e) => {
        let fin = "", interim = "";
        for (let i = 0; i < e.results.length; i++) {
          const t = e.results[i][0]?.transcript ?? "";
          if (e.results[i].isFinal) fin += t; else interim += t;
        }
        heard = fin;
        onText(fin.trim(), interim.trim());
      };
      rec.onerror = (e) => {
        if (e.error === "aborted") return;
        if (e.error === "language-not-supported" && rec.processLocally) local = false;
        if (e.error === "no-speech" && heard) return;
        say(ERRORS[e.error] ?? "Listening stopped. Tap the mic and try again.");
      };
      rec.onend = () => {
        if (V.listening === handle) V.listening = null;
        document.dispatchEvent(new CustomEvent("voice:listening", { detail: { on: false, stopped } }));
        resolve(heard.trim());
      };
      try { rec.start(); } catch (_) { if (V.listening === handle) V.listening = null; resolve(""); }
    });
    return handle;
  };
  // Learn early whether this device can keep speech on itself, so a tap on
  // the mic never has to wait for the answer.
  if (V.canListen() && SR.available) V.onDevice().then((st) => { local = st === "available"; });

  // Fields that must never get a mic: anything holding a key or a secret.
  const SECRET = /key|token|secret|pass|pin|otp|code/i;
  V.attachable = (field) => {
    if (!(field instanceof HTMLTextAreaElement || field instanceof HTMLInputElement)) return false;
    if (field instanceof HTMLInputElement && !["text", "search", ""].includes(field.type)) return false;
    if (field.autocomplete && /password|one-time-code|cc-/.test(field.autocomplete)) return false;
    return !SECRET.test(`${field.id} ${field.name}`);
  };

  // Puts a mic button beside a typing box. Words go in where the cursor was,
  // and an `input` event fires so the page treats them like typing.
  // `onDone(text)` runs after listening stops with something heard.
  V.mic = (field, { onDone = null, label = "Talk instead of typing" } = {}) => {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "voice-mic";
    b.setAttribute("aria-label", label);
    b.title = label;
    const icon = () => b.replaceChildren(svg(b.classList.contains("on") ? STOP : MIC, b.classList.contains("on")));
    icon();
    if (!V.canListen() || !V.attachable(field)) { b.hidden = true; return b; }
    let mine = null;
    const set = (on) => {
      b.classList.toggle("on", on);
      b.setAttribute("aria-pressed", on ? "true" : "false");
      b.setAttribute("aria-label", on ? "Stop listening" : label);
      b.title = on ? "Stop listening" : label;
      field.classList.toggle("voice-live", on);
      icon();
    };
    b.addEventListener("click", async (e) => {
      e.preventDefault();
      if (mine) { mine.stop(); return; }
      if (field.disabled || field.readOnly) return;
      const start = field.selectionStart ?? field.value.length;
      const end = field.selectionEnd ?? start;
      const before = field.value.slice(0, start);
      const after = field.value.slice(end);
      const gapBefore = before && !/\s$/.test(before) ? " " : "";
      const gapAfter = after && !/^\s/.test(after) ? " " : "";
      set(true);
      mine = V.listen({
        onText(fin, interim) {
          const words = [fin, interim].filter(Boolean).join(" ");
          field.value = before + (words ? gapBefore + words + gapAfter : "") + after;
          const caret = (before + gapBefore + words).length;
          try { field.setSelectionRange(caret, caret); } catch (_) {}
          field.dispatchEvent(new Event("input", { bubbles: true }));
        },
      });
      const text = await mine.done;
      mine = null;
      set(false);
      field.dispatchEvent(new Event("input", { bubbles: true }));
      field.focus({ preventScroll: true });
      if (text && onDone) onDone(text);
    });
    return b;
  };

  // ---------- Answering questions by voice ----------
  const STOPWORDS = new Set("a an the i i'd id im i'm it its it's is be to of and or for on in with that this those these one ones fine ok okay sure please lets let's go use want would like just yeah yes no not think say pick choose option choice answer they them their we you your my our me".split(" "));
  const norm = (s) => String(s ?? "").toLowerCase().replace(/[^\p{L}\p{N}\s']/gu, " ").replace(/\s+/g, " ").trim();
  const stem = (w) => w.replace(/'s$/, "").replace(/(ies)$/, "y").replace(/(?<=\w{3})s$/, "");
  const words = (s) => norm(s).split(" ").filter((w) => w && !STOPWORDS.has(w)).map(stem);
  const ORD = { first: 0, "1st": 0, one: 0, "1": 0, second: 1, "2nd": 1, two: 1, "2": 1, third: 2, "3rd": 2, three: 2, "3": 2, fourth: 3, "4th": 3, four: 3, "4": 3, fifth: 4, five: 4, "5": 4 };

  // Finds which choice a spoken answer means, or -1 when it isn't clear.
  // Order words count only when they are the whole answer ("the second
  // one", "option 2"), so "the clinical one" is never read as "one".
  V.matchChoice = (heard, choices) => {
    const h = norm(heard);
    if (!h || !choices?.length) return -1;
    const labels = choices.map(norm);
    const exact = labels.findIndex((l) => l === h);
    if (exact >= 0) return exact;
    const ord = h.match(/^(?:the |number |option |choice )?(first|1st|second|2nd|third|3rd|fourth|4th|fifth|one|two|three|four|five|[1-5]|last)(?: one| option| choice)?$/);
    if (ord) {
      const i = ord[1] === "last" ? choices.length - 1 : ORD[ord[1]];
      return i < choices.length ? i : -1;
    }
    const yn = h.match(/^(yes|yeah|yep|no|nope)\b/);
    if (yn) {
      const want = /^y/.test(yn[1]) ? "yes" : "no";
      const hits = labels.map((l, i) => (l.split(" ")[0] === want ? i : -1)).filter((i) => i >= 0);
      if (hits.length === 1 && h.split(" ").length <= 3) return hits[0];
    }
    const heardWords = new Set(words(h));
    // A word every choice shares ("role" in "clinical role" and "own role")
    // says nothing about which one was meant.
    const each = labels.map((l) => new Set(words(l)));
    const shared = (x) => each.length > 1 && each.every((set) => set.has(x));
    const scores = labels.map((l, i) => {
      if (l.length > 3 && h.includes(l)) return 1;
      const w = [...each[i]].filter((x) => !shared(x));
      if (!w.length) return 0;
      return w.filter((x) => heardWords.has(x)).length / w.length;
    });
    const best = Math.max(...scores);
    const at = scores.indexOf(best);
    const second = Math.max(...scores.filter((_, i) => i !== at), 0);
    return best >= 0.34 && best - second >= 0.2 ? at : -1;
  };

  // ---------- Settings panel ----------
  // A self-contained Voice section for a Settings screen. Text only goes in as
  // text, never as HTML.
  V.settingsPanel = () => {
    const el = (tag, props = {}, ...kids) => {
      const e = document.createElement(tag);
      Object.assign(e, props);
      e.append(...kids.filter((k) => k != null));
      return e;
    };
    const s = V.settings();
    const box = el("div", { className: "voice-settings" });
    const toggle = (key, title, hint) => {
      const input = el("input", { type: "checkbox", checked: !!s[key], id: `voice-${key}` });
      input.addEventListener("change", () => V.save({ [key]: input.checked }));
      return el("label", { className: "voice-row", htmlFor: input.id },
        el("span", {}, el("b", {}, title), el("small", {}, hint)), input);
    };
    if (!V.canListen() && !V.canSpeak()) {
      box.append(el("p", { className: "voice-hint" }, "This browser can't listen or read aloud. Use Chrome on the phone or the PC."));
      return box;
    }
    if (V.canSpeak()) {
      const pick = el("select", { id: "voice-voice" });
      const fill = () => {
        const want = V.settings().voice;
        const mine = V.voices().filter((v) => v.lang?.startsWith(lang().slice(0, 2)));
        pick.replaceChildren(el("option", { value: "", textContent: "This device's usual voice" }),
          ...mine.map((v) => el("option", { value: v.voiceURI, selected: v.voiceURI === want, textContent: v.localService ? `${v.name} (on this device)` : v.name })));
      };
      fill();
      synth.addEventListener?.("voiceschanged", fill);
      pick.addEventListener("change", () => V.save({ voice: pick.value }));
      const rate = el("input", { type: "range", min: "0.7", max: "1.5", step: "0.1", value: String(s.rate), id: "voice-rate" });
      rate.addEventListener("change", () => V.save({ rate: Number(rate.value) }));
      const sample = V.speaker("This is how I'll read replies and reports to you.", { label: "Hear it" });
      box.append(
        el("label", { className: "voice-row", htmlFor: pick.id }, el("span", {}, el("b", {}, "Voice")), pick),
        el("label", { className: "voice-row", htmlFor: rate.id }, el("span", {}, el("b", {}, "Speed")), rate),
        el("div", { className: "voice-row" }, el("span", {}, el("small", {}, "Try the voice and speed")), sample),
        toggle("readNew", "Read new replies aloud", "While the page is open on this device."),
      );
    }
    if (V.canListen()) {
      box.append(
        toggle("talk", "Talk mode", "Reads each reply from your project lead, then listens for yours."),
        toggle("autoSend", "Send when I stop talking", "Only in Talk mode. Off means you check the words and tap Send."),
      );
      if (SR?.available) {
        const line = el("small", {}, "Checking…");
        const act = el("button", { type: "button", className: "btn", hidden: true, textContent: "Download speech for this PC" });
        const row = el("div", { className: "voice-row" }, el("span", {}, el("b", {}, "Keep my voice on this device"), line), act);
        const show = async () => {
          const st = await V.onDevice();
          act.hidden = st !== "downloadable";
          line.textContent = st === "available" ? "On. Speech turns into words on this device, not online."
            : st === "downloading" ? "Downloading the speech pack…"
            : st === "downloadable" ? "Download a one-time speech pack so your voice stays on this device."
            : "Not offered by this browser. Speech goes to the browser's speech service.";
        };
        act.addEventListener("click", async () => {
          act.disabled = true;
          line.textContent = "Downloading the speech pack…";
          const ok = await V.installOnDevice();
          act.disabled = false;
          if (!ok) say("The speech pack didn't download. Check the connection and try again.");
          show();
        });
        show();
        box.append(row);
      }
    }
    box.append(el("p", { className: "voice-hint" }, "Decurion never records or keeps sound. Only the words you send are kept, the same as typing."));
    return box;
  };

  // ---------- Pages opt in with attributes ----------
  V.enhance = (root = document) => {
    for (const f of root.querySelectorAll?.("textarea[data-voice], input[data-voice]") ?? []) {
      if (f.voiceMic) continue;
      f.voiceMic = V.mic(f, { label: f.dataset.voiceLabel || undefined });
      if (f.voiceMic.hidden) continue;
      const slot = f.id && document.querySelector(`[data-voice-slot="${CSS.escape(f.id)}"]`);
      if (slot) slot.append(f.voiceMic); else f.after(f.voiceMic);
    }
    for (const node of root.querySelectorAll?.("[data-speak]") ?? []) {
      if (node.voiceSpeaker) continue;
      const btn = V.speaker(() => node.dataset.speak || textOf(node), { label: node.dataset.speakLabel || undefined });
      node.voiceSpeaker = btn;
      (node.querySelector("[data-speak-slot]") ?? node).append(btn);
    }
  };
  const textOf = (node) => {
    const copy = node.cloneNode(true);
    for (const b of copy.querySelectorAll(".voice-speak, .voice-mic, button, [aria-hidden='true']")) b.remove();
    return copy.innerText ?? copy.textContent ?? "";
  };

  // Stop everything when the page goes out of view: the mic must never stay
  // on in a background tab.
  document.addEventListener("visibilitychange", () => {
    if (document.hidden) { V.listening && V.listening.stop(); V.stop(); }
  });

  const style = document.createElement("style");
  style.textContent = `
    .voice-mic { flex:none; width:44px; height:44px; border-radius:50%; border:1px solid var(--field-line, #7d8695); background:var(--surface, #fff); color:var(--accent-ink, #1f4fd1); display:inline-grid; place-items:center; cursor:pointer; position:relative; padding:0; }
    .voice-mic[hidden], .voice-speak[hidden] { display:none; }
    .voice-mic:hover { background:var(--accent-soft, #e8eefc); }
    .voice-mic:focus-visible, .voice-speak:focus-visible { outline:2px solid var(--focus, #2553d6); outline-offset:2px; }
    .voice-mic.on { background:var(--red-solid, #c0261b); border-color:var(--red-solid, #c0261b); color:#fff; }
    .voice-mic.on::after { content:""; position:absolute; inset:-5px; border-radius:50%; border:2px solid var(--red-solid, #c0261b); animation:voice-ring 1.4s ease-out infinite; pointer-events:none; }
    @keyframes voice-ring { from { transform:scale(.9); opacity:.6; } to { transform:scale(1.25); opacity:0; } }
    .voice-live { border-color:var(--red-solid, #c0261b) !important; box-shadow:0 0 0 3px var(--red-soft, #fdeceb) !important; }
    .voice-speak { display:inline-flex; align-items:center; gap:5px; min-height:32px; border:1px solid var(--line, #e1e4ea); border-radius:99px; padding:4px 11px 4px 8px; background:var(--surface, #fff); color:var(--accent-ink, #1f4fd1); font:600 12.5px/1 inherit; font-family:inherit; cursor:pointer; vertical-align:middle; }
    .voice-speak svg { width:15px; height:15px; }
    .voice-speak.on { background:var(--accent, #2553d6); border-color:var(--accent, #2553d6); color:var(--on-accent, #fff); }
    .voice-settings { display:grid; gap:10px; }
    .voice-row { display:flex; align-items:center; justify-content:space-between; gap:12px; }
    .voice-row > span { display:grid; gap:2px; min-width:0; }
    .voice-row small, .voice-hint { color:var(--muted, #545d6b); font-size:12.5px; }
    .voice-row select { max-width:60%; }
    .voice-row input[type=checkbox] { width:20px; height:20px; flex:none; }
    .voice-note { position:fixed; left:50%; bottom:calc(16px + env(safe-area-inset-bottom, 0px)); transform:translateX(-50%); max-width:min(92vw, 420px); background:var(--ink, #14171c); color:var(--bg, #fff); border-radius:10px; padding:10px 14px; font-size:14px; z-index:50; }
    .voice-note:empty { display:none; }
    @media (prefers-reduced-motion: reduce) { .voice-mic.on::after { animation:none; } }
  `;
  document.head.append(style);

  const start = () => {
    V.enhance();
    new MutationObserver((changes) => {
      for (const c of changes) for (const n of c.addedNodes) if (n.nodeType === 1) {
        if (n.matches?.("textarea[data-voice], input[data-voice], [data-speak]")) V.enhance(n.parentNode ?? document);
        else V.enhance(n);
      }
    }).observe(document.body, { childList: true, subtree: true });
  };
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", start);
  else start();
})();
