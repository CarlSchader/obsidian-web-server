"use strict";

const state = {
  tree: null,
  currentPath: null,
  originalContent: "",
  openDirs: new Set(),
};

const $ = (id) => document.getElementById(id);
const app = $("app");
const treeEl = $("tree");
const editor = $("editor");
const currentPathEl = $("current-path");
const commitMessage = $("commit-message");
const btnSave = $("btn-save");
const btnSaveTop = $("btn-save-top");
const btnRename = $("btn-rename");
const btnDelete = $("btn-delete");
const btnNew = $("btn-new");
const btnRefresh = $("btn-refresh");
const btnToggleSidebar = $("btn-toggle-sidebar");
const btnCloseSidebar = $("btn-close-sidebar");
const sidebarScrim = $("sidebar-scrim");
const statusEl = $("status");

const fileButtons = [btnSave, btnSaveTop, btnRename, btnDelete];

function setStatus(msg, kind = "") {
  statusEl.textContent = msg;
  statusEl.className = kind;
}

function setBusy(busy) {
  for (const b of [
    btnSave,
    btnSaveTop,
    btnRename,
    btnDelete,
    btnNew,
    btnRefresh,
  ]) {
    b.disabled = busy ? true : b.dataset.shouldDisable === "true";
  }
}

function setFileLoaded(loaded) {
  const shouldDisable = loaded ? "false" : "true";
  for (const b of fileButtons) {
    b.dataset.shouldDisable = shouldDisable;
    b.disabled = !loaded;
  }
  editor.disabled = !loaded;
}

function setCurrentPath(path) {
  state.currentPath = path;
  if (path) {
    currentPathEl.textContent = path;
    currentPathEl.classList.add("has-file");
    currentPathEl.title = path;
  } else {
    currentPathEl.textContent = "No file";
    currentPathEl.classList.remove("has-file");
    currentPathEl.title = "";
  }
}

function isMobileViewport() {
  return window.matchMedia("(max-width: 767px)").matches;
}

function setSidebarOpen(open) {
  app.dataset.sidebarOpen = open ? "true" : "false";
}

function toggleSidebar() {
  setSidebarOpen(app.dataset.sidebarOpen !== "true");
}

async function api(method, url, body) {
  const opts = { method, headers: {} };
  if (body !== undefined) {
    opts.headers["Content-Type"] = "application/json";
    opts.body = JSON.stringify(body);
  }
  const resp = await fetch(url, opts);
  const text = await resp.text();
  let parsed = null;
  try {
    parsed = text ? JSON.parse(text) : null;
  } catch (_) {
    /* keep raw text */
  }
  if (!resp.ok) {
    const msg =
      (parsed && parsed.error) || text || `${resp.status} ${resp.statusText}`;
    throw new Error(msg);
  }
  return parsed;
}

async function loadTree() {
  try {
    const tree = await api("GET", "/api/tree");
    state.tree = tree;
    renderTree();
    setStatus("Tree loaded.");
  } catch (e) {
    setStatus(`Failed to load tree: ${e.message}`, "error");
  }
}

function renderTree() {
  treeEl.innerHTML = "";
  if (!state.tree || !state.tree.children) return;
  const ul = document.createElement("ul");
  for (const child of state.tree.children) {
    ul.appendChild(renderNode(child));
  }
  treeEl.appendChild(ul);
}

function renderNode(node) {
  const li = document.createElement("li");
  const span = document.createElement("span");
  span.className = `node ${node.kind}`;
  span.textContent = node.name;
  span.dataset.path = node.path;

  if (node.kind === "file" && state.currentPath === node.path) {
    span.classList.add("active");
  }

  if (node.kind === "dir") {
    const open = state.openDirs.has(node.path);
    if (open) span.classList.add("open");
    span.addEventListener("click", () => {
      if (state.openDirs.has(node.path)) {
        state.openDirs.delete(node.path);
      } else {
        state.openDirs.add(node.path);
      }
      renderTree();
    });
  } else {
    span.addEventListener("click", () => openFile(node.path));
  }

  li.appendChild(span);

  if (node.kind === "dir" && state.openDirs.has(node.path) && node.children) {
    const ul = document.createElement("ul");
    for (const c of node.children) ul.appendChild(renderNode(c));
    li.appendChild(ul);
  }

  return li;
}

async function openFile(path) {
  try {
    const data = await api("GET", `/api/file?path=${encodeURIComponent(path)}`);
    setCurrentPath(data.path);
    state.originalContent = data.content;
    editor.value = data.content;
    setFileLoaded(true);
    setStatus(`Opened ${data.path}`);
    renderTree();
    if (isMobileViewport()) setSidebarOpen(false);
  } catch (e) {
    setStatus(`Failed to open ${path}: ${e.message}`, "error");
  }
}

async function saveCurrent() {
  if (!state.currentPath) return;
  setBusy(true);
  try {
    const body = {
      path: state.currentPath,
      content: editor.value,
    };
    if (commitMessage.value.trim()) body.message = commitMessage.value.trim();
    const result = await api("PUT", "/api/file", body);
    if (result.committed) {
      setStatus(`Committed ${result.sha}: ${state.currentPath}`, "ok");
      state.originalContent = editor.value;
    } else {
      setStatus("Saved (no changes to commit).", "ok");
    }
    commitMessage.value = "";
  } catch (e) {
    setStatus(`Save failed: ${e.message}`, "error");
  } finally {
    setBusy(false);
  }
}

async function createNewFile() {
  const path = prompt("New file path (relative to vault root):", "");
  if (!path) return;
  setBusy(true);
  try {
    const body = { path, content: "" };
    if (commitMessage.value.trim()) body.message = commitMessage.value.trim();
    const result = await api("POST", "/api/file/create", body);
    setStatus(
      result.committed
        ? `Created ${path} in ${result.sha}.`
        : `Created ${path} (nothing to commit).`,
      "ok",
    );
    commitMessage.value = "";
    await loadTree();
    await openFile(path);
  } catch (e) {
    setStatus(`Create failed: ${e.message}`, "error");
  } finally {
    setBusy(false);
  }
}

async function renameCurrent() {
  if (!state.currentPath) return;
  const to = prompt(
    "Rename to (path relative to vault root):",
    state.currentPath,
  );
  if (!to || to === state.currentPath) return;
  setBusy(true);
  try {
    const body = { from: state.currentPath, to };
    if (commitMessage.value.trim()) body.message = commitMessage.value.trim();
    const result = await api("POST", "/api/file/rename", body);
    setStatus(
      result.committed
        ? `Renamed in ${result.sha}: ${state.currentPath} -> ${to}`
        : `Renamed (nothing to commit).`,
      "ok",
    );
    setCurrentPath(to);
    commitMessage.value = "";
    await loadTree();
  } catch (e) {
    setStatus(`Rename failed: ${e.message}`, "error");
  } finally {
    setBusy(false);
  }
}

async function deleteCurrent() {
  if (!state.currentPath) return;
  if (!confirm(`Delete ${state.currentPath}? This will be committed.`)) return;
  setBusy(true);
  try {
    const body = { path: state.currentPath };
    if (commitMessage.value.trim()) body.message = commitMessage.value.trim();
    const result = await api("DELETE", "/api/file", body);
    setStatus(
      result.committed
        ? `Deleted ${state.currentPath} in ${result.sha}.`
        : `Delete: nothing to commit.`,
      "ok",
    );
    setCurrentPath(null);
    state.originalContent = "";
    editor.value = "";
    setFileLoaded(false);
    commitMessage.value = "";
    await loadTree();
  } catch (e) {
    setStatus(`Delete failed: ${e.message}`, "error");
  } finally {
    setBusy(false);
  }
}

btnSave.addEventListener("click", saveCurrent);
btnSaveTop.addEventListener("click", saveCurrent);
btnRename.addEventListener("click", renameCurrent);
btnDelete.addEventListener("click", deleteCurrent);
btnNew.addEventListener("click", createNewFile);
btnRefresh.addEventListener("click", loadTree);

btnToggleSidebar.addEventListener("click", toggleSidebar);
btnCloseSidebar.addEventListener("click", () => setSidebarOpen(false));
sidebarScrim.addEventListener("click", () => setSidebarOpen(false));

document.addEventListener("keydown", (e) => {
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") {
    e.preventDefault();
    if (!btnSave.disabled) saveCurrent();
  }
  if (e.key === "Escape" && app.dataset.sidebarOpen === "true") {
    setSidebarOpen(false);
  }
});

window.addEventListener("beforeunload", (e) => {
  if (state.currentPath && editor.value !== state.originalContent) {
    e.preventDefault();
    e.returnValue = "";
  }
});

setFileLoaded(false);
setCurrentPath(null);
loadTree();
