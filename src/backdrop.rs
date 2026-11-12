//! Подложка под снимком: фон, отступы, тень и скругления, а также фон за окном при захвате окна.
//!
//! Подложка - часть документа редактора, поэтому её можно менять и отменять, как любую правку.
//! Размеры задаются в точках и переводятся в пиксели через масштаб снимка.

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

/// Пропорции холста для выпадающего списка: подпись и ширина к высоте. 0 - без ограничения.
pub const ASPECTS: [(&str, f64); 7] = [
    ("Free", 0.0),
    ("1:1", 1.0),
    ("4:3", 4.0 / 3.0),
    ("3:2", 1.5),
    ("16:10", 1.6),
    ("16:9", 16.0 / 9.0),
    ("9:16", 9.0 / 16.0),
];

/// Виды фона для выпадающего списка, по порядку `Fill::kind`.
pub const KINDS: [&str; 4] = ["Gradient", "Mesh", "Solid", "Blurred"];

/// Подобранные пары цветов градиентов.
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

/// Подобранные четвёрки цветов для углов: левый верхний, правый верхний, правый нижний, левый нижний.
const MESHES: [[u32; 4]; 6] = [
    [0xff_9a9e, 0xfa_d0c4, 0xa1_c4fd, 0xc2_e9fb],
    [0x84_fab0, 0x8f_d3f4, 0xa1_8cd1, 0xfb_c2eb],
    [0xf6_d365, 0xfd_a085, 0xf0_93fb, 0xf5_576c],
    [0x43_e97b, 0x38_f9d7, 0x4f_acfe, 0x00_f2fe],
    [0x30_cfd0, 0x66_7eea, 0x76_4ba2, 0x33_0867],
    [0xff_ecd2, 0xfc_b69f, 0xff_9a9e, 0xfe_cfef],
];

/// Чем залит фон.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Fill {
    /// Линейный градиент и его угол в градусах.
    Linear([Color; 2], f64),
    /// Плавный переход между цветами четырёх углов.
    Mesh([Color; 4]),
    Solid(Color),
    /// Сам снимок, растянутый на весь фон и размытый.
    Blurred,
}

impl Fill {
    /// Номер вида в `KINDS`.
    pub fn kind(self) -> usize {
        match self {
            Fill::Linear(..) => 0,
            Fill::Mesh(_) => 1,
            Fill::Solid(_) => 2,
            Fill::Blurred => 3,
        }
    }

    /// Первый подобранный фон вида `kind`. Для однотонного - `solid`.
    pub fn of_kind(kind: usize, solid: Color) -> Self {
        match kind {
            0 => Fill::Linear(GRADIENTS[0].map(hex), 135.0),
            1 => Fill::Mesh(MESHES[0].map(hex)),
            2 => Fill::Solid(solid),
            _ => Fill::Blurred,
        }
    }

    /// Другой подобранный фон того же вида. `seed` выбирает, какой именно.
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

    /// Градиент в обратную сторону.
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

/// Подложка снимка в редакторе.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Backdrop {
    pub fill: Fill,
    /// Отступ от снимка до края холста, в точках.
    pub padding: f64,
    /// Сила тени под снимком, от 0 до 1.
    pub shadow: f64,
    /// Скругление углов снимка, в точках.
    pub radius: f64,
    /// Скругление углов всего холста, в точках. Углы становятся прозрачными.
    pub outer_radius: f64,
    /// Поле цвета края снимка вокруг него, в точках.
    pub inset: f64,
    /// Номер пропорций в `ASPECTS`.
    pub aspect: usize,
    /// Обрезать по краям снимка ровный фон, прежде чем класть снимок на подложку.
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

/// Сколько пикселей ровного фона срезать с каждого края.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Trim {
    pub left: usize,
    pub top: usize,
    pub right: usize,
    pub bottom: usize,
}

/// Где что лежит на холсте с подложкой, в пикселях, начало вверху слева.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Layout {
    pub width: f64,
    pub height: f64,
    /// Карточка снимка вместе с полем `inset`.
    pub card: (f64, f64, f64, f64),
    /// Куда попадает левый верхний пиксель необрезанного снимка.
    pub content: (f64, f64),
}

/// Раскладка снимка `width` × `height` на подложке.
pub fn layout(width: f64, height: f64, trim: Trim, backdrop: &Backdrop, scale: f64) -> Layout {
    let visible_width = width - (trim.left + trim.right) as f64;
    let visible_height = height - (trim.top + trim.bottom) as f64;
    let inset = (backdrop.inset * scale).round();
    let padding = (backdrop.padding * scale).round();
    let (card_width, card_height) = (visible_width + 2.0 * inset, visible_height + 2.0 * inset);
    let (mut canvas_width, mut canvas_height) = (card_width + 2.0 * padding, card_height + 2.0 * padding);
    let ratio = ASPECTS.get(backdrop.aspect).map_or(0.0, |aspect| aspect.1);
    // Холст растёт только вширь или ввысь, снимок не уменьшается.
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

/// Края снимка, залитые тем же цветом, что и левый верхний пиксель. Хотя бы пиксель остаётся.
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

/// Кладёт снимок на подложку. Возвращает картинку и раскладку.
pub fn compose(content: &CGImage, backdrop: &Backdrop, scale: f64) -> Option<(CFRetained<CGImage>, Layout)> {
    let (width, height) = (CGImage::width(Some(content)) as f64, CGImage::height(Some(content)) as f64);
    let trimmed = if backdrop.auto_crop { trim(content) } else { Trim::default() };
    let layout = layout(width, height, trimmed, backdrop, scale);
    let context = bitmap(layout.width as usize, layout.height as usize)?;
    let ns = NSGraphicsContext::graphicsContextWithCGContext_flipped(&context, false);
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&ns));
    // У AppKit здесь начало внизу слева, у раскладки - вверху.
    let rect = |x: f64, y: f64, w: f64, h: f64| NSRect::new(NSPoint::new(x, layout.height - y - h), NSSize::new(w, h));
    let canvas = rect(0.0, 0.0, layout.width, layout.height);
    let outer = backdrop.outer_radius * scale;
    NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(canvas, outer, outer).addClip();
    fill(&backdrop.fill, content, canvas, scale);

    let (card_x, card_y, card_width, card_height) = layout.card;
    let card = rect(card_x, card_y, card_width, card_height);
    let radius = backdrop.radius * scale;
    let shape = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(card, radius, radius);
    // Поле вокруг снимка - цвет его края, так снимок на карточке выглядит цельным.
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
            // Угол AppKit отсчитывается против часовой стрелки, 135° - от левого верхнего угла к правому нижнему.
            if let Some(gradient) = gradient {
                gradient.drawInRect_angle(canvas, -angle + 90.0);
            }
        }
        Fill::Mesh(corners) => {
            // Картинка 2 × 2 из цветов углов, растянутая со сглаживанием, даёт плавный переход между ними.
            // Она шире холста вдвое, чтобы центры её пикселей пришлись точно на углы.
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

/// Картинка из четырёх пикселей: левый верхний, правый верхний, правый нижний, левый нижний.
fn corner_image(corners: [Color; 4]) -> Option<CFRetained<CGImage>> {
    let byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let rgba = |c: Color| [byte(c.red), byte(c.green), byte(c.blue), 255];
    let [top_left, top_right, bottom_right, bottom_left] = corners.map(rgba);
    let mut bytes: Vec<u8> = [top_left, top_right, bottom_left, bottom_right].concat();
    // SAFETY: системная константа имени цветового пространства живёт весь процесс.
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }));
    // SAFETY: буфер на 2 × 2 пикселя по 4 байта живёт дольше контекста, а картинка копирует его.
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

/// Прозрачный холст в sRGB.
fn bitmap(width: usize, height: usize) -> Option<CFRetained<CGContext>> {
    // SAFETY: системная константа имени цветового пространства живёт весь процесс.
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }));
    // SAFETY: буфер null - Core Graphics выделит его сам.
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

/// Наибольший радиус скругления окна в точках. Прозрачный край шире - форма самого окна, а не угол.
const MAX_CORNER: f64 = 40.0;

/// Достраивает скруглённые углы снятого окна до прямоугольника цветом его же края.
/// Возвращает None, если холст не создался.
pub fn square_corners(image: &CGImage, scale: f64) -> Option<CFRetained<CGImage>> {
    let (width, height) = (CGImage::width(Some(image)), CGImage::height(Some(image)));
    let mut pixels = vec![0u32; width * height];
    let space = CGImage::color_space(Some(image));
    // SAFETY: буфер вмещает width * height пикселей по 4 байта и живёт дольше контекста.
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
    fill_corners(&mut pixels, width, (MAX_CORNER * scale) as usize);
    CGBitmapContextCreateImage(Some(&context))
}

/// Закрашивает прозрачные края каждой строки цветом ближайшего непрозрачного пикселя.
/// Пиксели в порядке ARGB, край длиннее `max_run` остаётся прозрачным.
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

/// Обои экрана, где сняли окно, и место этого экрана в глобальных точках CG.
pub struct Wallpaper {
    pub path: PathBuf,
    pub screen: CGRect,
}

/// Подкладывает под снятое с тенью окно обои или цвет. Окно и тень лежат в середине картинки.
/// `window` - рамка окна в глобальных точках, `scale` - пикселей на точку.
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
            // Снимок с тенью в точках, по центру окна.
            let (points_width, points_height) = (width / scale, height / scale);
            let center = (window.origin.x + window.size.width / 2.0, window.origin.y + window.size.height / 2.0);
            let region = (center.0 - points_width / 2.0, center.1 - points_height / 2.0);
            // Обои заполняют экран целиком, лишнее по краям обрезается, как в настройках macOS.
            let (picture_width, picture_height) =
                (CGImage::width(Some(&picture)) as f64, CGImage::height(Some(&picture)) as f64);
            let cover = (screen.size.width / picture_width).max(screen.size.height / picture_height);
            let (fill_width, fill_height) = (picture_width * cover, picture_height * cover);
            let left = screen.origin.x + (screen.size.width - fill_width) / 2.0 - region.0;
            let top = screen.origin.y + (screen.size.height - fill_height) / 2.0 - region.1;
            // У контекста начало внизу, у глобальных точек - вверху.
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

/// Обои экрана, на котором снято окно. None, если macOS их не отдала.
pub fn wallpaper(mtm: MainThreadMarker, shot: &Shot) -> Option<Wallpaper> {
    let screen = preview::screen_for(mtm, shot)?;
    let url = NSWorkspace::sharedWorkspace().desktopImageURLForScreen(&screen)?;
    let path = PathBuf::from(url.path()?.to_string());
    // Переход между системами координат CG и Cocoa симметричен.
    Some(Wallpaper { path, screen: preview::to_cocoa(mtm, screen.frame()) })
}

fn load(path: &Path) -> Option<CFRetained<CGImage>> {
    let bytes = std::fs::read(path).ok()?;
    let source = unsafe { CGImageSource::with_data(&CFData::from_bytes(&bytes), None) }?;
    unsafe { source.image_at_index(0, None) }
}

/// Номер для «Перемешать»: меняется от нажатия к нажатию.
pub fn seed() -> u32 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_micros())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_and_aspect_place_the_card_in_the_middle() {
        let backdrop = Backdrop { padding: 10.0, inset: 2.0, aspect: 1, ..Backdrop::default() };
        // 100 × 50 пикселей при масштабе 2: поле 4, отступ 20
        let placed = layout(100.0, 50.0, Trim::default(), &backdrop, 2.0);
        assert_eq!((placed.width, placed.height), (148.0, 148.0));
        assert_eq!(placed.card, (20.0, 45.0, 108.0, 58.0));
        assert_eq!(placed.content, (24.0, 49.0));
        // срезанный край сдвигает снимок, чтобы видимая часть встала на место
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

    /// sRGB-картинка, залитая `background`, с прямоугольником `block` цвета `ink`.
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
        // SAFETY: системная константа имени цветового пространства живёт весь процесс.
        let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }));
        // SAFETY: буфер живёт до конца функции, а картинка копирует его.
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
        // целиком прозрачная строка
        assert_eq!(pixels[5..10], [clear; 5]);
        // край шире max_run
        assert_eq!(pixels[10..], [clear, clear, clear, clear, a]);
    }

    #[test]
    fn auto_crop_finds_the_content_inside_a_plain_border() {
        let picture = image(20, 10, [255, 255, 255], (8, 3, 4, 2), [0, 0, 0]);
        assert_eq!(trim(&picture), Trim { left: 8, top: 3, right: 8, bottom: 5 });
        // ровная картинка целиком - остаётся хотя бы пиксель
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
        // прозрачная картинка 4 × 4 с зелёным квадратом 2 × 2 посередине
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
