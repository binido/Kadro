#!/bin/sh
# Собирает build/Kadro.app из релизной сборки.
# Подпись - KADRO_SIGN_IDENTITY, по умолчанию ad-hoc. У ad-hoc подписи macOS
# сбрасывает доступ к записи экрана после каждой пересборки.
set -eu
cd "$(dirname "$0")/.."

cargo build --release
target_dir=$(cargo metadata --format-version 1 --no-deps | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
app=build/Kadro.app

rm -rf "$app" build/AppIcon.iconset
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$target_dir/release/kadro" "$app/Contents/MacOS/Kadro"

swift scripts/icon.swift assets/icons/aperture.svg build/AppIcon.iconset
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
</dict>
</plist>
EOF

codesign --force --options runtime --sign "${KADRO_SIGN_IDENTITY:--}" "$app"
codesign --verify --strict "$app"
echo "Built $app"
