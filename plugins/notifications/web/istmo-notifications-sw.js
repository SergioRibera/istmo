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
 * A click or a close is queued in IndexedDB first, then one window of
 * the app is told to drain the queue: the one a click focused or
 * opened, the focused one for a close; every window only when there is
 * no such window. The page drains the queue when it loads too, so a tap
 * that opened the app reaches it, and with several tabs open each entry
 * reaches exactly one.
 */
(function () {
  "use strict";

  const DB_NAME = "istmo-notifications";
  const DB_VERSION = 2;
  const DATA_VERSION = 1;
  const SCHEDULE = "scheduled";
  const QUEUE = "responses";
  /* The previous layout's store, dropped on upgrade. */
  const OLD_SCHEDULE = "schedule";
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
        if (db.objectStoreNames.contains(OLD_SCHEDULE)) {
          db.deleteObjectStore(OLD_SCHEDULE);
        }
        if (!db.objectStoreNames.contains(SCHEDULE)) {
          db.createObjectStore(SCHEDULE);
        }
        if (!db.objectStoreNames.contains(QUEUE)) {
          db.createObjectStore(QUEUE, { autoIncrement: true });
        }
      };
      request.onsuccess = function () {
        const db = request.result;
        /* Never block a newer version's upgrade. */
        db.onversionchange = function () {
          db.close();
        };
        resolve(db);
      };
      request.onerror = function () {
        reject(request.error);
      };
      /*
       * `blocked` is not a failure: the open completes once the older
       * connection closes (pages close theirs on `versionchange`).
       */
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

  /* The app's windows: same-origin pages inside this worker's scope. */
  async function windows() {
    const all = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
    const scope = self.registration.scope;
    return all.filter(function (c) {
      return typeof c.url === "string" && c.url.startsWith(scope);
    });
  }

  /* The window showing `open`, else the focused one, else a visible one, else any. */
  function pick(clients, open) {
    let target = null;
    if (open) {
      try {
        target = new URL(open, self.registration.scope).href;
      } catch (_err) {
        target = null;
      }
    }
    return (
      (target &&
        clients.find(function (c) {
          return c.url === target;
        })) ||
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

  /* Tells `target` to drain, or every window when there is no target. */
  async function drain(target) {
    const message = { istmo: MESSAGE, kind: "drain" };
    if (target) {
      target.postMessage(message);
      return;
    }
    for (const client of await windows()) {
      client.postMessage(message);
    }
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
        let target = null;
        if (!background) {
          const client = pick(before, data.open);
          if (client) {
            try {
              target = (await client.focus()) || client;
            } catch (_err) {
              /* Focus is best effort; the window still drains. */
              target = client;
            }
          } else if (self.clients.openWindow) {
            /* `null` for a URL outside the origin: then every window drains. */
            target = await self.clients.openWindow(data.open || self.registration.scope);
          }
        }
        await drain(target);
      })()
    );
  });

  self.addEventListener("notificationclose", function (event) {
    const data = ours(event.notification);
    if (!data) {
      return;
    }
    event.waitUntil(
      (async function () {
        await enqueue({
          kind: "close",
          id: data.id,
          action: "",
          launched: false,
          record: data.record,
          time: Date.now(),
        });
        const shown = (await windows()).find(function (c) {
          return c.focused || c.visibilityState === "visible";
        });
        await drain(shown || null);
      })().catch(function (err) {
        console.warn("istmo-notifications: close not queued", err);
      })
    );
  });
})();
