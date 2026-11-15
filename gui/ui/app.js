"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { Menu } = window.__TAURI__.menu;
const { LogicalPosition } = window.__TAURI__.dpi;

const $ = (selector) => document.querySelector(selector);
const numbers = new Intl.NumberFormat();
const dates = new Intl.DateTimeFormat(undefined, { dateStyle: "medium" });
const relative = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
const platform = navigator.userAgent;
const openFolderLabel = /Mac/.test(platform)
  ? "Open in Finder"
  : /Windows/.test(platform)
    ? "Open in File Explorer"
    : "Open Folder";

const svg = (body, viewBox = "0 0 24 24", cls = "") =>
  `<svg${cls ? ` class="${cls}"` : ""} viewBox="${viewBox}" aria-hidden="true">${body}</svg>`;
const FOLDER_ICON = svg('<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>');
const MORE_ICON = svg('<circle cx="3" cy="8" r="1.4"/><circle cx="8" cy="8" r="1.4"/><circle cx="13" cy="8" r="1.4"/>', "0 0 16 16");
const CLOSE_ICON = svg('<path d="M6 6l12 12M18 6L6 18"/>', "0 0 24 24", "stroke");
const CHEVRON_ICON = svg('<path d="M9 6l6 6-6 6"/>');
const DEVICE_ICONS = {
  tv: svg('<rect x="3" y="5" width="18" height="12" rx="2"/><path d="M8 21h8"/>'),
  computer: svg('<rect x="4" y="5" width="16" height="11" rx="1.5"/><path d="M2 19h20"/>'),
  tablet: svg('<rect x="5" y="3" width="14" height="18" rx="2.5"/><path d="M11 18h2"/>'),
  phone: svg('<rect x="7" y="3" width="10" height="18" rx="2.5"/><path d="M11 18h2"/>'),
};

/** Which picture suits a device, from the free-form platform its app reports. */
function deviceKind(platform) {
  if (/tv|roku|tizen|webos|fire ?stick/i.test(platform)) return "tv";
  if (/ipad|tablet/i.test(platform)) return "tablet";
  if (/mac|windows|linux|desktop|chrome ?os|web/i.test(platform)) return "computer";
  return "phone";
}

const plural = (count, one, many = `${one}s`) => `${numbers.format(count)} ${count === 1 ? one : many}`;

/** The last status from the backend. */
let current = null;
/** Commands in flight; controls stay disabled and aren't overwritten meanwhile. */
let pending = 0;
/** Folder ids whose name is being edited. */
const editing = new Set();
/** Rows by folder id or device client id, kept across refreshes so focus and state survive. */
const folderRows = new Map();
const deviceRows = new Map();
/** Pairing codes on screen, by pairing id. */
const pairings = new Map();

// MARK: Commands

let toastTimer = null;
function toast(message, { error = false } = {}) {
  const el = $("#toast");
  el.textContent = message;
  el.classList.toggle("error", error);
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (el.hidden = true), error ? 6000 : 2500);
}

/** What `run` returns when the command failed. */
const FAILED = Symbol("failed");

/** Runs a backend command, shows its error, and refreshes. */
async function run(command, args = {}) {
  pending++;
  render();
  try {
    return await invoke(command, args);
  } catch (error) {
    toast(String(error), { error: true });
    return FAILED;
  } finally {
    pending--;
    finished++;
    await refresh();
  }
}

/** Commands finished so far: a status fetched before one finished is stale. */
let finished = 0;
let refreshing = false;
async function refresh() {
  // The poll already in flight re-fetches if a command finishes meanwhile.
  if (refreshing) return;
  refreshing = true;
  try {
    let status;
    let started;
    do {
      started = finished;
      try {
        status = await invoke("status");
      } catch (error) {
        status = { phase: "failed", error: String(error), dataDir: "", server: null };
      }
    } while (started !== finished);
    current = status;
  } finally {
    refreshing = false;
  }
  render();
}

// MARK: Rendering

function render() {
  if (!current) return;
  const server = current.server;
  const ready = current.phase === "running" && pending === 0;

  renderBand(server, ready);

  $("#failure").hidden = current.phase !== "failed";
  $("#failure-text").textContent = current.error ?? "";
  $("#paused-banner").hidden = !server?.paused;

  $("#maintenance").hidden = !server;
  for (const id of ["#add-folder", "#add-first-folder", "#rescan-all", "#reindex"]) {
    $(id).disabled = !ready;
  }
  $("#data-dir").title = current.dataDir ? `Settings and index: ${current.dataDir}` : "";
  $("#data-dir").hidden = !current.dataDir;

  // Settings mean nothing until the server runs; the band and banners say why.
  $("main").hidden = !server;
  if (!server) return;
  document.title = `${server.name} – MiniMoon Media Server`;
  $("#server-name").textContent = server.name;
  $("#server-name").title = server.name;
  renderSettings(server, ready);
  renderFolders(server.folders, ready);
  renderDevices(server, ready);
  dismissPairedRequests(server.devices);
  // Switches animate from here on, not from their initial state.
  requestAnimationFrame(() => document.body.classList.add("loaded"));
}

function renderBand(server, ready) {
  let state = "sharing";
  let stateText = "Sharing on this network";
  if (current.phase === "starting") {
    state = "starting";
    stateText = "Starting…";
  } else if (current.phase === "failed") {
    state = "failed";
    stateText = "Not running";
  } else if (server.paused) {
    state = "paused";
    stateText = "Paused";
  }
  $("#state").className = `state ${state}`;
  $("#state-text").textContent = stateText;

  const pause = $("#pause");
  pause.hidden = !server;
  pause.disabled = !ready;
  pause.textContent = server?.paused ? "Resume Sharing" : "Pause Sharing";
  pause.classList.toggle("resume", !!server?.paused);

  $("#figures").hidden = !server;
  if (!server) return;

  // Pausing closes the listener, so there's no address until sharing resumes.
  const address = server.address ?? "";
  $("#address").hidden = !address;
  $("#address-text").textContent = address.replace(/^https?:\/\//, "");
  $("#no-address").hidden = !!address;
  $("#no-address").textContent = server.paused ? "Off while paused" : "Not available";

  const downloads = server.paused ? 0 : server.downloads;
  const streams = $("#streams");
  streams.textContent = downloads === 0 ? "Nothing" : plural(downloads, "stream");
  streams.classList.toggle("none", downloads === 0);

  const shared = server.folders.filter((folder) => folder.shared);
  const library = $("#library");
  library.textContent =
    shared.length === 0 ? "Nothing shared" : plural(shared.reduce((sum, folder) => sum + folder.files, 0), "file");
  library.classList.toggle("none", shared.length === 0);
}

function renderSettings(server, ready) {
  const name = $("#name-input");
  name.disabled = !ready && document.activeElement !== name;
  if (document.activeElement !== name) {
    name.value = server.customName ?? "";
    // With no saved name, the display name is the host name.
    if (!server.customName) name.placeholder = server.name;
  }
  for (const [id, on] of [
    ["#pairing", server.pairing],
    ["#keep-awake", server.keepAwake],
  ]) {
    const toggle = $(id);
    toggle.disabled = !ready;
    if (pending === 0) toggle.checked = on;
  }
}

/** Puts `rows` in `list` in order, creating and removing elements as needed. */
function reconcile(list, cache, items, key, create, update) {
  const seen = new Set();
  items.forEach((item, index) => {
    const id = key(item);
    seen.add(id);
    let row = cache.get(id);
    if (!row) {
      row = create(id);
      cache.set(id, row);
    }
    if (list.children[index] !== row.el) list.insertBefore(row.el, list.children[index] ?? null);
    update(row, item);
  });
  for (const [id, row] of cache) {
    if (!seen.has(id)) {
      row.el.remove();
      cache.delete(id);
    }
  }
}

function renderFolders(folders, ready) {
  $("#no-folders").hidden = folders.length > 0;
  $("#folders").hidden = folders.length === 0;
  $("#add-folder").hidden = folders.length === 0;
  for (const id of editing) {
    if (!folders.some((folder) => folder.id === id)) editing.delete(id);
  }
  reconcile($("#folders"), folderRows, folders, (folder) => folder.id, createFolderRow, (row, folder) =>
    updateFolderRow(row, folder, ready),
  );
}

function createFolderRow(id) {
  const el = document.createElement("li");
  el.className = "folder";
  el.innerHTML = `
    <div class="row">
      <span class="chip">${FOLDER_ICON}</span>
      <div class="row-main">
        <div class="row-title"><span class="name"></span><input class="rename" type="text" spellcheck="false" aria-label="Folder name" hidden /></div>
        <div class="row-sub path"></div>
      </div>
      <div class="status"><div></div><div></div></div>
      <button class="icon more" title="Folder actions" aria-label="Folder actions" aria-haspopup="menu">${MORE_ICON}</button>
    </div>
    <div class="progress" role="progressbar" hidden><div class="bar"></div></div>
    <details class="errors" hidden><summary>${CHEVRON_ICON}<span></span></summary><ul></ul></details>`;
  const row = {
    el,
    name: el.querySelector(".name"),
    input: el.querySelector(".rename"),
    path: el.querySelector(".path"),
    status: el.querySelector(".status"),
    main: el.querySelector(".status > :first-child"),
    sub: el.querySelector(".status > :last-child"),
    progress: el.querySelector(".progress"),
    bar: el.querySelector(".progress .bar"),
    errors: el.querySelector(".errors"),
    summary: el.querySelector(".errors summary span"),
    errorList: el.querySelector(".errors ul"),
    errorText: "",
    more: el.querySelector(".more"),
  };

  const startRename = () => {
    if (editing.has(id) || pending > 0) return;
    editing.add(id);
    row.input.value = row.name.textContent;
    row.input.hidden = false;
    row.name.hidden = true;
    row.input.focus();
    row.input.select();
  };
  const finish = (save) => {
    if (!editing.has(id)) return;
    editing.delete(id);
    row.input.hidden = true;
    row.name.hidden = false;
    const name = row.input.value.trim();
    if (save && name && name !== row.name.textContent) {
      row.name.textContent = name;
      run("rename_folder", { id, name });
    }
  };
  row.input.addEventListener("keydown", (event) => {
    if (event.key === "Enter") finish(true);
    if (event.key === "Escape") finish(false);
  });
  row.input.addEventListener("blur", () => finish(true));
  row.name.addEventListener("dblclick", startRename);

  const showMenu = async (at) => {
    if (pending > 0 || current?.phase !== "running") return;
    const menu = await Menu.new({
      items: [
        { text: "Rename", action: startRename },
        { text: "Rescan", action: () => run("rescan", { id }) },
        { text: openFolderLabel, action: () => run("open_folder", { id }) },
        { item: "Separator" },
        { text: "Stop Sharing…", action: () => run("remove_folder", { id }) },
      ],
    });
    row.more.setAttribute("aria-expanded", "true");
    try {
      await menu.popup(at);
    } finally {
      row.more.removeAttribute("aria-expanded");
    }
  };
  row.more.addEventListener("click", () => {
    const rect = row.more.getBoundingClientRect();
    showMenu(new LogicalPosition(rect.left, rect.bottom + 4));
  });
  el.addEventListener("contextmenu", (event) => {
    if (event.target.closest("input, .row-sub, details ul")) return;
    event.preventDefault();
    showMenu(new LogicalPosition(event.clientX, event.clientY));
  });
  return row;
}

/** Sets one line of a folder's status column: its text and how it reads. */
function statusLine(el, text, tone = "") {
  el.className = tone;
  el.textContent = text;
  el.hidden = !text;
}

function updateFolderRow(row, folder, ready) {
  if (!editing.has(folder.id)) row.name.textContent = folder.name;
  // The marks keep the path left to right inside the right-to-left box that trims its start.
  row.path.textContent = `‎${folder.path}‎`;
  row.path.title = folder.path;
  row.name.title = ready ? "Double-click to rename" : "";
  row.more.disabled = !ready;

  // Healthy folders show only their size; anything else replaces or follows it.
  const files = plural(folder.files, "file");
  const offline = !folder.shared && !!folder.note?.startsWith("Offline");
  row.el.classList.toggle("offline", offline);
  if (offline) {
    row.main.className = "";
    row.main.replaceChildren(Object.assign(document.createElement("span"), { className: "pill", textContent: "Offline" }));
    row.main.hidden = false;
    statusLine(row.sub, folder.note.replace(/^Offline:?\s*/, ""));
  } else if (folder.activity) {
    statusLine(row.main, folder.activity, "busy");
    if (folder.progress) {
      const [done, total] = folder.progress;
      statusLine(row.sub, `${numbers.format(done)} of ${numbers.format(total)}`);
    } else {
      statusLine(row.sub, folder.files > 0 ? files : "");
    }
  } else if (!folder.shared) {
    // "Not shared: why" is a problem, split over the two lines; waiting to be indexed isn't.
    const [, why] = folder.note?.match(/^Not shared: (.*)$/s) ?? [];
    statusLine(row.main, why ? "Not shared" : (folder.note ?? "Not shared"), why ? "warn" : "quiet");
    statusLine(row.sub, why ?? "");
  } else {
    statusLine(row.main, files);
    statusLine(
      row.sub,
      folder.unreadable > 0 ? `${numbers.format(folder.unreadable)} with unreadable metadata` : "",
      "warn",
    );
  }
  row.status.title = [row.main.textContent, row.sub.textContent].filter(Boolean).join("\n");

  row.progress.hidden = !folder.activity || offline;
  row.progress.classList.toggle("indeterminate", !folder.progress);
  row.progress.setAttribute("aria-label", folder.activity ?? "");
  if (folder.activity && folder.progress) {
    const [done, total] = folder.progress;
    row.bar.style.width = total > 0 ? `${(100 * done) / total}%` : "0";
    row.progress.setAttribute("aria-valuemax", total);
    row.progress.setAttribute("aria-valuenow", done);
  } else {
    row.bar.style.width = "";
    row.progress.removeAttribute("aria-valuemax");
    row.progress.removeAttribute("aria-valuenow");
  }

  // Rebuilt only when it changes, so an open list stays open.
  const errorText = folder.scanErrors.join("\n");
  if (errorText !== row.errorText) {
    row.errorText = errorText;
    row.errors.hidden = folder.scanErrors.length === 0;
    const count = folder.scanErrors.length;
    row.summary.textContent = `${count} ${count === 1 ? "item" : "items"} couldn't be read and aren't shared`;
    row.errorList.replaceChildren(
      ...folder.scanErrors.map((error) => {
        const li = document.createElement("li");
        li.textContent = error;
        return li;
      }),
    );
  }
}

function pairedText(pairedAt) {
  const days = Math.floor((Date.now() - pairedAt) / 86_400_000);
  return days < 30 ? `Paired ${relative.format(-days, "day")}` : `Paired ${dates.format(new Date(pairedAt))}`;
}

function renderDevices(server, ready) {
  const devices = server.devices;
  $("#no-devices").hidden = !server.pairing || devices.length > 0;
  $("#devices").hidden = devices.length === 0;
  reconcile($("#devices"), deviceRows, devices, (device) => device.clientId, createDeviceRow, (row, device) => {
    const kind = deviceKind(device.platform);
    if (row.kind !== kind) {
      row.kind = kind;
      row.chip.innerHTML = DEVICE_ICONS[kind];
    }
    row.name.textContent = device.clientName;
    row.sub.textContent = `${device.app} on ${device.platform} · ${pairedText(device.pairedAt)}`;
    row.sub.title = `Paired ${dates.format(new Date(device.pairedAt))}`;
    row.revoke.disabled = !ready;
    row.revoke.setAttribute("aria-label", `Revoke ${device.clientName}…`);
  });
}

function createDeviceRow(clientId) {
  const el = document.createElement("li");
  el.innerHTML = `
    <div class="row">
      <span class="chip neutral"></span>
      <div class="row-main">
        <div class="row-title"></div>
        <div class="row-sub"></div>
      </div>
      <button class="text-button">Revoke…</button>
    </div>`;
  const row = {
    el,
    kind: null,
    chip: el.querySelector(".chip"),
    name: el.querySelector(".row-title"),
    sub: el.querySelector(".row-sub"),
    revoke: el.querySelector("button"),
  };
  row.revoke.addEventListener("click", () => run("revoke_device", { clientId }));
  return row;
}

// MARK: Pairing

function showPairing(request) {
  if (pairings.has(request.pairingId)) return;
  const el = document.createElement("div");
  el.className = "pairing";
  el.setAttribute("role", "group");
  el.innerHTML = `
    <div class="who">
      <strong class="title"></strong>
      <span class="instruction"></span>
      <div class="time" aria-live="off">
        <div class="track" aria-hidden="true"><div class="bar"></div></div>
        <span class="left"></span>
      </div>
    </div>
    <div class="code-and-close">
      <div class="code" role="img" title="Enter this code on the device"></div>
      <button class="icon dismiss" title="Dismiss" aria-label="Dismiss">${CLOSE_ICON}</button>
    </div>`;
  const shown = Date.now();
  const lifetime = request.expiresInSecs * 1000;
  const expires = shown + lifetime;
  const left = el.querySelector(".left");
  const bar = el.querySelector(".bar");
  const title = `${request.clientName} wants to connect`;
  el.querySelector(".title").textContent = title;
  el.setAttribute("aria-label", title);
  el.querySelector(".instruction").textContent = `Enter this code in ${request.app} on the ${request.platform} device.`;
  const code = request.code;
  const digits = el.querySelector(".code");
  digits.setAttribute("aria-label", `Pairing code ${code.slice(0, 3)} ${code.slice(3)}`);
  [...code].forEach((digit, index) => {
    if (index === 3) digits.append(Object.assign(document.createElement("span"), { className: "gap" }));
    digits.append(Object.assign(document.createElement("span"), { className: "digit", textContent: digit }));
  });

  const tick = () => {
    const secondsLeft = Math.max(0, Math.round((expires - Date.now()) / 1000));
    if (secondsLeft === 0) return remove();
    const minutes = Math.floor(secondsLeft / 60);
    const seconds = String(secondsLeft % 60).padStart(2, "0");
    left.textContent = `${minutes}:${seconds} left`;
    bar.style.transform = `scaleX(${Math.max(0, (expires - Date.now() - 1000) / lifetime)})`;
  };
  const timer = setInterval(tick, 1000);
  const remove = () => {
    clearInterval(timer);
    el.remove();
    pairings.delete(request.pairingId);
  };
  el.querySelector(".dismiss").addEventListener("click", remove);
  pairings.set(request.pairingId, { clientName: request.clientName, shown, remove });
  tick();
  $("#pairings").append(el);
}

/** Takes down a code once its device shows up as paired. */
function dismissPairedRequests(devices) {
  for (const pairing of pairings.values()) {
    const paired = devices.some(
      (device) => device.clientName === pairing.clientName && device.pairedAt >= pairing.shown - 5000,
    );
    if (paired) pairing.remove();
  }
}

// MARK: Wiring

$("#pause").addEventListener("click", () => run("set_paused", { paused: !current?.server?.paused }));
$("#add-folder").addEventListener("click", () => run("add_folder"));
$("#add-first-folder").addEventListener("click", () => run("add_folder"));
$("#rescan-all").addEventListener("click", () => run("rescan", { id: null }));
$("#reindex").addEventListener("click", async () => {
  if ((await run("reindex_everything")) !== FAILED) toast("Reindexing in the background");
});
// Not gated on the server: after a failed start, the data folder is where to look.
$("#data-dir").addEventListener("click", async () => {
  try {
    await invoke("show_data_folder");
  } catch (error) {
    toast(String(error), { error: true });
  }
});
$("#address").addEventListener("click", async () => {
  const address = current?.server?.address;
  if (!address) return;
  try {
    await navigator.clipboard.writeText(address);
    toast(`Copied ${address}`);
  } catch (error) {
    toast(`Couldn't copy the address: ${error}`, { error: true });
  }
});

const nameInput = $("#name-input");
nameInput.addEventListener("keydown", (event) => {
  if (event.key === "Enter") nameInput.blur();
  if (event.key === "Escape") {
    nameInput.value = current?.server?.customName ?? "";
    nameInput.blur();
  }
});
nameInput.addEventListener("change", () => run("set_name", { name: nameInput.value }));

$("#pairing").addEventListener("change", (event) => run("set_pairing", { on: event.target.checked }));
$("#keep-awake").addEventListener("change", (event) => run("set_keep_awake", { on: event.target.checked }));

// The webview's own menu (Reload, Inspect) means nothing here.
document.addEventListener("contextmenu", (event) => {
  if (!event.defaultPrevented && !event.target.closest("input, .selectable, .row-sub, details ul")) {
    event.preventDefault();
  }
});

listen("pairing-request", (event) => showPairing(event.payload));

refresh();
setInterval(refresh, 1000);
