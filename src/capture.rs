//! Capturing the screen, an area and windows through ScreenCaptureKit.
//!
//! The finished screenshot goes to [`output::deliver`], and Screen Recording permission is checked before every capture.

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

/// Screenshot kind.
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

    /// kMDItemScreenCaptureType value, same as for system screenshots.
    pub fn finder_type(self) -> &'static str {
        match self {
            Kind::Screen => "display",
            Kind::Region => "selection",
            Kind::Window => "window",
        }
    }
}

/// What was captured: kind, frame on screen in global points, and the application.
#[derive(Clone, Debug)]
pub struct Shot {
    pub kind: Kind,
    pub global_rect: CGRect,
    pub app: String,
}

/// Checks Screen Recording permission. Without it, explains where to turn it on.
pub fn has_access(mtm: MainThreadMarker) -> bool {
    if CGPreflightScreenCaptureAccess() {
        return true;
    }
    // macOS shows its system prompt once, then silently denies.
    // So the first attempt per launch shows that prompt, later attempts show our own explanation.
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

/// Name of the app the user is working in, for the file name template.
pub fn frontmost_app_name() -> String {
    NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .and_then(|app| app.localizedName())
        .map(|name| name.to_string())
        .unwrap_or_default()
}

/// Captures the whole display under the cursor.
pub fn capture_screen(mtm: MainThreadMarker) {
    if !has_access(mtm) {
        return;
    }
    let cursor = CGEvent::location(CGEvent::new(None).as_deref());
    // SCDisplay.frame and the CGEvent location share coordinates, from the top left of the main display.
    capture(move |display| geometry::contains(unsafe { display.frame() }, cursor), None, frontmost_app_name());
}

/// Captures the whole screen after a countdown in the menu bar.
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
        // The countdown is removed before capture so it does not end up in the menu bar on the screenshot.
        crate::show_countdown(None);
        COUNTING_DOWN.set(false);
        capture_screen(mtm);
    });
    unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(1.0, true, &tick) };
}

/// Captures a rectangle in points relative to the display top left corner and remembers it.
pub fn capture_region(display_id: CGDirectDisplayID, rect: CGRect, app: String) {
    settings::set_string(settings::LAST_AREA, &encode_area(display_id, rect));
    capture(move |display| unsafe { display.displayID() } == display_id, Some(rect), app);
}

/// Captures the last selected area if its display is still connected.
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

/// Captures the window of the app the user is working in, without picking it with the mouse.
pub fn capture_active_window(mtm: MainThreadMarker) {
    if !has_access(mtm) {
        return;
    }
    let pid = NSWorkspace::sharedWorkspace().frontmostApplication().map(|app| app.processIdentifier());
    // The window list goes front to back, the app's first window is the active one.
    match pid.and_then(|pid| overlay::on_screen_windows().into_iter().find(|w| w.pid == pid)) {
        Some(window) => capture_window(window.id),
        None => NSBeep(),
    }
}

/// Frame margin around the window in points, larger than the widest shadow. In a tight frame ScreenCaptureKit
/// scales the window down: always with a shadow, and without one for a full-screen window on an external display.
/// output::deliver crops the empty margin.
const WINDOW_MARGIN: f64 = 200.0;

/// Captures a window, with or without a shadow per settings.
pub fn capture_window(window_id: CGWindowID) {
    with_content(move |content| {
        let Some(window) = unsafe { content.windows() }.iter().find(|w| unsafe { w.windowID() } == window_id) else {
            return eprintln!("kadro: window to capture is gone");
        };
        let filter = unsafe { SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), &window) };
        let scale = f64::from(unsafe { filter.pointPixelScale() });
        let frame = unsafe { window.frame() };
        let config = unsafe { SCStreamConfiguration::new() };
        let padded = CGSize::new(frame.size.width + WINDOW_MARGIN, frame.size.height + WINDOW_MARGIN);
        set_output_size(&config, padded, scale);
        unsafe {
            // The shadow is needed if there will be a background or transparency behind the window.
            config.setIgnoreShadowsSingleWindow(Settings::load().window_background == WindowBackground::Clean);
            config.setShowsCursor(shows_cursor(Kind::Window));
        }
        let app = unsafe { window.owningApplication() }
            .map(|app| unsafe { app.applicationName() }.to_string())
            .unwrap_or_default();
        shoot(&filter, &config, scale, Shot { kind: Kind::Window, global_rect: frame, app });
    });
}

/// Captures whole displays for the magnifier during area selection. Images arrive on the main thread.
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
                // SAFETY: ScreenCaptureKit returns a valid image or null, and retain extends its lifetime.
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

/// Display filter without Kadro's own windows, so the overlay stays out of the screenshot even if it is not hidden yet.
pub fn display_filter(content: &SCShareableContent, display: &SCDisplay) -> Retained<SCContentFilter> {
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

/// Screenshot size in pixels: size in points multiplied by the display density.
pub fn set_output_size(config: &SCStreamConfiguration, size: CGSize, scale: f64) {
    unsafe {
        config.setWidth(pixels(size.width, scale));
        config.setHeight(pixels(size.height, scale));
    }
}

fn pixels(points: f64, scale: f64) -> usize {
    (points * scale).round() as usize
}

/// Kadro process identifier in the form AppKit and ScreenCaptureKit report it.
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

pub fn with_content(f: impl Fn(&SCShareableContent) + 'static) {
    let handler = RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
        match unsafe { content.as_ref() } {
            Some(content) => f(content),
            None => {
                report(error);
                // Permission may have been revoked while Kadro was running.
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

/// The area lies entirely on a display of this size. A disconnected display has zero size.
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

pub fn report(error: *mut NSError) {
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
