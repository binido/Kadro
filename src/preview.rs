//! Превью снимка в углу экрана и подтверждения, когда превью выключено.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::ptr::NonNull;

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ProtocolObject, Sel};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSBezelStyle, NSBezierPath, NSBox, NSBoxType, NSButton, NSColor, NSDragOperation,
    NSDraggingContext, NSDraggingItem, NSDraggingSession, NSDraggingSource, NSEvent, NSEventMask,
    NSFloatingWindowLevel, NSGraphicsContext, NSImage, NSImageView, NSPanel, NSResponder, NSScreen, NSStackView,
    NSTextField, NSTrackingArea, NSTrackingAreaOptions, NSView, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{CGContext, CGImage};
use objc2_foundation::{
    NSArray, NSBundle, NSError, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSTimer, NSURL,
};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest, UNUserNotificationCenter,
};

use crate::capture::Shot;
use crate::output::{self, in_background};
use crate::settings::{Confirmation, PreviewHiding, Settings};
use crate::{assets, editor, geometry, pin, recognize, upload};

/// Наибольший размер миниатюры в точках.
const MAX_SIZE: NSSize = NSSize::new(220.0, 160.0);
const MARGIN: f64 = 16.0;
const BUTTON_SIZE: f64 = 28.0;
const AUTO_HIDE_SECONDS: f64 = 6.0;
const POPUP_SECONDS: f64 = 1.5;
/// На сколько точек нужно сдвинуть мышь, чтобы клик стал перетаскиванием.
const DRAG_THRESHOLD: f64 = 4.0;

thread_local! {
    static CURRENT: RefCell<Option<Preview>> = const { RefCell::new(None) };
    static POPUP: RefCell<Option<Retained<NSPanel>>> = const { RefCell::new(None) };
    // Закрытые окна живут до конца текущего события, освобождать их посреди обработчика нельзя.
    static RETIRED: RefCell<Vec<Retained<NSPanel>>> = const { RefCell::new(Vec::new()) };
}

struct Preview {
    panel: Retained<NSPanel>,
    view: Retained<ThumbnailView>,
    monitor: Option<Retained<AnyObject>>,
}

/// Почему превью закрывается.
#[derive(Clone, Copy, PartialEq)]
enum Dismissal {
    /// Пользователь закрыл сам или уже распорядился снимком.
    Done,
    /// Превью ушло само: по таймеру, клику мимо или из-за нового снимка.
    Expired,
}

struct ThumbnailIvars {
    image: CFRetained<CGImage>,
    scale: f64,
    shot: Shot,
    /// Снимок уже скопирован, сохранён или перетащен.
    handled: Cell<bool>,
    auto_hide: bool,
    press: Cell<Option<NSPoint>>,
    buttons: RefCell<Vec<Retained<NSButton>>>,
    timer: RefCell<Option<Retained<NSTimer>>>,
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ThumbnailIvars]
    struct ThumbnailView;

    impl ThumbnailView {
        // Панель не активирует Kadro, поэтому первый же клик должен дойти до вида.
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let bounds = self.bounds();
            let shape = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(bounds, 10.0, 10.0);
            NSGraphicsContext::saveGraphicsState_class();
            shape.addClip();
            if let Some(context) = NSGraphicsContext::currentContext().map(|c| c.CGContext()) {
                CGContext::draw_image(Some(&context), bounds, Some(&self.ivars().image));
            }
            NSGraphicsContext::restoreGraphicsState_class();
            NSColor::colorWithWhite_alpha(1.0, 0.35).setStroke();
            shape.setLineWidth(1.0);
            shape.stroke();
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _event: &NSEvent) {
            self.set_buttons_hidden(false);
            self.cancel_timer();
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            self.set_buttons_hidden(true);
            self.schedule_hide();
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.ivars().press.set(Some(event.locationInWindow()));
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let Some(press) = self.ivars().press.get() else { return };
            let point = event.locationInWindow();
            if (point.x - press.x).hypot(point.y - press.y) >= DRAG_THRESHOLD {
                self.ivars().press.set(None);
                self.start_drag(event);
            }
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            self.ivars().press.set(None);
        }

        #[unsafe(method(copyImage:))]
        fn copy_image(&self, _sender: Option<&AnyObject>) {
            output::copy_in_background(self.ivars().image.clone(), self.ivars().scale);
            self.ivars().handled.set(true);
            dismiss(Dismissal::Done);
        }

        #[unsafe(method(saveImage:))]
        fn save_image(&self, _sender: Option<&AnyObject>) {
            let (image, scale, shot) = (self.ivars().image.clone(), self.ivars().scale, self.ivars().shot.clone());
            in_background(move || {
                if let Err(err) = output::save(&Settings::load().directory, &image, scale, Some(&shot), None) {
                    eprintln!("kadro: failed to save screenshot: {err}");
                }
            });
            self.ivars().handled.set(true);
            dismiss(Dismissal::Done);
        }

        #[unsafe(method(editImage:))]
        fn edit_image(&self, _sender: Option<&AnyObject>) {
            let ivars = self.ivars();
            editor::open(self.mtm(), ivars.image.clone(), ivars.scale, Some(ivars.shot.clone()), "Screenshot");
            // Снимком дальше распоряжается редактор.
            ivars.handled.set(true);
            dismiss(Dismissal::Done);
        }

        // Распознанный текст уходит в буфер обмена, а сам снимок остаётся в превью.
        #[unsafe(method(recognizeText:))]
        fn recognize_text(&self, _sender: Option<&AnyObject>) {
            recognize::recognize_and_show(self.ivars().image.clone());
        }

        // Ссылка уходит в буфер обмена, когда загрузка закончится, а снимок считается отправленным.
        #[unsafe(method(uploadImage:))]
        fn upload_image(&self, _sender: Option<&AnyObject>) {
            let ivars = self.ivars();
            upload::upload(self.mtm(), ivars.image.clone(), ivars.scale, Some(ivars.shot.clone()));
            ivars.handled.set(true);
            dismiss(Dismissal::Done);
        }

        #[unsafe(method(pinImage:))]
        fn pin_image(&self, _sender: Option<&AnyObject>) {
            let ivars = self.ivars();
            pin::open(self.mtm(), ivars.image.clone(), ivars.scale, Some(ivars.shot.global_rect));
            ivars.handled.set(true);
            dismiss(Dismissal::Done);
        }

        #[unsafe(method(closePreview:))]
        fn close_preview(&self, _sender: Option<&AnyObject>) {
            dismiss(Dismissal::Done);
        }
    }

    unsafe impl NSObjectProtocol for ThumbnailView {}

    unsafe impl NSDraggingSource for ThumbnailView {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn source_operation_mask(&self, _session: &NSDraggingSession, _context: NSDraggingContext) -> NSDragOperation {
            NSDragOperation::Copy
        }

        // Снимок, который куда-то бросили, считается использованным, и превью больше не нужно.
        #[unsafe(method(draggingSession:endedAtPoint:operation:))]
        fn drag_ended(&self, _session: &NSDraggingSession, _point: NSPoint, operation: NSDragOperation) {
            if operation != NSDragOperation::None {
                self.ivars().handled.set(true);
                dismiss(Dismissal::Done);
            }
        }
    }
);

impl ThumbnailView {
    fn new(mtm: MainThreadMarker, size: NSSize, ivars: ThumbnailIvars) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ivars);
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: NSRect::new(NSPoint::ZERO, size)] };

        let options = NSTrackingAreaOptions::MouseEnteredAndExited
            | NSTrackingAreaOptions::ActiveAlways
            | NSTrackingAreaOptions::InVisibleRect;
        let area = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(
                NSTrackingArea::alloc(),
                NSRect::ZERO,
                options,
                Some(&this),
                None,
            )
        };
        this.addTrackingArea(&area);

        let inset = 6.0;
        // Нижний ряд по центру: Edit, Copy, Save, Upload, Text, Pin с зазором в 8 точек.
        let step = BUTTON_SIZE + 8.0;
        let row_left = size.width / 2.0 - (6.0 * BUTTON_SIZE + 40.0) / 2.0;
        let buttons = [
            (assets::X, sel!(closePreview:), NSPoint::new(inset, size.height - BUTTON_SIZE - inset)),
            (assets::SQUARE_PEN, sel!(editImage:), NSPoint::new(row_left, inset)),
            (assets::COPY, sel!(copyImage:), NSPoint::new(row_left + step, inset)),
            (assets::SAVE, sel!(saveImage:), NSPoint::new(row_left + 2.0 * step, inset)),
            (assets::CLOUD_UPLOAD, sel!(uploadImage:), NSPoint::new(row_left + 3.0 * step, inset)),
            (assets::SCAN_TEXT, sel!(recognizeText:), NSPoint::new(row_left + 4.0 * step, inset)),
            (assets::PIN, sel!(pinImage:), NSPoint::new(row_left + 5.0 * step, inset)),
        ]
        .map(|(icon, action, origin)| this.add_button(mtm, icon, action, origin));
        *this.ivars().buttons.borrow_mut() = buttons.to_vec();
        this.set_buttons_hidden(true);
        this
    }

    fn add_button(&self, mtm: MainThreadMarker, icon: &[u8], action: Sel, origin: NSPoint) -> Retained<NSButton> {
        let image = assets::icon(icon, 15.0).unwrap_or_default();
        let button = unsafe { NSButton::buttonWithImage_target_action(&image, Some(self), Some(action), mtm) };
        button.setBezelStyle(NSBezelStyle::Circular);
        button.setFrame(NSRect::new(origin, NSSize::new(BUTTON_SIZE, BUTTON_SIZE)));
        self.addSubview(&button);
        button
    }

    fn set_buttons_hidden(&self, hidden: bool) {
        for button in self.ivars().buttons.borrow().iter() {
            button.setHidden(hidden);
        }
    }

    /// Через несколько секунд без наведения прячет превью, если включено автоскрытие.
    fn schedule_hide(&self) {
        if !self.ivars().auto_hide {
            return;
        }
        self.cancel_timer();
        let fire = RcBlock::new(|_timer: NonNull<NSTimer>| dismiss(Dismissal::Expired));
        let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(AUTO_HIDE_SECONDS, false, &fire) };
        *self.ivars().timer.borrow_mut() = Some(timer);
    }

    fn cancel_timer(&self) {
        if let Some(timer) = self.ivars().timer.borrow_mut().take() {
            timer.invalidate();
        }
    }

    /// Перетаскивает снимок как файл. Файл пишется во временную папку в момент начала перетаскивания.
    fn start_drag(&self, event: &NSEvent) {
        let ivars = self.ivars();
        let directory = std::env::temp_dir().join("Kadro");
        // ponytail: кодирование идёт на главном потоке, на снимке 5K это заметная пауза в начале перетаскивания.
        // Если мешает, писать файл заранее в фоне сразу после показа превью.
        let path = match output::save(&directory, &ivars.image, ivars.scale, Some(&ivars.shot), None) {
            Ok(path) => path,
            Err(err) => return eprintln!("kadro: failed to prepare dragged file: {err}"),
        };
        let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        let item = NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), ProtocolObject::from_ref(&*url));
        let thumbnail = NSImage::initWithCGImage_size(NSImage::alloc(), &ivars.image, self.bounds().size);
        unsafe { item.setDraggingFrame_contents(self.bounds(), Some(&thumbnail)) };
        self.beginDraggingSessionWithItems_event_source(
            &NSArray::from_retained_slice(&[item]),
            event,
            ProtocolObject::from_ref(self),
        );
    }
}

/// Показывает превью снимка в углу экрана, где он сделан. Прежнее превью уходит.
pub fn show(mtm: MainThreadMarker, image: CFRetained<CGImage>, scale: f64, shot: Shot, handled: bool) {
    dismiss(Dismissal::Expired);
    let auto_hide = Settings::load().preview_hiding == PreviewHiding::Auto;
    let size = fit(
        NSSize::new(CGImage::width(Some(&image)) as f64 / scale, CGImage::height(Some(&image)) as f64 / scale),
        MAX_SIZE,
    );
    let Some(screen) = screen_for(mtm, &shot) else {
        eprintln!("kadro: no display to show the preview on");
        // Снимок не пропадает молча, как и при закрытии превью.
        if !handled {
            output::copy_in_background(image, scale);
        }
        return;
    };
    let visible = screen.visibleFrame();
    let frame = NSRect::new(
        NSPoint::new(visible.origin.x + visible.size.width - size.width - MARGIN, visible.origin.y + MARGIN),
        size,
    );

    let panel = floating_panel(mtm, frame);
    let ivars = ThumbnailIvars {
        image,
        scale,
        shot,
        handled: Cell::new(handled),
        auto_hide,
        press: Cell::new(None),
        buttons: RefCell::new(Vec::new()),
        timer: RefCell::new(None),
    };
    let view = ThumbnailView::new(mtm, size, ivars);
    panel.setContentView(Some(&view));
    panel.orderFrontRegardless();

    // Клик в другом приложении прячет превью. Клики по самой панели сюда не попадают.
    let monitor = auto_hide
        .then(|| {
            let block = RcBlock::new(|_event: NonNull<NSEvent>| dismiss(Dismissal::Expired));
            NSEvent::addGlobalMonitorForEventsMatchingMask_handler(
                NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown,
                &block,
            )
        })
        .flatten();
    view.schedule_hide();
    CURRENT.set(Some(Preview { panel, view, monitor }));
}

fn dismiss(reason: Dismissal) {
    let Some(preview) = CURRENT.take() else { return };
    if let Some(monitor) = &preview.monitor {
        unsafe { NSEvent::removeMonitor(monitor) };
    }
    preview.view.cancel_timer();
    preview.panel.orderOut(None);
    let ivars = preview.view.ivars();
    // Снимок не пропадает молча: если с ним ничего не сделали, он остаётся в буфере обмена.
    if reason == Dismissal::Expired && !ivars.handled.get() {
        output::copy_in_background(ivars.image.clone(), ivars.scale);
    }
    retire(preview.panel);
}

/// Подтверждает копирование и сохранение, когда превью выключено.
pub fn confirm(mtm: MainThreadMarker, copied: bool, saved: Option<&Path>, shot: &Shot) {
    let text = match (copied, saved.is_some()) {
        (true, true) => "Copied and saved",
        (true, false) => "Copied to clipboard",
        (false, true) => "Saved",
        (false, false) => return,
    };
    match Settings::load().confirmation {
        Confirmation::None => {}
        // Без бандла у процесса нет идентификатора, и центр уведомлений падает при обращении.
        Confirmation::Notification if NSBundle::mainBundle().bundleIdentifier().is_some() => notify(text, saved),
        Confirmation::Notification | Confirmation::Popup => popup(mtm, text, shot),
    }
}

/// Короткая плашка внизу экрана со снимком.
fn popup(mtm: MainThreadMarker, text: &str, shot: &Shot) {
    if let Some(screen) = screen_for(mtm, shot) {
        popup_on(mtm, text, &screen);
    }
}

/// Короткая плашка внизу экрана.
pub fn popup_on(mtm: MainThreadMarker, text: &str, screen: &NSScreen) {
    hide_popup();

    let icon = NSImageView::imageViewWithImage(&assets::icon(assets::CHECK, 16.0).unwrap_or_default(), mtm);
    icon.setContentTintColor(Some(&NSColor::whiteColor()));
    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    label.setFont(Some(&assets::font(13.0)));
    label.setTextColor(Some(&NSColor::whiteColor()));
    let row = NSStackView::stackViewWithViews(
        &NSArray::from_retained_slice(&[icon.into_super().into_super(), label.into_super().into_super()]),
        mtm,
    );
    row.setSpacing(8.0);
    let size = row.fittingSize();
    let padding = NSSize::new(16.0, 10.0);
    let panel_size = NSSize::new(size.width + 2.0 * padding.width, size.height + 2.0 * padding.height);

    let visible = screen.visibleFrame();
    let frame = NSRect::new(
        NSPoint::new(visible.origin.x + (visible.size.width - panel_size.width) / 2.0, visible.origin.y + 72.0),
        panel_size,
    );
    let panel = floating_panel(mtm, frame);
    panel.setIgnoresMouseEvents(true);

    let background = NSBox::new(mtm);
    background.setBoxType(NSBoxType::Custom);
    background.setBorderWidth(0.0);
    background.setCornerRadius(panel_size.height / 2.0);
    background.setFillColor(&NSColor::colorWithWhite_alpha(0.1, 0.85));
    background.setFrame(NSRect::new(NSPoint::ZERO, panel_size));
    row.setFrame(NSRect::new(NSPoint::new(padding.width, padding.height), size));
    background.addSubview(&row);
    panel.setContentView(Some(&background));
    panel.orderFrontRegardless();

    let fire = RcBlock::new(|_timer: NonNull<NSTimer>| hide_popup());
    unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(POPUP_SECONDS, false, &fire) };
    POPUP.set(Some(panel));
}

fn hide_popup() {
    if let Some(panel) = POPUP.take() {
        panel.orderOut(None);
        retire(panel);
    }
}

/// Уведомление macOS. Разрешение запрашивается при первом уведомлении, отказ просто глушит их.
fn notify(text: &str, saved: Option<&Path>) {
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(text));
    if let Some(name) = saved.and_then(|path| path.file_name()) {
        content.setBody(&NSString::from_str(&name.to_string_lossy()));
    }
    let identifier = NSString::from_str(&format!("kadro-{}", std::process::id()));
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&identifier, &content, None);
    let granted = RcBlock::new(move |granted: Bool, _error: *mut NSError| {
        if granted.as_bool() {
            UNUserNotificationCenter::currentNotificationCenter()
                .addNotificationRequest_withCompletionHandler(&request, None);
        }
    });
    center.requestAuthorizationWithOptions_completionHandler(UNAuthorizationOptions::Alert, &granted);
}

/// Неактивирующая плавающая панель без рамки: не забирает фокус у приложения, которое снимали.
fn floating_panel(mtm: MainThreadMarker, frame: NSRect) -> Retained<NSPanel> {
    let panel: Retained<NSPanel> = unsafe {
        msg_send![
            NSPanel::alloc(mtm),
            initWithContentRect: frame,
            styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            backing: NSBackingStoreType::Buffered,
            defer: false,
        ]
    };
    configure_floating(&panel);
    panel
}

/// Плавает над обычными окнами на всех рабочих столах и поверх полноэкранных приложений.
pub fn configure_floating(panel: &NSPanel) {
    // Иначе orderOut и close освободят панель второй раз, поверх Retained.
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setFloatingPanel(true);
    panel.setLevel(NSFloatingWindowLevel);
    panel.setHidesOnDeactivate(false);
    panel.setBecomesKeyOnlyIfNeeded(true);
    panel.setOpaque(false);
    panel.setHasShadow(true);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
}

/// Экран, где сделан снимок, по центру его области. Если такого уже нет - главный.
/// `None`, только если к Mac не подключён ни один дисплей.
pub fn screen_for(mtm: MainThreadMarker, shot: &Shot) -> Option<Retained<NSScreen>> {
    let center = geometry::center(to_cocoa(mtm, shot.global_rect));
    let screens = NSScreen::screens(mtm);
    screens
        .iter()
        .find(|s| geometry::contains(s.frame(), center))
        .or_else(|| screens.firstObject())
        .or_else(|| NSScreen::mainScreen(mtm))
}

/// Переводит прямоугольник из глобальных координат CG в координаты окон Cocoa.
/// У CG ось y идёт вниз от верха главного экрана, у Cocoa - вверх от его низа.
pub fn to_cocoa(mtm: MainThreadMarker, global: NSRect) -> NSRect {
    let primary_height = NSScreen::screens(mtm).firstObject().map_or(0.0, |s| s.frame().size.height);
    NSRect::new(NSPoint::new(global.origin.x, primary_height - global.origin.y - global.size.height), global.size)
}

/// Вписывает размер снимка в предел, сохраняя пропорции. Маленькие снимки не растягиваются.
fn fit(size: NSSize, limit: NSSize) -> NSSize {
    let ratio = (limit.width / size.width).min(limit.height / size.height).min(1.0);
    NSSize::new((size.width * ratio).round().max(1.0), (size.height * ratio).round().max(1.0))
}

/// Держит закрытую панель до конца текущего события и освобождает её следующим шагом главной очереди.
pub fn retire(panel: Retained<NSPanel>) {
    RETIRED.with_borrow_mut(|retired| retired.push(panel));
    DispatchQueue::main().exec_async(|| drop(RETIRED.take()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnail_keeps_aspect_and_never_upscales() {
        // широкий снимок упирается в ширину
        assert_eq!(fit(NSSize::new(2580.0, 1080.0), MAX_SIZE), NSSize::new(220.0, 92.0));
        // высокий упирается в высоту
        assert_eq!(fit(NSSize::new(400.0, 1200.0), MAX_SIZE), NSSize::new(53.0, 160.0));
        // маленький остаётся как есть
        assert_eq!(fit(NSSize::new(100.0, 60.0), MAX_SIZE), NSSize::new(100.0, 60.0));
    }
}
