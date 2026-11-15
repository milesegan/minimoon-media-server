"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (selector) => document.querySelector(selector);
const numbers = new Intl.NumberFormat();
const dates = new Intl.DateTimeFormat(undefined, { dateStyle: "medium" });

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
function toast(message) {
  const el = $("#toast");
  el.textContent = message;
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (el.hidden = true), 5000);
}

/** Runs a backend command, shows its error, and refreshes. */
async function run(command, args = {}) {
  pending++;
  render();
  try {
    return await invoke(command, args);
  } catch (error) {
    toast(String(error));
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

  const pill = $("#pill");
  pill.className = "pill";
  if (current.phase === "starting") {
    pill.textContent = "Starting…";
  } else if (current.phase === "failed") {
    pill.textContent = "Not running";
    pill.classList.add("failed");
  } else if (server.paused) {
    pill.textContent = "Paused";
  } else {
    pill.textContent = "Sharing";
    pill.classList.add("sharing");
  }

  $("#failure").hidden = current.phase !== "failed";
  $("#failure-text").textContent = current.error ?? "";

  const address = $("#address");
  address.hidden = !server?.address;
  address.textContent = server?.address ?? "";

  const pause = $("#pause");
  pause.disabled = !ready;
  pause.textContent = server?.paused ? "Resume Sharing" : "Pause Sharing";
  pause.classList.toggle("primary", !!server?.paused);

  for (const id of ["#add-folder", "#rescan-all", "#reindex"]) {
    $(id).disabled = !ready;
  }

  $("#data-dir").textContent = current.dataDir ? `Data: ${current.dataDir}` : "";
  $("#data-dir").title = current.dataDir;
  const downloads = server?.downloads ?? 0;
  $("#downloads").textContent =
    downloads === 0 ? "" : downloads === 1 ? "1 stream playing" : `${downloads} streams playing`;

  if (!server) return;
  document.title = `${server.name} – MiniMoon Media Server`;
  $("#server-name").textContent = server.name;
  renderSettings(server, ready);
  renderFolders(server.folders, ready);
  renderDevices(server, ready);
  dismissPairedRequests(server.devices);
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
    <div class="row-main">
      <div class="row-title"><span class="name"></span><input class="rename" type="text" spellcheck="false" hidden /></div>
      <div class="row-sub path"></div>
      <div class="folder-status"><span class="dot"></span><span class="text"></span></div>
      <div class="progress" hidden><div class="bar"></div></div>
      <details class="errors" hidden><summary></summary><ul></ul></details>
    </div>
    <div class="row-actions">
      <button class="small rescan">Rescan</button>
      <button class="small rename-button">Rename</button>
      <button class="small danger remove">Remove</button>
    </div>`;
  const row = {
    el,
    name: el.querySelector(".name"),
    input: el.querySelector(".rename"),
    path: el.querySelector(".path"),
    dot: el.querySelector(".dot"),
    text: el.querySelector(".text"),
    progress: el.querySelector(".progress"),
    bar: el.querySelector(".bar"),
    errors: el.querySelector(".errors"),
    summary: el.querySelector(".errors summary"),
    errorList: el.querySelector(".errors ul"),
    errorText: "",
    buttons: el.querySelectorAll("button"),
  };

  el.querySelector(".rescan").addEventListener("click", () => run("rescan", { id }));
  el.querySelector(".remove").addEventListener("click", () => run("remove_folder", { id }));
  el.querySelector(".rename-button").addEventListener("click", () => {
    editing.add(id);
    row.input.value = row.name.textContent;
    row.input.hidden = false;
    row.name.hidden = true;
    row.input.focus();
    row.input.select();
  });
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
  return row;
}

function updateFolderRow(row, folder, ready) {
  if (!editing.has(folder.id)) row.name.textContent = folder.name;
  row.path.textContent = folder.path;
  row.path.title = folder.path;
  for (const button of row.buttons) button.disabled = !ready;

  const parts = [];
  if (folder.shared) {
    parts.push("Shared", `${numbers.format(folder.files)} ${folder.files === 1 ? "file" : "files"}`);
  } else {
    parts.push(folder.note);
  }
  if (folder.unreadable > 0) {
    parts.push(`${numbers.format(folder.unreadable)} with unreadable metadata`);
  }
  if (folder.activity) {
    const [done, total] = folder.progress ?? [];
    parts.push(
      folder.progress
        ? `${folder.activity} ${numbers.format(done)} of ${numbers.format(total)}`
        : `${folder.activity}…`,
    );
  }
  row.text.textContent = parts.join(" · ");
  row.dot.className = "dot";
  if (folder.activity) row.dot.classList.add("busy");
  else if (folder.shared) row.dot.classList.add("ok");
  else row.dot.classList.add("warn");

  row.progress.hidden = !folder.activity;
  row.progress.classList.toggle("indeterminate", !!folder.activity && !folder.progress);
  if (folder.progress) {
    const [done, total] = folder.progress;
    row.bar.style.width = total > 0 ? `${(100 * done) / total}%` : "0";
  } else {
    row.bar.style.width = "";
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

function renderDevices(server, ready) {
  const devices = server.devices;
  $("#devices-section").hidden = !server.pairing && devices.length === 0;
  $("#no-devices").hidden = devices.length > 0;
  reconcile($("#devices"), deviceRows, devices, (device) => device.clientId, createDeviceRow, (row, device) => {
    row.name.textContent = device.clientName;
    row.sub.textContent = `${device.app} on ${device.platform} · Paired ${dates.format(new Date(device.pairedAt))}`;
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
    <div class="row-actions"><button class="small danger">Revoke</button></div>`;
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
  el.className = "banner";
  el.innerHTML = `
    <div class="pairing">
      <div class="who">
        <div class="row-title"></div>
        <div class="row-sub"></div>
      </div>
      <div class="code"></div>
      <button class="small">Dismiss</button>
    </div>`;
  const shown = Date.now();
  const expires = shown + request.expiresInSecs * 1000;
  const title = el.querySelector(".row-title");
  const sub = el.querySelector(".row-sub");
  title.textContent = `${request.clientName} wants to connect`;
  const code = request.code;
  el.querySelector(".code").textContent = `${code.slice(0, 3)} ${code.slice(3)}`;

  const tick = () => {
    const left = Math.max(0, Math.round((expires - Date.now()) / 1000));
    if (left === 0) return remove();
    const minutes = Math.floor(left / 60);
    const seconds = String(left % 60).padStart(2, "0");
    sub.textContent = `${request.app} on ${request.platform} · Enter this code on the device · ${minutes}:${seconds}`;
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
$("#rescan-all").addEventListener("click", () => run("rescan", { id: null }));
$("#reindex").addEventListener("click", () => run("reindex_everything"));

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

listen("pairing-request", (event) => showPairing(event.payload));

refresh();
setInterval(refresh, 1000);
