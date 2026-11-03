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

pub const APERTURE: &[u8] = include_bytes!("../assets/icons/aperture.svg");
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
