//! Встроенные в бинарник ресурсы: шрифт Space Grotesk и иконки Lucide.

use std::cell::OnceCell;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, Message};
use objc2_app_kit::{NSFont, NSFontDescriptor, NSFontVariationAttribute, NSImage};
use objc2_core_foundation::{CFData, CGSize};
use objc2_core_text::{CTFontDescriptor, CTFontManagerCreateFontDescriptorFromData};
use objc2_foundation::{NSData, NSDictionary, NSNumber, NSString};

use crate::bridge;

const SPACE_GROTESK: &[u8] = include_bytes!("../assets/SpaceGrotesk.ttf");
// Ось толщины 'wght' вариативного шрифта.
const WEIGHT_AXIS: u32 = u32::from_be_bytes(*b"wght");
const REGULAR_WEIGHT: f64 = 400.0;
const SEMIBOLD_WEIGHT: f64 = 600.0;

pub const KADRO: &[u8] = include_bytes!("../assets/icons/kadro.svg");
pub const SQUARE_DASHED: &[u8] = include_bytes!("../assets/icons/square-dashed.svg");
pub const MONITOR: &[u8] = include_bytes!("../assets/icons/monitor.svg");
pub const APP_WINDOW: &[u8] = include_bytes!("../assets/icons/app-window.svg");
pub const SETTINGS: &[u8] = include_bytes!("../assets/icons/settings.svg");
pub const SLIDERS: &[u8] = include_bytes!("../assets/icons/sliders-horizontal.svg");
pub const KEYBOARD: &[u8] = include_bytes!("../assets/icons/keyboard.svg");
pub const FOLDER_OPEN: &[u8] = include_bytes!("../assets/icons/folder-open.svg");
pub const REPEAT: &[u8] = include_bytes!("../assets/icons/repeat.svg");
pub const TIMER: &[u8] = include_bytes!("../assets/icons/timer.svg");
pub const APP_WINDOW_MAC: &[u8] = include_bytes!("../assets/icons/app-window-mac.svg");
pub const COPY: &[u8] = include_bytes!("../assets/icons/copy.svg");
pub const SAVE: &[u8] = include_bytes!("../assets/icons/save.svg");
pub const X: &[u8] = include_bytes!("../assets/icons/x.svg");
pub const CHECK: &[u8] = include_bytes!("../assets/icons/check.svg");
pub const PIN: &[u8] = include_bytes!("../assets/icons/pin.svg");
pub const CROP: &[u8] = include_bytes!("../assets/icons/crop.svg");
pub const EXPAND: &[u8] = include_bytes!("../assets/icons/expand.svg");
pub const SCALING: &[u8] = include_bytes!("../assets/icons/scaling.svg");
pub const ZOOM_IN: &[u8] = include_bytes!("../assets/icons/zoom-in.svg");
pub const ZOOM_OUT: &[u8] = include_bytes!("../assets/icons/zoom-out.svg");
pub const MAXIMIZE: &[u8] = include_bytes!("../assets/icons/maximize.svg");
pub const UNDO: &[u8] = include_bytes!("../assets/icons/undo-2.svg");
pub const REDO: &[u8] = include_bytes!("../assets/icons/redo-2.svg");
pub const PRINTER: &[u8] = include_bytes!("../assets/icons/printer.svg");
pub const SQUARE_PEN: &[u8] = include_bytes!("../assets/icons/square-pen.svg");
pub const CLIPBOARD_PASTE: &[u8] = include_bytes!("../assets/icons/clipboard-paste.svg");
pub const POINTER: &[u8] = include_bytes!("../assets/icons/mouse-pointer-2.svg");
pub const ARROW: &[u8] = include_bytes!("../assets/icons/move-up-right.svg");
pub const LINE: &[u8] = include_bytes!("../assets/icons/minus.svg");
pub const RECT: &[u8] = include_bytes!("../assets/icons/square.svg");
pub const OVAL: &[u8] = include_bytes!("../assets/icons/circle.svg");
pub const PEN: &[u8] = include_bytes!("../assets/icons/pencil.svg");
pub const HIGHLIGHTER: &[u8] = include_bytes!("../assets/icons/highlighter.svg");
pub const TEXT: &[u8] = include_bytes!("../assets/icons/type.svg");
pub const COUNTER: &[u8] = include_bytes!("../assets/icons/hash.svg");
pub const BLUR: &[u8] = include_bytes!("../assets/icons/droplets.svg");
pub const PIXELATE: &[u8] = include_bytes!("../assets/icons/grid-3x3.svg");
pub const SPOTLIGHT: &[u8] = include_bytes!("../assets/icons/flashlight.svg");
pub const MAGNIFIER: &[u8] = include_bytes!("../assets/icons/search.svg");
pub const LAYERS: &[u8] = include_bytes!("../assets/icons/layers.svg");
pub const RULER: &[u8] = include_bytes!("../assets/icons/ruler.svg");
pub const PIPETTE: &[u8] = include_bytes!("../assets/icons/pipette.svg");
pub const IMAGE_PLUS: &[u8] = include_bytes!("../assets/icons/image-plus.svg");
pub const FILM: &[u8] = include_bytes!("../assets/icons/film.svg");
pub const WALLPAPER: &[u8] = include_bytes!("../assets/icons/wallpaper.svg");
pub const SCAN_TEXT: &[u8] = include_bytes!("../assets/icons/scan-text.svg");
pub const SCROLL: &[u8] = include_bytes!("../assets/icons/arrow-down-to-line.svg");
pub const CLOUD_UPLOAD: &[u8] = include_bytes!("../assets/icons/cloud-upload.svg");
pub const LIBRARY: &[u8] = include_bytes!("../assets/icons/images.svg");
pub const CAMERA: &[u8] = include_bytes!("../assets/icons/camera.svg");
pub const FOLDER_DOWN: &[u8] = include_bytes!("../assets/icons/folder-down.svg");
pub const INFO: &[u8] = include_bytes!("../assets/icons/info.svg");
pub const ELLIPSIS: &[u8] = include_bytes!("../assets/icons/ellipsis.svg");

thread_local! {
    static FONT: OnceCell<Option<Retained<NSFontDescriptor>>> = const { OnceCell::new() };
}

/// Space Grotesk обычного начертания.
pub fn font(size: f64) -> Retained<NSFont> {
    weighted_font(size, REGULAR_WEIGHT)
}

/// Space Grotesk полужирного начертания для заголовков.
pub fn semibold_font(size: f64) -> Retained<NSFont> {
    weighted_font(size, SEMIBOLD_WEIGHT)
}

/// Возвращает системный шрифт, если файл шрифта не разобрался.
fn weighted_font(size: f64, weight: f64) -> Retained<NSFont> {
    let base = FONT.with(|cell| {
        cell.get_or_init(|| {
            let data = CFData::from_static_bytes(SPACE_GROTESK);
            let descriptor = unsafe { CTFontManagerCreateFontDescriptorFromData(&data) }?;
            // SAFETY: CTFontDescriptor и NSFontDescriptor взаимозаменяемы без копирования.
            let descriptor: &NSFontDescriptor = unsafe { bridge::toll_free::<CTFontDescriptor, _>(&descriptor) };
            Some(descriptor.retain())
        })
        .clone()
    });
    let Some(base) = base else {
        return NSFont::systemFontOfSize(size);
    };
    // Без явной толщины вариативный файл открывается начертанием Light.
    let axes = NSDictionary::from_slices(&[&*NSNumber::new_u32(WEIGHT_AXIS)], &[&*NSNumber::new_f64(weight)]);
    let attributes = NSDictionary::<NSString, AnyObject>::from_slices(
        &[unsafe { NSFontVariationAttribute }],
        &[&**axes as &AnyObject],
    );
    let descriptor = unsafe { base.fontDescriptorByAddingAttributes(&attributes) };
    NSFont::fontWithDescriptor_size(&descriptor, size).unwrap_or_else(|| NSFont::systemFontOfSize(size))
}

/// Иконка Lucide как шаблонное изображение, которое AppKit сам перекрашивает под тему.
pub fn icon(svg: &[u8], size: f64) -> Option<Retained<NSImage>> {
    let image = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(svg))?;
    image.setSize(CGSize::new(size, size));
    image.setTemplate(true);
    Some(image)
}
