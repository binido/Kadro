# Kadro roadmap

This file lives in the repository while Kadro grows from the 0.1 capture utility to 1.0. Each version below is one large feature. 1.0 is the release where every feature in this plan is shipped.

The feature list comes from the gap between Kadro and Shottr 1.9.3, read from Shottr's interface, settings, menus and URL scheme. Kadro reimplements the behavior. It does not copy Shottr's code, artwork or wording.

## Versioning

- `0.MINOR.0` adds one large feature from this plan.
- `0.MINOR.PATCH` fixes bugs in a released minor version and adds no features.
- `1.0.0` ships when every version below up to 0.13 is released and the 1.0 checklist passes.

To release a version:

1. Bump `version` in `Cargo.toml` in the PR that finishes the feature.
2. Merge the PR into `main`.
3. Push the tag: `git tag vX.Y.Z && git push origin vX.Y.Z`.
4. The release workflow checks that the tag matches `Cargo.toml`, builds `Kadro-X.Y.Z-arm64.dmg` and creates a draft release.
5. Review the draft on GitHub and publish it.

## Done means

A version is done when all of these hold:

- Every feature listed for it works from the menu bar menu, its shortcut and its settings, where the version adds those.
- New settings appear in the preferences window in the existing layout: right-aligned labels, grouped sections, Space Grotesk and Lucide icons on native controls.
- Non-trivial logic has a test: coordinate math, file naming, format choice, parsing.
- `cargo clippy --all-targets -- -D warnings` and `cargo test` pass.
- Idle CPU stays at 0%. With no editor or library window open, idle memory stays under 20 MB until the editor is first opened and under 30 MB after that. The first editor window loads AppKit window and toolbar machinery that stays resident, about 13 MB, and must not grow with further sessions.
- README describes the new feature.
- The version is tagged and its draft release has the DMG.

## Status

| Version | Feature | Status |
|---|---|---|
| 0.1 | Capture utility | Done |
| 0.2 | Capture modes and precision | Done |
| 0.3 | After-capture preview | Done |
| 0.4 | Pinned screenshots | Done |
| 0.5 | Editor foundation | Done |
| 0.6 | Annotation tools | Done |
| 0.7 | Measuring and composing | Done |
| 0.8 | Backgrounds and window frames | Done |
| 0.9 | Text and QR recognition | Done |
| 0.10 | Scrolling capture | Planned |
| 0.11 | Upload and share links | Planned |
| 0.12 | Screenshot library | Planned |
| 0.13 | URL scheme and app behavior | Planned |
| 1.0 | Complete, polished release | Planned |

Update the status column in the PR that ships each version.

---

## 0.1 — Capture utility (shipped)

- Capture a region, the full screen under the cursor, or a window picked with the mouse.
- Window captures have no shadow.
- Copy to the clipboard, save to a folder, or both. One of the two is always on.
- PNG, JPEG and HEIC. Optional downscaling of Retina captures to 1x when saving.
- Global shortcuts with conflict checks against Kadro, macOS and other apps.
- Preferences window with General and Shortcuts tabs.
- Launch at login, multiple displays, Screen Recording permission guidance.
- Signed app bundle, DMG installer, release workflow on version tags.

## 0.2 — Capture modes and precision (shipped)

**Goal:** every way of taking a screenshot exists before anything is built on top of the result.

Features:

- **Repeat area capture.** Captures the last selected area again without the overlay. Before any area capture, it explains that one is needed first.
- **Delayed capture.** Full-screen capture after a 3-second countdown, shown in the menu bar icon.
- **Active window capture.** Captures the frontmost window of the frontmost app straight away, without picking. The existing mouse-picked mode stays as "Any window".
- **Cursor in captures.** Setting with Include, Exclude and Auto. Auto includes the cursor only in full-screen captures.
- **Precise area selection.** The overlay shows the selection size in pixels and a magnifier loupe near the cursor. Arrow keys move the active corner by one point, Shift+arrows by ten. Shift while dragging keeps a square.
- **Finder metadata.** Saved files get the `kMDItemIsScreenCapture` and `kMDItemScreenCaptureGlobalRect` extended attributes, so Finder and Spotlight treat them as screenshots.
- **File name template.** Setting with tokens for date, time, capture mode and app name. The default stays `Kadro <date> at <time>`.
- **Auto format.** New format option that saves text-like captures as PNG and photo-like captures as JPEG, using a colour-count heuristic on a downscaled copy.

Shortcuts and menu: Repeat Area, Delayed Capture, Active Window.

Technical notes:

- The last area is stored as display ID plus rect in points; it is invalid if that display is gone.
- Active window comes from `NSWorkspace.frontmostApplication` and the first on-screen layer-0 window of that PID in the existing window list.
- Cursor capture uses `SCStreamConfiguration.showsCursor`.
- Extended attributes are written with `setxattr` after the file is saved; failure is logged and does not fail the save.

Done when: all four new capture paths save and copy correctly on Retina and non-Retina displays, and the template and Auto format have tests.

## 0.3 — After-capture preview (shipped)

**Goal:** after a capture, a small preview gives quick actions without opening a full window.

Features:

- **Preview thumbnail.** A floating thumbnail in the bottom corner of the display where the capture happened. Buttons: Copy, Save, Close. Later versions add Edit (0.5), Pin (0.4), Recognize Text (0.9) and Upload (0.11) to the same row.
- **Drag out.** Dragging the thumbnail drops the image as a file into other apps.
- **Hiding.** Setting: hide automatically after a few seconds or when clicking elsewhere, or keep until closed.
- **After-capture actions.** The General tab gets "After screenshot: Show preview / Copy / Save" as independent toggles. With every action off, the capture still copies, as today.
- **Confirmation style.** Setting: Kadro's own toast, macOS notification, or none. It confirms copy and save when the preview is off.

Technical notes:

- The thumbnail is a borderless non-activating `NSPanel` at the floating level, so it never steals focus from the app being captured.
- Dragging uses `NSDraggingSource` with a file promise written to a temporary folder.
- macOS notifications use `UNUserNotificationCenter` and ask for permission only when that style is chosen.

Done when: the preview appears within 150 ms of the capture, never takes focus, and closes correctly with several captures in a row.

## 0.4 — Pinned screenshots (shipped)

**Goal:** keep a screenshot floating above other windows as a reference.

Features:

- **Pin** from the preview thumbnail and from a new "Pin last screenshot" menu item and shortcut.
- The pinned window floats above normal windows, can be moved by dragging anywhere, and has no title bar.
- Context menu: Copy, Edit (once 0.5 ships), 50% / 100% / 200%, Semi-transparent, Close.
- Esc or double-click closes the focused pin. Several pins can be open at once.

Technical notes:

- Each pin is an `NSPanel` at the floating level with `isMovableByWindowBackground`.
- 100% means one image pixel per screen pixel on the display the pin is on.

Done when: pins survive switching Spaces and full-screen apps, and memory returns to the idle budget after all pins close.

## 0.5 — Editor foundation (shipped)

**Goal:** a native editor window that every later editing feature plugs into.

Features:

- **Editor window** opened from the preview (Edit), the menu ("Open Editor"), the "After screenshot: Show" toggle, "Open File…" and "Open from Clipboard". PNG, JPEG, HEIC and GIF files can also be opened with Kadro from Finder.
- **Canvas.** Zoom in and out, zoom to fit, 100%, zoom to selection, pinch and Command+scroll. Optional plain-scroll zoom setting.
- **Crop and uncrop.** Crop to a selection; uncrop restores the area cropped away.
- **Resize.** Width and height in pixels with a locked aspect ratio.
- **Undo and redo** for every edit.
- **Output.** Copy (Command+C), Save (Command+S) to the configured folder, Save As, Print.
- **Esc behaviour.** Setting: Esc closes the editor and copies, saves, or just closes.
- **Window settings.** Always on top, default zoom (fit or prefer 100%).

Technical notes:

- The document is the original capture plus an ordered list of edits; the image shown and exported is rendered from them. This makes undo, uncrop and later re-editing of annotations possible.
- Rendering uses Core Graphics in an `NSView` subclass; export reuses the existing ImageIO encoders.
- `Info.plist` declares document types for opening images.

Done when: a 6K Retina capture zooms and pans smoothly, undo covers every operation, and the exported file matches the canvas pixel for pixel.

## 0.6 — Annotation tools (shipped)

**Goal:** mark up screenshots in the editor.

Features:

- **Shapes:** arrow (several head styles), line (solid, dashed), rectangle, oval, each with stroke, fill and corner options.
- **Freehand** pen and **highlighter**.
- **Text** with size, colour and background options.
- **Counter**: numbered markers that renumber when one is removed.
- **Redact:** blur and pixelate, with strength.
- **Spotlight:** dims everything outside chosen areas.
- **Magnifier:** a circular zoomed inset of a chosen area.
- **Rasterize:** flattens annotations into the image.
- Shared toolbar for colour, size and style; tool shortcuts by single letters; every annotation stays selectable, movable and editable until rasterized.

Technical notes:

- Each tool is an edit in the 0.5 document model with its own hit-testing and drawing.
- Blur and pixelate use Core Image filters on the region under the shape.

Done when: every tool round-trips through undo, select, move and edit, and exported images match the canvas.

## 0.7 — Measuring and composing (shipped)

**Goal:** precise inspection and combining several captures.

Features:

- **Ruler.** Measure distances in pixels or points between two points, or across an edge detected under the cursor.
- **Colour picker.** Sample a pixel; copy it as HEX, RGB or HSL; a colour box shows the value under the cursor.
- **Add capture.** Take another capture from the editor and place it on the current canvas.
- **Alignment.** Snap placed captures and annotations to each other's edges and centres.
- **Two-frame GIF.** Export the overlay and the image under it as an alternating animated GIF.

Done when: ruler and picker values match pixel data in tests, and GIF export opens in Safari and Preview.

## 0.8 — Backgrounds and window frames (shipped)

**Goal:** presentation-ready screenshots.

Features:

- **Window capture background** setting: wallpaper, transparent, solid colour, or trimmed shadow (today's behaviour).
- **Backdrop editor** in the editor: background from gradients, mesh gradients or solid colours; padding, shadow, inner and outer corner radius, inset, blur, fixed aspect ratios, and auto-crop of the background around the content. Randomize and reverse for gradients.

Technical notes:

- Wallpaper comes from `NSWorkspace.desktopImageURL(for:)` for the window's screen.
- Backdrop is one more edit in the document model, so it stays adjustable.

Done when: each background option exports correctly for window, area and full-screen captures.

## 0.9 — Text and QR recognition (shipped)

**Goal:** get text and codes out of screenshots.

Features:

- **Recognize text** from the preview, the editor and an instant shortcut that selects an area and copies its text without saving an image.
- **QR and barcodes** in the same flow: links are offered to open, other payloads copy.
- Settings: primary recognition language, remove line breaks.

Technical notes:

- Apple Vision (`VNRecognizeTextRequest`, `VNDetectBarcodesRequest`); no network and no third-party models.
- Language list comes from `supportedRecognitionLanguages`.

Done when: recognition works offline on English and Russian samples kept as test fixtures.

## 0.10 — Scrolling capture

**Goal:** capture content taller than the screen.

Features:

- **Automatic scrolling capture.** Select an area; Kadro scrolls it and stitches frames until the end or the height limit. Esc stops it.
- **Scrolling up** variant and a **manual** mode where the user scrolls.
- Settings: maximum height in pixels, scroll speed, reverse scroll direction (for apps that invert scrolling).

Technical notes:

- Posting scroll events needs Accessibility access; Kadro asks for it only when this feature is first used, with the same kind of guidance as Screen Recording.
- Stitching finds the vertical offset between frames by comparing row signatures, ignoring fixed headers and footers.

Done when: long web pages, chat histories and code editors stitch without visible seams in manual tests, and the offset finder has tests on fixture frames.

## 0.11 — Upload and share links

**Goal:** share a screenshot as a link.

Features:

- **Upload** from the preview, the editor and a menu item; the link is copied when the upload finishes.
- **S3-compatible storage:** AWS, Cloudflare R2, Backblaze B2 and others. Settings: access key, secret key, bucket, region or endpoint, path prefix, Test Connection.
- **Links:** none, presigned (expiry up to 7 days), or public bucket URL.

Technical notes:

- Requests are signed with AWS Signature V4 using `URLSession`; no SDK.
- Keys are stored in the Keychain, never in user defaults.

Done when: uploads work against at least two providers, and signing has tests with known vectors.

## 0.12 — Screenshot library

**Goal:** find past screenshots.

Features:

- **Library window** listing screenshots saved by Kadro, as small, medium or large tiles or as a list.
- **Search** by name, date and recognized text (from 0.9).
- **Import** existing screenshots from the save folder on first open.
- Settings: enable or disable the library, limit on items kept in the index.

Technical notes:

- The index is a small file in Application Support; thumbnails are generated lazily and cached.
- Moving the save folder re-indexes after confirmation.

Done when: a folder with thousands of screenshots opens in under a second after the first index, and idle memory stays in budget while the window is closed.

## 0.13 — URL scheme and app behaviour

**Goal:** automation and the remaining app settings.

Features:

- **URL scheme** `kadro://` for every capture mode, OCR, pin, upload, opening files and the clipboard, and the settings that make sense to script. Off by default; Kadro asks before acting on a link from another app.
- **Hide menu bar icon,** with the preferences reachable by opening the app again.
- **Show Kadro** shortcut.
- **Update check** against GitHub Releases, offering to open the release page.

Done when: every documented URL works, and the URL list is in README.

## 1.0 — Complete release

**Goal:** everything above, finished and dependable.

Checklist:

- Every version from 0.2 to 0.13 is released.
- Full manual pass of every feature on a single display and on two displays with different scales.
- Performance budgets hold: idle CPU 0%, idle memory under 20 MB before the editor is first opened and under 30 MB after, capture to clipboard under 300 ms on a 5K display.
- No leak growth across 100 captures, editor sessions and pins.
- README covers every feature, shortcut, setting and URL.
- Signing with a Developer ID and notarization, if an Apple Developer account is available. Without it the DMG stays ad-hoc signed and README explains how to open it.

## Not planned

These exist in Shottr and are left out on purpose:

- Licensing, purchases and activation.
- Usage telemetry.
- Shottr Cloud, a service specific to Shottr. S3-compatible upload covers sharing.
- Splash and onboarding screens.
