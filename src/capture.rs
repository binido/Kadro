//! Захват экрана, области и окон через ScreenCaptureKit.
//!
//! Готовый снимок уходит в [`output::deliver`], а доступ к записи экрана проверяется перед каждым захватом.

use std::cell::Cell;
use std::ptr::NonNull;

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::{AnyThread, MainThreadMarker};
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSApplication, NSBeep, NSWorkspace};
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGDirectDisplayID, CGDisplayBounds, CGEvent, CGImage, CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess,
    CGWindowID,
};
use objc2_foundation::{NSArray, NSError, NSTimer, NSURL, ns_string};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCScreenshotManager, SCShareableContent, SCStreamConfiguration,
};

use crate::settings::{self, Cursor, Settings, WindowBackground};
use crate::{geometry, output, overlay};

const SCREEN_RECORDING_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture";
const DELAY_SECONDS: u32 = 3;

thread_local! {
    static ACCESS_REQUESTED: Cell<bool> = const { Cell::new(false) };
    static COUNTING_DOWN: Cell<bool> = const { Cell::new(false) };
}

/// Вид снимка.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Screen,
    Region,
    Window,
}

impl Kind {
    pub fn title(self) -> &'static str {
        match self {
            Kind::Screen => "Screen",
            Kind::Region => "Region",
            Kind::Window => "Window",
        }
    }

    /// Значение kMDItemScreenCaptureType, как у системных снимков.
    pub fn finder_type(self) -> &'static str {
        match self {
            Kind::Screen => "display",
            Kind::Region => "selection",
            Kind::Window => "window",
        }
    }
}

/// Что снято: вид, место на экране в глобальных точках и приложение.
#[derive(Clone, Debug)]
pub struct Shot {
    pub kind: Kind,
    pub global_rect: CGRect,
    pub app: String,
}

/// Проверяет доступ к записи экрана. Без доступа объясняет, где его включить.
pub fn has_access(mtm: MainThreadMarker) -> bool {
    if CGPreflightScreenCaptureAccess() {
        return true;
    }
    // Системный запрос macOS показывает один раз, потом молча отказывает.
    // Поэтому при первой попытке за запуск - он, а дальше своё объяснение.
    if ACCESS_REQUESTED.replace(true) {
        explain_missing_access(mtm);
    } else {
        CGRequestScreenCaptureAccess();
    }
    false
}

fn explain_missing_access(mtm: MainThreadMarker) {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(ns_string!("Kadro needs Screen Recording access"));
    alert.setInformativeText(ns_string!(
        "Turn on Kadro in System Settings → Privacy & Security → Screen & System Audio Recording, then quit and reopen Kadro."
    ));
    alert.addButtonWithTitle(ns_string!("Open System Settings"));
    alert.addButtonWithTitle(ns_string!("Cancel"));
    NSApplication::sharedApplication(mtm).activate();
    if alert.runModal() == NSAlertFirstButtonReturn
        && let Some(url) = NSURL::URLWithString(ns_string!(SCREEN_RECORDING_SETTINGS_URL))
    {
        NSWorkspace::sharedWorkspace().openURL(&url);
    }
}

/// Имя приложения, с которым работает пользователь, для шаблона имени файла.
pub fn frontmost_app_name() -> String {
    NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .and_then(|app| app.localizedName())
        .map(|name| name.to_string())
        .unwrap_or_default()
}

/// Снимает целиком дисплей, на котором стоит курсор.
pub fn capture_screen(mtm: MainThreadMarker) {
    if !has_access(mtm) {
        return;
    }
    let cursor = CGEvent::location(CGEvent::new(None).as_deref());
    // SCDisplay.frame и позиция CGEvent в одних координатах - от левого верхнего угла главного дисплея.
    capture(move |display| geometry::contains(unsafe { display.frame() }, cursor), None, frontmost_app_name());
}

/// Снимает весь экран после обратного отсчёта в строке меню.
pub fn capture_delayed(mtm: MainThreadMarker) {
    if !has_access(mtm) || COUNTING_DOWN.replace(true) {
        return;
    }
    let remaining = Cell::new(DELAY_SECONDS);
    crate::show_countdown(Some(DELAY_SECONDS));
    let tick = RcBlock::new(move |timer: NonNull<NSTimer>| {
        remaining.set(remaining.get() - 1);
        if remaining.get() > 0 {
            return crate::show_countdown(Some(remaining.get()));
        }
        unsafe { timer.as_ref() }.invalidate();
        // Отсчёт убирается до снимка, чтобы не попасть в строку меню на нём.
        crate::show_countdown(None);
        COUNTING_DOWN.set(false);
        capture_screen(mtm);
    });
    unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(1.0, true, &tick) };
}

/// Снимает прямоугольник в точках относительно левого верхнего угла дисплея и запоминает его.
pub fn capture_region(display_id: CGDirectDisplayID, rect: CGRect, app: String) {
    settings::set_string(settings::LAST_AREA, &encode_area(display_id, rect));
    capture(move |display| unsafe { display.displayID() } == display_id, Some(rect), app);
}

/// Снимает последнюю выделенную область, если её дисплей на месте.
pub fn repeat_area(mtm: MainThreadMarker) {
    if !has_access(mtm) {
        return;
    }
    let area = settings::string(settings::LAST_AREA).and_then(|text| decode_area(&text));
    match area.filter(|(display, rect)| fits(*rect, CGDisplayBounds(*display).size)) {
        Some((display, rect)) => capture_region(display, rect, frontmost_app_name()),
        None => {
            let alert = NSAlert::new(mtm);
            alert.setMessageText(ns_string!("No region to repeat yet"));
            alert.setInformativeText(ns_string!(
                "Repeat Last Region captures the area you selected last time. Capture a region first."
            ));
            NSApplication::sharedApplication(mtm).activate();
            alert.runModal();
        }
    }
}

/// Снимает окно приложения, с которым сейчас работает пользователь, без выбора мышью.
pub fn capture_active_window(mtm: MainThreadMarker) {
    if !has_access(mtm) {
        return;
    }
    let pid = NSWorkspace::sharedWorkspace().frontmostApplication().map(|app| app.processIdentifier());
    // Список окон идёт от переднего к заднему, первое окно приложения - активное.
    match pid.and_then(|pid| overlay::on_screen_windows().into_iter().find(|w| w.pid == pid)) {
        Some(window) => capture_window(window.id),
        None => NSBeep(),
    }
}

/// Снимает окно без тени.
pub fn capture_window(window_id: CGWindowID) {
    with_content(move |content| {
        let Some(window) = unsafe { content.windows() }.iter().find(|w| unsafe { w.windowID() } == window_id) else {
            return eprintln!("kadro: window to capture is gone");
        };
        let filter = unsafe { SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), &window) };
        let scale = f64::from(unsafe { filter.pointPixelScale() });
        let frame = unsafe { window.frame() };
        let config = unsafe { SCStreamConfiguration::new() };
        set_output_size(&config, frame.size, scale);
        unsafe {
            // Тень нужна, если за окном будет фон или прозрачность.
            config.setIgnoreShadowsSingleWindow(Settings::load().window_background == WindowBackground::Clean);
            config.setShowsCursor(shows_cursor(Kind::Window));
        }
        let app = unsafe { window.owningApplication() }
            .map(|app| unsafe { app.applicationName() }.to_string())
            .unwrap_or_default();
        shoot(&filter, &config, scale, Shot { kind: Kind::Window, global_rect: frame, app });
    });
}

/// Снимает дисплеи целиком для лупы при выделении области. Снимки приходят на главный поток.
pub fn freeze_displays() {
    with_content(|content| {
        for display in &unsafe { content.displays() } {
            let filter = display_filter(content, &display);
            let scale = f64::from(unsafe { filter.pointPixelScale() });
            let size = unsafe { filter.contentRect() }.size;
            let config = unsafe { SCStreamConfiguration::new() };
            set_output_size(&config, size, scale);
            unsafe { config.setShowsCursor(false) };
            let display_id = unsafe { display.displayID() };
            let handler = RcBlock::new(move |image: *mut CGImage, _error: *mut NSError| {
                // SAFETY: ScreenCaptureKit отдаёт действительный снимок или null, а retain продлевает ему жизнь.
                if let Some(image) = NonNull::new(image).map(|p| unsafe { CFRetained::retain(p) }) {
                    DispatchQueue::main().exec_async(move || overlay::show_frozen(display_id, image));
                }
            });
            unsafe {
                SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(
                    &filter,
                    &config,
                    Some(&handler),
                );
            };
        }
    });
}

fn capture(pick: impl Fn(&SCDisplay) -> bool + 'static, region: Option<CGRect>, app: String) {
    with_content(move |content| {
        let Some(display) = unsafe { content.displays() }.iter().find(|d| pick(d)) else {
            return eprintln!("kadro: display to capture is gone");
        };
        let filter = display_filter(content, &display);
        let scale = f64::from(unsafe { filter.pointPixelScale() });
        let display_frame = unsafe { display.frame() };
        let size = region.map_or_else(|| unsafe { filter.contentRect() }.size, |r| r.size);
        let kind = if region.is_some() { Kind::Region } else { Kind::Screen };
        let config = unsafe { SCStreamConfiguration::new() };
        unsafe {
            if let Some(region) = region {
                config.setSourceRect(region);
            }
            config.setShowsCursor(shows_cursor(kind));
        }
        set_output_size(&config, size, scale);
        let global_rect = match region {
            Some(r) => CGRect::new(
                CGPoint::new(display_frame.origin.x + r.origin.x, display_frame.origin.y + r.origin.y),
                r.size,
            ),
            None => display_frame,
        };
        shoot(&filter, &config, scale, Shot { kind, global_rect, app: app.clone() });
    });
}

/// Фильтр дисплея без окон самого Kadro: оверлей не попадает в снимок, даже если ещё не скрылся.
fn display_filter(content: &SCShareableContent, display: &SCDisplay) -> Retained<SCContentFilter> {
    let own_pid = own_pid();
    let own: Vec<_> =
        unsafe { content.applications() }.iter().filter(|app| unsafe { app.processID() } == own_pid).collect();
    unsafe {
        SCContentFilter::initWithDisplay_excludingApplications_exceptingWindows(
            SCContentFilter::alloc(),
            display,
            &NSArray::from_retained_slice(&own),
            &NSArray::new(),
        )
    }
}

/// Размер снимка в пикселях: размер в точках, умноженный на плотность дисплея.
fn set_output_size(config: &SCStreamConfiguration, size: CGSize, scale: f64) {
    unsafe {
        config.setWidth(pixels(size.width, scale));
        config.setHeight(pixels(size.height, scale));
    }
}

fn pixels(points: f64, scale: f64) -> usize {
    (points * scale).round() as usize
}

/// Идентификатор процесса Kadro в том виде, в каком его отдают AppKit и ScreenCaptureKit.
pub fn own_pid() -> i32 {
    std::process::id().cast_signed()
}

fn shows_cursor(kind: Kind) -> bool {
    match Settings::load().cursor {
        Cursor::Show => true,
        Cursor::Hide => false,
        Cursor::Auto => kind == Kind::Screen,
    }
}

fn with_content(f: impl Fn(&SCShareableContent) + 'static) {
    let handler = RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
        match unsafe { content.as_ref() } {
            Some(content) => f(content),
            None => {
                report(error);
                // Доступ могли отозвать, пока Kadro работал.
                if !CGPreflightScreenCaptureAccess() {
                    DispatchQueue::main().exec_async(|| {
                        if let Some(mtm) = MainThreadMarker::new() {
                            explain_missing_access(mtm);
                        }
                    });
                }
            }
        }
    });
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&handler) };
}

fn shoot(filter: &SCContentFilter, config: &SCStreamConfiguration, scale: f64, shot: Shot) {
    let handler = RcBlock::new(move |image: *mut CGImage, error: *mut NSError| match unsafe { image.as_ref() } {
        Some(image) => output::deliver(image, scale, &shot),
        None => report(error),
    });
    unsafe {
        SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(filter, config, Some(&handler));
    };
}

/// Область целиком лежит на дисплее этого размера. Отключённый дисплей имеет нулевой размер.
fn fits(rect: CGRect, display: CGSize) -> bool {
    rect.origin.x >= 0.0
        && rect.origin.y >= 0.0
        && rect.origin.x + rect.size.width <= display.width
        && rect.origin.y + rect.size.height <= display.height
}

fn encode_area(display: CGDirectDisplayID, rect: CGRect) -> String {
    format!("{display} {} {} {} {}", rect.origin.x, rect.origin.y, rect.size.width, rect.size.height)
}

fn decode_area(text: &str) -> Option<(CGDirectDisplayID, CGRect)> {
    let mut parts = text.split_whitespace();
    let display = parts.next()?.parse().ok()?;
    let mut number = || parts.next()?.parse::<f64>().ok();
    let rect = CGRect::new(CGPoint::new(number()?, number()?), CGSize::new(number()?, number()?));
    Some((display, rect))
}

fn report(error: *mut NSError) {
    match unsafe { error.as_ref() } {
        Some(error) => eprintln!("kadro: capture failed: {}", error.localizedDescription()),
        None => eprintln!("kadro: capture failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_area_round_trips_and_must_fit_its_display() {
        let rect = CGRect::new(CGPoint::new(10.5, 20.0), CGSize::new(300.0, 200.25));
        assert_eq!(decode_area(&encode_area(7, rect)), Some((7, rect)));
        assert_eq!(decode_area("7 1 2 3"), None);
        assert!(fits(rect, CGSize::new(1512.0, 982.0)));
        assert!(!fits(rect, CGSize::new(200.0, 982.0)));
        assert!(!fits(rect, CGSize::new(0.0, 0.0)));
    }
}
