// Decurion service worker: shows phone notifications and opens the inbox.
self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (event) => event.waitUntil(clients.claim()));
self.addEventListener("push", (event) => {
  let note = { title: "Decurion", body: "Something needs you.", url: "/inbox" };
  try { note = { ...note, ...event.data.json() }; } catch (_) {}
  event.waitUntil(self.registration.showNotification(note.title, {
    body: note.body,
    icon: "/icon-192.png",
    tag: "decurion-inbox",
    renotify: true,
    data: { url: note.url },
  }));
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const url = new URL(event.notification.data?.url ?? "/inbox", self.location.origin).href;
  event.waitUntil((async () => {
    const open = await clients.matchAll({ type: "window", includeUncontrolled: true });
    const tab = open.find((c) => c.url.startsWith(self.location.origin));
    if (tab) {
      // navigate() only works on pages this worker controls; focus either way.
      try { await tab.navigate(url); } catch (_) {}
      return tab.focus();
    }
    return clients.openWindow(url);
  })());
});
