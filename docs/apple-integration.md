# Apple Photos, Apple RAW and Vision

This fork adds Apple system services on macOS. The portable editor, catalogue and rendering
pipeline remain Rust. An isolated Swift helper calls PhotoKit, Core Image RAW and Vision over
a bounded JSON/file protocol. No new Rust unsafe code or third-party native library is involved.

## Build and use

Install Rust and Apple's Command Line Tools, then run:

```sh
cargo run -p lightcraft --features heif
```

`heif` enables HEIC originals from Photos. On macOS the helper is compiled and embedded
automatically. Windows, Linux and WebAssembly retain the portable pipeline. For a release app,
use `packaging/macos/package.sh --arch aarch64` (or `universal` with both Rust targets installed).
The bundle declares its Photos usage and contains a separately signed helper. This fork uses
`com.nicolassturmel.lightcraft` as its bundle identifier, distinct from upstream.

For a local app without notarization or a DMG, run `bash packaging/macos/develop.sh`, then open
`target/apple-development/LightCraft.app`. This bundles the Photos usage description and signs
both executables locally, including when only Apple's Command Line Tools are installed.
For PhotoKit, launch that bundle through Finder or `open target/apple-development/LightCraft.app`.
Executing its binary directly from a terminal or automation host can make macOS attribute
the Photos permission to that host instead of LightCraft. Restart after changing permission.

| Service | API | Minimum macOS |
|---|---|---|
| System Photo Library | PhotoKit | 11 |
| CR3 decoding | `CIRAWFilter` | 12 |
| Subject/background masks | `VNGenerateForegroundInstanceMaskRequest` | 14 |

## Apple Photos

Open **File → Apple Photos…**, allow the macOS Photos request, choose an album and select up
to 200 originals per import. The browser is paginated and shows names, dates, dimensions, RAW
status and favourites. Reading and copying happen in background tasks. Limited authorization
shows only the assets permitted by macOS. Denied access can be changed in System Settings →
Privacy & Security → Photos.

PhotoKit opens the **System Photo Library**, including iCloud assets. No private Photos database
or `.photoslibrary` internals are read. RAW+JPEG pairs prefer the RAW resource, including
`alternatePhoto`. Live Photos contribute the still image; videos are excluded. Enable
**Download originals from iCloud when needed** to retrieve originals absent from this Mac.
Unavailable originals produce individual errors while the remaining selection continues.

Unmodified resources are copied to `Originals/Apple Photos/<asset-resource-hash>/<original-name>`
inside the open LightCraft library. A completion marker stores the PhotoKit identifier after
the temporary download and final rename succeed. Reimport uses the same path, so normal
duplicate detection skips existing photos. EXIF metadata is read by the regular importer;
Photos-specific favourites and date changes are shown in the browser, not synced to the catalogue.
LightCraft's edits and ratings stay in its catalogue. This is a browse/import connection,
not automatic two-way synchronization. No original in Photos is edited or deleted.

Changing libraries during copying prevents the result being added to the new catalogue; the
copies stay in the library where the import began.

## CR3

The macOS import probe and source loader try Core Image before the Rust CR3 decoder. Apple
performs demosaicing, camera colour conversion and as-shot white balance. Optional tone boosts,
local tone mapping, sharpening, noise reduction, gamut mapping and lens correction are disabled.
The bridge exports oriented **extended linear sRGB RGBA float32**, and the engine converts it
to linear Rec.2020 without clipping highlight values above one. Previews request a reduced
scale; exports decode full resolution. Camera support depends on installed macOS RAW resources.

If Apple rejects a file, the Rust decoder and its existing embedded-preview fallback remain
available. Mosaic-based AI Enhance uses the Rust mosaic path because Core Image returns RGB,
not sensor samples. Other file types retain their portable decoder.

## Vision masks

For real photos on macOS, **Subject** and **Background** use Vision foreground-instance
segmentation. The worker receives an oriented, uncropped source in the catalogue's coordinates.
All foreground instances are combined into a 512 × 512 coverage grid; background is its
complement. No subject or an unsupported OS produces an explicit error.

Coverage becomes quantized compressed logits, saved as a `vision` mask shape. Edge refinement,
inversion, add/subtract/intersect and local adjustments use the existing pipeline. CPU/GPU
renders and exports sample the stored mask at their own resolutions and through crop geometry.
Stored masks render on other platforms without Apple services. New masks require macOS 14+.
Object/Describe retain optional SAM 3, Sky retains its semantic-matte/heuristic implementation,
and demo images retain procedural selections.

## Commands and lifecycle

The regular command registry exposes:

```json
{"command":"apple.status","params":{}}
{"command":"library.applePhotos","params":{"offset":0,"limit":100}}
{"command":"library.importApplePhotos","params":{"ids":["<PhotoKit asset id>"],"network":false}}
{"command":"mask.add","params":{"kind":"subject","provider":"apple"}}
{"command":"mask.addComponent","params":{"kind":"background","op":"subtract"}}
```

Desktop browse/import/mask commands return `{ "pending": true }`; `ui.inspect` exposes tasks
and the latest `applePhotos` page. The headless engine executes synchronously. A stored `seg`
on subject/background replays a mask without Vision; `provider: "heuristic"` explicitly chooses
the legacy selection. `apple.status` never requests Photos permission.

Requests, responses and dimensions are bounded; pixel data uses temporary files. Timed-out
children are killed and waited on. Cargo builds extract the embedded helper under
`~/Library/Application Support/LightCraft/Apple/`; bundles use their signed sibling executable.

## Verification

`cargo xtask ci` checks formatting, clippy, tests, layering, assets and WebAssembly. Apple tests
check helper capabilities without requesting access, invalid RAWs and hostile sizes. Stored
Vision masks are checked for serialization, complement, scaling and crop behavior. The optional
CC0 Canon R8 C-RAW at `corpus/raw/cr3-canon-r8-craw.cr3` exercises the real system decoder.
Personal-library authorization and importing an asset require an interactive local smoke test.
No personal images or Photos identifiers are included in the repository.
