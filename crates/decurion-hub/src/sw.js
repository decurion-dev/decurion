// Decurion service worker: shows phone notifications and opens the inbox.
self.addEventListener("push", (event) => {
  let note = { title: "Decurion", body: "Something needs you.", url: "/inbox" };
  try { note = { ...note, ...event.data.json() }; } catch (_) {}
  event.waitUntil(self.registration.showNotification(note.title, {
    body: note.body,
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
    if (tab) { await tab.navigate(url); return tab.focus(); }
    return clients.openWindow(url);
  })());
});
