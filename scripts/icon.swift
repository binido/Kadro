// Рисует иконку приложения из SVG Lucide: белый символ на тёмном скруглённом квадрате.
// Использование: swift scripts/icon.swift <symbol.svg> <out.iconset>
import AppKit

let args = CommandLine.arguments
guard args.count == 3, let symbol = NSImage(contentsOfFile: args[1]) else {
    FileHandle.standardError.write("usage: icon.swift <symbol.svg> <out.iconset>\n".data(using: .utf8)!)
    exit(1)
}
let output = URL(fileURLWithPath: args[2])
try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)

func render(_ pixels: Int) -> Data {
    let rep = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: pixels, pixelsHigh: pixels, bitsPerSample: 8,
        samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
        bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    let size = CGFloat(pixels)
    // Сетка иконок macOS: тело 824 из 1024, скругление 185.
    let body = NSRect(x: size * 100 / 1024, y: size * 100 / 1024, width: size * 824 / 1024, height: size * 824 / 1024)
    let radius = size * 185 / 1024
    NSGradient(starting: NSColor(white: 0.24, alpha: 1), ending: NSColor(white: 0.10, alpha: 1))!
        .draw(in: NSBezierPath(roundedRect: body, xRadius: radius, yRadius: radius), angle: -90)

    let glyph = body.insetBy(dx: body.width * 0.22, dy: body.height * 0.22)
    let tinted = NSImage(size: glyph.size, flipped: false) { rect in
        symbol.draw(in: rect)
        NSColor.white.set()
        rect.fill(using: .sourceAtop)
        return true
    }
    tinted.draw(in: glyph)
    NSGraphicsContext.restoreGraphicsState()
    return rep.representation(using: .png, properties: [:])!
}

for points in [16, 32, 128, 256, 512] {
    try render(points).write(to: output.appendingPathComponent("icon_\(points)x\(points).png"))
    try render(points * 2).write(to: output.appendingPathComponent("icon_\(points)x\(points)@2x.png"))
}
