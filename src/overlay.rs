//! Overlay over all screens: area selection with a magnifier or picking a window with the mouse.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSBezierPath, NSColor, NSCompositingOperation, NSCursor, NSEvent,
    NSEventModifierFlags, NSFontAttributeName, NSForegroundColorAttributeName, NSGraphicsContext, NSRectFill,
    NSRectFillUsingOperation, NSResponder, NSScreen, NSScreenSaverWindowLevel, NSStringDrawing, NSView, NSWindow,
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::{CFDictionary, CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGContext, CGDirectDisplayID, CGDisplayBounds, CGEvent, CGImage, CGInterpolationQuality,
    CGRectMakeWithDictionaryRepresentation, CGWindowID, CGWindowListCopyWindowInfo, CGWindowListOption,
    kCGNullWindowID,
};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSObject, NSPoint, NSRect, NSSize, NSString, ns_string};

use crate::bridge::{self, number};
use crate::{assets, capture, geometry, keys, output, scroll};

/// Magnifier side in points and how many screenshot pixels it shows on each axis.
const LOUPE_SIZE: f64 = 120.0;
const LOUPE_PIXELS: f64 = 15.0;

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Region,
    Window,
}

/// Window of another app in global CG coordinates, from the top left of the main display.
#[derive(Clone, Copy)]
pub struct TargetWindow {
    pub id: CGWindowID,
    pub pid: i32,
    pub frame: CGRect,
}

thread_local! {
    static WINDOWS: RefCell<Vec<Retained<OverlayWindow>>> = const { RefCell::new(Vec::new()) };
    static VIEWS: RefCell<Vec<Retained<SelectionView>>> = const { RefCell::new(Vec::new()) };
    static MODE: Cell<Mode> = const { Cell::new(Mode::Region) };
    // On-screen windows front to back, taken when the overlay opens.
    static TARGETS: RefCell<Vec<TargetWindow>> = const { RefCell::new(Vec::new()) };
    static HOVERED: Cell<Option<TargetWindow>> = const { Cell::new(None) };
    // App that was active before the overlay. The overlay itself makes Kadro active.
    static APP: RefCell<String> = const { RefCell::new(String::new()) };
    // Display images taken when the overlay opens, the magnifier is drawn from them.
    static FROZEN: RefCell<HashMap<CGDirectDisplayID, CFRetained<CGImage>>> = RefCell::new(HashMap::new());
}

define_class!(
    #[unsafe(super(NSWindow, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    struct OverlayWindow;

    impl OverlayWindow {
        // A borderless window gets no keyboard by default, and Esc does not work without it.
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            true
        }
    }
);

#[derive(Default)]
struct ViewIvars {
    display: Cell<CGDirectDisplayID>,
    start: Cell<Option<NSPoint>>,
    /// Mouse position without the arrow key offset.
    pointer: Cell<Option<NSPoint>>,
    /// Arrow key offset of the selection end, reset on a new mouse down.
    nudge: Cell<NSPoint>,
    selection: Cell<Option<NSRect>>,
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ViewIvars]
    struct SelectionView;

    impl SelectionView {
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
            match MODE.get() {
                Mode::Region => {
                    NSColor::colorWithWhite_alpha(0.0, 0.25).setFill();
                    NSRectFill(self.bounds());
                    if let Some(selection) = self.ivars().selection.get() {
                        NSRectFillUsingOperation(selection, NSCompositingOperation::Clear);
                        NSColor::whiteColor().setStroke();
                        NSBezierPath::strokeRect(selection);
                        self.draw_size(selection);
                    }
                    if let Some(point) = self.end_point() {
                        self.draw_loupe(point);
                    }
                }
                Mode::Window => {
                    if let Some(target) = HOVERED.get() {
                        let rect = self.local_rect(target.frame);
                        NSColor::selectedContentBackgroundColor().colorWithAlphaComponent(0.3).setFill();
                        NSRectFill(rect);
                        NSColor::selectedContentBackgroundColor().setStroke();
                        NSBezierPath::strokeRect(rect);
                        // Windows above the selected one will not be in the screenshot, so the highlight does not cover them.
                        TARGETS.with_borrow(|targets| {
                            for above in targets.iter().take_while(|t| t.id != target.id) {
                                NSRectFillUsingOperation(self.local_rect(above.frame), NSCompositingOperation::Clear);
                            }
                        });
                    }
                }
            }
        }

        // Mouse moves reach only the key window, so the position is sent to all screens at once.
        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, _event: &NSEvent) {
            match MODE.get() {
                Mode::Window => update_hover(),
                Mode::Region => track_pointer(),
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            match MODE.get() {
                Mode::Region => {
                    let point = self.point(event);
                    self.ivars().start.set(Some(point));
                    self.ivars().pointer.set(Some(point));
                    self.ivars().nudge.set(NSPoint::ZERO);
                    self.ivars().selection.set(None);
                }
                Mode::Window => {
                    update_hover();
                    if let Some(target) = HOVERED.get() {
                        close();
                        capture::capture_window(target.id);
                    }
                }
            }
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            self.ivars().pointer.set(Some(self.point(event)));
            self.update_selection();
        }

        // Shift pressed or released mid-drag turns the selection into a square or back right away.
        #[unsafe(method(flagsChanged:))]
        fn flags_changed(&self, _event: &NSEvent) {
            self.update_selection();
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            if MODE.get() != Mode::Region {
                return;
            }
            self.ivars().start.set(None);
            match self.ivars().selection.get() {
                // A click without dragging is not a selection, the overlay stays open.
                Some(rect) if rect.size.width >= 2.0 && rect.size.height >= 2.0 => {
                    let height = self.bounds().size.height;
                    // NSView has the origin at the bottom, ScreenCaptureKit at the top.
                    let flipped = CGRect::new(
                        CGPoint::new(rect.origin.x, height - rect.origin.y - rect.size.height),
                        rect.size,
                    );
                    let app = APP.with_borrow(Clone::clone);
                    close();
                    if output::take_scroll_route() {
                        scroll::start(self.ivars().display.get(), flipped, app);
                    } else {
                        capture::capture_region(self.ivars().display.get(), flipped, app);
                    }
                }
                _ => {
                    self.ivars().selection.set(None);
                    self.setNeedsDisplay(true);
                }
            }
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let step = if event.modifierFlags().contains(NSEventModifierFlags::Shift) { 10.0 } else { 1.0 };
            let delta = match event.keyCode() {
                keys::ESCAPE => {
                    output::cancel_route();
                    return close();
                }
                keys::LEFT => NSPoint::new(-step, 0.0),
                keys::RIGHT => NSPoint::new(step, 0.0),
                keys::DOWN => NSPoint::new(0.0, -step),
                keys::UP => NSPoint::new(0.0, step),
                _ => return,
            };
            // Arrows move the corner the mouse is dragging, so they work only while dragging.
            if self.ivars().start.get().is_some() {
                let nudge = self.ivars().nudge.get();
                self.ivars().nudge.set(NSPoint::new(nudge.x + delta.x, nudge.y + delta.y));
                self.update_selection();
            }
        }
    }
);

impl SelectionView {
    fn new(mtm: MainThreadMarker, frame: NSRect, display: CGDirectDisplayID) -> Retained<Self> {
        let ivars = ViewIvars::default();
        ivars.display.set(display);
        let this = Self::alloc(mtm).set_ivars(ivars);
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    /// Event position in view coordinates, clamped to its bounds.
    fn point(&self, event: &NSEvent) -> NSPoint {
        self.clamp(self.convertPoint_fromView(event.locationInWindow(), None))
    }

    fn clamp(&self, p: NSPoint) -> NSPoint {
        let size = self.bounds().size;
        NSPoint::new(p.x.clamp(0.0, size.width), p.y.clamp(0.0, size.height))
    }

    /// Selection end: the mouse plus the arrow key offset.
    fn end_point(&self) -> Option<NSPoint> {
        let pointer = self.ivars().pointer.get()?;
        let nudge = self.ivars().nudge.get();
        Some(self.clamp(NSPoint::new(pointer.x + nudge.x, pointer.y + nudge.y)))
    }

    fn update_selection(&self) {
        let (Some(start), Some(end)) = (self.ivars().start.get(), self.end_point()) else { return };
        let square = NSEvent::modifierFlags_class().contains(NSEventModifierFlags::Shift);
        let rect = selection_rect(start, end, square);
        // The square may go past the screen edge, then the edge clips it.
        self.ivars().selection.set(Some(clip(rect, self.bounds())));
        self.setNeedsDisplay(true);
    }

    fn scale(&self) -> f64 {
        self.window().map_or(1.0, |w| w.backingScaleFactor())
    }

    /// Selection size in screenshot pixels, below the selection or above it if there is no room below.
    fn draw_size(&self, selection: NSRect) {
        let scale = self.scale();
        let text = NSString::from_str(&format!(
            "{} × {}",
            (selection.size.width * scale).round(),
            (selection.size.height * scale).round()
        ));
        let attributes = text_attributes();
        let size = unsafe { text.sizeWithAttributes(Some(&attributes)) };
        let padding = NSSize::new(6.0, 3.0);
        let mut origin = NSPoint::new(
            selection.origin.x + selection.size.width - size.width - 2.0 * padding.width,
            selection.origin.y - size.height - 2.0 * padding.height - 6.0,
        );
        if origin.y < 0.0 {
            origin.y = selection.origin.y + selection.size.height + 6.0;
        }
        origin.x = origin.x.max(0.0);
        let badge =
            NSRect::new(origin, NSSize::new(size.width + 2.0 * padding.width, size.height + 2.0 * padding.height));
        NSColor::colorWithWhite_alpha(0.0, 0.7).setFill();
        NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(badge, 4.0, 4.0).fill();
        unsafe {
            text.drawAtPoint_withAttributes(
                NSPoint::new(origin.x + padding.width, origin.y + padding.height),
                Some(&attributes),
            );
        };
    }

    /// Magnifier next to the cursor: screenshot pixels under it enlarged, the center pixel framed.
    fn draw_loupe(&self, at: NSPoint) {
        let Some(image) = FROZEN.with_borrow(|frozen| frozen.get(&self.ivars().display.get()).cloned()) else {
            return;
        };
        let bounds = self.bounds();
        let scale = CGImage::width(Some(&image)) as f64 / bounds.size.width;
        // Pixel under the cursor in screenshot coordinates, origin at the top.
        let pixel = NSPoint::new((at.x * scale).floor(), ((bounds.size.height - at.y) * scale).floor());
        let half = (LOUPE_PIXELS / 2.0).floor();
        let source = CGRect::new(CGPoint::new(pixel.x - half, pixel.y - half), CGSize::new(LOUPE_PIXELS, LOUPE_PIXELS));
        let Some(crop) = CGImage::with_image_in_rect(Some(&image), source) else { return };

        // Up and to the right of the cursor, on the other side near the screen edge.
        let offset = 24.0;
        let mut origin = NSPoint::new(at.x + offset, at.y + offset);
        if origin.x + LOUPE_SIZE > bounds.size.width {
            origin.x = at.x - offset - LOUPE_SIZE;
        }
        if origin.y + LOUPE_SIZE > bounds.size.height {
            origin.y = at.y - offset - LOUPE_SIZE;
        }
        let frame = NSRect::new(origin, NSSize::new(LOUPE_SIZE, LOUPE_SIZE));
        let clip = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(frame, 8.0, 8.0);

        NSGraphicsContext::saveGraphicsState_class();
        clip.addClip();
        NSColor::blackColor().setFill();
        NSRectFill(frame);
        if let Some(context) = NSGraphicsContext::currentContext().map(|c| c.CGContext()) {
            // No smoothing, otherwise individual pixels are not visible.
            CGContext::set_interpolation_quality(Some(&context), CGInterpolationQuality::None);
            // At the screenshot edge the cutout is smaller than LOUPE_PIXELS, and its place in the magnifier shifts with the edge.
            let cell = LOUPE_SIZE / LOUPE_PIXELS;
            let cut = CGRect::new(
                CGPoint::new(source.origin.x.max(0.0), source.origin.y.max(0.0)),
                CGSize::new(CGImage::width(Some(&crop)) as f64, CGImage::height(Some(&crop)) as f64),
            );
            let left = (cut.origin.x - source.origin.x) * cell;
            let top = (cut.origin.y - source.origin.y) * cell;
            let drawn = CGRect::new(
                CGPoint::new(frame.origin.x + left, frame.origin.y + LOUPE_SIZE - top - cut.size.height * cell),
                CGSize::new(cut.size.width * cell, cut.size.height * cell),
            );
            CGContext::draw_image(Some(&context), drawn, Some(&crop));
        }
        NSGraphicsContext::restoreGraphicsState_class();

        let cell = LOUPE_SIZE / LOUPE_PIXELS;
        let center = NSRect::new(
            NSPoint::new(frame.origin.x + half * cell, frame.origin.y + half * cell),
            NSSize::new(cell, cell),
        );
        NSColor::whiteColor().setStroke();
        NSBezierPath::strokeRect(center);
        clip.setLineWidth(2.0);
        clip.stroke();
    }

    /// Converts a rectangle from global CG coordinates to view coordinates.
    fn local_rect(&self, global: CGRect) -> NSRect {
        to_display_local(global, CGDisplayBounds(self.ivars().display.get()))
    }
}

fn text_attributes() -> Retained<NSDictionary<NSString, AnyObject>> {
    let font = assets::font(12.0);
    let color = NSColor::whiteColor();
    let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    NSDictionary::from_slices(&keys, &[&*font as &AnyObject, &*color as &AnyObject])
}

fn clip(rect: NSRect, bounds: NSRect) -> NSRect {
    let x = rect.origin.x.max(bounds.origin.x);
    let y = rect.origin.y.max(bounds.origin.y);
    let right = (rect.origin.x + rect.size.width).min(bounds.origin.x + bounds.size.width);
    let top = (rect.origin.y + rect.size.height).min(bounds.origin.y + bounds.size.height);
    NSRect::new(NSPoint::new(x, y), NSSize::new((right - x).max(0.0), (top - y).max(0.0)))
}

/// Selection from the mouse down point to the end. The square grows toward the end along the longer side.
fn selection_rect(start: NSPoint, end: NSPoint, square: bool) -> NSRect {
    let (mut dx, mut dy) = (end.x - start.x, end.y - start.y);
    if square {
        let side = dx.abs().max(dy.abs());
        dx = side.copysign(dx);
        dy = side.copysign(dy);
    }
    NSRect::new(NSPoint::new(start.x.min(start.x + dx), start.y.min(start.y + dy)), NSSize::new(dx.abs(), dy.abs()))
}

/// Converts a rectangle from global CG coordinates (origin at the top left of the main display)
/// to view coordinates on `display` (origin at the bottom left of that display).
fn to_display_local(global: CGRect, display: CGRect) -> NSRect {
    let x = global.origin.x - display.origin.x;
    let top = global.origin.y - display.origin.y;
    NSRect::new(NSPoint::new(x, display.size.height - top - global.size.height), global.size)
}

/// Shows the overlay on all screens: area selection or window picking.
/// false means the overlay was not shown, because of missing permission or it is already open.
pub fn select(mtm: MainThreadMarker, mode: Mode) -> bool {
    if !capture::has_access(mtm) || WINDOWS.with_borrow(|w| !w.is_empty()) {
        return false;
    }
    MODE.set(mode);
    APP.set(capture::frontmost_app_name());
    match mode {
        Mode::Window => {
            TARGETS.set(on_screen_windows());
            HOVERED.set(window_under_cursor());
        }
        Mode::Region => capture::freeze_displays(),
    }

    let mouse = NSEvent::mouseLocation();
    let (windows, views): (Vec<_>, Vec<_>) =
        NSScreen::screens(mtm).iter().map(|screen| overlay_window(mtm, &screen)).unzip();
    NSApplication::sharedApplication(mtm).activate();
    for window in &windows {
        if geometry::contains(window.frame(), mouse) {
            window.makeKeyAndOrderFront(None);
        } else {
            window.orderFrontRegardless();
        }
    }
    WINDOWS.set(windows);
    VIEWS.set(views);
    if mode == Mode::Region {
        NSCursor::crosshairCursor().push();
        track_pointer();
    }
    true
}

/// Gives the mouse position to the view of the screen it is on, and an empty one to the rest.
fn track_pointer() {
    let mouse = NSEvent::mouseLocation();
    VIEWS.with_borrow(|views| {
        for view in views {
            // While dragging the view tracks the position itself through mouseDragged.
            if view.ivars().start.get().is_some() {
                continue;
            }
            let pointer = view
                .window()
                .map(|w| w.frame())
                .filter(|f| geometry::contains(*f, mouse))
                .map(|f| NSPoint::new(mouse.x - f.origin.x, mouse.y - f.origin.y));
            if pointer.is_some() || view.ivars().pointer.get().is_some() {
                view.ivars().pointer.set(pointer);
                view.setNeedsDisplay(true);
            }
        }
    });
}

/// Receives a display image for the magnifier. If the overlay is already closed, the image is not needed.
pub fn show_frozen(display: CGDirectDisplayID, image: CFRetained<CGImage>) {
    if WINDOWS.with_borrow(Vec::is_empty) {
        return;
    }
    FROZEN.with_borrow_mut(|frozen| frozen.insert(display, image));
    redraw_all();
}

fn overlay_window(mtm: MainThreadMarker, screen: &NSScreen) -> (Retained<OverlayWindow>, Retained<SelectionView>) {
    let frame = screen.frame();
    // ponytail: selection stays within one display, dragging across screens does not stitch images.
    let window: Retained<OverlayWindow> = unsafe {
        msg_send![
            OverlayWindow::alloc(mtm),
            initWithContentRect: frame,
            styleMask: NSWindowStyleMask::Borderless,
            backing: NSBackingStoreType::Buffered,
            defer: false,
        ]
    };
    // Otherwise close() would free the window a second time, on top of Retained.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setLevel(NSScreenSaverWindowLevel);
    window.setOpaque(false);
    window.setHasShadow(false);
    window.setAcceptsMouseMovedEvents(true);
    window.setBackgroundColor(Some(&NSColor::clearColor()));
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    let size = CGRect::new(CGPoint::ZERO, CGSize::new(frame.size.width, frame.size.height));
    let view = SelectionView::new(mtm, size, display_id(screen));
    window.setContentView(Some(&view));
    window.makeFirstResponder(Some(&view));
    window.setFrame_display(frame, false);
    (window, view)
}

fn display_id(screen: &NSScreen) -> CGDirectDisplayID {
    screen
        .deviceDescription()
        .objectForKey(ns_string!("NSScreenNumber"))
        .and_then(|n| n.downcast::<NSNumber>().ok())
        .map_or(0, |n| n.unsignedIntValue())
}

/// Collects regular windows of other apps, front to back.
pub fn on_screen_windows() -> Vec<TargetWindow> {
    let options = CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements;
    let Some(list) = CGWindowListCopyWindowInfo(options, kCGNullWindowID) else {
        return Vec::new();
    };
    // SAFETY: CFArray and NSArray are toll-free bridged, and the window list elements are dictionaries describing them.
    let list: &NSArray<NSDictionary<NSString, AnyObject>> = unsafe { bridge::toll_free(&*list) };
    let own_pid = capture::own_pid();

    list.iter()
        .filter_map(|info| {
            let pid = number(&info, ns_string!("kCGWindowOwnerPID"))?.intValue();
            // Layer 0 is regular windows, the menu bar, Dock and popups sit above.
            if number(&info, ns_string!("kCGWindowLayer"))?.integerValue() != 0
                || pid == own_pid
                // Browsers keep fully transparent windows over tabs, the cursor must not catch on them.
                || number(&info, ns_string!("kCGWindowAlpha"))?.doubleValue() == 0.0
            {
                return None;
            }
            let bounds = info.objectForKey(ns_string!("kCGWindowBounds"))?;
            let mut frame = CGRect::default();
            // SAFETY: kCGWindowBounds is a dictionary with x, y, Width and Height, and NSDictionary and CFDictionary are toll-free bridged.
            let bounds: &CFDictionary = unsafe { bridge::toll_free(&*bounds) };
            unsafe { CGRectMakeWithDictionaryRepresentation(Some(bounds), &raw mut frame) }.then_some(())?;
            let id = number(&info, ns_string!("kCGWindowNumber"))?.unsignedIntValue();
            Some(TargetWindow { id, pid, frame })
        })
        .collect()
}

fn window_under_cursor() -> Option<TargetWindow> {
    let cursor = CGEvent::location(CGEvent::new(None).as_deref());
    TARGETS.with_borrow(|targets| targets.iter().copied().find(|t| geometry::contains(t.frame, cursor)))
}

fn update_hover() {
    let hovered = window_under_cursor();
    if hovered.map(|t| t.id) != HOVERED.get().map(|t| t.id) {
        HOVERED.set(hovered);
        redraw_all();
    }
}

fn redraw_all() {
    WINDOWS.with_borrow(|windows| {
        for window in windows {
            if let Some(view) = window.contentView() {
                view.setNeedsDisplay(true);
            }
        }
    });
}

fn close() {
    WINDOWS.with_borrow(|windows| windows.iter().for_each(|w| w.orderOut(None)));
    if MODE.get() == Mode::Region {
        NSCursor::pop_class();
    }
    TARGETS.take();
    HOVERED.set(None);
    FROZEN.take();
    // Windows are freed after the event handler returns, not in the middle of it.
    DispatchQueue::main().exec_async(|| {
        drop(VIEWS.take());
        drop(WINDOWS.take());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_on_display_left_and_above_main_maps_into_its_view() {
        let display = CGRect::new(CGPoint::new(-1920.0, -400.0), CGSize::new(1920.0, 1080.0));
        let window = CGRect::new(CGPoint::new(-1820.0, -300.0), CGSize::new(800.0, 600.0));
        let local = to_display_local(window, display);
        assert_eq!(local.origin, NSPoint::new(100.0, 380.0));
        assert_eq!(local.size, window.size);
    }

    #[test]
    fn selection_follows_drag_and_squares_toward_the_pointer() {
        let start = NSPoint::new(100.0, 100.0);
        // dragging right and up
        assert_eq!(
            selection_rect(start, NSPoint::new(130.0, 115.0), false),
            NSRect::new(start, NSSize::new(30.0, 15.0))
        );
        assert_eq!(
            selection_rect(start, NSPoint::new(130.0, 115.0), true),
            NSRect::new(start, NSSize::new(30.0, 30.0))
        );
        // dragging left and down
        assert_eq!(
            selection_rect(start, NSPoint::new(90.0, 60.0), true),
            NSRect::new(NSPoint::new(60.0, 60.0), NSSize::new(40.0, 40.0))
        );
    }
}
