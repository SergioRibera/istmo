/*
 * istmo-notifications service worker.
 *
 * Plain JavaScript, no dependencies. Serve it next to the app's page as
 * `istmo-notifications-sw.js`; the web host registers it. Apps with
 * their own service worker add
 *
 *     importScripts("istmo-notifications-sw.js");
 *
 * to it instead. Only notifications the plugin showed (`data.istmo`)
 * are handled; others are left to the app's own listeners.
 *
 * A click or a close is queued in IndexedDB first, then every window of
 * the app is told to drain the queue. The page drains it when it loads
 * too, so a tap that opened the app reaches it, and with several tabs
 * open each entry reaches exactly one.
 */
(function () {
  "use strict";

  const DB_NAME = "istmo-notifications";
  const DB_VERSION = 1;
  const DATA_VERSION = 1;
  const SCHEDULE = "schedule";
  const QUEUE = "responses";
  const MESSAGE = "istmo.notifications";

  function ours(notification) {
    const data = notification && notification.data;
    return data && data.istmo === DATA_VERSION ? data : null;
  }

  function openDb() {
    return new Promise(function (resolve, reject) {
      const request = indexedDB.open(DB_NAME, DB_VERSION);
      request.onupgradeneeded = function () {
        const db = request.result;
        if (!db.objectStoreNames.contains(SCHEDULE)) {
          db.createObjectStore(SCHEDULE);
        }
        if (!db.objectStoreNames.contains(QUEUE)) {
          db.createObjectStore(QUEUE, { autoIncrement: true });
        }
      };
      request.onsuccess = function () {
        resolve(request.result);
      };
      request.onerror = function () {
        reject(request.error);
      };
      request.onblocked = function () {
        reject(new Error("istmo-notifications: database upgrade blocked"));
      };
    });
  }

  async function enqueue(entry) {
    const db = await openDb();
    try {
      await new Promise(function (resolve, reject) {
        const tx = db.transaction(QUEUE, "readwrite");
        tx.objectStore(QUEUE).add(entry);
        tx.oncomplete = resolve;
        tx.onerror = function () {
          reject(tx.error);
        };
        tx.onabort = function () {
          reject(tx.error);
        };
      });
    } finally {
      db.close();
    }
  }

  function windows() {
    return self.clients.matchAll({ type: "window", includeUncontrolled: true });
  }

  function drain(clients) {
    for (const client of clients) {
      client.postMessage({ istmo: MESSAGE, kind: "drain" });
    }
  }

  /* The focused window, else a visible one, else any. */
  function pick(clients) {
    return (
      clients.find(function (c) {
        return c.focused;
      }) ||
      clients.find(function (c) {
        return c.visibilityState === "visible";
      }) ||
      clients[0] ||
      null
    );
  }

  self.addEventListener("notificationclick", function (event) {
    const data = ours(event.notification);
    if (!data) {
      return;
    }
    const action = event.action || "";
    const background =
      action !== "" && Array.isArray(data.background) && data.background.includes(action);
    event.notification.close();
    event.waitUntil(
      (async function () {
        const before = await windows();
        const launched = !background && before.length === 0;
        await enqueue({
          kind: "click",
          id: data.id,
          action: action,
          launched: launched,
          record: data.record,
          time: Date.now(),
        }).catch(function (err) {
          console.warn("istmo-notifications: click not queued", err);
        });
        if (!background) {
          const client = pick(before);
          if (client) {
            try {
              await client.focus();
            } catch (_err) {
              /* Focus is best effort. */
            }
          } else if (self.clients.openWindow) {
            await self.clients.openWindow(data.open || self.registration.scope);
          }
        }
        drain(await windows());
      })()
    );
  });

  self.addEventListener("notificationclose", function (event) {
    const data = ours(event.notification);
    if (!data) {
      return;
    }
    event.waitUntil(
      enqueue({
        kind: "close",
        id: data.id,
        action: "",
        launched: false,
        record: data.record,
        time: Date.now(),
      })
        .then(windows)
        .then(drain)
        .catch(function (err) {
          console.warn("istmo-notifications: close not queued", err);
        })
    );
  });
})();
