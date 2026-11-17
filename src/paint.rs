//! Annotation rendering: into the image on export, and as an outline over the canvas while dragged.
//!
//! Drawing uses AppKit in a context with the origin at the top left, in base pixels.
//! Core Image does the blur, pixelation is downscaling and upscaling without smoothing.

use std::cell::OnceCell;
use std::ptr;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, Message};
use objc2_app_kit::{
    NSBezierPath, NSColor, NSCompositingOperation, NSFont, NSFontAttributeName, NSForegroundColorAttributeName,
    NSGraphicsContext, NSImage, NSImageInterpolation, NSLineCapStyle, NSLineJoinStyle, NSStringDrawing, NSWindingRule,
};
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGContext, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo,
    CGInterpolationQuality,
};
use objc2_core_image::{CIContext, CIImage};
use objc2_foundation::{NSDictionary, NSPoint, NSRect, NSSize, NSString};

use crate::annotation::{Annotation, Color, Point, Tool, counter_number};
use crate::document::Doc;
use crate::{measure, output};

thread_local! {
    // A Core Image context is expensive to create, one per thread is reused.
    static CI_CONTEXT: OnceCell<Retained<CIContext>> = const { OnceCell::new() };
}

/// How to draw an annotation.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Into the image, with all effects.
    Final,
    /// As an outline over the canvas: blur, magnifier and spotlight are only outlined so dragging
    /// does not wait for the screenshot to redraw.
    Live,
}

/// Draws document annotations into a context the size of its crop. Annotation coordinates are in base pixels.
/// The `hidden` annotation is skipped, but counter numbers are counted over the whole list.
/// `images` are the editor images: bases and added screenshots.
pub fn draw(context: &CGContext, images: &[CFRetained<CGImage>], doc: &Doc, hidden: Option<usize>, scale: f64) {
    let (crop, annotations, base) = (doc.state.crop, &doc.annotations, &*images[doc.base]);
    CGContext::save_g_state(Some(context));
    // Core Graphics has the origin at the bottom left, annotations at the top left in whole-base coordinates.
    CGContext::translate_ctm(Some(context), 0.0, crop.height);
    CGContext::scale_ctm(Some(context), 1.0, -1.0);
    CGContext::translate_ctm(Some(context), -crop.x, -crop.y);
    let ns = NSGraphicsContext::graphicsContextWithCGContext_flipped(context, true);
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&ns));

    let visible = NSRect::new(NSPoint::new(crop.x, crop.y), NSSize::new(crop.width, crop.height));
    // Added screenshots go on the base, blur and pixelation go on them, the spotlight dims all of that,
    // and shapes and text stay bright on top in drawing order.
    let redacts = |a: &Annotation| matches!(a.tool, Tool::Blur | Tool::Pixelate);
    let under = |a: &Annotation| redacts(a) || matches!(a.tool, Tool::Image | Tool::Spotlight);
    let shown = || annotations.iter().enumerate().filter(|(i, _)| Some(*i) != hidden);
    for (_, annotation) in shown().filter(|(_, a)| a.tool == Tool::Image) {
        draw_one(annotation, 0, Some(base), images, scale, Mode::Final);
    }
    if shown().any(|(_, a)| redacts(a)) {
        // Without added screenshots only the base is under the blur, and there is no need to copy what was drawn.
        let below = if shown().any(|(_, a)| a.tool == Tool::Image) {
            CGBitmapContextCreateImage(Some(context)).map(|image| (image, Point::new(crop.x, crop.y)))
        } else {
            Some((output::retain(base), Point::new(0.0, 0.0)))
        };
        if let Some((source, origin)) = below {
            for (_, annotation) in shown().filter(|(_, a)| redacts(a)) {
                if annotation.tool == Tool::Blur {
                    blur(&source, origin, annotation, scale);
                } else {
                    pixelate(&source, origin, annotation, scale);
                }
            }
        }
    }
    // All spotlights dim the background in one layer, otherwise each next one would darken the windows of the previous ones.
    if shown().any(|(_, a)| a.tool == Tool::Spotlight) {
        spotlight(visible, annotations, hidden);
    }
    for (index, annotation) in shown().filter(|(_, a)| !under(a)) {
        if annotation.tool == Tool::Magnifier {
            // The magnifier enlarges what is already drawn under it, otherwise it would show what the blur hides.
            if let Some(below) = CGBitmapContextCreateImage(Some(context)) {
                magnifier(&below, Point::new(crop.x, crop.y), annotation, scale);
            }
            continue;
        }
        draw_one(annotation, number(annotations, index), Some(base), images, scale, Mode::Final);
    }

    NSGraphicsContext::restoreGraphicsState_class();
    CGContext::restore_g_state(Some(context));
}

/// Draws one annotation into the current AppKit context with the origin at the top left.
/// Without `base`, effects that need screenshot pixels are drawn as in `Mode::Live`.
pub fn draw_one(
    annotation: &Annotation,
    number: usize,
    base: Option<&CGImage>,
    images: &[CFRetained<CGImage>],
    scale: f64,
    mode: Mode,
) {
    let color = ns_color(annotation.color);
    let width = annotation.size.stroke() * scale;
    let p = &annotation.points;
    let live = mode == Mode::Live || base.is_none();
    let origin = Point::new(0.0, 0.0);
    match annotation.tool {
        Tool::Arrow => arrow(p[0], p[1], width, annotation.style, &color),
        Tool::Line => {
            let path = segment(p[0], p[1], width);
            if annotation.style == 1 {
                let dash = [width * 3.0, width * 2.0];
                // SAFETY: the two-length array lives until the end of the call.
                unsafe { path.setLineDash_count_phase(dash.as_ptr(), 2, 0.0) };
            }
            color.setStroke();
            path.stroke();
        }
        Tool::Rect => {
            let rect = ns_rect(annotation);
            let radius = if annotation.style == 1 { width * 3.0 } else { 0.0 };
            let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, radius, radius);
            path.setLineWidth(width);
            if annotation.style == 2 {
                color.colorWithAlphaComponent(0.35).setFill();
                path.fill();
            }
            color.setStroke();
            path.stroke();
        }
        Tool::Oval => {
            let path = NSBezierPath::bezierPathWithOvalInRect(ns_rect(annotation));
            path.setLineWidth(width);
            if annotation.style == 1 {
                color.colorWithAlphaComponent(0.35).setFill();
                path.fill();
            }
            color.setStroke();
            path.stroke();
        }
        Tool::Pen => stroke_points(p, width, &color),
        Tool::Highlighter => stroke_points(p, width * 4.0, &color.colorWithAlphaComponent(0.35)),
        Tool::Text => text(annotation, scale, &color),
        Tool::Counter => counter(annotation, number, scale, &color),
        Tool::Ruler => ruler(annotation, scale, &color),
        Tool::Image => {
            if let Some(image) = images.get(annotation.image) {
                draw_image(image, ns_rect(annotation));
            }
        }
        Tool::Blur | Tool::Pixelate | Tool::Magnifier if live => dashed_outline(annotation, scale),
        Tool::Blur => {
            if let Some(base) = base {
                blur(base, origin, annotation, scale);
            }
        }
        Tool::Pixelate => {
            if let Some(base) = base {
                pixelate(base, origin, annotation, scale);
            }
        }
        Tool::Magnifier => {
            if let Some(base) = base {
                magnifier(base, origin, annotation, scale);
            }
        }
        Tool::Spotlight => dashed_outline(annotation, scale),
    }
}

/// Number for a counter, others do not need it.
pub fn number(annotations: &[Annotation], index: usize) -> usize {
    if annotations[index].tool == Tool::Counter { counter_number(annotations, index) } else { 0 }
}

/// Text size or counter diameter in base pixels, for mouse hit testing.
pub fn extent(annotation: &Annotation, scale: f64) -> (f64, f64) {
    match annotation.tool {
        Tool::Text => {
            let size = text_size(annotation, scale);
            (size.width, size.height)
        }
        Tool::Counter => {
            let diameter = annotation.size.counter() * scale;
            (diameter, diameter)
        }
        _ => (0.0, 0.0),
    }
}

fn ns_color(color: Color) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(color.red, color.green, color.blue, color.alpha)
}

fn ns_point(p: Point) -> NSPoint {
    NSPoint::new(p.x, p.y)
}

fn ns_rect(annotation: &Annotation) -> NSRect {
    let (min, max) = annotation.rect();
    NSRect::new(ns_point(min), NSSize::new(max.x - min.x, max.y - min.y))
}

fn segment(from: Point, to: Point, width: f64) -> Retained<NSBezierPath> {
    let path = NSBezierPath::bezierPath();
    path.moveToPoint(ns_point(from));
    path.lineToPoint(ns_point(to));
    path.setLineWidth(width);
    path.setLineCapStyle(NSLineCapStyle::Round);
    path
}

fn stroke_points(points: &[Point], width: f64, color: &NSColor) {
    let Some((first, rest)) = points.split_first() else { return };
    let path = NSBezierPath::bezierPath();
    path.moveToPoint(ns_point(*first));
    for p in rest {
        path.lineToPoint(ns_point(*p));
    }
    path.setLineWidth(width);
    path.setLineCapStyle(NSLineCapStyle::Round);
    path.setLineJoinStyle(NSLineJoinStyle::Round);
    color.setStroke();
    path.stroke();
}

/// Arrow: filled head, open head or heads on both ends.
fn arrow(from: Point, to: Point, width: f64, style: usize, color: &NSColor) {
    let length = (to.x - from.x).hypot(to.y - from.y);
    if length == 0.0 {
        return;
    }
    let (ux, uy) = ((to.x - from.x) / length, (to.y - from.y) / length);
    let head = (width * 3.5).max(width + 8.0).min(length);
    let half = head * 0.55;
    let wings = |tip: Point, ux: f64, uy: f64| {
        let back = Point::new(tip.x - ux * head, tip.y - uy * head);
        (Point::new(back.x - uy * half, back.y + ux * half), Point::new(back.x + uy * half, back.y - ux * half), back)
    };
    color.setStroke();
    color.setFill();
    if style == 1 {
        let (left, right, _) = wings(to, ux, uy);
        segment(from, to, width).stroke();
        segment(left, to, width).stroke();
        segment(right, to, width).stroke();
        return;
    }
    let filled_head = |tip: Point, ux: f64, uy: f64| {
        let (left, right, back) = wings(tip, ux, uy);
        let path = NSBezierPath::bezierPath();
        path.moveToPoint(ns_point(tip));
        path.lineToPoint(ns_point(left));
        path.lineToPoint(ns_point(right));
        path.closePath();
        path.fill();
        back
    };
    let end = filled_head(to, ux, uy);
    let start = if style == 2 { filled_head(from, -ux, -uy) } else { from };
    // The line stops at the base of the head, otherwise the round cap would stick out of the tip.
    segment(start, end, width).stroke();
}

fn text_attributes(
    annotation: &Annotation,
    scale: f64,
    color: &NSColor,
) -> Retained<NSDictionary<NSString, AnyObject>> {
    let font = NSFont::boldSystemFontOfSize(annotation.size.text() * scale);
    // SAFETY: the keys are system text attribute constants, they live for the whole process.
    let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    NSDictionary::from_slices(&keys, &[&*font as &AnyObject, color as &AnyObject])
}

fn text_size(annotation: &Annotation, scale: f64) -> NSSize {
    let attributes = text_attributes(annotation, scale, &NSColor::whiteColor());
    let size = unsafe { NSString::from_str(&annotation.text).sizeWithAttributes(Some(&attributes)) };
    let padding = text_padding(annotation, scale);
    NSSize::new(size.width + 2.0 * padding, size.height + 2.0 * padding)
}

fn text_padding(annotation: &Annotation, scale: f64) -> f64 {
    if annotation.style == 1 { annotation.size.text() * scale * 0.3 } else { 0.0 }
}

/// Text from the anchor point. With a background, white text on a plate in the annotation color.
fn text(annotation: &Annotation, scale: f64, color: &NSColor) {
    let origin = annotation.points[0];
    let padding = text_padding(annotation, scale);
    let ink = if annotation.style == 1 {
        let size = text_size(annotation, scale);
        let plate = NSRect::new(ns_point(origin), size);
        color.setFill();
        NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(plate, padding, padding).fill();
        NSColor::whiteColor()
    } else {
        color.retain()
    };
    let attributes = text_attributes(annotation, scale, &ink);
    let at = NSPoint::new(origin.x + padding, origin.y + padding);
    unsafe { NSString::from_str(&annotation.text).drawAtPoint_withAttributes(at, Some(&attributes)) };
}

/// Circle with a number, centered on the point.
fn counter(annotation: &Annotation, number: usize, scale: f64, color: &NSColor) {
    let diameter = annotation.size.counter() * scale;
    let center = annotation.points[0];
    let circle = NSRect::new(
        NSPoint::new(center.x - diameter / 2.0, center.y - diameter / 2.0),
        NSSize::new(diameter, diameter),
    );
    color.setFill();
    NSBezierPath::bezierPathWithOvalInRect(circle).fill();
    let font = NSFont::boldSystemFontOfSize(diameter * 0.55);
    let white = NSColor::whiteColor();
    // SAFETY: the keys are system text attribute constants, they live for the whole process.
    let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    let attributes =
        NSDictionary::<NSString, AnyObject>::from_slices(&keys, &[&*font as &AnyObject, &*white as &AnyObject]);
    let label = NSString::from_str(&number.to_string());
    let size = unsafe { label.sizeWithAttributes(Some(&attributes)) };
    let at = NSPoint::new(center.x - size.width / 2.0, center.y - size.height / 2.0);
    unsafe { label.drawAtPoint_withAttributes(at, Some(&attributes)) };
}

/// Segment with ticks at the ends and the length in the middle.
fn ruler(annotation: &Annotation, scale: f64, color: &NSColor) {
    let (from, to) = (annotation.points[0], annotation.points[1]);
    let length = (to.x - from.x).hypot(to.y - from.y);
    let width = (annotation.size.stroke() * scale / 2.0).max(1.0);
    let tick = width * 3.0 + 4.0 * scale;
    // Unit normal to the segment, toward the top or the left edge.
    let (nx, ny) = if length > 0.0 { ((to.y - from.y) / length, -(to.x - from.x) / length) } else { (0.0, -1.0) };
    let (nx, ny) = if ny > 0.0 || (ny == 0.0 && nx > 0.0) { (-nx, -ny) } else { (nx, ny) };
    color.setStroke();
    segment(from, to, width).stroke();
    if length > 0.0 {
        for end in [from, to] {
            let (dx, dy) = (nx * tick, ny * tick);
            segment(Point::new(end.x - dx, end.y - dy), Point::new(end.x + dx, end.y + dy), width).stroke();
        }
    }
    let font = NSFont::boldSystemFontOfSize(annotation.size.text() * scale * 0.7);
    let white = NSColor::whiteColor();
    // SAFETY: the keys are system text attribute constants, they live for the whole process.
    let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    let attributes =
        NSDictionary::<NSString, AnyObject>::from_slices(&keys, &[&*font as &AnyObject, &*white as &AnyObject]);
    let label = NSString::from_str(&measure::ruler_label(length, scale, annotation.style));
    let size = unsafe { label.sizeWithAttributes(Some(&attributes)) };
    let padding = size.height * 0.25;
    let (plate_width, plate_height) = (size.width + 2.0 * padding, size.height + padding);
    // The label sits next to the middle, not on it, so the two rulers from one click do not cover each other.
    let away = tick + nx.abs() * plate_width / 2.0 + ny.abs() * plate_height / 2.0;
    let center = Point::new((from.x + to.x) / 2.0 + nx * away, (from.y + to.y) / 2.0 + ny * away);
    let plate = NSRect::new(
        NSPoint::new(center.x - plate_width / 2.0, center.y - plate_height / 2.0),
        NSSize::new(plate_width, plate_height),
    );
    color.setFill();
    NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(plate, padding * 2.0, padding * 2.0).fill();
    let at = NSPoint::new(center.x - size.width / 2.0, center.y - size.height / 2.0);
    unsafe { label.drawAtPoint_withAttributes(at, Some(&attributes)) };
}

/// Dims the visible area, leaving windows where the spotlights are.
fn spotlight(visible: NSRect, annotations: &[Annotation], hidden: Option<usize>) {
    let path = NSBezierPath::bezierPathWithRect(visible);
    let spots = annotations.iter().enumerate().filter(|(i, a)| a.tool == Tool::Spotlight && Some(*i) != hidden);
    for (_, spot) in spots {
        let rect = ns_rect(spot);
        if spot.style == 1 {
            path.appendBezierPathWithOvalInRect(rect);
        } else {
            path.appendBezierPathWithRect(rect);
        }
    }
    path.setWindingRule(NSWindingRule::EvenOdd);
    NSColor::colorWithWhite_alpha(0.0, 0.55).setFill();
    path.fill();
}

fn dashed_outline(annotation: &Annotation, scale: f64) {
    let rect = ns_rect(annotation);
    let path = if annotation.tool == Tool::Magnifier {
        NSBezierPath::bezierPathWithOvalInRect(rect)
    } else {
        NSBezierPath::bezierPathWithRect(rect)
    };
    path.setLineWidth(1.5 * scale);
    let dash = [4.0 * scale, 3.0 * scale];
    // SAFETY: the two-length array lives until the end of the call.
    unsafe { path.setLineDash_count_phase(dash.as_ptr(), 2, 0.0) };
    NSColor::colorWithWhite_alpha(0.5, 0.25).setFill();
    path.fill();
    NSColor::whiteColor().setStroke();
    path.stroke();
}

/// Blur and pixelation strength in points for the Light, Medium and Strong variants.
fn strength(style: usize) -> f64 {
    [6.0, 12.0, 24.0][style.min(2)]
}

/// Draws an image into a rectangle, accounting for the flipped context.
fn draw_image(image: &CGImage, rect: NSRect) {
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

/// Image pixels in the rectangle, clipped by the image edges. The image top left corner
/// is at `origin` in base pixels, the rectangle and the result are in base pixels too.
fn region(image: &CGImage, origin: Point, rect: NSRect) -> Option<(CFRetained<CGImage>, NSRect)> {
    let (width, height) = (CGImage::width(Some(image)) as f64, CGImage::height(Some(image)) as f64);
    let left = (rect.origin.x - origin.x).max(0.0).floor();
    let top = (rect.origin.y - origin.y).max(0.0).floor();
    let right = (rect.origin.x - origin.x + rect.size.width).min(width).ceil();
    let bottom = (rect.origin.y - origin.y + rect.size.height).min(height).ceil();
    if right <= left || bottom <= top {
        return None;
    }
    let size = NSSize::new(right - left, bottom - top);
    let pixels = CGImage::with_image_in_rect(Some(image), CGRect::new(CGPoint::new(left, top), size))?;
    Some((pixels, NSRect::new(NSPoint::new(left + origin.x, top + origin.y), size)))
}

/// `source` is the image under the blur, its top left corner is at `origin` in base pixels.
fn blur(source: &CGImage, origin: Point, annotation: &Annotation, scale: f64) {
    let Some((pixels, rect)) = region(source, origin, ns_rect(annotation)) else { return };
    let sigma = strength(annotation.style) * scale;
    let blurred = CI_CONTEXT.with(|cell| {
        let context = cell.get_or_init(|| unsafe { CIContext::contextWithOptions(None) });
        let input = unsafe { CIImage::imageWithCGImage(&pixels) };
        // Edges are extended before blurring, otherwise a dark fringe would run along the rectangle edge.
        let output = unsafe { input.imageByClampingToExtent().imageByApplyingGaussianBlurWithSigma(sigma) };
        unsafe { context.createCGImage_fromRect(&output, input.extent()) }
    });
    if let Some(blurred) = blurred {
        draw_image(&blurred, rect);
    }
}

/// Shrinks the area to cells and stretches it back without smoothing.
fn pixelate(source: &CGImage, origin: Point, annotation: &Annotation, scale: f64) {
    let Some((pixels, rect)) = region(source, origin, ns_rect(annotation)) else { return };
    let cell = (strength(annotation.style) * scale).max(1.0);
    let (columns, rows) =
        ((rect.size.width / cell).ceil().max(1.0) as usize, (rect.size.height / cell).ceil().max(1.0) as usize);
    let space = CGImage::color_space(Some(&pixels));
    // SAFETY: a null buffer makes Core Graphics allocate it.
    let Some(small) = (unsafe {
        CGBitmapContextCreate(
            ptr::null_mut(),
            columns,
            rows,
            8,
            0,
            space.as_deref(),
            CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0,
        )
    }) else {
        return;
    };
    CGContext::set_interpolation_quality(Some(&small), CGInterpolationQuality::Medium);
    let cells = CGRect::new(CGPoint::ZERO, CGSize::new(columns as f64, rows as f64));
    CGContext::draw_image(Some(&small), cells, Some(&pixels));
    let Some(blocks) = CGBitmapContextCreateImage(Some(&small)) else { return };
    let Some(context) = NSGraphicsContext::currentContext() else { return };
    context.saveGraphicsState();
    context.setImageInterpolation(NSImageInterpolation::None);
    draw_image(&blocks, rect);
    context.restoreGraphicsState();
}

/// Round inset with the middle of the area enlarged. 2x, 3x or 4x by style.
/// `source` is the image under the magnifier, its top left corner is at `origin` in base pixels.
fn magnifier(source: &CGImage, origin: Point, annotation: &Annotation, scale: f64) {
    let rect = ns_rect(annotation);
    let zoom = (annotation.style + 2) as f64;
    let inner = NSRect::new(
        NSPoint::new(
            rect.origin.x + rect.size.width * (1.0 - 1.0 / zoom) / 2.0,
            rect.origin.y + rect.size.height * (1.0 - 1.0 / zoom) / 2.0,
        ),
        NSSize::new(rect.size.width / zoom, rect.size.height / zoom),
    );
    let Some((pixels, _)) = region(source, origin, inner) else { return };
    let circle = NSBezierPath::bezierPathWithOvalInRect(rect);
    NSGraphicsContext::saveGraphicsState_class();
    circle.addClip();
    draw_image(&pixels, rect);
    NSGraphicsContext::restoreGraphicsState_class();
    circle.setLineWidth(3.0 * scale);
    NSColor::whiteColor().setStroke();
    circle.stroke();
}
