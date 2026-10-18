// PDF Kernel UI. Every action is a Tauri command implemented in src-tauri/src/lib.rs.

const $ = (id) => document.getElementById(id);
const SYNC_EVERY_MS = 30_000;

const state = {
  info: null,          // app_info
  accounts: [],
  active: "local",
  docs: [],
  current: null,       // DocEntry
  chat: null,          // ChatLog for current
  busy: false,
};

async function call(cmd, args = {}) {
  const invoke = window.__TAURI__?.core?.invoke;
  if (!invoke) throw new Error("Tauri runtime not found");
  return invoke(cmd, args);
}

function toast(message, ms = 4000) {
  const t = $("toast");
  t.textContent = message;
  t.hidden = false;
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => (t.hidden = true), ms);
}

function el(tag, props = {}, ...children) {
  const node = Object.assign(document.createElement(tag), props);
  node.append(...children.filter((c) => c != null));
  return node;
}

const activeAccount = () => state.accounts.find((a) => a.id === state.active);

// ---------- accounts ----------

function avatarFor(account) {
  const span = el("span", { className: "avatar" });
  if (account?.picture) {
    span.append(el("img", { src: account.picture, alt: "", referrerPolicy: "no-referrer" }));
  } else {
    const initial = (account?.name || account?.email || "").trim()[0] || "?";
    span.textContent = account?.kind === "local" ? "⌂" : initial.toUpperCase();
  }
  return span;
}

function renderAccount() {
  const a = activeAccount();
  $("account-avatar").replaceWith(Object.assign(avatarFor(a), { id: "account-avatar" }));
  $("account-label").textContent = a?.kind === "local" ? "This device" : a?.email || a?.name || "";
  $("empty-hint").textContent =
    a?.kind === "local"
      ? "Files are saved on this device only. Sign in with Google to sync across devices."
      : "Files and chats are saved to your Google Drive, in the “PDF Kernel” folder.";
}

function renderAccountMenu() {
  const menu = $("account-menu");
  const items = state.accounts.map((a) => {
    const label = a.kind === "local" ? "This device" : a.name || a.email;
    const sub = a.kind === "local" ? "Not synced" : a.email;
    const b = el(
      "button",
      { role: "menuitemradio", ariaChecked: String(a.id === state.active) },
      avatarFor(a),
      el("span", { className: "who" }, el("span", { textContent: label }), el("span", { className: "muted", textContent: sub })),
      a.id === state.active ? el("span", { className: "check-mark", textContent: "✓" }) : null,
    );
    b.onclick = () => switchAccount(a.id);
    return b;
  });

  const add = el("button", { role: "menuitem", textContent: "+ Add Google account" });
  add.disabled = !state.info?.google_enabled;
  if (add.disabled) add.title = "Google sign-in is not set up in this build";
  add.onclick = addAccount;

  const current = activeAccount();
  const signOut = current?.kind === "google" ? el("button", { role: "menuitem", className: "danger-text", textContent: `Sign out of ${current.email || "this account"}` }) : null;
  if (signOut) signOut.onclick = () => removeAccount(current);

  menu.replaceChildren(...items, el("hr"), add, signOut);
}

function toggleMenu(open = $("account-menu").hidden) {
  if (open) renderAccountMenu();
  $("account-menu").hidden = !open;
  $("account-btn").setAttribute("aria-expanded", String(open));
}

function applyAccounts(view) {
  const changed = view.active !== state.active;
  state.accounts = view.accounts;
  state.active = view.active;
  renderAccount();
  if (changed) {
    selectDoc(null);
    state.docs = [];
    renderDocs();
    loadDocs();
  }
}

async function switchAccount(id) {
  toggleMenu(false);
  if (id === state.active) return;
  try {
    applyAccounts(await call("account_switch", { id }));
  } catch (e) {
    toast(String(e));
  }
}

async function addAccount() {
  toggleMenu(false);
  $("signin").showModal();
  try {
    applyAccounts(await call("account_add"));
    toast(`Signed in as ${activeAccount()?.email ?? "Google account"}`);
  } catch (e) {
    toast(String(e), 7000);
  } finally {
    $("signin").close();
  }
}

async function removeAccount(account) {
  toggleMenu(false);
  if (!confirm(`Sign out of ${account.email}? Files stay in your Google Drive.`)) return;
  try {
    applyAccounts(await call("account_remove", { id: account.id }));
  } catch (e) {
    toast(String(e));
  }
}

// ---------- documents ----------

function formatSize(bytes) {
  if (bytes == null) return "";
  if (bytes < 1024 * 1024) return `${Math.max(1, Math.round(bytes / 1024))} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

function formatDate(iso) {
  const d = new Date(iso);
  return isNaN(d) ? "" : d.toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" });
}

function setSyncStatus(offline, pending = 0) {
  const s = $("sync-status");
  const local = activeAccount()?.kind === "local";
  s.classList.toggle("offline", offline);
  if (offline) {
    s.textContent = pending ? `Offline · ${pending} message${pending > 1 ? "s" : ""} waiting to sync` : "Offline · showing saved copy";
  } else {
    s.textContent = local ? "Saved on this device" : `Synced with Google Drive · ${new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`;
  }
}

function renderDocs() {
  $("docs").replaceChildren(
    ...state.docs.map((d) => {
      const li = el(
        "li",
        { className: state.current?.doc_id === d.doc_id ? "active" : "", title: d.name },
        el("span", { className: "name", textContent: d.name }),
        el("span", { className: "meta", textContent: [formatDate(d.modified), formatSize(d.size)].filter(Boolean).join(" · ") }),
      );
      li.onclick = () => selectDoc(d);
      return li;
    }),
  );
  $("docs-empty").hidden = state.docs.length > 0;
}

async function loadDocs() {
  const account = state.active;
  try {
    const r = await call("documents");
    if (account !== state.active) return;
    state.docs = r.value;
    renderDocs();
    setSyncStatus(r.offline);
    if (state.current && !state.docs.some((d) => d.doc_id === state.current.doc_id)) selectDoc(null);
  } catch (e) {
    setSyncStatus(true);
    toast(String(e));
  }
}

function toBase64(buffer) {
  const bytes = new Uint8Array(buffer);
  let binary = "";
  for (let i = 0; i < bytes.length; i += 0x8000) binary += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
  return btoa(binary);
}

async function addFiles(files) {
  for (const file of files) {
    if (!/\.pdf$/i.test(file.name) && file.type !== "application/pdf") continue;
    toast(`Adding ${file.name}…`, 60_000);
    try {
      const doc = await call("document_upload", { name: file.name, dataBase64: toBase64(await file.arrayBuffer()) });
      state.docs.unshift(doc);
      renderDocs();
      toast(`Added ${file.name}`);
      selectDoc(doc);
    } catch (e) {
      toast(`${file.name}: ${e}`, 7000);
    }
  }
}

async function selectDoc(doc) {
  state.current = doc;
  state.chat = null;
  document.body.classList.remove("drawer-open");
  $("scrim").hidden = true;
  $("empty").hidden = !!doc;
  $("workspace").hidden = !doc;
  renderDocs();
  if (!doc) return;
  $("doc-name").textContent = doc.name;
  $("doc-meta").textContent = [formatDate(doc.modified), formatSize(doc.size)].filter(Boolean).join(" · ");
  $("thread").replaceChildren(el("div", { className: "msg pending", textContent: "Opening…" }));
  try {
    const r = await call("document_open", { doc });
    if (state.current?.doc_id !== doc.doc_id) return;
    applyChat(r);
    $("question").focus({ preventScroll: true });
  } catch (e) {
    $("thread").replaceChildren(el("div", { className: "msg error", textContent: String(e) }));
  }
}

async function deleteDoc() {
  const doc = state.current;
  if (!doc || !confirm(`Delete “${doc.name}” and its chat? ${activeAccount()?.kind === "google" ? "It goes to your Drive trash." : ""}`)) return;
  try {
    await call("document_delete", { doc });
    state.docs = state.docs.filter((d) => d.doc_id !== doc.doc_id);
    selectDoc(null);
  } catch (e) {
    toast(String(e));
  }
}

// ---------- chat ----------

function applyChat(synced) {
  const atBottom = isScrolledToBottom();
  state.chat = synced.value;
  setSyncStatus(synced.offline, synced.pending);
  renderThread();
  if (atBottom) scrollToBottom();
}

function isScrolledToBottom() {
  const t = $("thread");
  return t.scrollHeight - t.scrollTop - t.clientHeight < 40;
}

function scrollToBottom() {
  const t = $("thread");
  t.scrollTop = t.scrollHeight;
}

function deviceLabel(device) {
  if (!device || device === state.info?.device) return null;
  const os = device.split("-")[0];
  return { android: "phone", windows: "Windows PC", linux: "Linux PC", macos: "Mac", ios: "iPhone" }[os] || os;
}

function renderMessage(m) {
  const summary = m.kind === "summary";
  const kind = m.role === "user" ? "user" : summary ? "summary" : "";
  const parts = [];
  if (summary) parts.push(`${m.meta?.style ?? ""} summary`.trim());
  if (m.role !== "user" && m.provider) parts.push(`${m.provider} · ${m.model}`);
  const from = deviceLabel(m.device);
  if (from) parts.push(`from ${from}`);

  const node = el("div", { className: `msg ${kind}` });
  if (parts.length) node.append(el("span", { className: "label", textContent: parts.join(" · ") }));
  node.append(document.createTextNode(m.content));

  const sources = m.meta?.sources;
  if (sources?.length) {
    const details = el("details", { className: "sources" }, el("summary", { textContent: `${sources.length} excerpts used` }));
    for (const s of sources) {
      const pages = s.page_start === s.page_end ? `p. ${s.page_start}` : `pp. ${s.page_start}-${s.page_end}`;
      details.append(el("p", { textContent: `${pages}: ${s.excerpt}` }));
    }
    node.append(details);
  }
  return node;
}

function renderThread(extra = []) {
  const messages = state.chat?.messages ?? [];
  const nodes = messages.map(renderMessage);
  if (!messages.length && !extra.length) {
    nodes.push(el("div", { className: "msg pending", textContent: "Ask a question, or pick a summary style above." }));
  }
  $("thread").replaceChildren(...nodes, ...extra);
}

function historyForAsk() {
  return (state.chat?.messages ?? [])
    .filter((m) => m.kind !== "summary")
    .map((m) => ({ role: m.role, content: m.content }));
}

function setBusy(busy) {
  state.busy = busy;
  document.querySelectorAll("#workspace button, #question").forEach((b) => (b.disabled = busy));
}

async function ask(question) {
  const doc = state.current;
  const pendingNodes = [
    el("div", { className: "msg user", textContent: question }),
    el("div", { className: "msg pending", textContent: "Thinking…" }),
  ];
  renderThread(pendingNodes);
  scrollToBottom();
  setBusy(true);
  try {
    const r = await call("ask", { doc, question, history: historyForAsk() });
    if (state.current?.doc_id === doc.doc_id) applyChat(r);
    scrollToBottom();
  } catch (e) {
    pendingNodes[1].className = "msg error";
    pendingNodes[1].textContent = String(e);
    $("question").value = question;
  } finally {
    setBusy(false);
    $("question").focus({ preventScroll: true });
  }
}

async function summarize(style) {
  const doc = state.current;
  const pending = el("div", { className: "msg pending", textContent: `Writing ${style} summary…` });
  renderThread([pending]);
  scrollToBottom();
  setBusy(true);
  try {
    const r = await call("summarize", { doc, style });
    if (state.current?.doc_id === doc.doc_id) applyChat(r);
    scrollToBottom();
  } catch (e) {
    pending.className = "msg error";
    pending.textContent = String(e);
  } finally {
    setBusy(false);
  }
}

async function syncNow() {
  if (document.hidden) return;
  await loadDocs();
  const doc = state.current;
  if (!doc || state.busy) return;
  try {
    const r = await call("chat_sync", { doc });
    if (state.current?.doc_id === doc.doc_id && !state.busy) {
      const before = state.chat?.messages.length ?? 0;
      if (r.value.messages.length !== before || r.offline) applyChat(r);
      else setSyncStatus(r.offline, r.pending);
    }
  } catch {
    // Shown via sync status on the next successful call.
  }
}

// ---------- settings ----------

async function openSettings() {
  const f = $("settings-form");
  try {
    const s = await call("settings_get");
    f.groq_keys.value = s.groq_keys.join("\n");
    f.groq_model.value = s.groq_model;
    f.gemini_keys.value = s.gemini_keys.join("\n");
    f.gemini_model.value = s.gemini_model;
    f.lmstudio_enabled.checked = s.lmstudio_enabled;
    f.lmstudio_url.value = s.lmstudio_url;
    f.lmstudio_model.value = s.lmstudio_model;
    const order = s.order.join(",");
    f.order.value = [...f.order.options].some((o) => o.value === order) ? order : f.order.options[0].value;
    $("secret-storage").textContent = state.info?.secret_storage === "keychain" ? "your system keychain" : "this app's private storage";
    await showProviders();
    $("settings").showModal();
  } catch (e) {
    toast(String(e));
  }
}

async function showProviders(list) {
  list ??= await call("providers");
  $("provider-status").textContent = list.length
    ? "Active: " + list.map((p) => (p.keys ? `${p.name} (${p.keys.available}/${p.keys.total} keys ready)` : p.name)).join(", ")
    : "No AI provider set up yet. Add at least one key.";
}

async function saveSettings() {
  const f = $("settings-form");
  const lines = (v) => v.split(/[\n,]/).map((s) => s.trim()).filter(Boolean);
  const settings = {
    groq_keys: lines(f.groq_keys.value),
    groq_model: f.groq_model.value.trim(),
    gemini_keys: lines(f.gemini_keys.value),
    gemini_model: f.gemini_model.value.trim(),
    lmstudio_enabled: f.lmstudio_enabled.checked,
    lmstudio_url: f.lmstudio_url.value.trim(),
    lmstudio_model: f.lmstudio_model.value.trim(),
    order: f.order.value.split(","),
  };
  try {
    const list = await call("settings_set", { settings });
    showProviders(list);
    toast("AI settings saved");
  } catch (e) {
    toast(String(e), 7000);
  }
}

// ---------- wiring ----------

$("file").onchange = (e) => {
  addFiles([...e.target.files]);
  e.target.value = "";
};

document.addEventListener("dragover", (e) => {
  e.preventDefault();
  document.body.classList.add("drag-over");
});
document.addEventListener("dragleave", (e) => {
  if (!e.relatedTarget) document.body.classList.remove("drag-over");
});
document.addEventListener("drop", (e) => {
  e.preventDefault();
  document.body.classList.remove("drag-over");
  addFiles([...e.dataTransfer.files]);
});

$("account-btn").onclick = (e) => {
  e.stopPropagation();
  toggleMenu();
};
document.addEventListener("click", (e) => {
  if (!$("account-menu").hidden && !e.target.closest(".account")) toggleMenu(false);
});
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") toggleMenu(false);
});

$("menu-toggle").onclick = () => {
  document.body.classList.add("drawer-open");
  $("scrim").hidden = false;
};
$("scrim").onclick = () => {
  document.body.classList.remove("drawer-open");
  $("scrim").hidden = true;
};

document.querySelectorAll("[data-style]").forEach((b) => (b.onclick = () => summarize(b.dataset.style)));
$("delete-doc").onclick = deleteDoc;

$("ask").onsubmit = (e) => {
  e.preventDefault();
  const q = $("question").value.trim();
  if (!q || !state.current || state.busy) return;
  $("question").value = "";
  ask(q);
};

$("settings-btn").onclick = openSettings;
$("settings-form").addEventListener("submit", (e) => {
  if (e.submitter?.value === "save") saveSettings();
});
// The sign-in dialog must not be dismissed with Escape while waiting.
$("signin").addEventListener("cancel", (e) => e.preventDefault());

document.addEventListener("visibilitychange", () => !document.hidden && syncNow());
window.addEventListener("focus", syncNow);
setInterval(syncNow, SYNC_EVERY_MS);

(async function start() {
  try {
    state.info = await call("app_info");
    applyAccounts(await call("accounts_list"));
    await loadDocs();
    const providers = await call("providers");
    if (!providers.length) toast("Add your Groq or Gemini key in ⚙ AI settings to get started.", 8000);
  } catch (e) {
    toast(String(e), 10000);
  }
})();
