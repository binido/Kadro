use block2::RcBlock;
use objc2::AnyThread;
use objc2_core_foundation::{CGPoint, CGRect};
use objc2_core_graphics::{
    CGDirectDisplayID, CGEvent, CGImage, CGWindowID, CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess,
};
use objc2_foundation::{NSArray, NSError};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCScreenshotManager, SCShareableContent, SCStreamConfiguration,
};

use crate::output;

/// Проверяет доступ к записи экрана и при его отсутствии показывает системный запрос.
pub fn has_access() -> bool {
    if CGPreflightScreenCaptureAccess() {
        return true;
    }
    // Ответ на запрос вступает в силу только после перезапуска приложения.
    CGRequestScreenCaptureAccess();
    false
}

/// Снимает целиком дисплей, на котором стоит курсор.
pub fn capture_screen() {
    if !has_access() {
        return;
    }
    let cursor = CGEvent::location(CGEvent::new(None).as_deref());
    // SCDisplay.frame и позиция CGEvent в одних координатах - от левого верхнего угла главного дисплея.
    capture(move |display| contains(unsafe { display.frame() }, cursor), None);
}

/// Снимает прямоугольник в точках относительно левого верхнего угла дисплея.
pub fn capture_region(display_id: CGDirectDisplayID, rect: CGRect) {
    capture(move |display| unsafe { display.displayID() } == display_id, Some(rect));
}

/// Снимает окно без тени.
pub fn capture_window(window_id: CGWindowID) {
    with_content(move |content| {
        let Some(window) = unsafe { content.windows() }
            .iter()
            .find(|w| unsafe { w.windowID() } == window_id)
        else {
            return eprintln!("kadro: window to capture is gone");
        };
        let filter = unsafe {
            SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), &window)
        };
        let scale = unsafe { filter.pointPixelScale() } as f64;
        let size = unsafe { window.frame() }.size;
        let config = unsafe { SCStreamConfiguration::new() };
        unsafe {
            config.setWidth((size.width * scale).round() as usize);
            config.setHeight((size.height * scale).round() as usize);
            config.setIgnoreShadowsSingleWindow(true);
            config.setShowsCursor(false);
        }
        shoot(&filter, &config, scale);
    });
}

fn capture(pick: impl Fn(&SCDisplay) -> bool + 'static, region: Option<CGRect>) {
    with_content(move |content| {
        let Some(display) = unsafe { content.displays() }.iter().find(|d| pick(d)) else {
            return eprintln!("kadro: display to capture is gone");
        };
        // Собственные окна Kadro (оверлей выделения) в снимок не попадают, даже если ещё не успели скрыться.
        let own_pid = std::process::id() as i32;
        let own: Vec<_> = unsafe { content.applications() }
            .iter()
            .filter(|app| unsafe { app.processID() } == own_pid)
            .collect();

        let filter = unsafe {
            SCContentFilter::initWithDisplay_excludingApplications_exceptingWindows(
                SCContentFilter::alloc(),
                &display,
                &NSArray::from_retained_slice(&own),
                &NSArray::new(),
            )
        };
        let scale = unsafe { filter.pointPixelScale() } as f64;
        let size = region.map_or_else(|| unsafe { filter.contentRect() }.size, |r| r.size);
        let config = unsafe { SCStreamConfiguration::new() };
        unsafe {
            if let Some(region) = region {
                config.setSourceRect(region);
            }
            config.setWidth((size.width * scale).round() as usize);
            config.setHeight((size.height * scale).round() as usize);
            config.setShowsCursor(false);
        }
        shoot(&filter, &config, scale);
    });
}

fn with_content(f: impl Fn(&SCShareableContent) + 'static) {
    let handler = RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
        match unsafe { content.as_ref() } {
            Some(content) => f(content),
            None => report(error),
        }
    });
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&handler) };
}

fn shoot(filter: &SCContentFilter, config: &SCStreamConfiguration, scale: f64) {
    let handler = RcBlock::new(move |image: *mut CGImage, error: *mut NSError| {
        match unsafe { image.as_ref() } {
            Some(image) => output::deliver(image, scale),
            None => report(error),
        }
    });
    unsafe {
        SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(
            filter,
            config,
            Some(&handler),
        )
    };
}

pub fn contains(rect: CGRect, point: CGPoint) -> bool {
    point.x >= rect.origin.x
        && point.x < rect.origin.x + rect.size.width
        && point.y >= rect.origin.y
        && point.y < rect.origin.y + rect.size.height
}

fn report(error: *mut NSError) {
    match unsafe { error.as_ref() } {
        Some(error) => eprintln!("kadro: capture failed: {}", error.localizedDescription()),
        None => eprintln!("kadro: capture failed"),
    }
}
