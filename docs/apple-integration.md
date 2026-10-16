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

## AI object removal

Remove opens in **AI Remove** mode on macOS. Paint over an unwanted object and wait for
“AI removal applied”. Applied brush contours and pins then disappear, and the brush is ready
for another repair. Enable **Show Remove Edits** to select an existing pin, adjust its opacity,
delete it, or choose **Regenerate** (`/`). Undo/redo retains the repair independently of overlays.
The native desktop Web view controls this exact operation on the Mac. Inference runs on a
worker; changing the original or its edits while it runs discards the stale result. An original still
requires the explicit per-photo **Download original** action first.

This uses LaMa through Apple's public Core ML API, not Photos' proprietary Clean Up engine.
No public Clean Up inference API was found in the installed SDK. LaMa is bundled (about 217 MB
before compilation), pinned and SHA-256 verified by `packaging/models/apple-inpaint.json` and
`prepare-inpaint-model.sh`. Build-time preparation needs a network connection once; there is
no model download prompt or upload at runtime. The existing Apple service worker retains the
loaded network across strokes. For Cargo-only builds, run
`bash packaging/macos/prepare-inpaint-model.sh`; `LIGHTCRAFT_APPLE_INPAINT_MODEL` overrides the
compiled model path. macOS CLI packages also resolve the model from their adjacent **Models** folder.
Portable builds cannot generate repairs without the Mac desktop engine. Standalone browser workers
report an explicit unsupported error for imported AI repairs because they lack repair-asset
transport; use the desktop Web view to render those edits.

Original decoded pixels and Photos assets remain unchanged. The JSON edit description stores
source-bound immutable patch references and the brush recipe; generated linear floating-point
pixels and alpha live in **Photos Adjustments/AI Remove/*.lcaip** (or the file library's **AI
Remove** folder). These are permanent edit assets, not disposable previews: back up that folder
with adjustment JSON. Undo retains the assets for redo/history. Missing/corrupt assets produce
an error; selecting a photo never regenerates them or downloads an original. Preview, crop,
rotation, colour edits and rendered exports all composite the same saved patch before development.
The source/composite cache prevents re-reading Photos or rerunning inference on slider changes.
A Photos edited-copy recipe includes repair pixels used by its settings/history. Large recipes
are retained as immutable local snapshots with a small reference in Photos; recipes exceeding
16 MB fail explicitly before saving. Use rendered JPEG/PNG/TIFF exports for AI edits; exporting an
original/DNG plus a reference-only sidecar is refused rather than losing the generated repair.

APPLE.12 (2026-10-16) defaults Remove to AI on Mac, including reopening/restored panels, and
automatically hides applied contours/pins after successful inference. The shared
`view.removeEdits` command and panel checkbox reveal saved repairs. Hidden pins have no hit
areas, so another stroke can cover the same location.

APPLE.11 validation (2026-10-16): a real Core ML inference test removes a synthetic object,
preserves the original and unpainted pixels, and reloads its lossless repair asset. Engine tests
cover undo/redo, stale-job rejection, source binding, malformed/missing assets, explicit-original
requirements and reopening local/embedded Photos recipes. The native Mac UI test paints a
three-point stroke, exports a 1200×800 PNG, undoes/redoes and regenerates the repair. Across
three exposure edits, the helper retains the same PID and its request count stays unchanged.
The native Web view responds on the same session. Synthetic-only QA; no cleanup-quality parity
or broad real-photo quality claim.

The network sees a bounded 800×800 context crop. Fine detail, large removals, repeated patterns
and complex boundaries can still need another stroke or manual Heal/Clone. There is no automatic
object/distraction detection or guaranteed parity with Photos/Lightroom cleanup quality.

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
including nested folders. Live Photos contribute the still image; videos are excluded. One
initial metadata snapshot establishes a persistent public
[`PHPhotoLibraryChangeObserver`](https://developer.apple.com/documentation/photos/phphotolibrarychangeobserver)
subscription. It retains the PhotoKit fetch results and uses `PHChange.changeDetails(for:)` to
send additions, removals and changed metadata only. Album names, hierarchy and membership follow
Photos changes, including imports and edited-copy saves. Resource-availability and preview
notifications with identical projected metadata are ignored. Returning to the Mac window or
focusing a browser never rereads the library, and no periodic Photos scan or polling request runs.
PhotoKit can explicitly invalidate an incremental fetch result; that event reconciles its current
metadata. **Refresh Photos** manually reconnects and rereads, including after permission changes
or a stopped observer. Errors preserve the current library and show a message; they never cause
an automatic restart loop.

The helper registers before fetching and serializes subsequent change callbacks so startup
changes are not lost. Small JSON-line headers reference a size-bounded temporary metadata file;
acknowledgements and a bounded receiver provide backpressure while changes coalesce. The reader
wakes the native UI on events. Updates arriving during a slider gesture wait until it ends. The
engine validates the complete batch before applying it, preserves untouched photo identities,
selection, undo history and decoded pixels for ordinary metadata updates, and invalidates pixels
only for changed source revisions or removed photos. As with manual reconciliation, asset removal
clears the transient undo/redo stacks to prevent restoring Photos-owned records; saved adjustment
and history descriptions remain. Adjustment JSON remains independent. Closing or switching libraries
unregisters the observer and terminates/reaps its helper. The desktop Web view uses this same
subscription. Status is available in `ui.inspect.applePhotos.refresh`: `subscription`
(`starting`, `watching`, `stopped`), `started` (initial/manual connections), `changesReceived`,
`loading`, and `waitingForGesture`.

APPLE.9 validation (2026-10-15): native regression tests cover focus changes, bounded observer
headers/cleanup, atomic deltas, unchanged record identities, removals, and gesture deferral. An
isolated native app with a counted synthetic bridge applies three add/rename/remove events
through the real subscription transport, retains the selected decoded original and edits/undo,
stays on one connection while idle, preserves the library on interruption, and reconnects only
on the manual action. No original is retrieved except the one explicitly requested in that test.

The grid and filmstrip show locally cached, rendered **Photos previews** via
`PHImageManager.requestImage`, with network access disabled. Browsing does not retrieve RAW
resources. These thumbnails never become development or export sources, so they cannot replace
original pixels or apply baked Photos adjustments twice. A cached LightCraft render is preferred
when available. After downloading an original, the grid renders the current LightCraft edits
from the genuine cached original, even when its separate thumbnail-level source was evicted.

One persistent **system-service helper** serializes Photos previews, RAW decoding and Vision,
with one additional process for the PhotoKit observer. Browsing no longer spawns a process for
each image. Bounded requests/replies and per-call timeouts terminate and reap a failed helper;
operations are never replayed automatically. Normal image/permission errors keep a healthy
service alive. **Refresh Photos** explicitly reconnects a failed service; closing the app stops
both helpers. `ui.inspect.applePhotos.services` reports the PID, process starts and request count.

Locally rendered browsing pixels have a bounded memory cache keyed only by the asset identifier,
Photos source revision and a canonical 512-pixel size. Grid/filmstrip sizes reuse and resample
those pixels. Settings, slider changes and changes to other photos never invalidate that source
cache or trigger another PhotoKit read. Concurrent misses are coalesced. Unavailable previews also
have a bounded failure cache, so
retouches never retry every cloud-only image. Refresh Photos explicitly retries failures without
clearing successful pixels. These pixels remain browsing-only; the decoded original and edit-render caches are separate. Edits rebuild previews
from the cached original without reading Photos again. A genuine source-revision change misses
only that photo's cache; explicit Clear Previews changes its generation, so old jobs cannot
repopulate it. Cache entries may be evicted under the memory budget and do not survive app exit.
Queued thumbnails outside every panel's visible/prefetch range are dropped at the end of each
layout pass, including when scrolling or hiding the filmstrip; current reads are limited by the
render workers and the single native service.

Originals are retrieved through `PHAssetResourceManager` **only after the per-photo Download
original action**, including when the original is already local. Selection, neighbour prefetch,
mask/auto-adjustment preparation and export cannot implicitly request a resource. The editing
view offers that action until decoded original pixels are available; Photos browsing previews
remain in the library. RAW+JPEG pairs prefer the
RAW original, including `alternatePhoto`; rendered assets use their current Photos rendition.
The public PhotoKit resource API requires a short-lived decode file. Its enclosing temporary
directory is removed after decoding, errors or bridge timeouts. There is no permanent import of
Photos source files and no LightCraft `catalog.log` or `catalog.snap` for the Photos session.
Rendered previews and active decoded sources have memory caches. Explicitly downloaded originals
also have lossless local pixel backing, described below.

Use **Download original** on an individual grid photo, or on the selected photo in the sidebar,
to load just that original from Photos, using iCloud if needed. The background task shows **Downloading…** and
**Retry download** on failure. Network access is enabled only for this explicit request; browsing,
editing and exports never enable original retrieval automatically. Recipe reads also keep network
access disabled. This policy is enforced before the Rust helper call and before native PhotoKit
resource access, beyond PhotoKit's [network access option](https://developer.apple.com/documentation/photos/phassetresourcerequestoptions/isnetworkaccessallowed),
which alone does not prohibit reading a local resource. Downloaded pixels are cached in memory
at full, preview and thumbnail sizes. Their unedited linear Rec.2020 f32 pixels and Apple decoder
facts are also written atomically under `Photos Adjustments/Original Cache/`, keyed by asset and
source revision. This separate, recreatable cache has an 8 GiB budget and evicts older entries.
Its header-only index loads on the library worker; UI availability checks perform no disk I/O.
An export of an original evicted from RAM, or after restarting the app, reads this lossless local
backing without PhotoKit, RAW decoding or network access. Editing never rewrites it. A changed
source revision cannot reuse old pixels; an uncached original still needs the per-photo button.
Exports never silently substitute a lower-resolution image. **Import Photos…** and dropped image files create
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

Use **File → Export to Apple Photos (JPEG)**, the same item near the top of a photo’s
right-click menu (grid, detail or filmstrip), the sidebar export button, or **⌘S** to render a
full-size JPEG at quality 100 into a new asset in the Photos album **LightCraft**. The export
uses the cropped native resolution, sRGB with its ICC profile, 4:4:4 chroma, and no file-size
limit. It ignores the normal disk-export dialog’s resize/quality settings. The context menu
exports the clicked photo even when another selected photo is active. The desktop Web view
uses these same menus and the same background export job. Opening a menu never downloads an
original or starts an export; an unavailable original still needs its explicit download action.
The source’s capture date and location are carried onto the copy. Every save creates a new copy. The new
asset receives `PHAdjustmentData` with the original’s local identifier and the edit recipe or its local snapshot reference,
using `PHContentEditingOutput(placeholderForCreatedAsset:)` in the same PhotoKit change block.
Only asset creation requests are issued; no source edit or delete requests are issued.
If recipe attachment fails after the rendered copy is saved, LightCraft reports that partial
result and retains the copy and local draft.

APPLE.14 (2026-10-16) fixes the native service crash caused by PhotoKit rejecting large
adjustment data (observed `NSInvalidArgumentException: Adjustment data is too large`). Recipes
up to 64 KB remain embedded; larger descriptions, including masks, history and AI repair pixels,
are saved atomically as immutable SHA-256-named JSON files under
`~/Library/Application Support/LightCraft/Photos Adjustments/Export Recipes/`. Photos receives
only a small, source-bound reference. The helper rejects oversized metadata before accessing
Photos, so malformed requests return an error and leave the persistent worker alive. No failed
write is replayed automatically. Small embedded recipes from the previous version still reopen.

**Back up Export Recipes with Photos Adjustments.** Large descriptions do not travel through
iCloud with the JPEG; reopening such a copy on another Mac requires restoring this folder.
Reopening checks the complete file's length, SHA-256 and original identifier; missing or corrupt
files produce an error rather than applying incomplete edits. Later edits to the original's local
draft do not change the saved copy's snapshot. Synthetic regression tests round-trip an 8 MB
description and keep the native worker alive after an oversized direct save request, without
requesting Photos access or creating an asset in a personal library.

Copies display their already-rendered image with default settings. To resume a saved edit,
select a LightCraft copy and use **Photo → Reopen Photos Adjustments**: the recipe selects its
original and restores its settings. The image is never developed a second time on top of its
baked edits. Reopening requires the original to be accessible in the current Photos library;
PhotoKit local identifiers are specific to that library/device.

APPLE.13 (2026-10-16) puts the direct Photos action alongside File’s other export commands
and in the shared grid/detail/filmstrip context menu. Synthetic engine tests check actual JPEG
quantization at quality 100, full-size rendering from a genuine original rather than its smaller
previews, recipe/source preservation, and explicit clicked-photo targeting without calling Photos.

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
isolation, stale-download rejection, library-preview/original separation, full-size export after
RAM eviction/restart, exact cached float/decoder-fact recovery, truncated/failed cache writes and
the full-size copy render/recipe without requesting access to a personal photothèque. A SHA-256-pinned public CC0
Canon R8 C-RAW verifies native 6000×4000 decoding and preview colour conversion. UI snapshots
verify the Photos controls in the normal sidebar and download buttons on separate grid photos.
A native 500-photo synthetic run also checks one shared service PID across 190 requests, zero new
Photos reads through slider/mask/undo/redo changes, cancelled filmstrip work (peak queue 16),
metadata-only events with no pixel reads, and only one reread for a changed source. Downloading
another original and restarting retain byte-identical exported PNGs without another original read.
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
