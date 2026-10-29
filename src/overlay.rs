use std::cell::{Cell, RefCell};

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSBezierPath, NSColor, NSCompositingOperation, NSCursor,
    NSEvent, NSRectFill, NSRectFillUsingOperation, NSResponder, NSScreen, NSScreenSaverWindowLevel,
    NSView, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::{CFDictionary, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    kCGNullWindowID, CGDirectDisplayID, CGDisplayBounds, CGEvent, CGRectMakeWithDictionaryRepresentation,
    CGWindowID, CGWindowListCopyWindowInfo, CGWindowListOption,
};
use objc2_foundation::{ns_string, NSArray, NSDictionary, NSNumber, NSObject, NSPoint, NSRect, NSString};

use crate::capture;

const ESCAPE_KEY: u16 = 53;

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Region,
    Window,
}

/// Окно другого приложения в глобальных координатах CG - от левого верхнего угла главного дисплея.
#[derive(Clone, Copy)]
struct TargetWindow {
    id: CGWindowID,
    frame: CGRect,
}

thread_local! {
    static WINDOWS: RefCell<Vec<Retained<OverlayWindow>>> = const { RefCell::new(Vec::new()) };
    static MODE: Cell<Mode> = const { Cell::new(Mode::Region) };
    // Окна на экране от переднего к заднему, снятые в момент открытия оверлея.
    static TARGETS: RefCell<Vec<TargetWindow>> = const { RefCell::new(Vec::new()) };
    static HOVERED: Cell<Option<TargetWindow>> = const { Cell::new(None) };
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
            match MODE.get() {
                Mode::Region => {
                    NSColor::colorWithWhite_alpha(0.0, 0.25).setFill();
                    NSRectFill(self.bounds());
                    if let Some(selection) = self.ivars().selection.get() {
                        NSRectFillUsingOperation(selection, NSCompositingOperation::Clear);
                        NSColor::whiteColor().setStroke();
                        NSBezierPath::strokeRect(selection);
                    }
                }
                Mode::Window => {
                    if let Some(target) = HOVERED.get() {
                        let rect = self.local_rect(target.frame);
                        NSColor::selectedContentBackgroundColor().colorWithAlphaComponent(0.3).setFill();
                        NSRectFill(rect);
                        NSColor::selectedContentBackgroundColor().setStroke();
                        NSBezierPath::strokeRect(rect);
                        // Окна выше выбранного в снимок не попадут, поэтому подсветка их не закрашивает.
                        TARGETS.with_borrow(|targets| {
                            for above in targets.iter().take_while(|t| t.id != target.id) {
                                NSRectFillUsingOperation(self.local_rect(above.frame), NSCompositingOperation::Clear);
                            }
                        });
                    }
                }
            }
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, _event: &NSEvent) {
            if MODE.get() == Mode::Window {
                update_hover();
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            match MODE.get() {
                Mode::Region => {
                    self.ivars().start.set(Some(self.point(event)));
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
            let Some(start) = self.ivars().start.get() else { return };
            self.ivars().selection.set(Some(rect_between(start, self.point(event))));
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            if MODE.get() != Mode::Region {
                return;
            }
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
                    close();
                    capture::capture_region(self.ivars().display.get(), flipped);
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
                close();
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

    /// Переводит прямоугольник из глобальных координат CG в координаты вида.
    fn local_rect(&self, global: CGRect) -> NSRect {
        let display = CGDisplayBounds(self.ivars().display.get());
        let x = global.origin.x - display.origin.x;
        let top = global.origin.y - display.origin.y;
        NSRect::new(
            NSPoint::new(x, display.size.height - top - global.size.height),
            global.size,
        )
    }
}

/// Показывает оверлей на всех экранах: выделение области или выбор окна.
pub fn select(mtm: MainThreadMarker, mode: Mode) {
    if !capture::has_access() || WINDOWS.with_borrow(|w| !w.is_empty()) {
        return;
    }
    MODE.set(mode);
    if mode == Mode::Window {
        TARGETS.set(on_screen_windows());
        HOVERED.set(window_under_cursor());
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
    if mode == Mode::Region {
        NSCursor::crosshairCursor().push();
    }
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
    window
}

fn display_id(screen: &NSScreen) -> CGDirectDisplayID {
    screen
        .deviceDescription()
        .objectForKey(ns_string!("NSScreenNumber"))
        .and_then(|n| n.downcast::<NSNumber>().ok())
        .map_or(0, |n| n.unsignedIntValue())
}

/// Собирает обычные окна других приложений, от переднего к заднему.
fn on_screen_windows() -> Vec<TargetWindow> {
    let options = CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements;
    let Some(list) = CGWindowListCopyWindowInfo(options, kCGNullWindowID) else {
        return Vec::new();
    };
    // CFArray и NSArray взаимозаменяемы без копирования.
    let list: &NSArray<NSDictionary<NSString, AnyObject>> = unsafe { &*(&*list as *const _ as *const _) };
    let own_pid = std::process::id() as isize;
    let number = |info: &NSDictionary<NSString, AnyObject>, key: &NSString| {
        info.objectForKey(key)?.downcast::<NSNumber>().ok()
    };

    list.iter()
        .filter_map(|info| {
            // Слой 0 - обычные окна, выше лежат строка меню, Dock и всплывающие панели.
            if number(&info, ns_string!("kCGWindowLayer"))?.integerValue() != 0
                || number(&info, ns_string!("kCGWindowOwnerPID"))?.integerValue() == own_pid
                // Браузеры держат над вкладками полностью прозрачные окна, курсор не должен на них цепляться.
                || number(&info, ns_string!("kCGWindowAlpha"))?.doubleValue() == 0.0
            {
                return None;
            }
            let bounds = info.objectForKey(ns_string!("kCGWindowBounds"))?;
            let mut frame = CGRect::default();
            let bounds: &CFDictionary = unsafe { &*(&*bounds as *const AnyObject as *const CFDictionary) };
            unsafe { CGRectMakeWithDictionaryRepresentation(Some(bounds), &mut frame) }.then_some(())?;
            let id = number(&info, ns_string!("kCGWindowNumber"))?.unsignedIntValue();
            Some(TargetWindow { id, frame })
        })
        .collect()
}

fn window_under_cursor() -> Option<TargetWindow> {
    let cursor = CGEvent::location(CGEvent::new(None).as_deref());
    TARGETS.with_borrow(|targets| targets.iter().copied().find(|t| capture::contains(t.frame, cursor)))
}

fn update_hover() {
    let hovered = window_under_cursor();
    if hovered.map(|t| t.id) != HOVERED.get().map(|t| t.id) {
        HOVERED.set(hovered);
        WINDOWS.with_borrow(|windows| {
            for window in windows {
                if let Some(view) = window.contentView() {
                    view.setNeedsDisplay(true);
                }
            }
        });
    }
}

fn close() {
    WINDOWS.with_borrow(|windows| windows.iter().for_each(|w| w.orderOut(None)));
    if MODE.get() == Mode::Region {
        NSCursor::pop_class();
    }
    TARGETS.take();
    HOVERED.set(None);
    // Окна освобождаются после выхода из обработчика события, а не посреди него.
    DispatchQueue::main().exec_async(|| drop(WINDOWS.take()));
}

fn rect_between(a: NSPoint, b: NSPoint) -> NSRect {
    NSRect::new(
        NSPoint::new(a.x.min(b.x), a.y.min(b.y)),
        CGSize::new((a.x - b.x).abs(), (a.y - b.y).abs()),
    )
}
