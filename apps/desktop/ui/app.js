// Reference UI. Talks to the kernel through Tauri's IPC using the same JSON
// protocol as stdio, FFI and HTTP /v1/rpc.

const $ = (id) => document.getElementById(id);
let nextId = 1;
let current = null; // active document info
const threads = new Map(); // document id -> { history: [], nodes: [] }

async function call(method, params = {}) {
  const invoke = window.__TAURI__?.core?.invoke;
  if (!invoke) throw new Error("Tauri runtime not found");
  const res = await invoke("kernel", { request: { id: nextId++, method, params } });
  if (res.error) throw new Error(res.error.message);
  return res.result;
}

// ---------- documents ----------

async function refreshDocs() {
  const docs = await call("list_documents");
  const list = $("docs");
  list.replaceChildren(
    ...docs.map((d) => {
      const li = document.createElement("li");
      li.className = current?.id === d.id ? "active" : "";
      const name = document.createElement("span");
      name.className = "name";
      name.textContent = d.name;
      name.title = d.name;
      const rm = document.createElement("button");
      rm.textContent = "×";
      rm.title = "Close document";
      rm.onclick = async (e) => {
        e.stopPropagation();
        await call("remove_document", { document_id: d.id });
        threads.delete(d.id);
        if (current?.id === d.id) select(null);
        refreshDocs();
      };
      li.append(name, rm);
      li.onclick = () => select(d);
      return li;
    }),
  );
}

function select(doc) {
  current = doc;
  $("empty").hidden = !!doc;
  $("workspace").hidden = !doc;
  if (doc) {
    $("doc-name").textContent = doc.name;
    $("doc-meta").textContent = `${doc.pages} pages · ${doc.chars.toLocaleString()} characters`;
    if (!threads.has(doc.id)) threads.set(doc.id, { history: [], nodes: [] });
    $("thread").replaceChildren(...threads.get(doc.id).nodes);
    $("question").focus();
  }
  refreshDocs();
}

function toBase64(buffer) {
  const bytes = new Uint8Array(buffer);
  let binary = "";
  for (let i = 0; i < bytes.length; i += 0x8000) {
    binary += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
  }
  return btoa(binary);
}

async function openFiles(files) {
  for (const file of files) {
    if (!/\.pdf$/i.test(file.name) && file.type !== "application/pdf") continue;
    try {
      const data_base64 = toBase64(await file.arrayBuffer());
      const doc = await call("load_pdf", { name: file.name, data_base64 });
      select(doc);
    } catch (e) {
      alert(`${file.name}: ${e.message}`);
    }
  }
}

// ---------- thread ----------

function addMessage(kind, text, label) {
  const div = document.createElement("div");
  div.className = `msg ${kind}`;
  if (label) {
    const l = document.createElement("span");
    l.className = "label";
    l.textContent = label;
    div.append(l);
  }
  div.append(document.createTextNode(text));
  const thread = threads.get(current.id);
  thread.nodes.push(div);
  $("thread").append(div);
  div.scrollIntoView({ block: "end", behavior: "smooth" });
  return div;
}

function replaceMessage(node, kind, text, label, sources) {
  node.className = `msg ${kind}`;
  node.replaceChildren();
  if (label) {
    const l = document.createElement("span");
    l.className = "label";
    l.textContent = label;
    node.append(l);
  }
  node.append(document.createTextNode(text));
  if (sources?.length) {
    const d = document.createElement("details");
    d.className = "sources";
    const s = document.createElement("summary");
    s.textContent = `${sources.length} excerpts used`;
    d.append(s);
    for (const src of sources) {
      const p = document.createElement("p");
      const pages = src.page_start === src.page_end ? `p. ${src.page_start}` : `pp. ${src.page_start}-${src.page_end}`;
      p.textContent = `${pages}: ${src.excerpt}`;
      d.append(p);
    }
    node.append(d);
  }
}

function setBusy(busy) {
  document.querySelectorAll("button, #question").forEach((el) => (el.disabled = busy));
}

const provider = () => $("provider").value || null;

async function summarize(style) {
  const doc = current;
  const node = addMessage("pending", `Writing ${style} summary…`);
  setBusy(true);
  try {
    const s = await call("summarize", { document_id: doc.id, style, provider: provider() });
    replaceMessage(node, "", s.text, `${style} summary · ${s.provider} · ${s.model}`);
  } catch (e) {
    replaceMessage(node, "error", e.message);
  } finally {
    setBusy(false);
    refreshProviders();
  }
}

async function ask(question) {
  const doc = current;
  const thread = threads.get(doc.id);
  addMessage("user", question);
  const node = addMessage("pending", "Thinking…");
  setBusy(true);
  try {
    const a = await call("ask", {
      document_id: doc.id,
      question,
      provider: provider(),
      history: thread.history,
    });
    replaceMessage(node, "", a.text, `${a.provider} · ${a.model}`, a.sources);
    thread.history.push({ role: "user", content: question }, { role: "assistant", content: a.text });
  } catch (e) {
    replaceMessage(node, "error", e.message);
  } finally {
    setBusy(false);
    $("question").focus();
    refreshProviders();
  }
}

// ---------- providers ----------

async function refreshProviders() {
  const list = await call("providers");
  const select = $("provider");
  const chosen = select.value;
  select.replaceChildren(new Option("Auto (fallback order)", ""), ...list.map((p) => new Option(p.name, p.name)));
  select.value = list.some((p) => p.name === chosen) ? chosen : "";
  $("keys").textContent = list.length
    ? list.map((p) => (p.keys ? `${p.name}: ${p.keys.available}/${p.keys.total} keys ready` : `${p.name}: local`)).join(" · ")
    : "No providers configured. Add keys to your .env file.";
}

// ---------- wiring ----------

$("file").onchange = (e) => {
  openFiles([...e.target.files]);
  e.target.value = "";
};

const drop = $("drop");
for (const target of [drop, document.body]) {
  target.addEventListener("dragover", (e) => {
    e.preventDefault();
    drop.classList.add("over");
  });
  target.addEventListener("dragleave", () => drop.classList.remove("over"));
  target.addEventListener("drop", (e) => {
    e.preventDefault();
    drop.classList.remove("over");
    openFiles([...e.dataTransfer.files]);
  });
}

document.querySelectorAll("[data-style]").forEach((b) => (b.onclick = () => summarize(b.dataset.style)));

$("ask").onsubmit = (e) => {
  e.preventDefault();
  const q = $("question").value.trim();
  if (!q || !current) return;
  $("question").value = "";
  ask(q);
};

refreshProviders().catch((e) => ($("keys").textContent = e.message));
refreshDocs().catch(() => {});
