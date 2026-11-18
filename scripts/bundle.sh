#!/bin/sh
# Builds build/Kadro.app from a release build.
# Signing uses KADRO_SIGN_IDENTITY, ad hoc by default. With an ad hoc signature macOS
# resets Screen Recording permission after every rebuild.
set -eu
cd "$(dirname "$0")/.."

cargo build --release --locked
target_dir=$(cargo metadata --format-version 1 --no-deps | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
app=build/Kadro.app

rm -rf "$app" build/AppIcon.iconset
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$target_dir/release/kadro" "$app/Contents/MacOS/Kadro"

swift scripts/icon.swift assets/app-icon.svg build/AppIcon.iconset
iconutil -c icns build/AppIcon.iconset -o "$app/Contents/Resources/AppIcon.icns"

cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key><string>com.binido.kadro</string>
    <key>CFBundleName</key><string>Kadro</string>
    <key>CFBundleDisplayName</key><string>Kadro</string>
    <key>CFBundleExecutable</key><string>Kadro</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>LSMinimumSystemVersion</key><string>14.0</string>
    <key>LSUIElement</key><true/>
    <key>NSHighResolutionCapable</key><true/>
    <key>CFBundleURLTypes</key>
    <array>
        <dict>
            <key>CFBundleURLName</key><string>com.binido.kadro</string>
            <key>CFBundleURLSchemes</key><array><string>kadro</string></array>
        </dict>
    </array>
    <key>CFBundleDocumentTypes</key>
    <array>
        <dict>
            <key>CFBundleTypeName</key><string>Image</string>
            <key>CFBundleTypeRole</key><string>Editor</string>
            <!-- Alternate: Kadro shows up in "Open With" but does not take images over from Preview. -->
            <key>LSHandlerRank</key><string>Alternate</string>
            <key>LSItemContentTypes</key>
            <array>
                <string>public.png</string>
                <string>public.jpeg</string>
                <string>public.heic</string>
                <string>com.compuserve.gif</string>
            </array>
        </dict>
    </array>
</dict>
</plist>
EOF

codesign --force --options runtime --sign "${KADRO_SIGN_IDENTITY:--}" "$app"
codesign --verify --strict "$app"
echo "Built $app"
