# pdf-kernel

AI-powered PDF summarizer and Q&A. A Rust core with no UI, plus optional
front-ends. Use the bundled desktop app, or build your own UI on any platform
in any language.

```
                ┌──────────── crates/core (pdf-kernel) ───────────┐
                │ PDF extraction · chunking · BM25 retrieval ·    │
                │ summarize (map-reduce) · Q&A with page cites ·  │
                │ provider fallback · API-key rotation            │
                └─────────────────────────────────────────────────┘
       ▲                  ▲                  ▲                  ▲
  crates/ffi         pdfk stdio          pdfk serve         Rust crate
  C ABI (.so/.dylib  JSON lines over     HTTP API           (apps/desktop
  /.dll/.a)          stdin/stdout        (optional)          uses this)
       ▲                  ▲                  ▲
  SwiftUI, WinUI,     any language       web UIs, remote
  Qt, Kotlin, Python  that can spawn     machines
```

Every front-end speaks the same JSON protocol: [docs/PROTOCOL.md](docs/PROTOCOL.md).

## Providers

| Provider | Key | Notes |
|---|---|---|
| Groq | `GROQ_API_KEYS` | Fast; free tier has low tokens-per-minute limits |
| Google AI Studio (Gemini) | `GEMINI_API_KEYS` | Large context |
| LM Studio | none | Fully local; uses your GPU through LM Studio |

All three use the OpenAI-compatible chat API, so any similar endpoint can be
added through config (OpenRouter, Ollama `/v1`, vLLM, …).

**Key rotation:** list several keys, comma-separated. Each request takes the
next key in turn. A rate-limited key (HTTP 429) rests for its `Retry-After`
time (default 60 s) while the next key takes over. A rejected key (401/403) is
dropped. If every key for a provider is unusable, the next provider in
`PDFK_PROVIDER_ORDER` is tried. If all providers are only rate limited, the
engine waits up to 20 s for a key to free up.

## Setup

1. Install Rust: https://rustup.rs
2. `cp .env.example .env` and add your keys.

## CLI

```sh
cargo build --release            # binary: target/release/pdfk

pdfk providers                   # show providers and key health
pdfk summarize report.pdf -s bullets
pdfk ask report.pdf "What are the payment terms?"
pdfk chat report.pdf             # interactive follow-up questions
pdfk summarize report.pdf --json # machine-readable output
```

## Embedding

**Native apps (C ABI):**
```sh
cargo build --release -p pdf-kernel-ffi
# target/release/libpdf_kernel.so | libpdf_kernel.dylib | pdf_kernel.dll (+ static lib)
# header: crates/ffi/include/pdf_kernel.h
```
Call it from Swift, C#, Kotlin/JNI, C++, Dart or Python. See the examples in
[docs/PROTOCOL.md](docs/PROTOCOL.md).

**Any language, no linking:** spawn `pdfk stdio` and exchange JSON lines.

**HTTP (optional):**
```sh
pdfk serve --port 8787 --token secret
curl -H 'Authorization: Bearer secret' --data-binary @report.pdf 'localhost:8787/v1/documents?name=report.pdf'
```
It binds to `127.0.0.1` by default and allows no browser origins unless you
pass `--cors-origin`. For a binary with no server code at all, build with
`cargo build --release -p pdf-kernel-cli --no-default-features`.

## App (Linux, Windows, macOS, Android)

`apps/app` is the reference app, built with Tauri. It calls the core
in-process, with no server.

- **Accounts:** sign in with any number of Google accounts and switch between
  them, or use "This device" with no sign-in.
- **Sync:** each Google account's PDFs and chats are saved to its own Drive
  (`PDF Kernel/` folder), so a conversation started on a laptop continues on
  a phone. Messages typed offline upload later; edits from two devices merge.
- **Keys:** each user enters their own Groq / Gemini keys (or LM Studio URL)
  in Settings. They are stored in the OS keychain and never synced.

Google sign-in needs a free OAuth client: [docs/GOOGLE_SETUP.md](docs/GOOGLE_SETUP.md).

**Download builds:** every push to `main` builds Linux (`.deb`, `.rpm`,
`.AppImage`), Windows (`.msi`, `.exe`) and Android (`.apk`) in GitHub Actions
(see the run's *Artifacts*). Push a tag like `v0.1.0` to publish a release.

**Build locally:**
```sh
cd apps/app
npm ci                                  # installs the pinned Tauri CLI
npm run tauri dev                       # run
npm run tauri build                     # installers for this OS
npm run tauri -- android init           # once; needs Android SDK + NDK
npm run tauri -- android build --apk
```

Linux build deps (Debian/Ubuntu):
`sudo apt install libwebkit2gtk-4.1-dev build-essential libssl-dev libayatana-appindicator3-dev librsvg2-dev`

## Layout

```
crates/core   engine library (all AI/PDF logic)
crates/cloud  optional: Google sign-in, keychain, Drive storage, chat sync
crates/cli    pdfk: CLI, stdio protocol, HTTP API
crates/ffi    C ABI + header
apps/app      Tauri app (ui/ is plain HTML/CSS/JS, no build step)
docs/         protocol reference, Google setup
```

## Limits

- Scanned PDFs (images only) have no text layer. Run OCR first and use `load_text`.
- The CLI, stdio, HTTP and FFI front-ends keep documents in memory for the life of the process; the app persists them through `crates/cloud`.
