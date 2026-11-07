# Kadro

A lightweight, native macOS screenshot utility that lives in the menu bar.

## Features

Capture:

- A selected region. While selecting, Kadro shows the size in pixels and a magnifier loupe at the pointer. Arrow keys move the dragged corner by one point (Shift: ten), and holding Shift keeps the selection square.
- The last selected region again, without selecting.
- The entire screen under the pointer, right away or after a 3-second countdown in the menu bar.
- A window picked with the pointer, or the active window straight away. Behind a captured window: nothing (no shadow), transparency with its shadow, the wallpaper of its screen, or a chosen colour.
- Content taller than the screen with Scrolling Capture: select an area, then let Kadro scroll it down or up, or scroll it yourself. Frames are stitched into one image, fixed headers and footers appear once. It stops at the end, at the height limit, or on Return; Esc cancels. Automatic scrolling needs Accessibility access, scrolling yourself does not.
- Multiple displays and Retina displays, each at its own scale.

Output:

- After a capture: show a preview, copy to the clipboard, save to a folder, in any combination. At least one is always on; the UI does not allow turning off the last one.
- The preview is a thumbnail in the corner of the screen the capture came from. It never takes focus. Hovering shows Copy, Save and Close, and dragging it drops the screenshot as a file into other apps. It hides after a few seconds or a click elsewhere, or only with Close.
- A capture is never lost silently: if the preview hides or is replaced by a new capture before the screenshot was copied, saved or dragged, Kadro copies it. Only Close discards it.
- Without the preview, copying and saving are confirmed by a pop-up, a macOS notification, or not at all.

Editor:

- Opens from the preview (Edit), the menu (More → Edit Last Screenshot, Open File…, Open from Clipboard), a shortcut, or right after a capture when Show after capture is set to Editor. PNG, JPEG, HEIC and GIF files can be opened with Kadro from Finder.
- Zoom with pinch or ⌘ + scroll (or the plain scroll wheel, if chosen), ⌘+ / ⌘− / ⌘0 (100%) / ⌘9 (fit), or the toolbar. 100% is one image pixel per screen pixel.
- Crop (C or the toolbar, drag, Return), Uncrop to bring back everything cropped away, and Resize with a locked aspect ratio.
- Annotations from the tool column on the left or a single key: Select (V), Arrow (A), Line (L), Rectangle (R), Oval (O), Pen (P), Highlighter (H), Text (T), Counter (N), Blur (B), Pixelate (X), Spotlight (S) and Magnifier (M).
- Arrows with a filled or open head or heads on both ends, solid or dashed lines, outlined, rounded or filled rectangles, outlined or filled ovals, text plain or on a background, numbered counters that renumber when one is removed, blur and pixelate in three strengths, spotlights that dim everything outside rectangles or ellipses, and a 2×, 3× or 4× circular magnifier.
- Colour, size and style in the toolbar apply to the next annotation and to the selected one. Every annotation can be selected, moved by its body, reshaped by its ends or corners, restyled and deleted (Delete) until Rasterize flattens them into the image. Double-click a text to edit it.
- Ruler (U): drag to measure, or click to measure the plain area under the pointer from edge to edge, across and along. Lengths show in pixels or points.
- Color Picker (I): a box next to the pointer shows the colour under it, and a click copies it as HEX, RGB or HSL (sRGB).
- Add Capture takes a region capture and places it on the canvas, where it moves and resizes like any annotation. Moved annotations snap to the edges and centres of the canvas and of each other.
- Export GIF saves a two-frame GIF that alternates the image with and without annotations every second.
- Backdrop puts the screenshot on a gradient, mesh gradient, solid colour or a blurred copy of itself, with padding, shadow, corner radius for the screenshot and for the whole image, an inset in the screenshot's edge colour, a fixed aspect ratio and trimming of plain edges. Randomize and Reverse change the gradient. The backdrop stays adjustable and can be undone like any edit.
- Every edit can be undone (⌘Z) and redone (⇧⌘Z). Edits never lose pixels: the result is always redrawn from the original capture, and the exported image is the canvas pixel for pixel.
- Copy (⌘C), Save to the screenshots folder (⌘S), Save As (⇧⌘S, format from the extension), Print (⌘P). Esc copies, saves or just closes, as chosen in settings.

Text and codes:

- Recognize Text (a shortcut or the menu) selects an area and copies its text without saving an image. The preview and the editor have the same action for the whole screenshot.
- QR codes and barcodes are read in the same pass: a link is offered to open or copy, other contents are copied.
- Recognition uses Apple Vision on the Mac, with no network. The language is the system's or one chosen in settings, and lines can be joined into one.

Upload:

- Upload a screenshot from the preview, the editor or the menu (More → Upload Last Screenshot) to any S3-compatible storage: Amazon S3, Cloudflare R2, Backblaze B2, MinIO and others. When the upload finishes, the link is on the clipboard.
- The link is temporary (1 hour, 1 day or 7 days), public (the bucket's address or your own domain), or none.
- Requests are signed with AWS Signature V4 by Kadro itself, with no SDK. The access and secret keys are kept in the Keychain. Test Connection uploads and deletes a small file to check the keys, the bucket and write access.

Library:

- The Library window (menu or shortcut) shows screenshots saved by Kadro as small, medium or large tiles or as a list. On first open it also brings in the images already in the screenshots folder.
- Search by name, date and the text on the screenshot. The text is recognized in the background, offline.
- Double-click or Return opens a screenshot in the editor; the context menu also has Copy, Show in Finder, Upload and Move to Trash.
- The index is a small file in Application Support and thumbnails are cached, so thousands of screenshots open at once. Text recognition runs in a separate short-lived process, so Kadro's own memory stays low.

Pinned screenshots:

- Pin a capture from its preview, or pin the last screenshot from the menu (More) or a shortcut. The pin opens exactly where the capture was taken, at 100% (one image pixel per screen pixel).
- Pins float above other windows on every Space and over full-screen apps, without taking focus from them. Drag anywhere to move one.
- The context menu has Copy, 50% / 100% / 200%, Semi-transparent and Close. ⌘C copies; Esc or a double-click closes. Several pins can be open at once.
- The last screenshot is kept as a file in the temporary folder, not in memory.
- PNG, JPEG, HEIC, or Auto, which saves text and interface captures as PNG and photo-like ones as JPEG.
- Optional downscaling of Retina captures to 1x when saving.
- File names from a template with `{date}`, `{time}`, `{mode}` and `{app}`. The default is `Kadro {date} at {time}`.
- Saved files are marked as screenshots for Finder and Spotlight, like the ones macOS takes.
- The pointer appears in full-screen captures only (Auto), in every capture, or never.

App:

- Native menu bar app with configurable global shortcuts and launch at login.
- The menu keeps the everyday actions on top: region, window, full screen, scrolling capture, Recognize Text, Library, Settings. The rest (repeat last region, delayed and active window capture, pin, edit and upload last screenshot, Open File…, Open from Clipboard, Check for Updates…) is under More.
- The menu bar icon can be hidden. Opening Kadro again from Applications shows Settings, and the Show Kadro Menu shortcut opens the menu at the pointer.
- Check for Updates compares with the latest GitHub release and offers to open its page; an automatic check runs once a day unless turned off.

## Links

`kadro://` links let other apps, scripts and Shortcuts drive Kadro. They are off by default; in Settings → General they can be allowed or set to ask before every link.

| Link | What it does |
|---|---|
| `kadro://capture/region` | Capture a region |
| `kadro://capture/repeat` | Capture the last region again |
| `kadro://capture/screen` | Capture the screen under the pointer |
| `kadro://capture/delayed` | Capture the screen after 3 seconds |
| `kadro://capture/window` | Pick a window to capture |
| `kadro://capture/active-window` | Capture the active window |
| `kadro://capture/scrolling` | Scrolling capture |
| `kadro://recognize` | Select an area and copy its text |
| `kadro://pin` | Pin the last screenshot |
| `kadro://edit` | Open the last screenshot in the editor |
| `kadro://upload` | Upload the last screenshot |
| `kadro://library` | Open the library |
| `kadro://open?path=/absolute/path.png` | Open an image in the editor |
| `kadro://clipboard` | Open the image on the clipboard in the editor |
| `kadro://settings` | Open Settings |
| `kadro://set?key=value&…` | Change settings |

Settings that `kadro://set` accepts: `format` (`png`, `jpeg`, `heic`, `auto`), `cursor` (`auto`, `show`, `hide`), `after` (`preview`, `editor`, `nothing`), `hide-preview` (`auto`, `manual`), `window` (`clean`, `shadow`, `wallpaper`, `color`), `copy` and `save` (`on`, `off`), `folder` (an absolute path). Every value is checked before anything changes, for example `kadro://set?format=png&copy=on&save=off`.

## UI

The app runs from the menu bar.

A compact native preferences window uses Space Grotesk typography and Lucide icons on top of standard macOS controls and behavior.

**General**

- Launch at login
- Hide the menu bar icon
- kadro:// links: off, ask, allow
- Daily update check, Check Now

**Capture**

- Show after capture: preview, editor, nothing
- Where the screenshot goes: copy, save
- Hide preview automatically or manually
- Confirmation without preview: pop-up, notification, none
- Cursor in screenshots
- What is behind captured windows: no shadow, transparent, wallpaper, colour
- Scrolling capture height limit, scroll speed, reversed direction

**Saving**

- Screenshots folder, file name template, save format, Retina downscaling
- Library on or off, how many screenshots it remembers
- Text recognition language, joining recognized lines into one

**Editor**

- What Esc does: copy and close, save and close, just close
- Open screenshots fitted to the window or at 100% when they fit
- Keep the editor above other windows
- Zoom with the scroll wheel

**Upload**

- Endpoint, region, bucket, path prefix
- Access key and secret key, stored in the Keychain
- Link to copy after upload: none, temporary (1 hour, 1 day, 7 days), public with an optional custom address
- Test Connection

**Shortcuts**

- Capture region, repeat last region
- Capture full screen, now or in 3 seconds
- Capture window, capture active window
- Scrolling capture, pin last screenshot, edit last screenshot, upload last screenshot, library, recognize text, show the Kadro menu
- Every shortcut can be recorded or cleared, with conflict detection against Kadro, macOS and other apps

**About**

- Version, author, MIT License and a link to the repository

## Technical constraints

- Rust is the primary language.
- AppKit and Apple frameworks are used through maintained Rust bindings (the `objc2` crate family).
- No Tauri, Electron, webviews, or HTML/CSS.
- Capture uses ScreenCaptureKit and Core Graphics, picking the right API per capture mode.
- Clipboard and image encoding use native APIs (`NSPasteboard`, ImageIO).
- Fast, memory-efficient, and near-zero resource use while idle.

## Development order

1. Native app lifecycle and menu bar
2. Full-screen capture
3. Clipboard and file saving
4. Region selection
5. Window selection
6. Preferences and keyboard shortcuts
7. Multi-monitor support, permissions, and Retina handling
8. Performance optimization and packaging

These stages made up version 0.1. Later versions are planned in [ROADMAP.md](ROADMAP.md).

## Architecture

Keep it simple.

## Building

Requires macOS 14 or later, Rust, and the Xcode Command Line Tools.

```sh
./scripts/bundle.sh
```

This builds `build/Kadro.app`. Copy it to `/Applications` before turning on Launch at login.

The app is signed ad-hoc unless `KADRO_SIGN_IDENTITY` names a signing identity. macOS ties Screen Recording access to the signature, so an ad-hoc build asks for access again after every rebuild.

During development, `cargo run` starts Kadro without a bundle. It then inherits Screen Recording access from the terminal, and Launch at login is unavailable.

## Licenses

Kadro is MIT licensed. It bundles [Space Grotesk](https://github.com/floriankarsten/space-grotesk) under the SIL Open Font License (`assets/SpaceGrotesk-OFL.txt`) and [Lucide](https://lucide.dev) icons under the ISC License (`assets/icons/LICENSE`).
