# Contributing to Kadro

Thanks for helping make Kadro a better screenshot tool. Bug reports, testing, documentation,
translations and focused code changes are welcome. You do not need to write Rust to contribute.

## Before you start

Search [existing issues](https://github.com/binido/Kadro/issues) and check the [roadmap](ROADMAP.md).
For a large feature or a change to capture, saving or permissions, open an issue describing the
problem and intended behaviour before investing in an implementation. Small fixes can go straight
into a pull request.

Use English for shared documentation and pull request descriptions where possible. Keep
[README.md](README.md) and [README.ru.md](README.ru.md) in sync when changing user-facing information.
The detailed feature and settings reference lives in [docs/guide.md](docs/guide.md).

## Report a bug or suggest a feature

Use the [bug report](https://github.com/binido/Kadro/issues/new?template=bug_report.yml) or
[feature request](https://github.com/binido/Kadro/issues/new?template=feature_request.yml) form.
A useful bug report includes the Kadro and macOS versions, Mac chip, reproduction steps,
expected and actual results, and whether it happens consistently.

For capture or positioning bugs, include display count, resolution and scaling. For shortcut bugs,
include the keys and any conflicting macOS shortcuts. A short recording or screenshot helps;
remove personal content, credentials and private share links before posting it.

## Set up a development build

Use a Mac running macOS 14 or later, a current stable Rust toolchain and Xcode Command Line Tools.
The published DMG is built for Apple Silicon. Intel builds are not covered by the release workflow.

```sh
git clone https://github.com/YOUR-USERNAME/Kadro.git
cd Kadro
cargo run
```

Fork the repository first if you plan to submit a pull request. `cargo run` starts the unbundled app;
Screen Recording permission is associated with the launching terminal, and launch at login is
unavailable. Avoid running a second copy alongside an installed Kadro when testing global shortcuts.

To test the actual application bundle:

```sh
./scripts/bundle.sh
```

This creates `build/Kadro.app`. Copy it to `/Applications` if you need to test launch at login.
The default signature is ad hoc. Rebuilding can reset Screen Recording permission; use
`KADRO_SIGN_IDENTITY` with your own signing identity when appropriate. Do not commit certificates
or signing credentials. Automatic scrolling also requires Accessibility permission.

## Find your way around

| Area | Files |
| --- | --- |
| App lifecycle, menu, shortcuts, settings | `src/main.rs`, `src/hotkeys.rs`, `src/settings.rs`, `src/prefs.rs` |
| Capture and selection | `src/capture.rs`, `src/overlay.rs`, `src/scroll.rs`, `src/stitch.rs` |
| Editor and drawing | `src/editor.rs`, `src/document.rs`, `src/annotation.rs`, `src/paint.rs`, `src/backdrop.rs` |
| Saving, preview, pins and library | `src/output.rs`, `src/preview.rs`, `src/pin.rs`, `src/library.rs`, `src/browser.rs` |
| OCR, uploads, automation and updates | `src/recognize.rs`, `src/upload.rs`, `src/sigv4.rs`, `src/links.rs`, `src/updates.rs` |

Kadro uses Rust with AppKit and Apple frameworks through `objc2`. Keep the interface native;
reuse the existing controls, icons and helpers. Avoid new dependencies or abstractions for work
that the platform or current code already handles. Keep unrelated cleanup out of a focused fix.

## Check your change

Run the checks used by the release workflow from the repository root:

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test
```

Tests live alongside the Rust modules; OCR and scrolling fixtures are in `tests/fixtures`.
Add a small regression test for non-trivial logic such as coordinates, naming, parsing or image
stitching. A documentation-only change needs link, spelling and rendering checks rather than new
Rust tests.

For UI changes, exercise the affected path in the app. Check the menu and shortcut entry points,
undo/redo where relevant, and that copied or saved output matches the preview. For capture changes,
check Retina scaling and multiple displays when hardware is available. Say what you could not test;
do not mark unavailable hardware as tested.

For documentation images, use real app windows and non-sensitive sample content. Keep files in
`docs/images`, add useful alt text, and note the app version in [the image notes](docs/images/README.md).
Do not use generated mockups as evidence of working features.

## Submit a pull request

1. Create a branch in your fork and keep the change focused on one problem.
2. Explain the problem, the resulting behaviour and how you checked it.
3. Include before/after screenshots for visible changes and update the relevant documentation.
4. Note any limitations, follow-up work or checks you could not run.

The pull request template is a short checklist, not a requirement to invent tests for prose.
Keep the existing MIT license and third-party notices intact. Version bumps, tags and release
publication are coordinated by the maintainer; the release procedure is in [ROADMAP.md](ROADMAP.md#versioning).
