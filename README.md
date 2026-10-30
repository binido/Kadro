# Kadro

A minimal, lightweight, native macOS screenshot utility focused entirely on fast screen capture. It lives in the menu bar, captures, and gets out of the way.

Kadro is not a full Shottr replacement. There is no editor, no annotation, and no preview window after a capture.

## MVP scope

1. Capture a selected screen region.
2. Capture the entire screen.
3. Capture an individual application window.
4. Copy captures to the system clipboard.
5. Save captures to a user-configurable directory.
6. Clipboard and file saving can be enabled at the same time.
7. Output formats: PNG, JPEG, HEIC.
8. Configurable global keyboard shortcuts.
9. Launch at login.
10. Native macOS menu bar interface.
11. Multiple monitors and Retina displays.
12. Optional downscaling of Retina captures when saving.
13. Window captures exclude the window shadow by default.

At least one destination (clipboard or file) must always be enabled. The UI does not allow turning off the last one.

## Out of scope

Not implemented, and not allowed to shape the architecture unless absolutely necessary:

- Screenshot editing
- Drawing or annotation tools
- Scrolling screenshots
- OCR
- QR code scanning
- Backgrounds or decorative frames
- Cloud uploads
- Licensing or payment systems

## UI

The app runs from the menu bar. No window stays open permanently, and nothing opens after a capture.

A compact native preferences window uses Space Grotesk typography and Lucide icons on top of standard macOS controls and behavior.

**General**

- Copy to clipboard
- Save to file
- Image format
- Screenshot directory
- Downscale Retina captures
- Launch at login

**Shortcuts**

- Capture region
- Capture full screen
- Capture window
- Editing shortcuts, with conflict detection

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

## Architecture

Keep it simple. No abstractions for features outside the MVP scope.

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
