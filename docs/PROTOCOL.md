# pdf-kernel protocol (v1)

One JSON contract, four transports. Use whichever fits your app:

| Transport | How | Good for |
|---|---|---|
| **C FFI** | link `libpdf_kernel` (`.so` / `.dylib` / `.dll` / static `.a`), call `pk_engine_call` | Native apps: SwiftUI, WinUI/C#, Kotlin (JNI), C/C++, Qt, Flutter (dart:ffi), Python (ctypes) |
| **stdio** | spawn `pdfk stdio`, write one JSON request per line, read one JSON response per line | Any language that can run a process; editors; scripts |
| **HTTP** | `pdfk serve`, then `POST /v1/rpc` (or the REST routes below) | Web UIs, other machines, shared servers |
| **Rust** | depend on the `pdf-kernel` crate and call `Engine` directly | Rust apps (the Tauri app does this) |

## Message shape

Request:

```json
{"id": 1, "method": "summarize", "params": {"document_id": "…", "style": "bullets"}}
```

Response (success / failure):

```json
{"id": 1, "result": { … }}
{"id": 1, "error": {"code": "document_not_found", "message": "document not found: …"}}
```

`id` can be any JSON value and is echoed back. Over stdio, requests run
concurrently, so match responses by `id`.

## Methods

### `version`
→ `{"name": "pdf-kernel", "version": "0.1.0", "protocol": 1}`

### `providers`
→ `[{"name": "groq", "base_url": "…", "model": "llama-3.3-70b-versatile", "requires_key": true, "keys": {"total": 3, "available": 2, "cooling_down": 1, "disabled": 0}}, …]`

Keys themselves are never returned.

### `load_pdf`
Params, one of:
- `{"path": "/abs/path/file.pdf"}` (not allowed over HTTP)
- `{"name": "file.pdf", "data_base64": "JVBERi0…"}`

→ `{"id": "9f2c…", "name": "file.pdf", "pages": 12, "chars": 48210, "chunks": 27}`

### `load_text`
Add a document from text you extracted yourself (OCR, DOCX, web page…).
Params: `{"name": "notes", "pages": ["page 1 text", "page 2 text"]}` → same as `load_pdf`.

### `list_documents`
→ array of document info objects.

### `remove_document`
Params: `{"document_id": "…"}` → `{"removed": true}`

### `summarize`
Params:
```json
{"document_id": "…", "style": "brief | detailed | bullets", "provider": "groq"}
```
`style` defaults to `detailed`; `provider` is optional (omit for automatic fallback).

→ `{"document_id": "…", "style": "bullets", "text": "…", "provider": "groq", "model": "…", "sections": 3}`

Long documents are summarized in sections (in parallel), then merged;
`sections` tells you how many.

### `ask`
Params:
```json
{
  "document_id": "…",
  "question": "What is the termination clause?",
  "provider": "gemini",
  "history": [{"role": "user", "content": "…"}, {"role": "assistant", "content": "…"}],
  "top_k": 8
}
```
Only `document_id` and `question` are required. `history` enables follow-up questions.

→
```json
{
  "document_id": "…",
  "text": "Either party may terminate with 30 days notice (p. 7).",
  "used_full_document": false,
  "sources": [{"page_start": 7, "page_end": 7, "excerpt": "…"}],
  "provider": "gemini",
  "model": "gemini-2.5-flash"
}
```

## Error codes

`parse_error`, `invalid_request`, `unknown_method`, `pdf_error`, `empty_document`,
`document_not_found`, `no_provider`, `providers_failed`, `config_error`, `io_error`, `internal`.

## Config JSON (FFI `pk_engine_new`)

Pass `NULL` to read the environment / `.env`, or:

```json
{
  "providers": [
    {"name": "groq", "base_url": "https://api.groq.com/openai/v1", "model": "llama-3.3-70b-versatile", "api_keys": ["gsk_1", "gsk_2"]},
    {"name": "gemini", "base_url": "https://generativelanguage.googleapis.com/v1beta/openai", "model": "gemini-2.5-flash", "api_keys": ["AIza…"]},
    {"name": "lmstudio", "base_url": "http://localhost:1234/v1", "requires_key": false}
  ],
  "max_context_chars": 16000,
  "concurrency": 3
}
```

Any OpenAI-compatible endpoint works as a provider (OpenRouter, Ollama's `/v1`, vLLM, …).

## HTTP REST routes

Everything except `/v1/health` needs `Authorization: Bearer <token>` when the
server runs with `--token`.

| Method | Path | Body |
|---|---|---|
| GET | `/v1/health` | – |
| GET | `/v1/providers` | – |
| GET | `/v1/documents` | – |
| POST | `/v1/documents?name=file.pdf` | raw PDF bytes |
| DELETE | `/v1/documents/{id}` | – |
| POST | `/v1/documents/{id}/summary` | `{"style": "brief", "provider": "groq"}` (optional) |
| POST | `/v1/documents/{id}/ask` | `{"question": "…", "history": [], "provider": null}` |
| POST | `/v1/rpc` | any protocol request |

Errors return a matching status code and `{"error": {"code", "message"}}`.

## Examples

### stdio (any language)

```sh
printf '%s\n' \
  '{"id":1,"method":"load_pdf","params":{"path":"report.pdf"}}' \
  | pdfk stdio
```

### Python via the C library

```python
import ctypes, json
lib = ctypes.CDLL("./target/release/libpdf_kernel.so")  # .dylib on macOS, pdf_kernel.dll on Windows
lib.pk_engine_new.restype = ctypes.c_void_p
lib.pk_engine_call.restype = ctypes.c_void_p
lib.pk_engine_call.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
lib.pk_string_free.argtypes = [ctypes.c_void_p]

engine = lib.pk_engine_new(None, None)

def call(method, **params):
    ptr = lib.pk_engine_call(engine, json.dumps({"id": 1, "method": method, "params": params}).encode())
    out = json.loads(ctypes.string_at(ptr).decode())
    lib.pk_string_free(ptr)
    return out

doc = call("load_pdf", path="report.pdf")["result"]
print(call("summarize", document_id=doc["id"], style="bullets")["result"]["text"])
```

### Swift

```swift
let engine = pk_engine_new(nil, nil)
let request = #"{"id":1,"method":"load_pdf","params":{"path":"/tmp/a.pdf"}}"#
let raw = pk_engine_call(engine, request)!
let json = String(cString: raw)
pk_string_free(raw)
```

### C# (P/Invoke)

```csharp
[DllImport("pdf_kernel")] static extern IntPtr pk_engine_new(string? config, IntPtr err);
[DllImport("pdf_kernel")] static extern IntPtr pk_engine_call(IntPtr engine, string request);
[DllImport("pdf_kernel")] static extern void pk_string_free(IntPtr s);
```
