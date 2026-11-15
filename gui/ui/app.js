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

const FOLDER_ICON = `<svg class="row-icon" viewBox="0 0 24 24" aria-hidden="true"><path d="M3 6.5A1.5 1.5 0 0 1 4.5 5h4.38a1.5 1.5 0 0 1 1.06.44L11.5 7h8A1.5 1.5 0 0 1 21 8.5v9a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 17.5z"/></svg>`;
const MORE_ICON = `<svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="3" cy="8" r="1.4"/><circle cx="8" cy="8" r="1.4"/><circle cx="13" cy="8" r="1.4"/></svg>`;

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

  const state = $("#state");
  state.className = "state";
  let stateText = "Sharing";
  if (current.phase === "starting") {
    stateText = "Starting…";
  } else if (current.phase === "failed") {
    stateText = "Not running";
    state.classList.add("failed");
  } else if (server.paused) {
    stateText = "Paused";
  } else {
    state.classList.add("sharing");
  }
  $("#state-text").textContent = stateText;

  const downloads = server?.paused ? 0 : (server?.downloads ?? 0);
  const streams = $("#streams");
  streams.hidden = downloads === 0;
  streams.textContent = downloads === 1 ? "1 stream playing" : `${downloads} streams playing`;

  const address = $("#address");
  address.hidden = !server?.address || server.paused;
  address.textContent = (server?.address ?? "").replace(/^https?:\/\//, "");

  $("#failure").hidden = current.phase !== "failed";
  $("#failure-text").textContent = current.error ?? "";
  $("#paused-banner").hidden = !server?.paused;

  const pause = $("#pause");
  pause.disabled = !ready;
  pause.textContent = server?.paused ? "Resume Sharing" : "Pause Sharing";
  pause.classList.toggle("primary", !!server?.paused);

  for (const id of ["#add-folder", "#add-first-folder", "#rescan-all", "#reindex"]) {
    $(id).disabled = !ready;
  }

  $("#data-dir").textContent = current.dataDir ? `Settings and index: ${current.dataDir}` : "";
  $("#data-dir").title = current.dataDir;

  // Settings mean nothing until the server runs; the header and banners say why.
  $("main").hidden = !server;
  if (!server) return;
  document.title = `${server.name} – MiniMoon Media Server`;
  $("#server-name").textContent = server.name;
  renderSettings(server, ready);
  renderFolders(server.folders, ready);
  renderDevices(server, ready);
  dismissPairedRequests(server.devices);
  // Switches animate from here on, not from their initial state.
  requestAnimationFrame(() => document.body.classList.add("loaded"));
}

function renderSettings(server, ready) {
  const name = $("#name-input");
  name.disabled = !ready && document.activeElement !== name;
  if (document.activeElement !== name) {
    name.value = server.customName ?? "";
    // With no saved name, the display name is the host name.
    if (!server.customName) name.placeholder = server.name;
  }
  for (const radio of document.querySelectorAll('input[name="access"]')) {
    radio.disabled = !ready;
    if (pending === 0) radio.checked = radio.value === (server.pairing ? "pairing" : "open");
  }
  const keepAwake = $("#keep-awake");
  keepAwake.disabled = !ready;
  if (pending === 0) keepAwake.checked = server.keepAwake;
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
  el.innerHTML = `
    ${FOLDER_ICON}
    <div class="row-main">
      <div class="row-title"><span class="name"></span><input class="rename" type="text" spellcheck="false" hidden /></div>
      <div class="row-sub path"></div>
      <div class="folder-status"><span class="dot"></span><span class="text"></span></div>
      <div class="activity" hidden><div class="progress"><div class="bar"></div></div><span class="label"></span></div>
      <details class="errors" hidden><summary></summary><ul></ul></details>
    </div>
    <div class="row-actions">
      <button class="icon more" title="Folder actions" aria-label="Folder actions" aria-haspopup="menu">${MORE_ICON}</button>
    </div>`;
  const row = {
    el,
    icon: el.querySelector(".row-icon"),
    name: el.querySelector(".name"),
    input: el.querySelector(".rename"),
    path: el.querySelector(".path"),
    dot: el.querySelector(".dot"),
    text: el.querySelector(".text"),
    activity: el.querySelector(".activity"),
    progress: el.querySelector(".progress"),
    bar: el.querySelector(".bar"),
    activityLabel: el.querySelector(".activity .label"),
    errors: el.querySelector(".errors"),
    summary: el.querySelector(".errors summary"),
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

function updateFolderRow(row, folder, ready) {
  if (!editing.has(folder.id)) row.name.textContent = folder.name;
  // The marks keep the path left to right inside the right-to-left box that trims its start.
  row.path.textContent = `‎${folder.path}‎`;
  row.path.title = folder.path;
  row.name.title = ready ? "Double-click to rename" : "";
  row.more.disabled = !ready;

  const parts = [];
  if (folder.shared) {
    parts.push("Shared", `${numbers.format(folder.files)} ${folder.files === 1 ? "file" : "files"}`);
  } else {
    parts.push(folder.note);
  }
  if (folder.unreadable > 0) {
    parts.push(`${numbers.format(folder.unreadable)} with unreadable metadata`);
  }
  row.text.textContent = parts.join(" · ");
  const offline = !folder.shared && folder.note?.startsWith("Offline");
  row.dot.className = `dot ${folder.shared ? "ok" : folder.activity ? "busy" : "warn"}`;
  row.icon.classList.toggle("offline", offline);

  row.activity.hidden = !folder.activity;
  row.progress.classList.toggle("indeterminate", !!folder.activity && !folder.progress);
  if (folder.activity && folder.progress) {
    const [done, total] = folder.progress;
    row.bar.style.width = total > 0 ? `${(100 * done) / total}%` : "0";
    row.activityLabel.textContent = `${folder.activity} · ${numbers.format(done)} of ${numbers.format(total)}`;
  } else {
    row.bar.style.width = "";
    row.activityLabel.textContent = folder.activity ? `${folder.activity}…` : "";
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
  $("#devices-section").hidden = !server.pairing && devices.length === 0;
  $("#no-devices").hidden = devices.length > 0;
  $("#devices").hidden = devices.length === 0;
  reconcile($("#devices"), deviceRows, devices, (device) => device.clientId, createDeviceRow, (row, device) => {
    row.name.textContent = device.clientName;
    row.sub.textContent = `${device.app} on ${device.platform} · ${pairedText(device.pairedAt)}`;
    row.sub.title = `Paired ${dates.format(new Date(device.pairedAt))}`;
    row.revoke.disabled = !ready;
  });
}

function createDeviceRow(clientId) {
  const el = document.createElement("li");
  el.innerHTML = `
    <div class="row-main">
      <div class="row-title"></div>
      <div class="row-sub"></div>
    </div>
    <div class="row-actions"><button class="small">Revoke…</button></div>`;
  const row = {
    el,
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
  el.className = "banner pairing";
  el.innerHTML = `
    <div class="pairing-body">
      <div class="who">
        <strong class="title"></strong>
        <span class="row-sub"></span>
      </div>
      <div class="code" title="Enter this code on the device"></div>
      <button class="small">Dismiss</button>
    </div>
    <div class="countdown"></div>`;
  const shown = Date.now();
  const lifetime = request.expiresInSecs * 1000;
  const expires = shown + lifetime;
  const sub = el.querySelector(".row-sub");
  const countdown = el.querySelector(".countdown");
  el.querySelector(".title").textContent = `${request.clientName} wants to connect`;
  const code = request.code;
  el.querySelector(".code").textContent = `${code.slice(0, 3)} ${code.slice(3)}`;

  const tick = () => {
    const left = Math.max(0, Math.round((expires - Date.now()) / 1000));
    if (left === 0) return remove();
    const minutes = Math.floor(left / 60);
    const seconds = String(left % 60).padStart(2, "0");
    sub.textContent = `Enter this code in ${request.app} on the ${request.platform} device · ${minutes}:${seconds} left`;
    countdown.style.transform = `scaleX(${(expires - Date.now() - 1000) / lifetime})`;
  };
  const timer = setInterval(tick, 1000);
  const remove = () => {
    clearInterval(timer);
    el.remove();
    pairings.delete(request.pairingId);
  };
  el.querySelector("button").addEventListener("click", remove);
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

for (const radio of document.querySelectorAll('input[name="access"]')) {
  radio.addEventListener("change", () => run("set_pairing", { on: radio.value === "pairing" }));
}
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
