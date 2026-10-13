# LightCraft in the browser

This fork’s **desktop Web view** is a thin controller and preview of the running Mac session.
The desktop owns the engine, Photos library, selection, edits, history, RAW decoding and Apple
Vision/Core ML inference. The browser receives a rendered view and sends input/commands; it
creates no catalogue, imports no originals and starts no WASM engine or model download.

## Desktop view

Build the native bundle, then launch it with its Web listener:

```sh
bash packaging/macos/develop.sh
open target/apple-development/LightCraft.app --args --web 8080 --web-bind 0.0.0.0 --control 18088
```

Launch these options after closing the previous instance. Open `http://127.0.0.1:8080/`.
`LIGHTCRAFT_WEB_PORT` is an alternative to `--web`; `--no-web` disables the listener. The Web
listener defaults to `0.0.0.0`; the existing JSON-lines/MCP transport remains on loopback.
Both transports feed the same desktop UI request queue, never separate engine sessions.

Navigation is shared: selecting an album/photo, moving a slider, changing a preset or creating a
mask in either view changes the same desktop session. Undo/redo and the Photos adjustment JSON
are shared too. The per-photo iCloud download button and original-safe export rules remain in
the native engine. The browser sees the desktop’s normal library thumbnails and editing preview.
Closing LightCraft ends the service; the browser displays a disconnected state and disables
input. **Reconnecter** reconnects to the running desktop, without creating a fallback library.

The complete native view is sent as JPEG previews (quality 90, 4:4:4, at most 1920 pixels on the
long edge); preview pixels travel over the network, not RAW originals. Pointer positions use
the desktop’s logical viewport, independent of JPEG size/browser scaling/Retina density. Window
size, active photo/source, panel and active-mask changes invalidate stale input. Pressed buttons
and keys are released after a disconnected gesture expires. Input remains application-scoped:
no OS-level synthetic events are used. Render/export still reconstruct from the original and
its separate description. This preview stream is not a full-resolution export or an HDR stream.

**Actions** exposes the native command registry (including menus outside the Mac window).
Keyboard, text input, scroll, right-click and slider/brush/crop gestures are forwarded. Native
file pickers and other OS dialogs appear on the Mac. Browser navigation shortcuts stay with the
browser; ordinary editing shortcuts use the desktop’s bindings.

Localhost connects directly. From another device, open the Mac's ordinary address
`http://<Mac-IP>:8080/` and click **Demander l’accès**. The browser and LightCraft on the Mac show
the same six-character code: compare it, then choose **Autoriser** in the Mac window. The view
opens automatically; reloads use the existing HttpOnly, SameSite session cookie. No special link
is required. **Copier l’adresse** copies the ordinary LAN address. Approval lasts for the running
desktop session; restarting LightCraft requires a new approval. A refused request stays refused
until its two-minute expiry. Requests are limited to eight total and two per client IP.

The pending cookie cannot read previews or run commands. Approval only reaches the server through
the native UI's decision channel, never a public JSON method. While approval is visible, browser
commands/input are suspended and already-queued gestures are cleared, so another connected
browser cannot press the approval button. Native input and loopback control remain available.
The previous fragment-key links remain compatible. Image/command endpoints require the approved
session and refuse foreign origins/DNS rebinding. Responses are not stored in the browser cache.
The server bounds headers, JSON bodies, frame dimensions, output size, connections and frame work.

Protocol: `GET /api/connect`, `POST /api/pair` (request), `GET /api/pair` (poll native decision),
`GET /api/frame` (JPEG with viewport/cursor/state headers),
`POST /api/rpc {method, params}`. Native `ui.webInput` validates complete input batches before
queuing them; the binary `ControlRequest::desktop_frame` supplies previews without temporary
image files or JSON pixel arrays. See `apps/lightcraft/src/web_server.rs`, `apps/lightcraft/web/`
and `crates/ui-egui/src/remote.rs`.

Tests cover direct-address pairing, native approval/refusal, expiry, request bounds, IP binding,
blocked browser self-approval, transport bounds, authentication/origin isolation, shared command dispatch, Retina
preview bounds, stale/invalid input, gesture expiry and native-menu keyboard routing. End-to-end
checks use the browser and TCP control channel on the same temporary native session, including
selection, exposure, undo/redo, native masks and unchanged input-file hashes.

## Standalone WASM build

The original experimental `apps/lightcraft-web` mode remains available separately. It runs the
egui UI in the browser, compiled to WebAssembly and drawn with WebGL2 (eframe’s `glow` backend).
It owns browser storage and cannot access Apple Photos or the Mac’s live session. The following
WASM build instructions apply to that separate mode; the desktop view needs no WASM build.

## Build and run locally

One-time setup:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version <the wasm-bindgen version in Cargo.lock> --locked
```

`cargo xtask web` checks the CLI against `Cargo.lock` and prints the exact install command if
it's missing or a different version.

Build, then serve:

```sh
cargo xtask web            # → <target>/web-site/{index.html, worker.js, lightcraft_web.js, lightcraft_web_bg.wasm}
cargo xtask web --serve    # build, then serve on http://127.0.0.1:8080/ (or `--serve 9000`)
cargo xtask web --dev      # unoptimized build with debug info (faster to compile, slow to run)
```

`<target>` is `target/` unless `CARGO_TARGET_DIR` is set. Any static HTTP server works, as long as
it serves `.wasm` as `application/wasm`. The bundle can't be opened from `file://` (module
workers and OPFS need an HTTP(S) origin; `localhost`/`127.0.0.1` count as secure). If `wasm-opt`
(binaryen) is on `PATH`, the release build also runs it over the module.

The release build uses the `web` Cargo profile (see *Bundle size* below) and writes gzip (`-9`)
and brotli (`-11`) precompressed copies next to each file (`*.gz`, `*.br`, pure Rust, printed
as a size table). `--serve` sends them with `Content-Encoding` when the browser accepts them;
configure a production server the same way (e.g. nginx `gzip_static`/`brotli_static`).

## Bundle size

Measured on the full bundle (every codec and raw decoder is in the module), without `wasm-opt`:

| `web` profile                                   | `.wasm` bytes | gzip -9 | brotli -11 | slider job* |
|-------------------------------------------------|--------------:|--------:|-----------:|------------:|
| before: release, thin LTO                       |    15 742 091 | 5 251 664 | 3 473 726 | 3.8 ms |
| fat LTO, 1 CGU, strip, panic=abort, opt 3       |    12 924 849 | 4 779 686 | 3 210 246 | 3.9 ms |
| same, opt-level "s"                             |    13 056 715 | 4 362 536 | 2 959 126 | 6.1 ms |
| same, opt-level "z"                             |    12 648 958 | 4 152 661 | 2 860 173 | 9.0 ms |
| **current:** "s", per-pixel crates at opt 3     |    13 533 126 | 4 583 465 | 3 093 780 | 4.0 ms |

\* median Exposure draft render of the `?bench` loupe in a worker (stage-cached), headless
Chrome. Size-optimizing the pipeline crates costs 50–130 % render time, so `Cargo.toml` keeps
them (pipeline, raster, color, develop, geom, raw, codecs, scenes, preview and the JPEG/PNG
codecs) at opt-level 3 and the rest (egui, eframe, serde, glue) at "s". What goes over the wire
is the brotli column: 3.1 MB, 11 % less than the old build's brotli size and 41 % less than its
gzip size (5.25 MB). `wasm-opt -Oz`, when installed, shrinks it further.

**Fonts.** The browser has no system fonts to fall back on, so CJK text comes from
[craft-fonts](https://github.com/storytold/craft-fonts), the optional `CRAFT_FONTS_DIR` build
input (`CRAFT_FONTS_DIR=../craft-fonts cargo xtask web`; release builds always set it). On wasm32
`crates/engine/build.rs` embeds only BIZ UDPGothic Regular (UI and watermark fallback). The web
bundle also ships Noto Sans CJK SC Regular as `lightcraft_zh_hans.otf` and loads it before the UI
starts. Keeping the 16 MB Chinese face separate leaves the module under Cloudflare's 25 MiB
per-file limit (the older Japanese-only module measured 21.8 MB). Without `CRAFT_FONTS_DIR`, the
web build still works, but Chinese and Japanese text lack glyphs.

## Deploying: headers

`cargo xtask web --serve` sends these on every response. A production server may send them too;
today they are optional:

```
Cross-Origin-Opener-Policy: same-origin
Cross-Origin-Embedder-Policy: require-corp
Cross-Origin-Resource-Policy: same-origin
```

COOP + COEP make the page *cross-origin isolated* (`crossOriginIsolated === true`). That will be
**required** for `SharedArrayBuffer`, i.e. for a future wasm-threads build (`+atomics`, nightly
`build-std`), and gives full-resolution `performance.now()`. The current render workers (below)
don't share memory, so the app runs without them; with COEP on, every subresource must be
same-origin or send CORP/CORS headers (the bundle has no third-party resources). The file names
are the same in every version, so serve them with `Cache-Control: no-cache` (not `immutable`);
`packaging/web/README.md` (shipped as `HOSTING.md`) has the hosting details.

## What works

- **A persistent library.** The library lives in the browser's storage for the page's origin:
  [OPFS](https://developer.mozilla.org/docs/Web/API/File_System_API/Origin_private_file_system)
  when the main thread can write it (`FileSystemFileHandle.createWritable`: Chrome, Edge,
  Firefox, recent Safari), otherwise IndexedDB. Both hold the same layout:
  - `library/catalog.snap`, `library/catalog.log`: the same crash-safe journal as the desktop app
    (`lightcraft-catalog`), plus `presets.json`, `view.json`, `prefs.json` and `ui.json` (panel
    layout). The catalog `Store` is a memory mirror loaded at start-up; every change is flushed in
    the background within a frame or two, each file replaced atomically, in modification order
    (`apps/lightcraft-web/src/files.rs`). View state and UI prefs are saved every second when they
    change (a tab can close without notice).
  - Known limitation: the browser storage has no file locks, so two tabs of the same origin open
    the same library and the last one to write a snapshot wins (the desktop app and the CLI lock
    a library folder: `catalog.lock`, issue #99). Use one tab at a time; a guard through the Web
    Locks API (`navigator.locks`) is still to do.
  - `originals/<content hash>`: the bytes of every imported photo. The catalog refers to them as
    `web/<hash>/<file name>`; the main thread keeps recently used originals in memory (≤ 768 MB)
    and loads the rest on demand (the active photo is prefetched).
  - `thumbs/<key>.jpg` + `thumbs/index.json`: the rendered-thumbnail cache, with the desktop's
    budget (2 GB, least recently used pruned to 80 %).

  A new library starts with the procedural demo photos. URL options: `?store=idb` forces
  IndexedDB, `?store=memory` keeps nothing, `?reset` deletes the stored library first (after a
  confirmation: it deletes every imported photo too).
- **Keeping the library safe** (experimental: the library lives only in this browser):
  - The app asks for persistent storage (`navigator.storage.persist()`); when the browser doesn't
    grant it, a notice says the library may be evicted under storage pressure.
  - **File ▸ Back Up Library…** downloads a zip of the library files and every stored original
    (`originals/<hash>/<file name>`, so an unzipped backup is browsable; entries are stored, not
    compressed; at most 4 GB). **File ▸ Restore Library from Backup…** reads such a zip into a
    new library folder (`library-restored-<time>/`; originals already stored are skipped, every
    entry's checksum is verified, and each restored original must match its content hash) and
    only then switches to it (`active-library` names the folder in use) and reloads: the previous
    library stays in storage. Both are web-only (`file.backupLibrary`, `file.restoreLibrary`; the
    desktop library is a folder).
  - A failed save (quota exceeded, storage cleared) is not silent: the catalog then refuses new
    writes, so commands report `saved in memory but not written`, the top bar shows the unsaved
    warning (as on the desktop, see `docs/control-protocol.md`), and saving is retried every
    2 s until it works (`web.stats` → `saveError`).
  - A picked or dropped photo whose bytes can't be stored is not added (it would be gone after
    a reload); a notice says why.
  - One tab at a time: the page holds a Web Lock (`navigator.locks`, `lightcraft-library`); a
    second tab or window shows "LightCraft is already open in another tab" instead of loading
    its own copy (two copies would overwrite each other's saves). Browsers without Web Locks, or
    that refuse the lock request, aren't protected: the app starts without the guard.
  - If the stored library can't be opened, a notice says the session is temporary and nothing
    is saved; the stored library is left as it was.
  - A panic (wasm is built with `panic = "abort"`) replaces the page with a message saying the
    stored library is kept and to reload, instead of a frozen canvas.
- **Importing photos with no filesystem.** *File ▸ Import Photos…* (<kbd>⌘⇧I</kbd>) opens the
  browser's file picker. You can also drop files anywhere on the page. The bytes are written to
  storage, then imported and decoded by the same engine code as the desktop app
  (`lightcraft_engine::files::{probe_bytes, load_bytes}`): JPEG/PNG/TIFF/WebP and the supported
  raw formats. "Copy into library" is the same as "Add" here.
- **Rendering in Web Workers.** Renders don't run on the main thread: up to four dedicated
  workers (`hardwareConcurrency − 1`, `?workers=N` to override, `?workers=0` for the old inline
  path) each run a second instance of the same wasm module. `index.html` compiles the module once
  and posts the compiled `WebAssembly.Module` to `worker.js`. A job crosses as JSON (the photo's
  source, develop settings, request); the worker reads the original or the cached thumbnail from
  storage itself, keeps decoded sources and the loupe's stage cache, renders, and transfers the
  RGBA bytes back. Jobs for a photo go back to the worker that decoded it. This needs no wasm
  threads or `SharedArrayBuffer`. If no worker starts, rendering falls back to the main thread.
- **Every develop control** (sliders, curves, mixer, grading, masking, crop…) works as it does
  on the desktop, since it's the same crate.
- **Export downloads the file.** *Export…* (<kbd>⌘⇧E</kbd>) runs the same `app.export` path as
  the desktop app (`lightcraft_engine::export`: JPEG/PNG/TIFF/WebP, sizing, naming). The host's
  `write` service hands each file to the browser as a download instead of writing it to disk.
- **Automation.** `await lightcraft.command("library.info", "{}")` runs any engine or UI command
  by id on the next frame and resolves to the JSON result (`web.stats` reports storage, workers
  and the render queue). This is how the headless-Chrome checks drive the page.

## Not yet

- **Exports and auto-adjustments run on the main thread** (they need the original's pixels
  there). Right after a reload, the first such command on a photo whose original isn't in memory
  yet fails with "still loading" and works a moment later.
- **Durability is "a frame later", not fsync-before-return:** a crash or tab kill in the few
  milliseconds between a command and its flush loses that command.
- **Data-safety gaps still open** (issue #107): a panic in a decoder still ends the page (and a
  photo that panics while its thumbnail renders can do so again after a reload; there is no
  `?safe` start that skips rendering yet); removing photos doesn't delete their stored originals
  (`originals/<hash>`; they are kept, and included in backups); restored-over libraries stay in
  storage until the site's data is cleared; backups larger than 4 GB need zip64, which isn't
  written yet.
- **Preset files** (import/export `.json`) aren't wired to browser pickers yet.
- **Control channel / MCP.** These are desktop-only, because they need a TCP socket.
- **AVIF export** is untested in the browser; it's the one encoder that may not be wasm-safe.

## Measuring

Open `http://127.0.0.1:8080/?bench` to run a scripted measurement:

1. Wait for the grid thumbnails.
2. Open the first photo in Detail.
3. Drag Exposure through 8 steps, exactly as a slider drag does (begin interaction → `develop.set`
   ×8 → end).

The page then logs one console line:

```
lightcraft-bench {"first_frame_ms":…,"thumbs_done_ms":…,"slider_draft_ms":[…],"slider_draft_median_ms":…,"slider_job_ms":[…],"release_full_ms":…}
```

- `slider_draft_ms`: time from the `develop.set` command until the new loupe texture is ready.
- `slider_job_ms`: the pipeline's share of that time.
- `release_full_ms`: the full-quality render after the drag ends.
