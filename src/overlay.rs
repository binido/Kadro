use std::cell::{Cell, RefCell};

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSBezierPath, NSColor, NSCompositingOperation, NSCursor,
    NSEvent, NSRectFill, NSRectFillUsingOperation, NSResponder, NSScreen, NSScreenSaverWindowLevel,
    NSView, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGDirectDisplayID;
use objc2_foundation::{ns_string, NSNumber, NSObject, NSPoint, NSRect};

use crate::capture;

const ESCAPE_KEY: u16 = 53;

thread_local! {
    static WINDOWS: RefCell<Vec<Retained<OverlayWindow>>> = const { RefCell::new(Vec::new()) };
}

define_class!(
    #[unsafe(super(NSWindow, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    struct OverlayWindow;

    impl OverlayWindow {
        // Окно без рамки по умолчанию не получает клавиатуру, а без неё не работает Esc.
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
            NSColor::colorWithWhite_alpha(0.0, 0.25).setFill();
            NSRectFill(self.bounds());
            if let Some(selection) = self.ivars().selection.get() {
                NSRectFillUsingOperation(selection, NSCompositingOperation::Clear);
                NSColor::whiteColor().setStroke();
                NSBezierPath::strokeRect(selection);
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.ivars().start.set(Some(self.point(event)));
            self.ivars().selection.set(None);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let Some(start) = self.ivars().start.get() else { return };
            self.ivars().selection.set(Some(rect_between(start, self.point(event))));
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            self.ivars().start.set(None);
            match self.ivars().selection.get() {
                // Клик без протягивания - не выделение, оверлей остаётся открытым.
                Some(rect) if rect.size.width >= 2.0 && rect.size.height >= 2.0 => {
                    let height = self.bounds().size.height;
                    // У NSView начало координат внизу, у ScreenCaptureKit - вверху.
                    let flipped = CGRect::new(
                        CGPoint::new(rect.origin.x, height - rect.origin.y - rect.size.height),
                        rect.size,
                    );
                    finish(Some((self.ivars().display.get(), flipped)));
                }
                _ => {
                    self.ivars().selection.set(None);
                    self.setNeedsDisplay(true);
                }
            }
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            if event.keyCode() == ESCAPE_KEY {
                finish(None);
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

    /// Позиция события в координатах вида, прижатая к его границам.
    fn point(&self, event: &NSEvent) -> NSPoint {
        let p = self.convertPoint_fromView(event.locationInWindow(), None);
        let size = self.bounds().size;
        NSPoint::new(p.x.clamp(0.0, size.width), p.y.clamp(0.0, size.height))
    }
}

/// Показывает оверлей выделения области на всех экранах.
pub fn select_region(mtm: MainThreadMarker) {
    if !capture::has_access() || WINDOWS.with_borrow(|w| !w.is_empty()) {
        return;
    }
    let mouse = NSEvent::mouseLocation();
    let windows: Vec<_> = NSScreen::screens(mtm)
        .iter()
        .map(|screen| overlay_window(mtm, &screen))
        .collect();
    NSApplication::sharedApplication(mtm).activate();
    for window in &windows {
        if capture::contains(window.frame(), mouse) {
            window.makeKeyAndOrderFront(None);
        } else {
            window.orderFrontRegardless();
        }
    }
    NSCursor::crosshairCursor().push();
    WINDOWS.set(windows);
}

fn overlay_window(mtm: MainThreadMarker, screen: &NSScreen) -> Retained<OverlayWindow> {
    let frame = screen.frame();
    // ponytail: выделение в пределах одного дисплея, протягивание через границу экранов не склеивает снимки.
    let window: Retained<OverlayWindow> = unsafe {
        msg_send![
            OverlayWindow::alloc(mtm),
            initWithContentRect: frame,
            styleMask: NSWindowStyleMask::Borderless,
            backing: NSBackingStoreType::Buffered,
            defer: false,
        ]
    };
    // Иначе close() освободит окно второй раз, поверх Retained.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setLevel(NSScreenSaverWindowLevel);
    window.setOpaque(false);
    window.setHasShadow(false);
    window.setBackgroundColor(Some(&NSColor::clearColor()));
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    let size = CGRect::new(CGPoint::ZERO, CGSize::new(frame.size.width, frame.size.height));
    let view = SelectionView::new(mtm, size, display_id(screen));
    window.setContentView(Some(&view));
    window.makeFirstResponder(Some(&view));
    window.setFrame_display(frame, false);
    window
}

fn display_id(screen: &NSScreen) -> CGDirectDisplayID {
    screen
        .deviceDescription()
        .objectForKey(ns_string!("NSScreenNumber"))
        .and_then(|n| n.downcast::<NSNumber>().ok())
        .map_or(0, |n| n.unsignedIntValue())
}

fn finish(selection: Option<(CGDirectDisplayID, CGRect)>) {
    WINDOWS.with_borrow(|windows| windows.iter().for_each(|w| w.orderOut(None)));
    NSCursor::pop_class();
    // Окна освобождаются после выхода из обработчика события, а не посреди него.
    DispatchQueue::main().exec_async(|| drop(WINDOWS.take()));
    if let Some((display, rect)) = selection {
        capture::capture_region(display, rect);
    }
}

fn rect_between(a: NSPoint, b: NSPoint) -> NSRect {
    NSRect::new(
        NSPoint::new(a.x.min(b.x), a.y.min(b.y)),
        CGSize::new((a.x - b.x).abs(), (a.y - b.y).abs()),
    )
}
