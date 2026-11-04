//! Отрисовка аннотаций: в картинку при экспорте и контуром поверх холста, пока их тянут мышью.
//!
//! Рисование идёт средствами AppKit в контексте с началом вверху слева, в пикселях основы.
//! Размытие делает Core Image, пикселизация - уменьшение и увеличение без сглаживания.

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
    // Контекст Core Image дорого создавать, один на поток переиспользуется.
    static CI_CONTEXT: OnceCell<Retained<CIContext>> = const { OnceCell::new() };
}

/// Как рисовать аннотацию.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// В картинку, со всеми эффектами.
    Final,
    /// Контуром поверх холста: размытие, лупа и прожектор только обозначаются, чтобы перетаскивание
    /// не ждало перерисовки снимка.
    Live,
}

/// Рисует аннотации документа в контекст размера его обрезки. Координаты аннотаций - в пикселях основы.
/// Аннотация `hidden` пропускается, но номера счётчиков считаются по всему списку.
/// `images` - картинки редактора: основы и добавленные снимки.
pub fn draw(context: &CGContext, images: &[CFRetained<CGImage>], doc: &Doc, hidden: Option<usize>, scale: f64) {
    let (crop, annotations, base) = (doc.state.crop, &doc.annotations, &*images[doc.base]);
    CGContext::save_g_state(Some(context));
    // У Core Graphics начало внизу слева, у аннотаций - вверху слева в координатах всей основы.
    CGContext::translate_ctm(Some(context), 0.0, crop.height);
    CGContext::scale_ctm(Some(context), 1.0, -1.0);
    CGContext::translate_ctm(Some(context), -crop.x, -crop.y);
    let ns = NSGraphicsContext::graphicsContextWithCGContext_flipped(context, true);
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&ns));

    let visible = NSRect::new(NSPoint::new(crop.x, crop.y), NSSize::new(crop.width, crop.height));
    // Добавленные снимки ложатся на основу, размытие и пикселизация - на них, прожектор гасит всё это,
    // а фигуры и текст остаются яркими поверх в порядке рисования.
    let redacts = |a: &Annotation| matches!(a.tool, Tool::Blur | Tool::Pixelate);
    let under = |a: &Annotation| redacts(a) || matches!(a.tool, Tool::Image | Tool::Spotlight);
    let shown = || annotations.iter().enumerate().filter(|(i, _)| Some(*i) != hidden);
    for (_, annotation) in shown().filter(|(_, a)| a.tool == Tool::Image) {
        draw_one(annotation, 0, Some(base), images, scale, Mode::Final);
    }
    if shown().any(|(_, a)| redacts(a)) {
        // Без добавленных снимков под размытием только основа, и копировать нарисованное незачем.
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
    // Все прожекторы гасят фон одним слоем, иначе каждый следующий затемнял бы окна предыдущих.
    if shown().any(|(_, a)| a.tool == Tool::Spotlight) {
        spotlight(visible, annotations, hidden);
    }
    for (index, annotation) in shown().filter(|(_, a)| !under(a)) {
        if annotation.tool == Tool::Magnifier {
            // Лупа увеличивает уже нарисованное под ней, иначе она показала бы то, что скрыто размытием.
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

/// Рисует одну аннотацию в текущий контекст AppKit с началом вверху слева.
/// Без `base` эффекты, которым нужны пиксели снимка, рисуются как в `Mode::Live`.
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
                // SAFETY: массив из двух длин живёт до конца вызова.
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

/// Номер для счётчика, остальным он не нужен.
pub fn number(annotations: &[Annotation], index: usize) -> usize {
    if annotations[index].tool == Tool::Counter { counter_number(annotations, index) } else { 0 }
}

/// Размер текста или диаметр счётчика в пикселях основы, для попадания мышью.
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

/// Стрелка: залитый наконечник, открытый или наконечники на обоих концах.
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
    // Линия доходит только до основания наконечника, иначе круглый конец торчал бы из острия.
    segment(start, end, width).stroke();
}

fn text_attributes(
    annotation: &Annotation,
    scale: f64,
    color: &NSColor,
) -> Retained<NSDictionary<NSString, AnyObject>> {
    let font = NSFont::boldSystemFontOfSize(annotation.size.text() * scale);
    // SAFETY: ключи - системные константы атрибутов текста, они живут весь процесс.
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

/// Текст от точки привязки. С фоном - белый текст на плашке цвета аннотации.
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

/// Круг с номером по центру точки.
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
    // SAFETY: ключи - системные константы атрибутов текста, они живут весь процесс.
    let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    let attributes =
        NSDictionary::<NSString, AnyObject>::from_slices(&keys, &[&*font as &AnyObject, &*white as &AnyObject]);
    let label = NSString::from_str(&number.to_string());
    let size = unsafe { label.sizeWithAttributes(Some(&attributes)) };
    let at = NSPoint::new(center.x - size.width / 2.0, center.y - size.height / 2.0);
    unsafe { label.drawAtPoint_withAttributes(at, Some(&attributes)) };
}

/// Отрезок с засечками на концах и длиной посередине.
fn ruler(annotation: &Annotation, scale: f64, color: &NSColor) {
    let (from, to) = (annotation.points[0], annotation.points[1]);
    let length = (to.x - from.x).hypot(to.y - from.y);
    let width = (annotation.size.stroke() * scale / 2.0).max(1.0);
    let tick = width * 3.0 + 4.0 * scale;
    // Единичная нормаль к отрезку в сторону верха или левого края.
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
    // SAFETY: ключи - системные константы атрибутов текста, они живут весь процесс.
    let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    let attributes =
        NSDictionary::<NSString, AnyObject>::from_slices(&keys, &[&*font as &AnyObject, &*white as &AnyObject]);
    let label = NSString::from_str(&measure::ruler_label(length, scale, annotation.style));
    let size = unsafe { label.sizeWithAttributes(Some(&attributes)) };
    let padding = size.height * 0.25;
    let (plate_width, plate_height) = (size.width + 2.0 * padding, size.height + padding);
    // Подпись стоит рядом с серединой, а не на ней, чтобы две линейки из одного щелчка не закрывали друг друга.
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

/// Затемняет видимую область, оставляя окна в местах прожекторов.
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
    // SAFETY: массив из двух длин живёт до конца вызова.
    unsafe { path.setLineDash_count_phase(dash.as_ptr(), 2, 0.0) };
    NSColor::colorWithWhite_alpha(0.5, 0.25).setFill();
    path.fill();
    NSColor::whiteColor().setStroke();
    path.stroke();
}

/// Сила размытия и пикселизации в точках для вариантов Light, Medium, Strong.
fn strength(style: usize) -> f64 {
    [6.0, 12.0, 24.0][style.min(2)]
}

/// Рисует картинку в прямоугольник с учётом того, что контекст перевёрнут.
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

/// Пиксели картинки в прямоугольнике, обрезанном её краями. Левый верхний угол картинки
/// лежит в точке `origin` пикселей основы, прямоугольник и результат - тоже в пикселях основы.
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

/// `source` - картинка под размытием, её левый верхний угол в точке `origin` пикселей основы.
fn blur(source: &CGImage, origin: Point, annotation: &Annotation, scale: f64) {
    let Some((pixels, rect)) = region(source, origin, ns_rect(annotation)) else { return };
    let sigma = strength(annotation.style) * scale;
    let blurred = CI_CONTEXT.with(|cell| {
        let context = cell.get_or_init(|| unsafe { CIContext::contextWithOptions(None) });
        let input = unsafe { CIImage::imageWithCGImage(&pixels) };
        // Края растягиваются перед размытием, иначе по краю прямоугольника пошла бы тёмная кайма.
        let output = unsafe { input.imageByClampingToExtent().imageByApplyingGaussianBlurWithSigma(sigma) };
        unsafe { context.createCGImage_fromRect(&output, input.extent()) }
    });
    if let Some(blurred) = blurred {
        draw_image(&blurred, rect);
    }
}

/// Уменьшает область до клеток и растягивает обратно без сглаживания.
fn pixelate(source: &CGImage, origin: Point, annotation: &Annotation, scale: f64) {
    let Some((pixels, rect)) = region(source, origin, ns_rect(annotation)) else { return };
    let cell = (strength(annotation.style) * scale).max(1.0);
    let (columns, rows) =
        ((rect.size.width / cell).ceil().max(1.0) as usize, (rect.size.height / cell).ceil().max(1.0) as usize);
    let space = CGImage::color_space(Some(&pixels));
    // SAFETY: буфер null - Core Graphics выделит его сам.
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

/// Круглая врезка с увеличенной серединой области. 2×, 3× или 4× по стилю.
/// `source` - картинка под лупой, её левый верхний угол в точке `origin` пикселей основы.
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
