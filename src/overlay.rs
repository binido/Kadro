//! Оверлей поверх всех экранов: выделение области с лупой или выбор окна мышью.

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
use crate::{assets, capture, geometry, keys, output};

/// Сторона лупы в точках и сколько пикселей снимка в ней видно по каждой оси.
const LOUPE_SIZE: f64 = 120.0;
const LOUPE_PIXELS: f64 = 15.0;

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Region,
    Window,
}

/// Окно другого приложения в глобальных координатах CG - от левого верхнего угла главного дисплея.
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
    // Окна на экране от переднего к заднему, снятые в момент открытия оверлея.
    static TARGETS: RefCell<Vec<TargetWindow>> = const { RefCell::new(Vec::new()) };
    static HOVERED: Cell<Option<TargetWindow>> = const { Cell::new(None) };
    // Приложение, которое было активным до оверлея. Сам оверлей делает активным Kadro.
    static APP: RefCell<String> = const { RefCell::new(String::new()) };
    // Снимки дисплеев на момент открытия оверлея, из них рисуется лупа.
    static FROZEN: RefCell<HashMap<CGDirectDisplayID, CFRetained<CGImage>>> = RefCell::new(HashMap::new());
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
    /// Позиция мыши без сдвига стрелками.
    pointer: Cell<Option<NSPoint>>,
    /// Сдвиг конца выделения стрелками, обнуляется с новым нажатием мыши.
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

        // Движение мыши приходит только ключевому окну, поэтому позиция раздаётся всем экранам сразу.
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

        // Shift нажат или отпущен посреди протягивания - выделение сразу становится квадратом или обратно.
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
                // Клик без протягивания - не выделение, оверлей остаётся открытым.
                Some(rect) if rect.size.width >= 2.0 && rect.size.height >= 2.0 => {
                    let height = self.bounds().size.height;
                    // У NSView начало координат внизу, у ScreenCaptureKit - вверху.
                    let flipped = CGRect::new(
                        CGPoint::new(rect.origin.x, height - rect.origin.y - rect.size.height),
                        rect.size,
                    );
                    let app = APP.with_borrow(Clone::clone);
                    close();
                    capture::capture_region(self.ivars().display.get(), flipped, app);
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
            // Стрелки двигают угол, который тянет мышь, поэтому работают только во время протягивания.
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

    /// Позиция события в координатах вида, прижатая к его границам.
    fn point(&self, event: &NSEvent) -> NSPoint {
        self.clamp(self.convertPoint_fromView(event.locationInWindow(), None))
    }

    fn clamp(&self, p: NSPoint) -> NSPoint {
        let size = self.bounds().size;
        NSPoint::new(p.x.clamp(0.0, size.width), p.y.clamp(0.0, size.height))
    }

    /// Конец выделения: мышь плюс сдвиг стрелками.
    fn end_point(&self) -> Option<NSPoint> {
        let pointer = self.ivars().pointer.get()?;
        let nudge = self.ivars().nudge.get();
        Some(self.clamp(NSPoint::new(pointer.x + nudge.x, pointer.y + nudge.y)))
    }

    fn update_selection(&self) {
        let (Some(start), Some(end)) = (self.ivars().start.get(), self.end_point()) else { return };
        let square = NSEvent::modifierFlags_class().contains(NSEventModifierFlags::Shift);
        let rect = selection_rect(start, end, square);
        // Квадрат может выйти за край экрана, тогда он обрезается краем.
        self.ivars().selection.set(Some(clip(rect, self.bounds())));
        self.setNeedsDisplay(true);
    }

    fn scale(&self) -> f64 {
        self.window().map_or(1.0, |w| w.backingScaleFactor())
    }

    /// Размер выделения в пикселях снимка, под выделением или над ним, если внизу нет места.
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

    /// Лупа рядом с курсором: пиксели снимка под ним крупно, центральный пиксель в рамке.
    fn draw_loupe(&self, at: NSPoint) {
        let Some(image) = FROZEN.with_borrow(|frozen| frozen.get(&self.ivars().display.get()).cloned()) else {
            return;
        };
        let bounds = self.bounds();
        let scale = CGImage::width(Some(&image)) as f64 / bounds.size.width;
        // Пиксель под курсором в координатах снимка, у которого начало вверху.
        let pixel = NSPoint::new((at.x * scale).floor(), ((bounds.size.height - at.y) * scale).floor());
        let half = (LOUPE_PIXELS / 2.0).floor();
        let source = CGRect::new(CGPoint::new(pixel.x - half, pixel.y - half), CGSize::new(LOUPE_PIXELS, LOUPE_PIXELS));
        let Some(crop) = CGImage::with_image_in_rect(Some(&image), source) else { return };

        // Справа сверху от курсора, а у края экрана - с другой стороны.
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
            // Без сглаживания, иначе отдельных пикселей не видно.
            CGContext::set_interpolation_quality(Some(&context), CGInterpolationQuality::None);
            // У края снимка вырезка меньше LOUPE_PIXELS, и её место в лупе сдвигается вместе с краем.
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

    /// Переводит прямоугольник из глобальных координат CG в координаты вида.
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

/// Выделение от точки нажатия до конца. Квадрат растёт в сторону конца по большей стороне.
fn selection_rect(start: NSPoint, end: NSPoint, square: bool) -> NSRect {
    let (mut dx, mut dy) = (end.x - start.x, end.y - start.y);
    if square {
        let side = dx.abs().max(dy.abs());
        dx = side.copysign(dx);
        dy = side.copysign(dy);
    }
    NSRect::new(NSPoint::new(start.x.min(start.x + dx), start.y.min(start.y + dy)), NSSize::new(dx.abs(), dy.abs()))
}

/// Переводит прямоугольник из глобальных координат CG (начало вверху слева главного дисплея)
/// в координаты вида на дисплее `display` (начало внизу слева этого дисплея).
fn to_display_local(global: CGRect, display: CGRect) -> NSRect {
    let x = global.origin.x - display.origin.x;
    let top = global.origin.y - display.origin.y;
    NSRect::new(NSPoint::new(x, display.size.height - top - global.size.height), global.size)
}

/// Показывает оверлей на всех экранах: выделение области или выбор окна.
/// false - оверлей не показан: нет доступа или он уже открыт.
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

/// Отдаёт позицию мыши виду того экрана, где она сейчас, остальным - пустую.
fn track_pointer() {
    let mouse = NSEvent::mouseLocation();
    VIEWS.with_borrow(|views| {
        for view in views {
            // Во время протягивания позицию ведёт сам вид через mouseDragged.
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

/// Принимает снимок дисплея для лупы. Если оверлей уже закрыт, снимок не нужен.
pub fn show_frozen(display: CGDirectDisplayID, image: CFRetained<CGImage>) {
    if WINDOWS.with_borrow(Vec::is_empty) {
        return;
    }
    FROZEN.with_borrow_mut(|frozen| frozen.insert(display, image));
    redraw_all();
}

fn overlay_window(mtm: MainThreadMarker, screen: &NSScreen) -> (Retained<OverlayWindow>, Retained<SelectionView>) {
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
    (window, view)
}

fn display_id(screen: &NSScreen) -> CGDirectDisplayID {
    screen
        .deviceDescription()
        .objectForKey(ns_string!("NSScreenNumber"))
        .and_then(|n| n.downcast::<NSNumber>().ok())
        .map_or(0, |n| n.unsignedIntValue())
}

/// Собирает обычные окна других приложений, от переднего к заднему.
pub fn on_screen_windows() -> Vec<TargetWindow> {
    let options = CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements;
    let Some(list) = CGWindowListCopyWindowInfo(options, kCGNullWindowID) else {
        return Vec::new();
    };
    // SAFETY: CFArray и NSArray взаимозаменяемы, а элементы списка окон - словари с их описанием.
    let list: &NSArray<NSDictionary<NSString, AnyObject>> = unsafe { bridge::toll_free(&*list) };
    let own_pid = capture::own_pid();

    list.iter()
        .filter_map(|info| {
            let pid = number(&info, ns_string!("kCGWindowOwnerPID"))?.intValue();
            // Слой 0 - обычные окна, выше лежат строка меню, Dock и всплывающие панели.
            if number(&info, ns_string!("kCGWindowLayer"))?.integerValue() != 0
                || pid == own_pid
                // Браузеры держат над вкладками полностью прозрачные окна, курсор не должен на них цепляться.
                || number(&info, ns_string!("kCGWindowAlpha"))?.doubleValue() == 0.0
            {
                return None;
            }
            let bounds = info.objectForKey(ns_string!("kCGWindowBounds"))?;
            let mut frame = CGRect::default();
            // SAFETY: kCGWindowBounds - словарь с x, y, Width и Height, а NSDictionary и CFDictionary взаимозаменяемы.
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
    // Окна освобождаются после выхода из обработчика события, а не посреди него.
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
        // протягивание вправо-вверх
        assert_eq!(
            selection_rect(start, NSPoint::new(130.0, 115.0), false),
            NSRect::new(start, NSSize::new(30.0, 15.0))
        );
        assert_eq!(
            selection_rect(start, NSPoint::new(130.0, 115.0), true),
            NSRect::new(start, NSSize::new(30.0, 30.0))
        );
        // протягивание влево-вниз
        assert_eq!(
            selection_rect(start, NSPoint::new(90.0, 60.0), true),
            NSRect::new(NSPoint::new(60.0, 60.0), NSSize::new(40.0, 40.0))
        );
    }
}
