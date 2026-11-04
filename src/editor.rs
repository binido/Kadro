//! Редактор: холст с масштабом, обрезка, изменение размера, отмена и вывод снимка.
//!
//! Правки хранит [`document::History`], а холст каждый раз показывает картинку, заново
//! нарисованную из исходного снимка. Окно живёт в `EDITORS` до закрытия.

use std::cell::{Cell, OnceCell, RefCell};
use std::path::Path;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSAutoresizingMaskOptions, NSBackingStoreType, NSBeep,
    NSBezierPath, NSClipView, NSColor, NSCompositingOperation, NSControlTextEditingDelegate, NSEvent,
    NSEventModifierFlags, NSFloatingWindowLevel, NSImage, NSImageScaling, NSImageView, NSModalResponseOK,
    NSNormalWindowLevel, NSOpenPanel, NSPasteboard, NSPasteboardTypePNG, NSPasteboardTypeTIFF, NSPrintOperation,
    NSRectFill, NSRectFillUsingOperation, NSResponder, NSSavePanel, NSScreen, NSScrollView, NSStackView, NSTextField,
    NSTextFieldDelegate, NSToolbar, NSToolbarDelegate, NSToolbarDisplayMode, NSToolbarFlexibleSpaceItemIdentifier,
    NSToolbarItem, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask, NSWindowToolbarStyle,
};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGImage;
use objc2_foundation::{
    NSArray, NSCopying, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSTimer, NSURL,
    ns_string,
};
use objc2_uniform_type_identifiers::UTType;

use crate::capture::Shot;
use crate::document::{History, PixelRect, State, render};
use crate::settings::{DefaultZoom, EscAction, Settings};
use crate::{assets, keys, output, pin, preview};

const ZOOM_STEP: f64 = 1.25;
const MIN_ZOOM: f64 = 0.05;
const MAX_ZOOM: f64 = 32.0;
/// Окно при открытии занимает не больше этой доли экрана.
const MAX_SCREEN_SHARE: f64 = 0.85;
const MIN_WINDOW: NSSize = NSSize::new(480.0, 320.0);

unsafe extern "C" {
    fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
}

thread_local! {
    static EDITORS: RefCell<Vec<Retained<Editor>>> = const { RefCell::new(Vec::new()) };
    // Закрытые редакторы живут до конца обработчика закрытия, освобождать их посреди него нельзя.
    static RETIRED: RefCell<Vec<Retained<Editor>>> = const { RefCell::new(Vec::new()) };
}

/// Кнопка панели инструментов: идентификатор, подпись, иконка и действие.
struct Tool {
    id: &'static str,
    label: &'static str,
    icon: &'static [u8],
    action: Sel,
}

fn tools() -> [Tool; 12] {
    [
        Tool { id: "undo", label: "Undo", icon: assets::UNDO, action: sel!(undoEdit:) },
        Tool { id: "redo", label: "Redo", icon: assets::REDO, action: sel!(redoEdit:) },
        Tool { id: "crop", label: "Crop", icon: assets::CROP, action: sel!(toggleCrop:) },
        Tool { id: "uncrop", label: "Uncrop", icon: assets::EXPAND, action: sel!(uncrop:) },
        Tool { id: "resize", label: "Resize", icon: assets::SCALING, action: sel!(resize:) },
        Tool { id: "zoomOut", label: "Zoom Out", icon: assets::ZOOM_OUT, action: sel!(zoomOut:) },
        Tool { id: "zoomFit", label: "Fit", icon: assets::MAXIMIZE, action: sel!(zoomToFit:) },
        Tool { id: "zoomIn", label: "Zoom In", icon: assets::ZOOM_IN, action: sel!(zoomIn:) },
        Tool { id: "copy", label: "Copy", icon: assets::COPY, action: sel!(copyImage:) },
        Tool { id: "save", label: "Save", icon: assets::SAVE, action: sel!(saveImage:) },
        Tool { id: "saveAs", label: "Save As", icon: assets::FOLDER_OPEN, action: sel!(saveImageAs:) },
        Tool { id: "print", label: "Print", icon: assets::PRINTER, action: sel!(printImage:) },
    ]
}

/// Порядок кнопок: группы разделены гибким пробелом.
const TOOLBAR_LAYOUT: [&str; 16] = [
    "undo", "redo", "", "crop", "uncrop", "resize", "", "zoomOut", "zoomFit", "zoomIn", "", "copy", "save", "saveAs",
    "print", "",
];

struct EditorIvars {
    original: CFRetained<CGImage>,
    /// Пикселей снимка на точку экрана, где его сняли. Нужен для DPI при экспорте.
    scale: f64,
    shot: Option<Shot>,
    history: RefCell<History>,
    rendered: RefCell<CFRetained<CGImage>>,
    window: OnceCell<Retained<NSWindow>>,
    scroll: OnceCell<Retained<NSScrollView>>,
    canvas: OnceCell<Retained<Canvas>>,
    image_view: OnceCell<Retained<NSImageView>>,
    /// Поля ширины и высоты открытого окна изменения размера.
    size_fields: RefCell<Option<(Retained<NSTextField>, Retained<NSTextField>)>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = EditorIvars]
    struct Editor;

    impl Editor {
        #[unsafe(method(undoEdit:))]
        fn undo_edit(&self, _sender: Option<&AnyObject>) {
            self.undo();
        }

        #[unsafe(method(redoEdit:))]
        fn redo_edit(&self, _sender: Option<&AnyObject>) {
            self.redo();
        }

        #[unsafe(method(toggleCrop:))]
        fn toggle_crop(&self, _sender: Option<&AnyObject>) {
            self.canvas().set_cropping(!self.canvas().ivars().cropping.get());
        }

        #[unsafe(method(uncrop:))]
        fn uncrop_action(&self, _sender: Option<&AnyObject>) {
            self.uncrop();
        }

        #[unsafe(method(resize:))]
        fn resize_action(&self, _sender: Option<&AnyObject>) {
            self.ask_size();
        }

        #[unsafe(method(zoomIn:))]
        fn zoom_in(&self, _sender: Option<&AnyObject>) {
            self.zoom_by(ZOOM_STEP);
        }

        #[unsafe(method(zoomOut:))]
        fn zoom_out(&self, _sender: Option<&AnyObject>) {
            self.zoom_by(1.0 / ZOOM_STEP);
        }

        #[unsafe(method(zoomToFit:))]
        fn zoom_to_fit(&self, _sender: Option<&AnyObject>) {
            self.fit();
        }

        #[unsafe(method(copyImage:))]
        fn copy_action(&self, _sender: Option<&AnyObject>) {
            self.copy();
        }

        #[unsafe(method(saveImage:))]
        fn save_action(&self, _sender: Option<&AnyObject>) {
            self.save();
        }

        #[unsafe(method(saveImageAs:))]
        fn save_as_action(&self, _sender: Option<&AnyObject>) {
            self.save_as();
        }

        #[unsafe(method(printImage:))]
        fn print_action(&self, _sender: Option<&AnyObject>) {
            self.print();
        }
    }

    unsafe impl NSObjectProtocol for Editor {}

    unsafe impl NSWindowDelegate for Editor {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            let closed = EDITORS.with_borrow_mut(|editors| {
                let index = editors.iter().position(|e| std::ptr::from_ref::<Editor>(e) == std::ptr::from_ref(self))?;
                Some(editors.remove(index))
            });
            if let Some(editor) = closed {
                RETIRED.with_borrow_mut(|retired| retired.push(editor));
                dispatch2::DispatchQueue::main().exec_async(|| drop(RETIRED.take()));
                // Освобождённые буферы снимка malloc держит про запас, и Kadro оставался бы на сотнях
                // мегабайт после закрытия редактора. Через секунду Core Animation успевает отпустить
                // свои копии картинки, и свободное возвращается системе.
                let relieve = RcBlock::new(|_timer: NonNull<NSTimer>| {
                    // SAFETY: null означает все зоны malloc, функция возвращает системе только свободное.
                    unsafe { malloc_zone_pressure_relief(std::ptr::null_mut(), 0) };
                });
                unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(1.0, false, &relieve) };
            }
        }
    }

    unsafe impl NSToolbarDelegate for Editor {
        #[unsafe(method_id(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:))]
        fn toolbar_item(&self, _toolbar: &NSToolbar, identifier: &NSString, _inserted: bool) -> Option<Retained<NSToolbarItem>> {
            self.toolbar_item_for(identifier)
        }

        #[unsafe(method_id(toolbarDefaultItemIdentifiers:))]
        fn default_items(&self, _toolbar: &NSToolbar) -> Retained<NSArray<NSString>> {
            toolbar_identifiers()
        }

        #[unsafe(method_id(toolbarAllowedItemIdentifiers:))]
        fn allowed_items(&self, _toolbar: &NSToolbar) -> Retained<NSArray<NSString>> {
            toolbar_identifiers()
        }
    }

    unsafe impl NSControlTextEditingDelegate for Editor {
        // Ширина и высота связаны пропорцией: правка одного поля пересчитывает другое.
        #[unsafe(method(controlTextDidChange:))]
        fn size_field_changed(&self, notification: &NSNotification) {
            let fields = self.ivars().size_fields.borrow();
            let Some((width_field, height_field)) = fields.as_ref() else { return };
            let state = self.ivars().history.borrow().current();
            let ratio = state.height / state.width;
            let edited = notification.object();
            let width_ptr = std::ptr::from_ref::<NSTextField>(width_field).cast::<AnyObject>();
            let is_width = edited.as_deref().is_some_and(|o| std::ptr::from_ref(o) == width_ptr);
            if is_width {
                if let Some(width) = parse(width_field) {
                    height_field.setStringValue(&NSString::from_str(&format!("{}", (width * ratio).round())));
                }
            } else if let Some(height) = parse(height_field) {
                width_field.setStringValue(&NSString::from_str(&format!("{}", (height / ratio).round())));
            }
        }
    }

    unsafe impl NSTextFieldDelegate for Editor {}
);

fn toolbar_identifiers() -> Retained<NSArray<NSString>> {
    let ids: Vec<_> = TOOLBAR_LAYOUT
        .iter()
        .map(|id| {
            // SAFETY: системная константа - неизменяемая строка, живущая весь процесс.
            if id.is_empty() { unsafe { NSToolbarFlexibleSpaceItemIdentifier }.copy() } else { NSString::from_str(id) }
        })
        .collect();
    NSArray::from_retained_slice(&ids)
}

fn parse(field: &NSTextField) -> Option<f64> {
    field.stringValue().to_string().trim().parse::<f64>().ok().filter(|v| *v >= 1.0)
}

impl Editor {
    fn toolbar_item_for(&self, identifier: &NSString) -> Option<Retained<NSToolbarItem>> {
        let tool = tools().into_iter().find(|t| identifier.to_string() == t.id)?;
        let item = NSToolbarItem::initWithItemIdentifier(NSToolbarItem::alloc(self.mtm()), identifier);
        item.setLabel(&NSString::from_str(tool.label));
        item.setToolTip(Some(&NSString::from_str(tool.label)));
        item.setImage(assets::icon(tool.icon, 18.0).as_deref());
        unsafe {
            item.setTarget(Some(self));
            item.setAction(Some(tool.action));
        }
        Some(item)
    }

    fn canvas(&self) -> &Canvas {
        self.ivars().canvas.get().expect("built with the window")
    }

    fn window(&self) -> &NSWindow {
        self.ivars().window.get().expect("built with the window")
    }

    fn scroll(&self) -> &NSScrollView {
        self.ivars().scroll.get().expect("built with the window")
    }

    fn original_size(&self) -> (f64, f64) {
        let image = &self.ivars().original;
        (CGImage::width(Some(image)) as f64, CGImage::height(Some(image)) as f64)
    }

    /// Применяет новое состояние: запоминает его в истории и перерисовывает холст.
    fn apply(&self, state: State) {
        self.ivars().history.borrow_mut().push(state);
        self.refresh(true);
    }

    fn undo(&self) {
        let moved = self.ivars().history.borrow_mut().undo();
        if moved { self.refresh(true) } else { NSBeep() }
    }

    fn redo(&self) {
        let moved = self.ivars().history.borrow_mut().redo();
        if moved { self.refresh(true) } else { NSBeep() }
    }

    fn uncrop(&self) {
        let (width, height) = self.original_size();
        let state = self.ivars().history.borrow().current();
        if state.is_cropped(width, height) { self.apply(state.uncropped(width, height)) } else { NSBeep() }
    }

    /// Обрезает по выделению на холсте, переведённому в пиксели результата.
    fn crop_to(&self, selection: NSRect) {
        let state = self.ivars().history.borrow().current();
        let size = self.canvas().frame().size;
        let factor = state.width / size.width;
        let rect = PixelRect::new(
            selection.origin.x * factor,
            // У холста начало внизу, у пикселей снимка - вверху.
            (size.height - selection.origin.y - selection.size.height) * factor,
            selection.size.width * factor,
            selection.size.height * factor,
        );
        self.apply(state.cropped(rect));
    }

    /// Перерисовывает результат и подгоняет под него холст.
    fn refresh(&self, refit: bool) {
        let state = self.ivars().history.borrow().current();
        let Some(image) = render(&self.ivars().original, state) else {
            return eprintln!("kadro: failed to render the edited image");
        };
        let size = self.points(state.width, state.height);
        let ns_image = NSImage::initWithCGImage_size(NSImage::alloc(), &image, size);
        *self.ivars().rendered.borrow_mut() = image;
        let image_view = self.ivars().image_view.get().expect("built with the window");
        image_view.setImage(Some(&ns_image));
        self.canvas().setFrameSize(size);
        self.window().setSubtitle(&NSString::from_str(&format!("{} × {}", state.width, state.height)));
        if refit {
            self.fit();
        }
    }

    /// Размер в точках, при котором масштаб 1 - это один пиксель снимка на пиксель экрана.
    fn points(&self, width: f64, height: f64) -> NSSize {
        let backing = self.ivars().window.get().map_or(2.0, |w| w.backingScaleFactor());
        NSSize::new(width / backing, height / backing)
    }

    fn fit(&self) {
        self.scroll().magnifyToFitRect(self.canvas().bounds());
    }

    fn zoom_by(&self, factor: f64) {
        let scroll = self.scroll();
        let visible = scroll.documentVisibleRect();
        let center =
            NSPoint::new(visible.origin.x + visible.size.width / 2.0, visible.origin.y + visible.size.height / 2.0);
        self.zoom_to(scroll.magnification() * factor, center);
    }

    fn zoom_to(&self, magnification: f64, center: NSPoint) {
        self.scroll().setMagnification_centeredAtPoint(magnification.clamp(MIN_ZOOM, MAX_ZOOM), center);
    }

    fn zoom_actual(&self) {
        self.zoom_to(
            1.0,
            NSPoint::new(self.canvas().frame().size.width / 2.0, self.canvas().frame().size.height / 2.0),
        );
    }

    /// 100%, если снимок помещается в окно, иначе целиком в окне.
    fn initial_zoom(&self) {
        let fits = {
            let content = self.scroll().contentSize();
            let size = self.canvas().frame().size;
            size.width <= content.width && size.height <= content.height
        };
        match Settings::load().default_zoom {
            DefaultZoom::Actual if fits => self.zoom_actual(),
            _ => self.fit(),
        }
    }

    fn rendered(&self) -> CFRetained<CGImage> {
        self.ivars().rendered.borrow().clone()
    }

    fn copy(&self) {
        output::copy_in_background(self.rendered(), self.ivars().scale);
    }

    fn save(&self) {
        let (image, scale, shot) = (self.rendered(), self.ivars().scale, self.ivars().shot.clone());
        output::in_background(move || {
            if let Err(err) = output::save(&Settings::load().directory, &image, scale, shot.as_ref(), None) {
                eprintln!("kadro: failed to save screenshot: {err}");
            }
        });
        if let Some(screen) = self.window().screen() {
            preview::popup_on(self.mtm(), "Saved", &screen);
        }
    }

    fn save_as(&self) {
        let panel = NSSavePanel::savePanel(self.mtm());
        panel.setNameFieldStringValue(&NSString::from_str(&format!("{}.png", self.window().title())));
        panel.setCanCreateDirectories(true);
        if panel.runModal() != NSModalResponseOK {
            return;
        }
        let Some(path) = panel.URL().and_then(|url| url.path()) else { return };
        let path = std::path::PathBuf::from(path.to_string());
        if let Err(err) = output::write_to(&path, &self.rendered(), self.ivars().scale) {
            let alert = NSAlert::new(self.mtm());
            alert.setMessageText(ns_string!("Couldn't save the image"));
            alert.setInformativeText(&NSString::from_str(&format!("{err}. Use .png, .jpg or .heic.")));
            alert.runModal();
        }
    }

    fn print(&self) {
        let view = self.ivars().image_view.get().expect("built with the window");
        NSPrintOperation::printOperationWithView(view).runOperation();
    }

    /// Спрашивает новый размер. Ширина и высота связаны пропорцией.
    fn ask_size(&self) {
        let mtm = self.mtm();
        let state = self.ivars().history.borrow().current();
        let field = |value: f64| {
            let field = NSTextField::textFieldWithString(&NSString::from_str(&format!("{value}")), mtm);
            field.setFrameSize(NSSize::new(80.0, 22.0));
            unsafe { field.setDelegate(Some(ProtocolObject::from_ref(self))) };
            field
        };
        let (width, height) = (field(state.width), field(state.height));
        let times = NSTextField::labelWithString(ns_string!("×"), mtm);
        let unit = NSTextField::labelWithString(ns_string!("px"), mtm);
        let row = NSStackView::stackViewWithViews(
            &NSArray::from_retained_slice(&[view(&width), view(&times), view(&height), view(&unit)]),
            mtm,
        );
        row.setFrameSize(row.fittingSize());
        *self.ivars().size_fields.borrow_mut() = Some((width.clone(), height));

        let alert = NSAlert::new(mtm);
        alert.setMessageText(ns_string!("Resize"));
        alert.setInformativeText(ns_string!("The aspect ratio stays the same."));
        alert.setAccessoryView(Some(&row));
        alert.addButtonWithTitle(ns_string!("Resize"));
        alert.addButtonWithTitle(ns_string!("Cancel"));
        let response = alert.runModal();
        self.ivars().size_fields.borrow_mut().take();
        if response == NSAlertFirstButtonReturn {
            match parse(&width) {
                Some(value) => self.apply(state.resized(value)),
                None => NSBeep(),
            }
        }
    }

    /// Esc: копирует, сохраняет или просто закрывает - как выбрано в настройках.
    fn escape(&self) {
        match Settings::load().esc_action {
            EscAction::Copy => self.copy(),
            EscAction::Save => self.save(),
            EscAction::Close => {}
        }
        self.window().close();
    }

    /// Клавиши с ⌘. Окно меню не имеет, поэтому сочетания разбираются здесь.
    fn key_equivalent(&self, event: &NSEvent) -> bool {
        let flags = event.modifierFlags();
        if !flags.contains(NSEventModifierFlags::Command) {
            return false;
        }
        let shift = flags.contains(NSEventModifierFlags::Shift);
        match event.keyCode() {
            keys::Z if shift => self.redo(),
            keys::Z => self.undo(),
            keys::C => self.copy(),
            keys::S if shift => self.save_as(),
            keys::S => self.save(),
            keys::P => self.print(),
            keys::EQUAL => self.zoom_by(ZOOM_STEP),
            keys::MINUS => self.zoom_by(1.0 / ZOOM_STEP),
            keys::ZERO => self.zoom_actual(),
            keys::NINE => self.fit(),
            keys::W => self.window().close(),
            _ => return false,
        }
        true
    }
}

fn view(view: &NSView) -> Retained<NSView> {
    objc2::Message::retain(view)
}

struct CanvasIvars {
    editor: Weak<Editor>,
    overlay: OnceCell<Retained<CropOverlay>>,
    cropping: Cell<bool>,
    start: Cell<Option<NSPoint>>,
    selection: Cell<Option<NSRect>>,
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = CanvasIvars]
    struct Canvas;

    impl Canvas {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            if self.ivars().cropping.get() {
                self.ivars().start.set(Some(self.point(event)));
                self.ivars().selection.set(None);
                self.redraw_overlay();
            }
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let Some(start) = self.ivars().start.get() else { return };
            let end = self.point(event);
            let rect = NSRect::new(
                NSPoint::new(start.x.min(end.x), start.y.min(end.y)),
                NSSize::new((start.x - end.x).abs(), (start.y - end.y).abs()),
            );
            self.ivars().selection.set(Some(rect));
            self.redraw_overlay();
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            self.ivars().start.set(None);
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let Some(editor) = self.ivars().editor.load() else { return };
            let plain = !event.modifierFlags().intersects(
                NSEventModifierFlags::Command | NSEventModifierFlags::Control | NSEventModifierFlags::Option,
            );
            match event.keyCode() {
                keys::ESCAPE if self.ivars().cropping.get() => self.set_cropping(false),
                keys::ESCAPE => editor.escape(),
                keys::RETURN | keys::ENTER if self.ivars().cropping.get() => {
                    if let Some(selection) = self.ivars().selection.get().filter(|s| s.size.width >= 1.0 && s.size.height >= 1.0) {
                        self.set_cropping(false);
                        editor.crop_to(selection);
                    }
                }
                keys::C if plain => self.set_cropping(!self.ivars().cropping.get()),
                _ => unsafe { msg_send![super(self), keyDown: event] },
            }
        }

        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> bool {
            let handled = self.ivars().editor.load().is_some_and(|editor| editor.key_equivalent(event));
            handled || unsafe { msg_send![super(self), performKeyEquivalent: event] }
        }

        // ⌘ + колесо масштабирует всегда, просто колесо - если так выбрано в настройках.
        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let command = event.modifierFlags().contains(NSEventModifierFlags::Command);
            match self.ivars().editor.load() {
                Some(editor) if command || Settings::load().scroll_zoom => {
                    let step = if event.hasPreciseScrollingDeltas() { 0.01 } else { 0.1 };
                    let factor = (event.scrollingDeltaY() * step).exp();
                    editor.zoom_to(editor.scroll().magnification() * factor, self.point(event));
                }
                _ => unsafe { msg_send![super(self), scrollWheel: event] },
            }
        }
    }
);

impl Canvas {
    fn point(&self, event: &NSEvent) -> NSPoint {
        let p = self.convertPoint_fromView(event.locationInWindow(), None);
        let size = self.bounds().size;
        NSPoint::new(p.x.clamp(0.0, size.width), p.y.clamp(0.0, size.height))
    }

    fn magnification(&self) -> f64 {
        self.ivars().editor.load().map_or(1.0, |e| e.scroll().magnification())
    }

    fn set_cropping(&self, cropping: bool) {
        self.ivars().cropping.set(cropping);
        self.ivars().start.set(None);
        self.ivars().selection.set(None);
        self.redraw_overlay();
    }

    fn redraw_overlay(&self) {
        if let Some(overlay) = self.ivars().overlay.get() {
            overlay.setNeedsDisplay(true);
        }
    }
}

struct OverlayIvars {
    canvas: Weak<Canvas>,
}

define_class!(
    /// Затемнение и рамка обрезки поверх картинки. Мышь проходит сквозь него к холсту.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = OverlayIvars]
    struct CropOverlay;

    impl CropOverlay {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let Some(canvas) = self.ivars().canvas.load() else { return };
            if !canvas.ivars().cropping.get() {
                return;
            }
            NSColor::colorWithWhite_alpha(0.0, 0.45).setFill();
            NSRectFill(self.bounds());
            if let Some(selection) = canvas.ivars().selection.get() {
                NSRectFillUsingOperation(selection, NSCompositingOperation::Clear);
                NSColor::whiteColor().setStroke();
                let border = NSBezierPath::bezierPathWithRect(selection);
                // Рамка в один экранный пиксель при любом масштабе.
                border.setLineWidth(1.0 / canvas.magnification());
                border.stroke();
            }
        }

        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, _point: NSPoint) -> Option<Retained<NSView>> {
            None
        }
    }
);

define_class!(
    /// Прокрутка, которая держит холст по центру, когда он меньше окна.
    #[unsafe(super(NSClipView, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    struct CenteringClipView;

    impl CenteringClipView {
        #[unsafe(method(constrainBoundsRect:))]
        fn constrain_bounds_rect(&self, proposed: NSRect) -> NSRect {
            let mut bounds: NSRect = unsafe { msg_send![super(self), constrainBoundsRect: proposed] };
            if let Some(document) = self.documentView() {
                let frame = document.frame();
                if frame.size.width < bounds.size.width {
                    bounds.origin.x = (frame.size.width - bounds.size.width) / 2.0;
                }
                if frame.size.height < bounds.size.height {
                    bounds.origin.y = (frame.size.height - bounds.size.height) / 2.0;
                }
            }
            bounds
        }
    }
);

/// Открывает снимок в редакторе. Без `shot` - картинка из файла или буфера.
pub fn open(mtm: MainThreadMarker, image: CFRetained<CGImage>, scale: f64, shot: Option<Shot>, title: &str) {
    let (width, height) = (CGImage::width(Some(&image)) as f64, CGImage::height(Some(&image)) as f64);
    let ivars = EditorIvars {
        rendered: RefCell::new(image.clone()),
        original: image,
        scale,
        shot,
        history: RefCell::new(History::new(State::original(width, height))),
        window: OnceCell::new(),
        scroll: OnceCell::new(),
        canvas: OnceCell::new(),
        image_view: OnceCell::new(),
        size_fields: RefCell::new(None),
    };
    let editor: Retained<Editor> = unsafe { msg_send![super(Editor::alloc(mtm).set_ivars(ivars)), init] };
    let settings = Settings::load();

    let screen = NSScreen::mainScreen(mtm);
    let backing = screen.as_ref().map_or(2.0, |s| s.backingScaleFactor());
    let visible = screen.as_ref().map_or(NSRect::new(NSPoint::ZERO, NSSize::new(1440.0, 900.0)), |s| s.visibleFrame());
    let natural = NSSize::new(width / backing, height / backing);
    let content = NSSize::new(
        natural.width.min(visible.size.width * MAX_SCREEN_SHARE).max(MIN_WINDOW.width),
        natural.height.min(visible.size.height * MAX_SCREEN_SHARE).max(MIN_WINDOW.height),
    );

    let window: Retained<NSWindow> = unsafe {
        msg_send![
            NSWindow::alloc(mtm),
            initWithContentRect: NSRect::new(NSPoint::ZERO, content),
            styleMask: NSWindowStyleMask::Titled
                | NSWindowStyleMask::Closable
                | NSWindowStyleMask::Miniaturizable
                | NSWindowStyleMask::Resizable,
            backing: NSBackingStoreType::Buffered,
            defer: false,
        ]
    };
    // Окно держит EDITORS через редактор, иначе close() освободит его второй раз.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str(title));
    window.setContentMinSize(MIN_WINDOW);
    window.setLevel(if settings.editor_on_top { NSFloatingWindowLevel } else { NSNormalWindowLevel });
    window.setDelegate(Some(ProtocolObject::from_ref(&*editor)));

    let toolbar = NSToolbar::initWithIdentifier(NSToolbar::alloc(mtm), ns_string!("KadroEditor"));
    toolbar.setDisplayMode(NSToolbarDisplayMode::IconOnly);
    toolbar.setDelegate(Some(ProtocolObject::from_ref(&*editor)));
    window.setToolbar(Some(&toolbar));
    window.setToolbarStyle(NSWindowToolbarStyle::Unified);

    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), NSRect::new(NSPoint::ZERO, content));
    let clip: Retained<CenteringClipView> =
        unsafe { msg_send![CenteringClipView::alloc(mtm), initWithFrame: NSRect::ZERO] };
    scroll.setContentView(&clip);
    scroll.setHasVerticalScroller(true);
    scroll.setHasHorizontalScroller(true);
    scroll.setAllowsMagnification(true);
    scroll.setMinMagnification(MIN_ZOOM);
    scroll.setMaxMagnification(MAX_ZOOM);
    scroll.setBackgroundColor(&NSColor::windowBackgroundColor());
    scroll.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );

    let canvas_ivars = CanvasIvars {
        editor: Weak::from_retained(&editor),
        overlay: OnceCell::new(),
        cropping: Cell::new(false),
        start: Cell::new(None),
        selection: Cell::new(None),
    };
    let canvas: Retained<Canvas> = unsafe {
        msg_send![super(Canvas::alloc(mtm).set_ivars(canvas_ivars)), initWithFrame: NSRect::new(NSPoint::ZERO, natural)]
    };
    let image_view = NSImageView::initWithFrame(NSImageView::alloc(mtm), NSRect::new(NSPoint::ZERO, natural));
    image_view.setImageScaling(NSImageScaling::ScaleAxesIndependently);
    image_view.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    canvas.addSubview(&image_view);
    let overlay: Retained<CropOverlay> = unsafe {
        msg_send![
            super(CropOverlay::alloc(mtm).set_ivars(OverlayIvars { canvas: Weak::from_retained(&canvas) })),
            initWithFrame: NSRect::new(NSPoint::ZERO, natural)
        ]
    };
    overlay.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    canvas.addSubview(&overlay);
    let _ = canvas.ivars().overlay.set(overlay);
    scroll.setDocumentView(Some(&canvas));
    window.setContentView(Some(&scroll));
    window.makeFirstResponder(Some(&canvas));

    let _ = editor.ivars().window.set(window.clone());
    let _ = editor.ivars().scroll.set(scroll);
    let _ = editor.ivars().canvas.set(canvas);
    let _ = editor.ivars().image_view.set(image_view);
    editor.refresh(false);
    window.center();
    NSApplication::sharedApplication(mtm).activate();
    window.makeKeyAndOrderFront(None);
    editor.initial_zoom();
    EDITORS.with_borrow_mut(|editors| editors.push(editor));
}

/// Открывает в редакторе последний снимок. Если снимков ещё не было, только сигналит.
pub fn open_last(mtm: MainThreadMarker) {
    match pin::load_last() {
        Some((image, scale)) => open(mtm, image, scale, pin::last_shot(), "Screenshot"),
        None => NSBeep(),
    }
}

/// Открывает картинку из файла.
pub fn open_file(mtm: MainThreadMarker, path: &Path) {
    let decoded = std::fs::read(path).ok().and_then(|bytes| output::decode(&bytes));
    let Some((image, scale)) = decoded else {
        let alert = NSAlert::new(mtm);
        alert.setMessageText(ns_string!("Couldn't open the image"));
        alert.setInformativeText(&NSString::from_str(&path.display().to_string()));
        alert.runModal();
        return;
    };
    let title = path.file_stem().map_or_else(|| "Image".into(), |s| s.to_string_lossy().into_owned());
    open(mtm, image, scale, None, &title);
}

/// Предлагает выбрать картинку и открывает её.
pub fn choose_file(mtm: MainThreadMarker) {
    let panel = NSOpenPanel::openPanel(mtm);
    if let Some(images) = UTType::typeWithIdentifier(ns_string!("public.image")) {
        panel.setAllowedContentTypes(&NSArray::from_retained_slice(&[images]));
    }
    NSApplication::sharedApplication(mtm).activate();
    if panel.runModal() == NSModalResponseOK
        && let Some(path) = panel.URL().and_then(|url| url.path())
    {
        open_file(mtm, Path::new(&path.to_string()));
    }
}

/// Открывает картинку из буфера обмена. Если там нет картинки, только сигналит.
pub fn open_clipboard(mtm: MainThreadMarker) {
    let pasteboard = NSPasteboard::generalPasteboard();
    // SAFETY: системные константы типов буфера обмена живут весь процесс.
    let types = unsafe { [NSPasteboardTypePNG, NSPasteboardTypeTIFF] };
    let decoded =
        types.iter().find_map(|kind| pasteboard.dataForType(kind)).and_then(|data| output::decode(&data.to_vec()));
    match decoded {
        Some((image, scale)) => open(mtm, image, scale, None, "Clipboard"),
        None => NSBeep(),
    }
}

/// Открывает файлы, которые Finder передал Kadro.
pub fn open_urls(mtm: MainThreadMarker, urls: &NSArray<NSURL>) {
    for url in urls {
        if let Some(path) = url.path() {
            open_file(mtm, Path::new(&path.to_string()));
        }
    }
}
