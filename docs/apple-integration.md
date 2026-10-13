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

Packaging fetches Apple’s Apache-2.0 DETR-ResNet50 semantic segmentation model (F16/P8, about
43 MB), verifies the revision and SHA-256 checksums in `packaging/models/apple-sky.json`, and
compiles it with the public `MLModel.compileModel` API. Full Xcode is not required. The compiled
model and its licence are included in the app and macOS CLI package. Model preparation needs a
network connection once; inference is offline and photos are never uploaded. Weights remain an
ignored build input under `target/apple-models/`, not repository assets.

For a Cargo-only development/test build, first run `bash packaging/macos/prepare-sky-model.sh`.
`LIGHTCRAFT_APPLE_MODEL_CACHE` changes the packaging cache location;
`LIGHTCRAFT_APPLE_SKY_MODEL` can explicitly point a Cargo/CLI run at a compiled `.mlmodelc`.
An absent or incompatible model reports an error rather than downloading at runtime or switching
to sky-colour guesses. `apple.status` reports its availability as `sky`.

| Service | API | Minimum macOS |
|---|---|---|
| System Photo Library and edited copies | PhotoKit | 11 |
| Photos image decoding and CR3 | Core Image / `CIRAWFilter` | 12 |
| Subject/background/object masks | `VNGenerateForegroundInstanceMaskRequest`, `VNInstanceMaskObservation` | 14 |
| Semantic sky masks | Apple DETR semantic model via `VNCoreMLRequest`, plus source-edge/foreground refinement | 14 |

Normal macOS launches use Photos automatically, including when old UI preferences contain a
file-library path. `--library DIR` or an explicit `LIGHTCRAFT_LIBRARY` environment variable opens
a legacy file catalogue. `--memory` / `--demo` provides a throwaway test session. Other platforms
retain the portable Rust catalogue and rendering pipeline.

## Desktop Web view

Launch the Mac bundle with `--web 8080 --web-bind 0.0.0.0` to expose its current session at
`http://127.0.0.1:8080/`. The browser displays the native view and sends application-scoped
input/commands. The same Photos projection, selection, adjustment JSON, history, previews and
Apple RAW/Vision/Core ML services are used; no second catalogue, engine or browser original
store is created. OS permissions and file pickers remain on the Mac. See [Web view](web.md#desktop-view).

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

Selecting a preset **replaces the previously selected look**. Its `preset_selection` description
records the preset ID, amount, and only the parameter values before/after its application.
Switching presets, hover previews and the Amount slider all use the recovered baseline;
subsequent manual changes and personal masks are retained. Amount never invokes Undo on an
unrelated edit. The description survives JSON save/reopen and undo/redo. Older consecutive preset
histories recover their recorded baseline and replay intervening manual changes when available.
The lookup is cached against the photo revision and does not rewrite old history. Preset parameters and masks
remain recipe data: preview and export render from the original decoded pixels, never from a
previously rendered preset image. Edited copies in Photos remain explicit save operations.

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

On macOS 14+, Subject, Background and Object masks use Apple Vision foreground-instance
segmentation on an oriented, uncropped preview. Subject selects the detected foreground;
Background complements it. **Object** creates an empty selection immediately: click a detected
foreground object to include its instance, or Option-click to exclude an entire detected instance.
Multiple clicks can include several objects. Excluding the last selected object produces an
empty mask. A click on undetected background reports an error and preserves the previous mask.
Vision can group related objects into one instance; use Brush for finer manual corrections.

**Sky** uses a source’s embedded semantic sky matte when present. Otherwise it runs Apple’s
published DETR semantic segmentation model through `VNCoreMLRequest`. The sky category is read
from the model’s class metadata, rather than guessed from colours or a horizon. The complete
oriented original preview is scaled to the model input without centre cropping. A bounded
edge-connectivity pass can remove thin structures connected to classified non-sky regions;
it cannot add pixels outside the semantic selection. Vision’s soft foreground contours protect
subjects. A landscape without a detected foreground subject is valid, and no-sky classifications
stay empty. The selection is independent of exposure, B&W, presets and crop, then stored as
`skySelection` logits with the existing Edge control.

The model distinguishes sky, buildings, water and other scene classes, but it is not perfect:
fine poles, foliage, weak boundaries and cloud banks misclassified as mountains can still need
Add/Subtract Brush corrections. No SAM download is used. **Update Sky Selection** recomputes
an existing sky component from the original using the current method, keeping its Edge, local
adjustments, name, inversion, operation and other components (including Brush corrections).
This is an undoable description edit; existing masks are never silently regenerated during
rendering. Old heuristic selections need this explicit update to benefit from the new model.

The result is stored as portable quantized logits in the adjustment metadata. Crop, scaling
and edge refinement use that stored selection; rendering and export never require a model.
Selection runs in a background task and stale results are discarded after photo, mask or
library changes. Originals are read with PhotoKit network access disabled, so a cloud-only
original still requires its explicit per-photo download first.

Real macOS libraries never prepare, refine with, offer or download SAM for masks. **Describe**
is disabled with an explanation: this Vision API does not provide text-prompt segmentation.
Existing stored Describe masks remain renderable. Demo sessions and other platforms retain
the portable implementation; explicit heuristic Subject/Background selections remain available.
No-subject and malformed native responses report errors without switching providers.

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
Mask regressions verify that Object/Add/Subtract/Intersect never open a SAM dialog, text selection
stays disabled, and stale object results cannot write adjustments. The CC0 R8 fixture also checks
real Vision inference, click/mask orientation, exclusion of the sole selected instance, and
rejection of a background click without touching the personal Photos library.
Sky tests cover the semantic prior on identically coloured sky/ground, uniform overcast coverage,
no-sky inputs, finite/bounded masks, soft foreground protection and ground-connected pole removal
across exposure levels. An optional native test runs the prepared model through the complete
engine path. Visual checks on three locally available photographs exercise overcast sky, coastal
water, foreground subjects and vertical urban buildings; they do not establish general Lightroom
mask parity. Sky-update tests cover retained adjustments, undo/redo and stale-result guards.
Preset tests cover switching, Amount, per-photo baselines, manual corrections, mask ownership,
undo/redo and JSON recovery.
Real PhotoKit writes require an authorized
macOS session and are not performed against a personal library by the automated tests.

API references: [PhotoKit](https://developer.apple.com/documentation/photos),
[Photos preview requests](https://developer.apple.com/documentation/photos/phimagemanager/requestimage(for:targetsize:contentmode:options:resulthandler:)),
[Preview network policy](https://developer.apple.com/documentation/photos/phimagerequestoptions/isnetworkaccessallowed),
[PHContentEditingOutput](https://developer.apple.com/documentation/photos/phcontenteditingoutput),
[PHAdjustmentData](https://developer.apple.com/documentation/photos/phadjustmentdata),
[CIRAWFilter](https://developer.apple.com/documentation/coreimage/cirawfilter),
[Vision foreground masks](https://developer.apple.com/documentation/vision/vngenerateforegroundinstancemaskrequest),
[Vision instance labels](https://developer.apple.com/documentation/vision/vninstancemaskobservation/instancemask),
[Apple semantic segmentation sample](https://developer.apple.com/documentation/coreml/using-core-ml-for-semantic-image-segmentation),
[Apple model and licence](https://huggingface.co/apple/coreml-detr-semantic-segmentation).
