"use strict";

const state = {
  tree: null,
  currentPath: null,
  originalContent: "",
  openDirs: new Set(),
};

const $ = (id) => document.getElementById(id);
const treeEl = $("tree");
const editor = $("editor");
const pathDisplay = $("path-display");
const commitMessage = $("commit-message");
const btnSave = $("btn-save");
const btnRename = $("btn-rename");
const btnDelete = $("btn-delete");
const btnNew = $("btn-new");
const btnRefresh = $("btn-refresh");
const statusEl = $("status");

function setStatus(msg, kind = "") {
  statusEl.textContent = msg;
  statusEl.className = kind;
}

function setBusy(busy) {
  for (const b of [btnSave, btnRename, btnDelete, btnNew, btnRefresh]) {
    b.disabled = busy ? true : b.dataset.shouldDisable === "true";
  }
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
    state.currentPath = data.path;
    state.originalContent = data.content;
    pathDisplay.value = data.path;
    editor.value = data.content;
    editor.disabled = false;
    btnSave.disabled = false;
    btnSave.dataset.shouldDisable = "false";
    btnRename.disabled = false;
    btnRename.dataset.shouldDisable = "false";
    btnDelete.disabled = false;
    btnDelete.dataset.shouldDisable = "false";
    setStatus(`Opened ${data.path}`);
    renderTree();
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
  const to = prompt("Rename to (path relative to vault root):", state.currentPath);
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
    state.currentPath = to;
    pathDisplay.value = to;
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
    state.currentPath = null;
    state.originalContent = "";
    pathDisplay.value = "";
    editor.value = "";
    editor.disabled = true;
    btnSave.disabled = true;
    btnSave.dataset.shouldDisable = "true";
    btnRename.disabled = true;
    btnRename.dataset.shouldDisable = "true";
    btnDelete.disabled = true;
    btnDelete.dataset.shouldDisable = "true";
    commitMessage.value = "";
    await loadTree();
  } catch (e) {
    setStatus(`Delete failed: ${e.message}`, "error");
  } finally {
    setBusy(false);
  }
}

btnSave.addEventListener("click", saveCurrent);
btnRename.addEventListener("click", renameCurrent);
btnDelete.addEventListener("click", deleteCurrent);
btnNew.addEventListener("click", createNewFile);
btnRefresh.addEventListener("click", loadTree);

document.addEventListener("keydown", (e) => {
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") {
    e.preventDefault();
    if (!btnSave.disabled) saveCurrent();
  }
});

window.addEventListener("beforeunload", (e) => {
  if (state.currentPath && editor.value !== state.originalContent) {
    e.preventDefault();
    e.returnValue = "";
  }
});

loadTree();
