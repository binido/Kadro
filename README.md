# Kadro

A lightweight, native macOS screenshot utility that lives in the menu bar.

## Features

Capture:

- A selected region. While selecting, Kadro shows the size in pixels and a magnifier loupe at the pointer. Arrow keys move the dragged corner by one point (Shift: ten), and holding Shift keeps the selection square.
- The last selected region again, without selecting.
- The entire screen under the pointer, right away or after a 3-second countdown in the menu bar.
- A window picked with the pointer, or the active window straight away. Window captures have no shadow.
- Multiple displays and Retina displays, each at its own scale.

Output:

- Copy to the clipboard, save to a folder, or both. At least one is always on; the UI does not allow turning off the last one.
- PNG, JPEG, HEIC, or Auto, which saves text and interface captures as PNG and photo-like ones as JPEG.
- Optional downscaling of Retina captures to 1x when saving.
- File names from a template with `{date}`, `{time}`, `{mode}` and `{app}`. The default is `Kadro {date} at {time}`.
- Saved files are marked as screenshots for Finder and Spotlight, like the ones macOS takes.
- The pointer appears in full-screen captures only (Auto), in every capture, or never.

App:

- Native menu bar app with configurable global shortcuts and launch at login.

## UI

The app runs from the menu bar.

A compact native preferences window uses Space Grotesk typography and Lucide icons on top of standard macOS controls and behavior.

**General**

- Screenshots folder, file name template, save format, Retina downscaling
- Cursor in screenshots
- Launch at login
- After screenshot: copy, save

**Shortcuts**

- Capture region, repeat last region
- Capture full screen, now or in 3 seconds
- Capture window, capture active window
- Every shortcut can be recorded or cleared, with conflict detection against Kadro, macOS and other apps

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
