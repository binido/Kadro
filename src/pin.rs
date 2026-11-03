use std::cell::{Cell, RefCell};
use std::path::PathBuf;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSBeep, NSBezierPath, NSColor, NSControlStateValueOff, NSControlStateValueOn, NSEvent,
    NSEventModifierFlags, NSGraphicsContext, NSMenu, NSMenuItem, NSPanel, NSResponder, NSScreen, NSView, NSWindow,
    NSWindowStyleMask,
};
use objc2_core_foundation::{CFData, CFDictionary, CFRetained};
use objc2_core_graphics::{CGContext, CGImage, CGInterpolationQuality};
use objc2_foundation::{ns_string, NSDictionary, NSNumber, NSObject, NSPoint, NSRect, NSSize, NSString};
use objc2_image_io::{kCGImagePropertyDPIWidth, CGImageSource};

use crate::capture::Shot;
use crate::{output, preview};

const ESCAPE_KEY: u16 = 53;
const C_KEY: u16 = 8;
const ZOOMS: [f64; 3] = [0.5, 1.0, 2.0];
const TRANSLUCENT_ALPHA: f64 = 0.5;

thread_local! {
    static PINS: RefCell<Vec<Retained<PinPanel>>> = const { RefCell::new(Vec::new()) };
    // Где был последний снимок, чтобы закрепить его на том же месте.
    static LAST_SHOT: RefCell<Option<Shot>> = const { RefCell::new(None) };
}

define_class!(
    #[unsafe(super(NSPanel, NSWindow, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    struct PinPanel;

    impl PinPanel {
        // Окно без рамки по умолчанию не получает клавиатуру, а без неё не работают Esc и ⌘C.
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            true
        }
    }
);

struct PinIvars {
    image: CFRetained<CGImage>,
    /// Пикселей снимка на точку экрана, где его сняли.
    scale: f64,
    zoom: Cell<f64>,
    translucent: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = PinIvars]
    struct PinView;

    impl PinView {
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let bounds = self.bounds();
            if let Some(context) = NSGraphicsContext::currentContext().map(|c| c.CGContext()) {
                CGContext::set_interpolation_quality(Some(&context), CGInterpolationQuality::High);
                CGContext::draw_image(Some(&context), bounds, Some(&self.ivars().image));
            }
            NSColor::colorWithWhite_alpha(0.5, 0.6).setStroke();
            NSBezierPath::strokeRect(bounds);
        }

        // Двойной клик закрывает, одиночный тянет окно за любое место.
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            if event.clickCount() == 2 {
                self.close();
            } else if let Some(window) = self.window() {
                window.performWindowDragWithEvent(event);
            }
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            if event.keyCode() == ESCAPE_KEY {
                self.close();
            } else {
                unsafe { msg_send![super(self), keyDown: event] }
            }
        }

        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> bool {
            let command = event.modifierFlags().contains(NSEventModifierFlags::Command);
            if command && event.keyCode() == C_KEY {
                self.copy();
                true
            } else {
                unsafe { msg_send![super(self), performKeyEquivalent: event] }
            }
        }

        #[unsafe(method_id(menuForEvent:))]
        fn menu_for_event(&self, _event: &NSEvent) -> Option<Retained<NSMenu>> {
            Some(self.context_menu())
        }

        #[unsafe(method(copyPin:))]
        fn copy_pin(&self, _sender: Option<&AnyObject>) {
            self.copy();
        }

        #[unsafe(method(zoomPin:))]
        fn zoom_pin(&self, sender: &NSMenuItem) {
            if let Some(zoom) = ZOOMS.get(sender.tag() as usize) {
                self.ivars().zoom.set(*zoom);
                self.apply_zoom();
            }
        }

        #[unsafe(method(toggleTranslucent:))]
        fn toggle_translucent(&self, _sender: Option<&AnyObject>) {
            let translucent = !self.ivars().translucent.get();
            self.ivars().translucent.set(translucent);
            if let Some(window) = self.window() {
                window.setAlphaValue(if translucent { TRANSLUCENT_ALPHA } else { 1.0 });
            }
        }

        #[unsafe(method(closePin:))]
        fn close_pin(&self, _sender: Option<&AnyObject>) {
            self.close();
        }
    }
);

impl PinView {
    fn copy(&self) {
        let (image, scale) = (self.ivars().image.clone(), self.ivars().scale);
        preview::in_background(move || {
            output::copy(&image, scale);
        });
    }

    fn close(&self) {
        let Some(window) = self.window() else { return };
        window.orderOut(None);
        let closed = PINS.with_borrow_mut(|pins| {
            let index = pins.iter().position(|p| p.windowNumber() == window.windowNumber())?;
            Some(pins.remove(index))
        });
        if let Some(pin) = closed {
            preview::retire(pin.into_super());
        }
    }

    fn context_menu(&self) -> Retained<NSMenu> {
        let mtm = self.mtm();
        let menu = NSMenu::new(mtm);
        self.add_item(&menu, "Copy", sel!(copyPin:), 0, false);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        for (index, zoom) in ZOOMS.iter().enumerate() {
            let title = format!("{}%", (zoom * 100.0) as u32);
            self.add_item(&menu, &title, sel!(zoomPin:), index, *zoom == self.ivars().zoom.get());
        }
        self.add_item(&menu, "Semi-transparent", sel!(toggleTranslucent:), 0, self.ivars().translucent.get());
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        self.add_item(&menu, "Close", sel!(closePin:), 0, false);
        menu
    }

    fn add_item(&self, menu: &NSMenu, title: &str, action: Sel, tag: usize, checked: bool) {
        let item = unsafe {
            menu.addItemWithTitle_action_keyEquivalent(&NSString::from_str(title), Some(action), ns_string!(""))
        };
        unsafe { item.setTarget(Some(self)) };
        item.setTag(tag as isize);
        item.setState(if checked { NSControlStateValueOn } else { NSControlStateValueOff });
    }

    /// Размер окна в точках: один пиксель снимка на пиксель экрана, умноженный на масштаб.
    fn size_on(&self, screen_scale: f64) -> NSSize {
        let image = &self.ivars().image;
        let zoom = self.ivars().zoom.get();
        NSSize::new(
            CGImage::width(Some(image)) as f64 / screen_scale * zoom,
            CGImage::height(Some(image)) as f64 / screen_scale * zoom,
        )
    }

    /// Меняет размер окна, оставляя на месте его левый верхний угол.
    fn apply_zoom(&self) {
        let Some(window) = self.window() else { return };
        let frame = window.frame();
        let size = self.size_on(window.backingScaleFactor());
        let top = frame.origin.y + frame.size.height;
        window.setFrame_display(NSRect::new(NSPoint::new(frame.origin.x, top - size.height), size), true);
    }
}

/// Закрепляет снимок поверх окон. Если известно, где его сняли, окно встаёт ровно туда.
pub fn open(mtm: MainThreadMarker, image: CFRetained<CGImage>, scale: f64, place: Option<NSRect>) {
    let screens = NSScreen::screens(mtm);
    let cocoa_place = place.map(|r| preview::to_cocoa(mtm, r));
    let mouse = NSEvent::mouseLocation();
    let anchor = cocoa_place.map_or(mouse, |r| NSPoint::new(r.origin.x + r.size.width / 2.0, r.origin.y + r.size.height / 2.0));
    let Some(screen) = screens
        .iter()
        .find(|s| crate::capture::contains(s.frame(), anchor))
        .or_else(|| NSScreen::mainScreen(mtm))
    else {
        return;
    };

    let view_ivars = PinIvars { image, scale, zoom: Cell::new(1.0), translucent: Cell::new(false) };
    let view = PinView::alloc(mtm).set_ivars(view_ivars);
    let view: Retained<PinView> = unsafe { msg_send![super(view), initWithFrame: NSRect::ZERO] };
    let size = view.size_on(screen.backingScaleFactor());
    let origin = match cocoa_place {
        // Левый верхний угол там же, где был у снятой области.
        Some(r) => NSPoint::new(r.origin.x, r.origin.y + r.size.height - size.height),
        None => {
            let visible = screen.visibleFrame();
            NSPoint::new(
                visible.origin.x + (visible.size.width - size.width) / 2.0,
                visible.origin.y + (visible.size.height - size.height) / 2.0,
            )
        }
    };
    let frame = NSRect::new(origin, size);

    let panel: Retained<PinPanel> = unsafe {
        msg_send![
            PinPanel::alloc(mtm),
            initWithContentRect: frame,
            styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            backing: NSBackingStoreType::Buffered,
            defer: false,
        ]
    };
    preview::configure_floating(&panel);
    // Клик по закреплённому снимку делает его ключевым, иначе Esc и ⌘C до него не дойдут.
    panel.setBecomesKeyOnlyIfNeeded(false);
    view.setFrame(NSRect::new(NSPoint::ZERO, size));
    panel.setContentView(Some(&view));
    panel.makeFirstResponder(Some(&view));
    panel.orderFrontRegardless();
    PINS.with_borrow_mut(|pins| pins.push(panel));
}

/// Где лежит последний снимок для "Pin Last Screenshot".
fn last_path() -> PathBuf {
    std::env::temp_dir().join("Kadro").join("last.png")
}

/// Запоминает последний снимок на диске, а не в памяти, чтобы Kadro в простое не держал десятки мегабайт.
pub fn remember(image: &CGImage, scale: f64, png: Option<&[u8]>) {
    let encoded;
    let bytes = match png {
        Some(png) => png,
        None => match output::encode_png(image, scale) {
            Some(png) => {
                encoded = png;
                &encoded
            }
            None => return,
        },
    };
    let path = last_path();
    let written = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::write(&path, bytes));
    if let Err(err) = written {
        eprintln!("kadro: failed to remember last screenshot: {err}");
    }
}

pub fn remember_place(shot: Shot) {
    LAST_SHOT.set(Some(shot));
}

/// Закрепляет последний снимок. Если снимков ещё не было, только сигналит.
pub fn pin_last(mtm: MainThreadMarker) {
    match load_last() {
        Some((image, scale)) => {
            let place = LAST_SHOT.with_borrow(|shot| shot.as_ref().map(|s| s.global_rect));
            open(mtm, image, scale, place);
        }
        None => NSBeep(),
    }
}

fn load_last() -> Option<(CFRetained<CGImage>, f64)> {
    decode(&std::fs::read(last_path()).ok()?)
}

/// Разбирает PNG и его масштаб из DPI, записанного при кодировании.
fn decode(bytes: &[u8]) -> Option<(CFRetained<CGImage>, f64)> {
    let source = unsafe { CGImageSource::with_data(&CFData::from_bytes(bytes), None) }?;
    let image = unsafe { source.image_at_index(0, None) }?;
    let properties = unsafe { source.properties_at_index(0, None) }?;
    // CFDictionary и NSDictionary взаимозаменяемы без копирования.
    let properties: &NSDictionary<NSString, AnyObject> =
        unsafe { &*(&*properties as *const CFDictionary as *const NSDictionary<NSString, AnyObject>) };
    let key: &NSString = unsafe { &*(kCGImagePropertyDPIWidth as *const _ as *const NSString) };
    let dpi = properties.objectForKey(key).and_then(|n| n.downcast::<NSNumber>().ok()).map_or(72.0, |n| n.doubleValue());
    Some((image, (dpi / 72.0).max(1.0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_core_graphics::{CGBitmapContextCreate, CGBitmapContextCreateImage, CGColorSpace, CGImageAlphaInfo};

    #[test]
    fn last_screenshot_keeps_its_retina_scale_through_the_file() {
        let space = CGColorSpace::new_device_rgb();
        let context = unsafe {
            CGBitmapContextCreate(std::ptr::null_mut(), 4, 2, 8, 0, space.as_deref(), CGImageAlphaInfo::PremultipliedLast.0)
        }
        .unwrap();
        let image = CGBitmapContextCreateImage(Some(&context)).unwrap();
        let png = output::encode_png(&image, 2.0).unwrap();
        let (decoded, scale) = decode(&png).unwrap();
        assert_eq!(scale, 2.0);
        assert_eq!((CGImage::width(Some(&decoded)), CGImage::height(Some(&decoded))), (4, 2));
    }
}
