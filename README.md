# pdf-kernel

AI-powered PDF summarizer and Q&A. A Rust core with no UI, plus optional
front-ends. Use the ready-made app (Linux, Windows, Android), or build your
own UI on any platform in any language.

- [Install the app](#install-the-app)
- [Use from the command line](#command-line)
- [Embed in your own app](#embedding)
- [Build from source](#build-from-source)
- [Google sign-in setup (for maintainers)](docs/GOOGLE_SETUP.md)

```
                ┌──────────── crates/core (pdf-kernel) ───────────┐
                │ PDF extraction · chunking · BM25 retrieval ·    │
                │ summarize (map-reduce) · Q&A with page cites ·  │
                │ provider fallback · API-key rotation            │
                └─────────────────────────────────────────────────┘
       ▲                  ▲                  ▲                  ▲
  crates/ffi         pdfk stdio          pdfk serve         Rust crate
  C ABI (.so/.dylib  JSON lines over     HTTP API           (apps/app
  /.dll/.a)          stdin/stdout        (optional)          uses this)
       ▲                  ▲                  ▲
  SwiftUI, WinUI,     any language       web UIs, remote
  Qt, Kotlin, Python  that can spawn     machines
```

Every front-end speaks the same JSON protocol: [docs/PROTOCOL.md](docs/PROTOCOL.md).

## Install the app

Download the latest build from the
[Releases page](https://github.com/Shantil-K/PDF-Kernel/releases).

| Platform | File | How to install |
|---|---|---|
| Windows 10/11 | `*_x64-setup.exe` or `*.msi` | Run it. If SmartScreen warns "unrecognized app", click **More info → Run anyway** (the build is not code-signed yet). |
| Debian / Ubuntu | `*.deb` | `sudo apt install ./PDF.Kernel_*.deb` |
| Fedora / openSUSE | `*.rpm` | `sudo dnf install ./PDF.Kernel-*.rpm` |
| Any Linux | `*.AppImage` | `chmod +x PDF.Kernel_*.AppImage` then run it |
| Android 8+ | `pdf-kernel-android.apk` | Open the file on the phone and allow "Install unknown apps" for your browser or file manager. |

Linux needs a desktop keyring (GNOME Keyring or KWallet, present on most
desktops) to store keys securely; without one the app falls back to its own
private storage.

### First run

1. **Add an AI key.** Open **⚙ AI settings** and paste at least one key:
   - Groq: https://console.groq.com/keys (free tier available)
   - Google AI Studio: https://aistudio.google.com/apikey (free tier available)
   - or tick **LM Studio**, start LM Studio's server (Developer tab), and load a model.
     On a phone, use your PC's address, e.g. `http://192.168.1.20:1234/v1`.

   Several keys per provider (one per line) are used in rotation. Keys stay on
   the device and are never uploaded.
2. **Choose where files are kept.** From the account menu (top right):
   - **This device**: works immediately, no account, nothing leaves the device.
   - **Add Google account**: PDFs and chats are saved to your Google Drive in a
     `PDF Kernel` folder, so you can continue on any device signed in to the
     same account. You can add several Google accounts and switch between them.
     The app can only see files it created, never the rest of your Drive.
3. **Add a PDF** (button or drag and drop), then pick a summary style or ask questions.
   Answers cite page numbers.

On a phone, tap ☰ to see your documents. After Google sign-in in the browser,
switch back to the app if it doesn't return by itself.

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

## Command line

1. Install Rust: https://rustup.rs
2. `cp .env.example .env` and add your keys (see comments in the file).

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

## Build from source

**CI builds:** every push to `main` builds Linux, Windows and Android in
GitHub Actions (download them from the run's *Artifacts*). Pushing a tag like
`v0.1.0` also publishes them as a GitHub Release. To include Google sign-in
and a stable Android signing key, add the repository secrets described in
[docs/GOOGLE_SETUP.md](docs/GOOGLE_SETUP.md).

**Locally:** install Rust (https://rustup.rs) and Node.js 20+, then:
```sh
cd apps/app
npm ci                                  # installs the pinned Tauri CLI
npm run tauri dev                       # run
npm run tauri build                     # installers for this OS
npm run tauri -- android init           # once; needs Android SDK + NDK
npm run tauri -- android build --apk
```

Extra requirements:
- **Linux** (Debian/Ubuntu): `sudo apt install libwebkit2gtk-4.1-dev build-essential libssl-dev libayatana-appindicator3-dev librsvg2-dev`
- **Windows**: Microsoft C++ Build Tools ("Desktop development with C++") and WebView2 (preinstalled on Windows 10/11).
- **Android**: Android Studio (SDK, NDK, Java 17) and
  `rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android`.
  Set `ANDROID_HOME` and `NDK_HOME`.
- **Google sign-in in local builds**: put `GOOGLE_CLIENT_ID` and
  `GOOGLE_CLIENT_SECRET` in `.env` ([how to get them](docs/GOOGLE_SETUP.md)).

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
