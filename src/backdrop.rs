//! Backdrop under the screenshot: background, padding, shadow and corners, plus the background behind a captured window.
//!
//! The backdrop is part of the editor document, so it can be changed and undone like any edit.
//! Sizes are in points and are converted to pixels with the screenshot scale.

use std::path::{Path, PathBuf};
use std::ptr;

use objc2::rc::Retained;
use objc2::{AnyThread, MainThreadMarker};
use objc2_app_kit::{
    NSBezierPath, NSColor, NSCompositingOperation, NSGradient, NSGraphicsContext, NSImage, NSImageInterpolation,
    NSShadow, NSWorkspace,
};
use objc2_core_foundation::{CFData, CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo,
    CGImageByteOrderInfo, kCGColorSpaceSRGB,
};
use objc2_core_image::{CIContext, CIImage};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use objc2_image_io::CGImageSource;

use crate::annotation::Color;
use crate::capture::Shot;
use crate::{measure, preview};

/// Canvas aspect ratios for the popup menu: label and width to height. 0 means unconstrained.
pub const ASPECTS: [(&str, f64); 7] = [
    ("Free", 0.0),
    ("1:1", 1.0),
    ("4:3", 4.0 / 3.0),
    ("3:2", 1.5),
    ("16:10", 1.6),
    ("16:9", 16.0 / 9.0),
    ("9:16", 9.0 / 16.0),
];

/// Background kinds for the popup menu, in `Fill::kind` order.
pub const KINDS: [&str; 4] = ["Gradient", "Mesh", "Solid", "Blurred"];

/// Curated gradient color pairs.
const GRADIENTS: [[u32; 2]; 10] = [
    [0xff_6a88, 0xff_99ac],
    [0x4f_acfe, 0x00_f2fe],
    [0x43_e97b, 0x38_f9d7],
    [0xfa_709a, 0xfe_e140],
    [0x66_7eea, 0x76_4ba2],
    [0xf8_9b29, 0xff_0f7b],
    [0x30_cfd0, 0x33_0867],
    [0xa1_8cd1, 0xfb_c2eb],
    [0x0b_a360, 0x3c_ba92],
    [0x2b_5876, 0x4e_4376],
];

/// Curated corner color sets: top left, top right, bottom right, bottom left.
const MESHES: [[u32; 4]; 6] = [
    [0xff_9a9e, 0xfa_d0c4, 0xa1_c4fd, 0xc2_e9fb],
    [0x84_fab0, 0x8f_d3f4, 0xa1_8cd1, 0xfb_c2eb],
    [0xf6_d365, 0xfd_a085, 0xf0_93fb, 0xf5_576c],
    [0x43_e97b, 0x38_f9d7, 0x4f_acfe, 0x00_f2fe],
    [0x30_cfd0, 0x66_7eea, 0x76_4ba2, 0x33_0867],
    [0xff_ecd2, 0xfc_b69f, 0xff_9a9e, 0xfe_cfef],
];

/// Background fill.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Fill {
    /// Linear gradient and its angle in degrees.
    Linear([Color; 2], f64),
    /// Smooth blend between the colors of the four corners.
    Mesh([Color; 4]),
    Solid(Color),
    /// The screenshot itself, stretched over the whole background and blurred.
    Blurred,
}

impl Fill {
    /// Index of the kind in `KINDS`.
    pub fn kind(self) -> usize {
        match self {
            Fill::Linear(..) => 0,
            Fill::Mesh(_) => 1,
            Fill::Solid(_) => 2,
            Fill::Blurred => 3,
        }
    }

    /// First curated background of `kind`. For a solid fill, `solid`.
    pub fn of_kind(kind: usize, solid: Color) -> Self {
        match kind {
            0 => Fill::Linear(GRADIENTS[0].map(hex), 135.0),
            1 => Fill::Mesh(MESHES[0].map(hex)),
            2 => Fill::Solid(solid),
            _ => Fill::Blurred,
        }
    }

    /// Another curated background of the same kind. `seed` picks which one.
    pub fn randomized(self, seed: u32) -> Self {
        let seed = seed as usize;
        match self {
            Fill::Linear(colors, _) => {
                let next = GRADIENTS.iter().map(|pair| pair.map(hex)).cycle().skip(seed % GRADIENTS.len());
                let pair = next.take(GRADIENTS.len()).find(|pair| *pair != colors).unwrap_or(colors);
                Fill::Linear(pair, [45.0, 90.0, 135.0, 180.0][seed / 7 % 4])
            }
            Fill::Mesh(colors) => {
                let next = MESHES.iter().map(|quad| quad.map(hex)).cycle().skip(seed % MESHES.len());
                Fill::Mesh(next.take(MESHES.len()).find(|quad| *quad != colors).unwrap_or(colors))
            }
            other => other,
        }
    }

    /// The gradient reversed.
    pub fn reversed(self) -> Self {
        match self {
            Fill::Linear([from, to], angle) => Fill::Linear([to, from], angle),
            Fill::Mesh([a, b, c, d]) => Fill::Mesh([c, d, a, b]),
            other => other,
        }
    }
}

fn hex(rgb: u32) -> Color {
    let channel = |shift: u32| f64::from((rgb >> shift) & 0xff) / 255.0;
    Color { red: channel(16), green: channel(8), blue: channel(0), alpha: 1.0 }
}

/// Screenshot backdrop in the editor.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Backdrop {
    pub fill: Fill,
    /// Padding from the screenshot to the canvas edge, in points.
    pub padding: f64,
    /// Shadow strength under the screenshot, from 0 to 1.
    pub shadow: f64,
    /// Screenshot corner radius, in points.
    pub radius: f64,
    /// Corner radius of the whole canvas, in points. The corners become transparent.
    pub outer_radius: f64,
    /// Margin around the screenshot filled with its edge color, in points.
    pub inset: f64,
    /// Index of the aspect ratio in `ASPECTS`.
    pub aspect: usize,
    /// Crop flat background off the screenshot edges before placing it on the backdrop.
    pub auto_crop: bool,
}

impl Default for Backdrop {
    fn default() -> Self {
        Backdrop {
            fill: Fill::of_kind(0, Color { red: 1.0, green: 1.0, blue: 1.0, alpha: 1.0 }),
            padding: 48.0,
            shadow: 0.5,
            radius: 10.0,
            outer_radius: 0.0,
            inset: 0.0,
            aspect: 0,
            auto_crop: false,
        }
    }
}

/// How many pixels of flat background to crop from each edge.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Trim {
    pub left: usize,
    pub top: usize,
    pub right: usize,
    pub bottom: usize,
}

/// Where everything sits on the backdrop canvas, in pixels, origin at the top left.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Layout {
    pub width: f64,
    pub height: f64,
    /// Screenshot card including the `inset` margin.
    pub card: (f64, f64, f64, f64),
    /// Where the top left pixel of the uncropped screenshot lands.
    pub content: (f64, f64),
}

/// Layout of a `width` x `height` screenshot on the backdrop.
pub fn layout(width: f64, height: f64, trim: Trim, backdrop: &Backdrop, scale: f64) -> Layout {
    let visible_width = width - (trim.left + trim.right) as f64;
    let visible_height = height - (trim.top + trim.bottom) as f64;
    let inset = (backdrop.inset * scale).round();
    let padding = (backdrop.padding * scale).round();
    let (card_width, card_height) = (visible_width + 2.0 * inset, visible_height + 2.0 * inset);
    let (mut canvas_width, mut canvas_height) = (card_width + 2.0 * padding, card_height + 2.0 * padding);
    let ratio = ASPECTS.get(backdrop.aspect).map_or(0.0, |aspect| aspect.1);
    // The canvas only grows in width or height, the screenshot never shrinks.
    if ratio > 0.0 {
        if canvas_width / canvas_height < ratio {
            canvas_width = (canvas_height * ratio).round();
        } else {
            canvas_height = (canvas_width / ratio).round();
        }
    }
    let card_x = ((canvas_width - card_width) / 2.0).round();
    let card_y = ((canvas_height - card_height) / 2.0).round();
    Layout {
        width: canvas_width,
        height: canvas_height,
        card: (card_x, card_y, card_width, card_height),
        content: (card_x + inset - trim.left as f64, card_y + inset - trim.top as f64),
    }
}

/// Screenshot edges filled with the same color as the top left pixel. At least one pixel remains.
pub fn trim(image: &CGImage) -> Trim {
    let (width, height) = (CGImage::width(Some(image)), CGImage::height(Some(image)));
    let Some(pixels) = measure::pixels(image) else { return Trim::default() };
    let reference = pixels[0];
    let plain = |p: &[u8; 3]| measure::similar(*p, reference);
    let row_plain = |y: usize| pixels[y * width..(y + 1) * width].iter().all(plain);
    let column_plain = |x: usize, top: usize, bottom: usize| (top..bottom).all(|y| plain(&pixels[y * width + x]));
    let top = (0..height).take_while(|y| row_plain(*y)).count().min(height - 1);
    let bottom = (top + 1..height).rev().take_while(|y| row_plain(*y)).count();
    let left = (0..width).take_while(|x| column_plain(*x, top, height - bottom)).count().min(width - 1);
    let right = (left + 1..width).rev().take_while(|x| column_plain(*x, top, height - bottom)).count();
    Trim { left, top, right, bottom }
}

/// Places the screenshot on the backdrop. Returns the image and the layout.
pub fn compose(content: &CGImage, backdrop: &Backdrop, scale: f64) -> Option<(CFRetained<CGImage>, Layout)> {
    let (width, height) = (CGImage::width(Some(content)) as f64, CGImage::height(Some(content)) as f64);
    let trimmed = if backdrop.auto_crop { trim(content) } else { Trim::default() };
    let layout = layout(width, height, trimmed, backdrop, scale);
    let context = bitmap(layout.width as usize, layout.height as usize)?;
    let ns = NSGraphicsContext::graphicsContextWithCGContext_flipped(&context, false);
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&ns));
    // AppKit has the origin at the bottom left here, the layout at the top.
    let rect = |x: f64, y: f64, w: f64, h: f64| NSRect::new(NSPoint::new(x, layout.height - y - h), NSSize::new(w, h));
    let canvas = rect(0.0, 0.0, layout.width, layout.height);
    let outer = backdrop.outer_radius * scale;
    NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(canvas, outer, outer).addClip();
    fill(&backdrop.fill, content, canvas, scale);

    let (card_x, card_y, card_width, card_height) = layout.card;
    let card = rect(card_x, card_y, card_width, card_height);
    let radius = backdrop.radius * scale;
    let shape = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(card, radius, radius);
    // The margin around the screenshot uses its edge color, so the screenshot looks seamless on the card.
    let edge = measure::pixel(content, trimmed.left, trimmed.top).unwrap_or([255, 255, 255]);
    let [red, green, blue] = edge.map(|c| f64::from(c) / 255.0);
    let edge = NSColor::colorWithSRGBRed_green_blue_alpha(red, green, blue, 1.0);
    if backdrop.shadow > 0.0 {
        NSGraphicsContext::saveGraphicsState_class();
        let shadow = NSShadow::new();
        shadow.setShadowBlurRadius(40.0 * scale * backdrop.shadow);
        shadow.setShadowOffset(NSSize::new(0.0, -12.0 * scale * backdrop.shadow));
        shadow.setShadowColor(Some(&NSColor::colorWithWhite_alpha(0.0, 0.25 + 0.35 * backdrop.shadow)));
        shadow.set();
        edge.setFill();
        shape.fill();
        NSGraphicsContext::restoreGraphicsState_class();
    }
    shape.addClip();
    edge.setFill();
    shape.fill();
    let (x, y) = layout.content;
    draw(content, rect(x, y, width, height));
    NSGraphicsContext::restoreGraphicsState_class();
    drop(ns);
    Some((CGBitmapContextCreateImage(Some(&context))?, layout))
}

fn fill(fill: &Fill, content: &CGImage, canvas: NSRect, scale: f64) {
    let ns = |c: Color| NSColor::colorWithSRGBRed_green_blue_alpha(c.red, c.green, c.blue, c.alpha);
    match *fill {
        Fill::Solid(color) => {
            ns(color).setFill();
            NSBezierPath::fillRect(canvas);
        }
        Fill::Linear([from, to], angle) => {
            let gradient = NSGradient::initWithStartingColor_endingColor(NSGradient::alloc(), &ns(from), &ns(to));
            // The AppKit angle runs counterclockwise, 135° goes from the top left corner to the bottom right.
            if let Some(gradient) = gradient {
                gradient.drawInRect_angle(canvas, -angle + 90.0);
            }
        }
        Fill::Mesh(corners) => {
            // A 2 x 2 image of the corner colors, stretched with smoothing, gives a smooth blend between them.
            // It is twice as wide as the canvas so its pixel centers land exactly on the corners.
            let Some(tiny) = corner_image(corners) else { return };
            let Some(context) = NSGraphicsContext::currentContext() else { return };
            context.saveGraphicsState();
            context.setImageInterpolation(NSImageInterpolation::Low);
            let size = canvas.size;
            let stretched = NSRect::new(
                NSPoint::new(canvas.origin.x - size.width / 2.0, canvas.origin.y - size.height / 2.0),
                NSSize::new(size.width * 2.0, size.height * 2.0),
            );
            draw(&tiny, stretched);
            context.restoreGraphicsState();
        }
        Fill::Blurred => {
            let (width, height) = (CGImage::width(Some(content)) as f64, CGImage::height(Some(content)) as f64);
            let cover = (canvas.size.width / width).max(canvas.size.height / height);
            let (w, h) = (width * cover, height * cover);
            let rect = NSRect::new(
                NSPoint::new(
                    canvas.origin.x + (canvas.size.width - w) / 2.0,
                    canvas.origin.y + (canvas.size.height - h) / 2.0,
                ),
                NSSize::new(w, h),
            );
            if let Some(blurred) = blur(content, 30.0 * scale / cover.max(0.01)) {
                draw(&blurred, rect);
            }
            NSColor::colorWithWhite_alpha(0.0, 0.15).setFill();
            NSBezierPath::fillRect(canvas);
        }
    }
}

/// Four-pixel image: top left, top right, bottom right, bottom left.
fn corner_image(corners: [Color; 4]) -> Option<CFRetained<CGImage>> {
    let byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let rgba = |c: Color| [byte(c.red), byte(c.green), byte(c.blue), 255];
    let [top_left, top_right, bottom_right, bottom_left] = corners.map(rgba);
    let mut bytes: Vec<u8> = [top_left, top_right, bottom_left, bottom_right].concat();
    // SAFETY: the system color space name constant lives for the whole process.
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }));
    // SAFETY: the 2 x 2 buffer of 4-byte pixels outlives the context, and the image copies it.
    let context = unsafe {
        CGBitmapContextCreate(bytes.as_mut_ptr().cast(), 2, 2, 8, 8, space.as_deref(), CGImageAlphaInfo::NoneSkipLast.0)
    }?;
    CGBitmapContextCreateImage(Some(&context))
}

fn blur(image: &CGImage, sigma: f64) -> Option<Retained<CGImage>> {
    let context = unsafe { CIContext::contextWithOptions(None) };
    let input = unsafe { CIImage::imageWithCGImage(image) };
    let output = unsafe { input.imageByClampingToExtent().imageByApplyingGaussianBlurWithSigma(sigma) };
    unsafe { context.createCGImage_fromRect(&output, input.extent()) }
}

fn draw(image: &CGImage, rect: NSRect) {
    let ns_image = NSImage::initWithCGImage_size(NSImage::alloc(), image, rect.size);
    unsafe {
        ns_image.drawInRect_fromRect_operation_fraction_respectFlipped_hints(
            rect,
            NSRect::ZERO,
            NSCompositingOperation::SourceOver,
            1.0,
            true,
            None,
        );
    }
}

/// Transparent sRGB canvas.
fn bitmap(width: usize, height: usize) -> Option<CFRetained<CGContext>> {
    // SAFETY: the system color space name constant lives for the whole process.
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }));
    // SAFETY: a null buffer makes Core Graphics allocate it.
    unsafe {
        CGBitmapContextCreate(
            ptr::null_mut(),
            width,
            height,
            8,
            0,
            space.as_deref(),
            CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0,
        )
    }
}

/// Largest window corner radius in points. A wider transparent edge is the window shape, not a corner.
const MAX_CORNER: f64 = 40.0;

/// Fills the rounded corners of a captured window to a rectangle with its own edge color.
/// Returns None if the canvas could not be created.
pub fn square_corners(image: &CGImage, scale: f64) -> Option<CFRetained<CGImage>> {
    let width = CGImage::width(Some(image));
    let mut pixels = vec![0u32; width * CGImage::height(Some(image))];
    let context = draw_argb(image, &mut pixels)?;
    fill_corners(&mut pixels, width, (MAX_CORNER * scale) as usize);
    CGBitmapContextCreateImage(Some(&context))
}

/// Crops fully transparent margins. None if there is nothing to crop or the canvas could not be created.
pub fn crop_transparent(image: &CGImage) -> Option<CFRetained<CGImage>> {
    let (width, height) = (CGImage::width(Some(image)), CGImage::height(Some(image)));
    let mut pixels = vec![0u32; width * height];
    draw_argb(image, &mut pixels)?;
    let (x, y, w, h) = visible_bounds(&pixels, width)?;
    if (w, h) == (width, height) {
        return None;
    }
    CGImage::with_image_in_rect(
        Some(image),
        CGRect::new(CGPoint::new(x as f64, y as f64), CGSize::new(w as f64, h as f64)),
    )
}

/// Draws the image into `pixels` in ARGB order. The context writes into the same buffer.
fn draw_argb(image: &CGImage, pixels: &mut [u32]) -> Option<CFRetained<CGContext>> {
    let (width, height) = (CGImage::width(Some(image)), CGImage::height(Some(image)));
    let space = CGImage::color_space(Some(image));
    // SAFETY: the buffer holds width * height 4-byte pixels and outlives the context.
    let context = unsafe {
        CGBitmapContextCreate(
            pixels.as_mut_ptr().cast(),
            width,
            height,
            8,
            width * 4,
            space.as_deref(),
            CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0,
        )
    }?;
    let bounds = CGRect::new(CGPoint::ZERO, CGSize::new(width as f64, height as f64));
    CGContext::draw_image(Some(&context), bounds, Some(image));
    Some(context)
}

/// Rectangle x, y, width, height around all pixels that are not fully transparent, origin at the top left.
/// None if the image is fully transparent.
fn visible_bounds(pixels: &[u32], width: usize) -> Option<(usize, usize, usize, usize)> {
    let visible = |pixel: &u32| pixel >> 24 != 0;
    let rows: Vec<usize> =
        (0..pixels.len() / width).filter(|y| pixels[y * width..(y + 1) * width].iter().any(visible)).collect();
    let (&top, &bottom) = (rows.first()?, rows.last()?);
    let column = |x: &usize| (top..=bottom).any(|y| visible(&pixels[y * width + x]));
    let left = (0..width).find(column)?;
    let right = (0..width).rfind(column)?;
    Some((left, top, right - left + 1, bottom - top + 1))
}

/// Fills the transparent ends of each row with the color of the nearest opaque pixel.
/// Pixels are in ARGB order, an edge longer than `max_run` stays transparent.
fn fill_corners(pixels: &mut [u32], width: usize, max_run: usize) {
    let opaque = |pixel: &u32| pixel >> 24 == 0xFF;
    for row in pixels.chunks_exact_mut(width) {
        let (Some(first), Some(last)) = (row.iter().position(opaque), row.iter().rposition(opaque)) else {
            continue;
        };
        if first <= max_run {
            let color = row[first];
            row[..first].fill(color);
        }
        if width - 1 - last <= max_run {
            let color = row[last];
            row[last + 1..].fill(color);
        }
    }
}

/// Wallpaper of the screen where the window was captured, and that screen's frame in global CG points.
pub struct Wallpaper {
    pub path: PathBuf,
    pub screen: CGRect,
}

/// Puts the wallpaper or a color under a window captured with its shadow. Window and shadow sit in the middle.
/// `window` is the window frame in global points, `scale` is pixels per point.
pub fn behind_window(
    image: &CGImage,
    scale: f64,
    window: CGRect,
    wallpaper: Option<&Wallpaper>,
    color: Color,
) -> Option<CFRetained<CGImage>> {
    let (width, height) = (CGImage::width(Some(image)) as f64, CGImage::height(Some(image)) as f64);
    let context = bitmap(width as usize, height as usize)?;
    let bounds = CGRect::new(CGPoint::ZERO, CGSize::new(width, height));
    let wallpaper_image = wallpaper.and_then(|w| load(&w.path).map(|image| (image, w.screen)));
    match wallpaper_image {
        Some((picture, screen)) => {
            // Screenshot with shadow in points, centered on the window.
            let (points_width, points_height) = (width / scale, height / scale);
            let center = (window.origin.x + window.size.width / 2.0, window.origin.y + window.size.height / 2.0);
            let region = (center.0 - points_width / 2.0, center.1 - points_height / 2.0);
            // The wallpaper fills the whole screen, the excess is cropped like in macOS settings.
            let (picture_width, picture_height) =
                (CGImage::width(Some(&picture)) as f64, CGImage::height(Some(&picture)) as f64);
            let cover = (screen.size.width / picture_width).max(screen.size.height / picture_height);
            let (fill_width, fill_height) = (picture_width * cover, picture_height * cover);
            let left = screen.origin.x + (screen.size.width - fill_width) / 2.0 - region.0;
            let top = screen.origin.y + (screen.size.height - fill_height) / 2.0 - region.1;
            // The context has the origin at the bottom, global points at the top.
            let rect = CGRect::new(
                CGPoint::new(left * scale, height - (top + fill_height) * scale),
                CGSize::new(fill_width * scale, fill_height * scale),
            );
            CGContext::draw_image(Some(&context), rect, Some(&picture));
        }
        None => {
            CGContext::set_rgb_fill_color(Some(&context), color.red, color.green, color.blue, 1.0);
            CGContext::fill_rect(Some(&context), bounds);
        }
    }
    CGContext::draw_image(Some(&context), bounds, Some(image));
    CGBitmapContextCreateImage(Some(&context))
}

/// Wallpaper of the screen the window was captured on. None if macOS does not provide it.
pub fn wallpaper(mtm: MainThreadMarker, shot: &Shot) -> Option<Wallpaper> {
    let screen = preview::screen_for(mtm, shot)?;
    let url = NSWorkspace::sharedWorkspace().desktopImageURLForScreen(&screen)?;
    let path = PathBuf::from(url.path()?.to_string());
    // Converting between CG and Cocoa coordinates is symmetric.
    Some(Wallpaper { path, screen: preview::to_cocoa(mtm, screen.frame()) })
}

fn load(path: &Path) -> Option<CFRetained<CGImage>> {
    let bytes = std::fs::read(path).ok()?;
    let source = unsafe { CGImageSource::with_data(&CFData::from_bytes(&bytes), None) }?;
    unsafe { source.image_at_index(0, None) }
}

/// Seed for "Shuffle", changes from one click to the next.
pub fn seed() -> u32 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_micros())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_and_aspect_place_the_card_in_the_middle() {
        let backdrop = Backdrop { padding: 10.0, inset: 2.0, aspect: 1, ..Backdrop::default() };
        // 100 x 50 pixels at scale 2: margin 4, padding 20
        let placed = layout(100.0, 50.0, Trim::default(), &backdrop, 2.0);
        assert_eq!((placed.width, placed.height), (148.0, 148.0));
        assert_eq!(placed.card, (20.0, 45.0, 108.0, 58.0));
        assert_eq!(placed.content, (24.0, 49.0));
        // the cropped edge shifts the screenshot so the visible part stays in place
        let trimmed = layout(
            100.0,
            50.0,
            Trim { left: 10, top: 5, right: 0, bottom: 5 },
            &Backdrop { aspect: 0, ..backdrop },
            2.0,
        );
        assert_eq!(trimmed.card, (20.0, 20.0, 98.0, 48.0));
        assert_eq!(trimmed.content, (14.0, 19.0));
    }

    /// sRGB image filled with `background`, with a `block` rectangle in `ink`.
    fn image(
        width: usize,
        height: usize,
        background: [u8; 3],
        block: (usize, usize, usize, usize),
        ink: [u8; 3],
    ) -> CFRetained<CGImage> {
        let mut bytes = Vec::with_capacity(width * height * 4);
        for y in 0..height {
            for x in 0..width {
                let inside = x >= block.0 && x < block.0 + block.2 && y >= block.1 && y < block.1 + block.3;
                let [r, g, b] = if inside { ink } else { background };
                bytes.extend([r, g, b, 255]);
            }
        }
        // SAFETY: the system color space name constant lives for the whole process.
        let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }));
        // SAFETY: the buffer lives until the end of the function, and the image copies it.
        let context = unsafe {
            CGBitmapContextCreate(
                bytes.as_mut_ptr().cast(),
                width,
                height,
                8,
                width * 4,
                space.as_deref(),
                CGImageAlphaInfo::NoneSkipLast.0,
            )
        }
        .unwrap();
        CGBitmapContextCreateImage(Some(&context)).unwrap()
    }

    #[test]
    fn transparent_corners_take_the_edge_colour_and_wide_gaps_stay() {
        let (clear, a, b) = (0u32, 0xFF11_2233, 0xFF44_5566);
        #[rustfmt::skip]
        let mut pixels = [
            clear, clear, a,     b,     clear,
            clear, clear, clear, clear, clear,
            clear, clear, clear, clear, a,
        ];
        fill_corners(&mut pixels, 5, 2);
        assert_eq!(pixels[..5], [a, a, a, b, b]);
        // fully transparent row
        assert_eq!(pixels[5..10], [clear; 5]);
        // edge wider than max_run
        assert_eq!(pixels[10..], [clear, clear, clear, clear, a]);
    }

    #[test]
    fn visible_bounds_skip_fully_transparent_edges() {
        let (clear, a, shadow) = (0u32, 0xFF11_2233, 0x0100_0000);
        #[rustfmt::skip]
        let pixels = [
            clear, clear,  clear, clear,
            a,     shadow, clear, clear,
            shadow, clear, clear, clear,
        ];
        assert_eq!(visible_bounds(&pixels, 4), Some((0, 1, 2, 2)));
        assert_eq!(visible_bounds(&[clear; 4], 2), None);
    }

    #[test]
    fn auto_crop_finds_the_content_inside_a_plain_border() {
        let picture = image(20, 10, [255, 255, 255], (8, 3, 4, 2), [0, 0, 0]);
        assert_eq!(trim(&picture), Trim { left: 8, top: 3, right: 8, bottom: 5 });
        // fully flat image, at least one pixel remains
        let plain = image(5, 4, [10, 20, 30], (0, 0, 0, 0), [0, 0, 0]);
        let trimmed = trim(&plain);
        assert!(trimmed.left + trimmed.right < 5 && trimmed.top + trimmed.bottom < 4);
    }

    #[test]
    fn composed_image_has_the_fill_around_and_the_content_inside() {
        let red = [255, 0, 0];
        let content = image(20, 10, red, (0, 0, 0, 0), red);
        let blue = Color { red: 0.0, green: 0.0, blue: 1.0, alpha: 1.0 };
        let backdrop =
            Backdrop { fill: Fill::Solid(blue), padding: 5.0, shadow: 0.0, radius: 0.0, ..Backdrop::default() };
        let (composed, placed) = compose(&content, &backdrop, 1.0).unwrap();
        assert_eq!((CGImage::width(Some(&composed)), CGImage::height(Some(&composed))), (30, 20));
        assert_eq!(placed.content, (5.0, 5.0));
        assert_eq!(measure::pixel(&composed, 0, 0), Some([0, 0, 255]));
        assert_eq!(measure::pixel(&composed, 4, 10), Some([0, 0, 255]));
        assert_eq!(measure::pixel(&composed, 5, 5), Some(red));
        assert_eq!(measure::pixel(&composed, 24, 14), Some(red));
        assert_eq!(measure::pixel(&composed, 25, 14), Some([0, 0, 255]));
    }

    #[test]
    fn solid_colour_fills_behind_a_window_shadow() {
        // transparent 4 x 4 image with a green 2 x 2 square in the middle
        let window_image = {
            let context = bitmap(4, 4).unwrap();
            CGContext::set_rgb_fill_color(Some(&context), 0.0, 1.0, 0.0, 1.0);
            CGContext::fill_rect(Some(&context), CGRect::new(CGPoint::new(1.0, 1.0), CGSize::new(2.0, 2.0)));
            CGBitmapContextCreateImage(Some(&context)).unwrap()
        };
        let white = Color { red: 1.0, green: 1.0, blue: 1.0, alpha: 1.0 };
        let window = CGRect::new(CGPoint::new(10.0, 10.0), CGSize::new(1.0, 1.0));
        let composed = behind_window(&window_image, 2.0, window, None, white).unwrap();
        assert_eq!(measure::pixel(&composed, 0, 0), Some([255, 255, 255]));
        assert_eq!(measure::pixel(&composed, 1, 1), Some([0, 255, 0]));
    }

    #[test]
    fn gradient_reverses_and_randomizes_within_its_kind() {
        let fill = Fill::of_kind(0, Color::RED);
        let Fill::Linear([from, to], _) = fill else { panic!("gradient expected") };
        assert_eq!(fill.reversed(), Fill::Linear([to, from], 135.0));
        let other = fill.randomized(3);
        assert_eq!(other.kind(), 0);
        assert_ne!(other, fill);
        assert_eq!(Fill::Blurred.randomized(5), Fill::Blurred);
    }
}
