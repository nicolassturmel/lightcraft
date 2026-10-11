# Apple Photos, Apple RAW and Vision

On macOS, this fork uses the **System Photo Library as its main library**. The normal grid,
filmstrip and album tree are populated through PhotoKit. Photo ownership and album membership
belong to Photos. LightCraft stores editable settings and renders edited copies into Photos.

## Build and launch

Install Rust and Apple’s Command Line Tools. Build the locally signed bundle:

```sh
bash packaging/macos/develop.sh
open target/apple-development/LightCraft.app
```

The helper is compiled from `crates/apple/native/AppleBridge.swift`, bundled and signed alongside
the Rust executable. A local ad-hoc signature is suitable for development; distributed builds
need Apple signing and notarization. The app identifier is `com.nicolassturmel.lightcraft`.
Launch through Finder or `open` so macOS attributes Photos consent to LightCraft. Launching a
binary directly from a terminal or automation host can attribute consent to that host.

| Service | API | Minimum macOS |
|---|---|---|
| System Photo Library and edited copies | PhotoKit | 11 |
| Photos image decoding and CR3 | Core Image / `CIRAWFilter` | 12 |
| Subject/background masks | `VNGenerateForegroundInstanceMaskRequest` | 14 |

Normal macOS launches use Photos automatically, including when old UI preferences contain a
file-library path. `--library DIR` or an explicit `LIGHTCRAFT_LIBRARY` environment variable opens
a legacy file catalogue. `--memory` / `--demo` provides a throwaway test session. Other platforms
retain the portable Rust catalogue and rendering pipeline.

## Main library

Allow the macOS Photos permission request on first launch. Denied access is reported in the
main sidebar; allow LightCraft in **System Settings → Privacy & Security → Photos**, then use
**Refresh Photos**. Limited access projects only the permitted images. No private database or
`.photoslibrary` internals are read.

PhotoKit supplies image identifiers, filenames, dimensions, capture dates and user albums,
including nested folders. Live Photos contribute the still image; videos are excluded. Refresh
runs in the background at launch, on returning to the app, periodically, and after imports or
copy saves. Stable native identifiers preserve selection and edit drafts as Photos adds,
removes or reorganizes images. A malformed or incomplete response keeps the current view.

The grid and filmstrip show locally cached, rendered **Photos previews** via
`PHImageManager.requestImage`, with network access disabled. Browsing does not retrieve RAW
resources. These thumbnails never become development or export sources, so they cannot replace
original pixels or apply baked Photos adjustments twice. A cached LightCraft render is preferred
when available. After downloading an original, the grid can render the current LightCraft edits.

Originals are retrieved through `PHAssetResourceManager`. RAW+JPEG pairs prefer the
RAW original, including `alternatePhoto`; rendered assets use their current Photos rendition.
The public PhotoKit resource API requires a short-lived decode file. Its enclosing temporary
directory is removed after decoding, errors or bridge timeouts. There is no permanent import of
Photos originals, no `Originals` directory, and no LightCraft `catalog.log` or `catalog.snap` for
the Photos session. Render and decoded-source caches are in memory.

Use **Download original** on an individual grid photo, or on the selected photo in the sidebar,
to retrieve just that original from iCloud. The background task shows **Downloading…** and
**Retry download** on failure. Network access is enabled only for this explicit request; browsing,
editing, recipe reads and exports never enable downloads automatically. A missing cloud original
explains this per-photo action instead of reporting opaque PhotoKit error 3164. Downloaded pixels
are cached in memory at full, preview and thumbnail sizes. Photos manages its own local/iCloud
cache; if an original is later evicted, another explicit download may be needed. Exports never
silently substitute a lower-resolution image. **Import Photos…** and dropped image files create
assets directly in Photos and retain the input files. File imports are limited to 200 per call;
use Photos for importing folders and managing albums, names, deletions and original metadata.
Those file-catalogue operations are disabled in LightCraft’s Photos mode.

The transient metadata response is capped at 128 MB, 500,000 images and 20,000 collections.
Exceeding a bound reports an error instead of accepting a partial library.

## Originals, settings and edited copies

Changing exposure, crop, masks, ratings or annotations changes LightCraft’s **adjustment
metadata only**. Per-asset JSON files are saved atomically and durably in:

```
~/Library/Application Support/LightCraft/Photos Adjustments/<asset-id-hash>.json
```

`LIGHTCRAFT_CONFIG_DIR` changes this settings location. Each JSON contains its native asset ID,
develop settings (including stored masks), history, versions, ratings, flags and annotations.
It contains no photo catalogue, original bytes or album membership. Read errors are reported;
damaged metadata is preserved. Failed saves keep changes queued and report an unsaved state.
Native Photos source images, favourites, dates and metadata remain unchanged.

Use **Save edited copy to Photos**, **File → Save Edited Copy to Photos**, or **⌘S** to render a
full-size JPEG at quality 95 into a new asset in the Photos album **LightCraft**. The source’s
capture date and location are carried onto the copy. Every save creates a new copy. The new
asset receives `PHAdjustmentData` with the original’s local identifier and the edit recipe,
using `PHContentEditingOutput(placeholderForCreatedAsset:)` in the same PhotoKit change block.
Only asset creation requests are issued; no source edit or delete requests are issued.
If recipe attachment fails after the rendered copy is saved, LightCraft reports that partial
result and retains the copy and local draft.

Copies display their already-rendered image with default settings. To resume a saved edit,
select a LightCraft copy and use **Photo → Reopen Photos Adjustments**: the recipe selects its
original and restores its settings. The image is never developed a second time on top of its
baked edits. Reopening requires the original to be accessible in the current Photos library;
PhotoKit local identifiers are specific to that library/device.

## CR3 and masks

Core Image performs RAW demosaicing, camera colour conversion and as-shot white balance.
Optional tone boosts, local tone mapping, sharpening, noise reduction, gamut mapping and lens
correction are disabled. The bridge returns oriented extended-linear-sRGB RGBA float32; the
engine converts it to linear Rec.2020 without clipping highlight values above one. Preview
scaling and full-size decoding use the same native RAW decoder. Camera support depends on the
installed macOS RAW support. The portable file loader falls back to Rust when Apple cannot
open a CR3; the Photos path reports Apple decoding errors directly.

On macOS 14+, Subject and Background masks use Apple Vision foreground-instance segmentation
on an oriented, uncropped preview. The result is stored as portable quantized logits in the
adjustment metadata; Background complements that coverage. Crop, scaling and edge refinement
use the same stored selection. Demo images and explicit heuristic selections retain the
portable implementation. No-subject and malformed responses report an error.

## Commands and verification

The desktop dispatches slow native operations through background tasks. Synchronous engine
commands remain available to CLI/MCP clients:

```json
{"command":"library.applePhotos","params":{}}
{"command":"photo.downloadApplePhotosOriginal","params":{"id":42}}
{"command":"photo.saveApplePhotosCopy","params":{}}
{"command":"photo.reopenApplePhotosRecipe","params":{}}
{"command":"library.importIntoApplePhotos","params":{"paths":["/path/to/image.CR3"]}}
{"command":"mask.add","params":{"kind":"subject"}}
```

`library.info` identifies the `applePhotos` backend and its ownership/save status. `apple.status`
reports native capability and consent without requesting access. JSON requests/responses and
binary buffers are bounded. Timeout, process errors and parent shutdown reap the helper and
remove temporary decode files.

Regression tests cover native asset identity, nested albums, additions/removals, atomic refresh,
metadata recovery after restart, failed-save retries, ownership guards, per-photo download
isolation, stale-download rejection, library-preview/original separation and the full-size copy
render/recipe without requesting access to a personal photothèque. A SHA-256-pinned public CC0
Canon R8 C-RAW verifies native 6000×4000 decoding and preview colour conversion. UI snapshots
verify the Photos controls in the normal sidebar and download buttons on separate grid photos.
Real PhotoKit writes require an authorized
macOS session and are not performed against a personal library by the automated tests.

API references: [PhotoKit](https://developer.apple.com/documentation/photos),
[Photos preview requests](https://developer.apple.com/documentation/photos/phimagemanager/requestimage(for:targetsize:contentmode:options:resulthandler:)),
[Preview network policy](https://developer.apple.com/documentation/photos/phimagerequestoptions/isnetworkaccessallowed),
[PHContentEditingOutput](https://developer.apple.com/documentation/photos/phcontenteditingoutput),
[PHAdjustmentData](https://developer.apple.com/documentation/photos/phadjustmentdata),
[CIRAWFilter](https://developer.apple.com/documentation/coreimage/cirawfilter),
[Vision foreground masks](https://developer.apple.com/documentation/vision/vngenerateforegroundinstancemaskrequest).
