// Decurion service worker: shows phone notifications, opens Home, and
// makes the one tap a notice offers (Approve plan, Accept, Open pull
// request). Those taps go through the same owner routes as Home, with
// this phone's own sign-in cookie and the receipt the hub sent, so a tap on
// two phones, or twice, still acts once. Nothing here holds a key.
self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (event) => event.waitUntil(clients.claim()));

const id = (n) => (Number.isSafeInteger(n) && n > 0 ? n : null);
const word = (s) => (typeof s === "string" && /^[A-Za-z0-9_-]{1,64}$/.test(s) ? s : null);

// Only the three acts the hub knows, with their numbers checked, become
// buttons. Anything else is shown as a plain notice.
function actOf(note) {
  const a = note.act;
  if (!a || typeof a !== "object") return null;
  if (a.kind === "approve" && id(a.goal) && id(a.plan) && word(note.receipt)) return a;
  if (a.kind === "accept" && id(a.thread) && word(note.receipt)) return a;
  if (a.kind === "open_pr" && String(note.url).startsWith("https://github.com/")) return a;
  return null;
}

const BUTTONS = {
  approve: [{ action: "approve", title: "Approve plan" }, { action: "open", title: "Open plan" }],
  accept: [{ action: "accept", title: "Accept" }, { action: "open", title: "Look first" }],
  open_pr: [{ action: "open", title: "Open pull request" }],
};

self.addEventListener("push", (event) => {
  let note = { title: "Decurion", body: "Something needs you.", url: "/inbox", tag: "decurion" };
  try { note = { ...note, ...event.data.json() }; } catch (_) {}
  const act = actOf(note);
  event.waitUntil(self.registration.showNotification(note.title, {
    body: note.body,
    icon: "/icon-192.png",
    tag: word(note.tag) ?? "decurion",
    renotify: true,
    actions: act ? BUTTONS[act.kind] : [],
    data: { url: note.url, app: typeof note.app === "string" ? note.app : "", act, receipt: note.receipt },
  }));
});

// Opens `url`: a page of the hub in its open tab, or a pull request on
// GitHub in its own window. Nothing else outside the hub is ever opened.
async function openUrl(raw) {
  let url = new URL(raw ?? "/inbox", self.location.origin).href;
  if (url.startsWith("https://github.com/")) return clients.openWindow(url);
  if (!url.startsWith(self.location.origin + "/")) url = new URL("/inbox", self.location.origin).href;
  const open = await clients.matchAll({ type: "window", includeUncontrolled: true });
  const tab = open.find((c) => c.url.startsWith(self.location.origin));
  if (tab) {
    // navigate() only works on pages this worker controls; focus either way.
    try { await tab.navigate(url); } catch (_) {}
    return tab.focus();
  }
  return clients.openWindow(url);
}

// One POST with this phone's cookie. If the PC can't be reached, the same
// receipt goes once more, so the hub never does the same thing twice.
async function post(path, body) {
  const opts = {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  };
  try {
    return await fetch(path, opts);
  } catch (_) {
    await new Promise((ok) => setTimeout(ok, 1000));
    return fetch(path, opts);
  }
}

const sentence = (s) => {
  const t = String(s ?? "").trim().slice(0, 160);
  return t ? t[0].toUpperCase() + t.slice(1) + (/[.!?]$/.test(t) ? "" : ".") : "";
};

// Makes the notice's tap and replaces the notice with what happened. A
// failed tap says why and opens the page to finish it there.
async function act(data, kind, tag) {
  const a = data.act;
  const name = (head) => (data.app ? `${data.app}: ${head}` : sentence(head).replace(/\.$/, ""));
  let r, title, body, url = data.url;
  try {
    if (kind === "approve") {
      r = await post(`/api/goals/${a.goal}/approve`, { plan_message_id: a.plan, receipt: data.receipt });
    } else {
      r = await post(`/api/threads/${a.thread}/move`, { to: "done", expect: "in_review", receipt: data.receipt });
    }
  } catch (_) {
    r = null;
  }
  if (r && r.ok) {
    title = name(kind === "approve" ? "plan approved" : "accepted");
    body = kind === "approve" ? "The work has started." : "Decurion is sending it to GitHub. Open Decurion to see what comes next.";
  } else {
    title = name(kind === "approve" ? "plan not approved" : "not accepted");
    if (!r) body = "Can't reach your PC. Open Decurion to try again.";
    else if (r.status === 401) body = "This phone is signed out. Open Decurion, sign in, and try again.";
    else body = sentence(await r.text().catch(() => "")) || "It did not go through. Open Decurion to see why.";
  }
  return self.registration.showNotification(title, {
    body,
    icon: "/icon-192.png",
    tag,
    silent: true,
    data: { url, app: data.app },
  });
}

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const data = event.notification.data ?? {};
  const a = data.act;
  if (a && (event.action === "approve" || event.action === "accept") && event.action === a.kind) {
    event.waitUntil(act(data, a.kind, event.notification.tag));
    return;
  }
  event.waitUntil(openUrl(data.url));
});
