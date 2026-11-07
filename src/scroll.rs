//! Захват с прокруткой: выделенная область снимается кадр за кадром, пока её прокручивают,
//! и кадры склеиваются в один длинный снимок.
//!
//! Автоматическая прокрутка шлёт события колеса туда, где выделена область, и после каждого шага
//! ждёт, пока картинка перестанет меняться. В ручном режиме прокручивает пользователь, а Kadro
//! подхватывает каждое устоявшееся положение. Сеанс живёт в `SESSION` до конца или отмены.

use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr::NonNull;

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertSecondButtonReturn, NSApplication, NSBackingStoreType, NSBezierPath,
    NSButton, NSColor, NSPanel, NSResponder, NSScreenSaverWindowLevel, NSStackView, NSTextField, NSView, NSWindow,
    NSWindowCollectionBehavior, NSWindowStyleMask, NSWorkspace,
};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFRetained, CFString, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGDirectDisplayID, CGDisplayBounds, CGEvent, CGEventTapLocation, CGImage, CGScrollEventUnit,
    CGWarpMouseCursorPosition,
};
use objc2_foundation::{
    NSArray, NSError, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSTimer, NSURL, ns_string,
};
use objc2_screen_capture_kit::{SCContentFilter, SCScreenshotManager, SCStreamConfiguration};

use crate::capture::{self, Kind, Shot};
use crate::settings::Settings;
use crate::stitch::{Frame, Push, Stitcher};
use crate::{assets, output, preview};

const ACCESSIBILITY_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";
/// Пауза после шага прокрутки и между проверками, устоялась ли картинка.
const SETTLE_SECONDS: f64 = 0.12;
/// Сколько раз проверять, устоялась ли картинка, прежде чем взять кадр как есть.
const SETTLE_TRIES: u32 = 8;
/// Как часто снимать кадр в ручном режиме.
const MANUAL_SECONDS: f64 = 0.15;
/// Самое большое смещение одного события колеса в точках. Крупный шаг дробится на несколько событий.
const WHEEL_STEP: f64 = 60.0;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
}

thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
    static CONTROLLER: RefCell<Option<Retained<Controller>>> = const { RefCell::new(None) };
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Down,
    Up,
    Manual,
}

struct Session {
    /// Область в глобальных точках CG.
    area: CGRect,
    scale: f64,
    app: String,
    filter: Retained<SCContentFilter>,
    config: Retained<SCStreamConfiguration>,
    mode: Option<Mode>,
    stitcher: Option<Stitcher>,
    /// Последний снятый кадр, чтобы понять, устоялась ли картинка.
    previous: Option<Frame>,
    tries: u32,
    limit: usize,
    hud: Retained<NSPanel>,
    border: Retained<NSWindow>,
    status: Retained<NSTextField>,
    start_buttons: Vec<Retained<NSButton>>,
    done: Retained<NSButton>,
    timer: Option<Retained<NSTimer>>,
}

define_class!(
    /// Кнопки окна управления прокруткой.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    struct Controller;

    impl Controller {
        #[unsafe(method(scrollDown:))]
        fn scroll_down(&self, _sender: Option<&AnyObject>) {
            begin(self.mtm(), Mode::Down);
        }

        #[unsafe(method(scrollUp:))]
        fn scroll_up(&self, _sender: Option<&AnyObject>) {
            begin(self.mtm(), Mode::Up);
        }

        #[unsafe(method(scrollManually:))]
        fn scroll_manually(&self, _sender: Option<&AnyObject>) {
            begin(self.mtm(), Mode::Manual);
        }

        #[unsafe(method(finishScrolling:))]
        fn finish_scrolling(&self, _sender: Option<&AnyObject>) {
            finish();
        }

        #[unsafe(method(cancelScrolling:))]
        fn cancel_scrolling(&self, _sender: Option<&AnyObject>) {
            close();
        }
    }

    unsafe impl NSObjectProtocol for Controller {}
);

define_class!(
    /// Окно управления без рамки, которое всё же принимает клавиатуру: Return и Esc.
    #[unsafe(super(NSPanel, NSWindow, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    struct HudPanel;

    impl HudPanel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            true
        }
    }
);

define_class!(
    /// Пунктирная рамка вокруг прокручиваемой области. Мышь проходит сквозь неё.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    struct BorderView;

    impl BorderView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let bounds = self.bounds();
            let border = NSBezierPath::bezierPathWithRect(NSRect::new(
                NSPoint::new(bounds.origin.x + 1.5, bounds.origin.y + 1.5),
                NSSize::new(bounds.size.width - 3.0, bounds.size.height - 3.0),
            ));
            border.setLineWidth(2.0);
            let dash = [6.0, 4.0];
            // SAFETY: массив из двух длин живёт до конца вызова.
            unsafe { border.setLineDash_count_phase(dash.as_ptr(), 2, 0.0) };
            NSColor::systemPinkColor().setStroke();
            border.stroke();
        }
    }
);

/// Начинает сеанс для области `rect` в точках от левого верхнего угла дисплея: показывает рамку
/// и кнопки выбора направления.
pub fn start(display: CGDirectDisplayID, rect: CGRect, app: String) {
    capture::with_content(move |content| {
        let Some(found) = unsafe { content.displays() }.iter().find(|d| unsafe { d.displayID() } == display) else {
            return eprintln!("kadro: display to capture is gone");
        };
        let filter = capture::display_filter(content, &found);
        let scale = f64::from(unsafe { filter.pointPixelScale() });
        let config = unsafe { SCStreamConfiguration::new() };
        unsafe {
            config.setSourceRect(rect);
            config.setShowsCursor(false);
        }
        capture::set_output_size(&config, rect.size, scale);
        let origin = CGDisplayBounds(display).origin;
        let area = CGRect::new(CGPoint::new(origin.x + rect.origin.x, origin.y + rect.origin.y), rect.size);
        let app = app.clone();
        // Фильтр и настройки создаются один раз и дальше используются только на главном потоке.
        let parts = Sendable((filter, config));
        DispatchQueue::main().exec_async(move || {
            let Some(mtm) = MainThreadMarker::new() else { return };
            let (filter, config) = parts.into_inner();
            open(mtm, area, scale, app, filter, config);
        });
    });
}

/// Обёртка, чтобы передать объекты ScreenCaptureKit на главный поток, где с ними и работают.
struct Sendable<T>(T);
// SAFETY: объекты создаются в обработчике ScreenCaptureKit и дальше используются только на главном потоке.
unsafe impl<T> Send for Sendable<T> {}

impl<T> Sendable<T> {
    /// Метод, а не разбор поля: замыкание забирает обёртку целиком, а не её непересылаемое поле.
    fn into_inner(self) -> T {
        self.0
    }
}

fn open(
    mtm: MainThreadMarker,
    area: CGRect,
    scale: f64,
    app: String,
    filter: Retained<SCContentFilter>,
    config: Retained<SCStreamConfiguration>,
) {
    close();
    let controller: Retained<Controller> = unsafe { msg_send![Controller::alloc(mtm), init] };
    let button = |title: &str, action: Sel, key: &str| {
        let button = unsafe {
            NSButton::buttonWithTitle_target_action(&NSString::from_str(title), Some(&controller), Some(action), mtm)
        };
        button.setKeyEquivalent(&NSString::from_str(key));
        button
    };
    let start_buttons = vec![
        button("Scroll Down", sel!(scrollDown:), "\r"),
        button("Scroll Up", sel!(scrollUp:), ""),
        button("Scroll Myself", sel!(scrollManually:), ""),
    ];
    let done = button("Done", sel!(finishScrolling:), "");
    done.setHidden(true);
    let cancel = button("Cancel", sel!(cancelScrolling:), "\u{1b}");
    let status = NSTextField::labelWithString(ns_string!("Choose how to scroll"), mtm);
    status.setFont(Some(&assets::font(13.0)));
    let mut views: Vec<Retained<NSView>> = vec![Retained::into_super(Retained::into_super(status.clone()))];
    views.extend(start_buttons.iter().map(|b| Retained::into_super(Retained::into_super(b.clone()))));
    views.push(Retained::into_super(Retained::into_super(done.clone())));
    views.push(Retained::into_super(Retained::into_super(cancel)));
    let row = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&views), mtm);
    row.setSpacing(8.0);
    let size = row.fittingSize();
    let size = NSSize::new(size.width + 24.0, size.height + 16.0);
    row.setFrame(NSRect::new(NSPoint::new(12.0, 8.0), NSSize::new(size.width - 24.0, size.height - 16.0)));

    // Окно управления под областью, а если внизу нет места - внутри неё у нижнего края.
    let cocoa = preview::to_cocoa(mtm, area);
    let below = cocoa.origin.y - size.height - 10.0;
    let y = if below > 0.0 { below } else { cocoa.origin.y + 10.0 };
    let x = cocoa.origin.x + (cocoa.size.width - size.width) / 2.0;
    let hud: Retained<HudPanel> = unsafe {
        msg_send![
            HudPanel::alloc(mtm),
            initWithContentRect: NSRect::new(NSPoint::new(x, y), size),
            styleMask: NSWindowStyleMask::Borderless,
            backing: NSBackingStoreType::Buffered,
            defer: false,
        ]
    };
    let hud: Retained<NSPanel> = Retained::into_super(hud);
    unsafe { hud.setReleasedWhenClosed(false) };
    // Панель по умолчанию прячется, когда Kadro не активен, а прокручиваемое приложение может перехватить фокус.
    hud.setHidesOnDeactivate(false);
    hud.setLevel(NSScreenSaverWindowLevel);
    hud.setBackgroundColor(Some(&NSColor::windowBackgroundColor()));
    hud.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    if let Some(content) = hud.contentView() {
        content.addSubview(&row);
    }

    let frame = NSRect::new(
        NSPoint::new(cocoa.origin.x - 3.0, cocoa.origin.y - 3.0),
        NSSize::new(cocoa.size.width + 6.0, cocoa.size.height + 6.0),
    );
    let border: Retained<NSWindow> = unsafe {
        msg_send![
            NSWindow::alloc(mtm),
            initWithContentRect: frame,
            styleMask: NSWindowStyleMask::Borderless,
            backing: NSBackingStoreType::Buffered,
            defer: false,
        ]
    };
    unsafe { border.setReleasedWhenClosed(false) };
    border.setLevel(NSScreenSaverWindowLevel);
    border.setOpaque(false);
    border.setHasShadow(false);
    border.setIgnoresMouseEvents(true);
    border.setBackgroundColor(Some(&NSColor::clearColor()));
    let view: Retained<BorderView> =
        unsafe { msg_send![BorderView::alloc(mtm), initWithFrame: NSRect::new(NSPoint::ZERO, frame.size)] };
    border.setContentView(Some(&view));
    border.orderFrontRegardless();

    NSApplication::sharedApplication(mtm).activate();
    hud.makeKeyAndOrderFront(None);
    CONTROLLER.set(Some(controller));
    let limit = Settings::load().scroll_limit.pixels();
    SESSION.set(Some(Session {
        area,
        scale,
        app,
        filter,
        config,
        mode: None,
        stitcher: None,
        previous: None,
        tries: 0,
        limit,
        hud,
        border,
        status,
        start_buttons,
        done,
        timer: None,
    }));
}

/// Начинает прокрутку в выбранном режиме. Автоматической нужен доступ к универсальному доступу.
fn begin(mtm: MainThreadMarker, mode: Mode) {
    if mode != Mode::Manual && !trusted() {
        match explain_accessibility(mtm) {
            Some(Mode::Manual) => return begin(mtm, Mode::Manual),
            _ => return,
        }
    }
    with_session(|session| {
        session.mode = Some(mode);
        for button in &session.start_buttons {
            button.setHidden(true);
        }
        session.done.setHidden(false);
        session.done.setKeyEquivalent(ns_string!("\r"));
        let hint = if mode == Mode::Manual { "Scroll slowly, then press Return" } else { "Scrolling… Esc stops" };
        session.status.setStringValue(&NSString::from_str(hint));
    });
    grab();
}

fn with_session<R>(f: impl FnOnce(&mut Session) -> R) -> Option<R> {
    SESSION.with_borrow_mut(|session| session.as_mut().map(f))
}

/// Снимает кадр области. Результат приходит на главный поток в `received`.
fn grab() {
    let Some((filter, config)) = with_session(|s| (s.filter.clone(), s.config.clone())) else { return };
    let handler = RcBlock::new(move |image: *mut CGImage, error: *mut NSError| {
        let frame = unsafe { image.as_ref() }.and_then(Frame::from_image);
        if frame.is_none() {
            capture::report(error);
        }
        DispatchQueue::main().exec_async(move || received(frame));
    });
    unsafe {
        SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(&filter, &config, Some(&handler));
    }
}

/// Следующий кадр. Склеивается, только когда картинка перестала меняться.
fn received(frame: Option<Frame>) {
    let Some(frame) = frame else { return close() };
    let Some(mode) = with_session(|s| s.mode).flatten() else { return };
    let frame = if mode == Mode::Up { frame.flipped() } else { frame };
    let next = with_session(|session| {
        let settled = session.previous.as_ref().is_some_and(|p| p.pixels == frame.pixels);
        let first = session.stitcher.is_none();
        session.tries += 1;
        let ready = first || settled || (mode != Mode::Manual && session.tries >= SETTLE_TRIES);
        if !ready {
            session.previous = Some(frame);
            return Next::Wait;
        }
        session.tries = 0;
        session.previous = Some(frame.clone());
        let Some(stitcher) = session.stitcher.as_mut() else {
            session.stitcher = Some(Stitcher::new(&frame));
            return session.stepped(mode);
        };
        let expected = (session.area.size.height * Settings::load().scroll_speed.step() * session.scale) as usize;
        let hint = (mode != Mode::Manual).then_some(expected);
        let pushed = stitcher.push(&frame, hint);
        let height = stitcher.height();
        session.status.setStringValue(&NSString::from_str(&format!("{height} px. Return finishes, Esc cancels")));
        match pushed {
            _ if height >= session.limit => Next::Finish,
            Push::Added(_) => session.stepped(mode),
            // Автоматическая прокрутка упёрлась в конец или потеряла страницу.
            Push::Same | Push::Lost if mode != Mode::Manual => Next::Finish,
            Push::Same | Push::Lost => Next::Wait,
        }
    });
    match (next, mode) {
        (Some(Next::Finish), _) => finish(),
        (Some(Next::Step), Mode::Down | Mode::Up) => {
            scroll(mode);
            later(SETTLE_SECONDS);
        }
        (Some(Next::Wait), Mode::Down | Mode::Up) => later(SETTLE_SECONDS),
        (Some(_), Mode::Manual) => later(MANUAL_SECONDS),
        (None, _) => {}
    }
}

enum Next {
    Step,
    Wait,
    Finish,
}

impl Session {
    /// После шага прокрутки первый же кадр может быть ещё старым, поэтому устоявшимся
    /// считается только второй одинаковый подряд.
    fn stepped(&mut self, mode: Mode) -> Next {
        if mode != Mode::Manual {
            self.previous = None;
        }
        Next::Step
    }
}

fn later(seconds: f64) {
    let block = RcBlock::new(|_timer: NonNull<NSTimer>| grab());
    let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(seconds, false, &block) };
    with_session(|session| session.timer = Some(timer));
}

/// Прокручивает область на шаг. Курсор ставится в её середину, туда macOS и отдаёт колесо.
fn scroll(mode: Mode) {
    let Some(area) = with_session(|s| s.area) else { return };
    let settings = Settings::load();
    let center = CGPoint::new(area.origin.x + area.size.width / 2.0, area.origin.y + area.size.height / 2.0);
    let _ = CGWarpMouseCursorPosition(center);
    // Отрицательное колесо показывает то, что ниже.
    let direction = if (mode == Mode::Down) != settings.scroll_reverse { -1.0 } else { 1.0 };
    let mut left = area.size.height * settings.scroll_speed.step();
    while left > 0.0 {
        let step = left.min(WHEEL_STEP);
        left -= step;
        if let Some(event) =
            CGEvent::new_scroll_wheel_event2(None, CGScrollEventUnit::Pixel, 1, (direction * step) as i32, 0, 0)
        {
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
        }
    }
}

/// Отдаёт склеенный снимок как обычный снимок области.
fn finish() {
    let Some(result) = with_session(|session| {
        let flipped = session.mode == Some(Mode::Up);
        let image = session.stitcher.as_ref().and_then(|s| s.image(flipped));
        (image, session.scale, session.area, session.app.clone())
    }) else {
        return;
    };
    close();
    let (Some(image), scale, area, app) = result else { return };
    let height = CGImage::height(Some(&image)) as f64 / scale;
    let shot =
        Shot { kind: Kind::Region, global_rect: CGRect::new(area.origin, CGSize::new(area.size.width, height)), app };
    output::in_background(move || output::deliver(&image, scale, &shot));
}

/// Закрывает окна сеанса без снимка.
fn close() {
    if let Some(session) = SESSION.take() {
        if let Some(timer) = session.timer {
            timer.invalidate();
        }
        session.hud.orderOut(None);
        session.border.orderOut(None);
    }
    CONTROLLER.take();
}

/// Есть ли у Kadro универсальный доступ. Без него macOS не пропускает события колеса.
fn trusted() -> bool {
    let key = CFString::from_static_str("AXTrustedCheckOptionPrompt");
    let options = CFDictionary::<CFString, CFBoolean>::from_slices(&[&*key], &[CFBoolean::new(false)]);
    // SAFETY: словарь живёт до конца вызова, функция только читает его.
    unsafe { AXIsProcessTrustedWithOptions(CFRetained::as_ptr(&options).as_ptr().cast()) }
}

/// Объясняет, зачем нужен универсальный доступ. Возвращает ручной режим, если выбран он.
fn explain_accessibility(mtm: MainThreadMarker) -> Option<Mode> {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(ns_string!("Kadro needs Accessibility access to scroll"));
    alert.setInformativeText(ns_string!(
        "Turn on Kadro in System Settings → Privacy & Security → Accessibility, then try again. You can also scroll yourself while Kadro captures."
    ));
    alert.addButtonWithTitle(ns_string!("Open System Settings"));
    alert.addButtonWithTitle(ns_string!("Scroll Myself"));
    alert.addButtonWithTitle(ns_string!("Cancel"));
    NSApplication::sharedApplication(mtm).activate();
    let response = alert.runModal();
    if response == NSAlertFirstButtonReturn {
        // Системный запрос добавляет Kadro в список, остаётся включить переключатель.
        let key = CFString::from_static_str("AXTrustedCheckOptionPrompt");
        let options = CFDictionary::<CFString, CFBoolean>::from_slices(&[&*key], &[CFBoolean::new(true)]);
        // SAFETY: словарь живёт до конца вызова, функция только читает его.
        unsafe { AXIsProcessTrustedWithOptions(CFRetained::as_ptr(&options).as_ptr().cast()) };
        if let Some(url) = NSURL::URLWithString(ns_string!(ACCESSIBILITY_SETTINGS_URL)) {
            NSWorkspace::sharedWorkspace().openURL(&url);
        }
        None
    } else if response == NSAlertSecondButtonReturn {
        Some(Mode::Manual)
    } else {
        None
    }
}
