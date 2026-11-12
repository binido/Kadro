//! Редактор: холст с масштабом, обрезка, изменение размера, аннотации, отмена и вывод снимка.
//!
//! Правки хранит [`document::History`], а холст каждый раз показывает картинку, заново
//! нарисованную из основы и документа. Аннотацию, которую тянут мышью, рисует поверх
//! слой `Marks`, чтобы не перерисовывать весь снимок на каждое движение. Окно живёт в `EDITORS` до закрытия.

use std::cell::{Cell, OnceCell, RefCell};
use std::path::Path;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAffineTransformNSAppKitAdditions, NSAlert, NSAlertFirstButtonReturn, NSApplication, NSAutoresizingMaskOptions,
    NSBackingStoreType, NSBeep, NSBezelStyle, NSBezierPath, NSButton, NSButtonType, NSClipView, NSColor, NSColorSpace,
    NSColorWell, NSColorWellStyle, NSCompositingOperation, NSControlStateValueOff, NSControlStateValueOn,
    NSControlTextEditingDelegate, NSCursor, NSEvent, NSEventModifierFlags, NSEventType, NSFloatingWindowLevel,
    NSGraphicsContext, NSGridCellPlacement, NSGridView, NSImage, NSImageScaling, NSImageView, NSModalResponseOK,
    NSNormalWindowLevel, NSOpenPanel, NSPasteboard, NSPasteboardTypePNG, NSPasteboardTypeString, NSPasteboardTypeTIFF,
    NSPopUpButton, NSPopover, NSPopoverBehavior, NSPrintOperation, NSRectFill, NSRectFillUsingOperation, NSResponder,
    NSSavePanel, NSScreen, NSScrollView, NSSlider, NSStackView, NSStackViewGravity, NSTextField, NSTextFieldDelegate,
    NSToolbar, NSToolbarDelegate, NSToolbarDisplayMode, NSToolbarFlexibleSpaceItemIdentifier, NSToolbarItem,
    NSToolbarSpaceItemIdentifier, NSTrackingArea, NSTrackingAreaOptions, NSUserInterfaceLayoutOrientation, NSView,
    NSViewController, NSWindow, NSWindowDelegate, NSWindowStyleMask, NSWindowToolbarStyle,
};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGImage;
use objc2_foundation::{
    NSAffineTransform, NSArray, NSCopying, NSEdgeInsets, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect,
    NSSize, NSString, NSTimer, ns_string,
};
use objc2_uniform_type_identifiers::UTType;

use crate::annotation::{self, Annotation, Color, Grip, Point, Size, Snap, Tool};
use crate::backdrop::{self, Backdrop, Fill, Layout};
use crate::capture::Shot;
use crate::document::{self, Doc, History, PixelRect, State, present};
use crate::paint::{self, Mode};
use crate::settings::{DefaultZoom, EscAction, Settings};
use crate::{assets, keys, measure, output, overlay, pin, preview, recognize, upload};

const ZOOM_STEP: f64 = 1.25;
const MIN_ZOOM: f64 = 0.05;
const MAX_ZOOM: f64 = 32.0;
/// Окно при открытии занимает не больше этой доли экрана.
const MAX_SCREEN_SHARE: f64 = 0.85;
const MIN_WINDOW: NSSize = NSSize::new(640.0, 540.0);
const PALETTE_WIDTH: f64 = 44.0;
const PALETTE_BUTTON: f64 = 28.0;
/// На сколько экранных точек можно промахнуться мимо аннотации.
const HIT_TOLERANCE: f64 = 6.0;

unsafe extern "C" {
    fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
}

thread_local! {
    static EDITORS: RefCell<Vec<Retained<Editor>>> = const { RefCell::new(Vec::new()) };
    // Закрытые редакторы живут до конца обработчика закрытия, освобождать их посреди него нельзя.
    static RETIRED: RefCell<Vec<Retained<Editor>>> = const { RefCell::new(Vec::new()) };
    /// Редактор, который ждёт снимок области для вставки.
    static ADD_TARGET: RefCell<Option<Weak<Editor>>> = const { RefCell::new(None) };
}

/// Кнопка панели инструментов: идентификатор, подпись, иконка и действие.
struct Button {
    id: &'static str,
    label: &'static str,
    icon: &'static [u8],
    action: Sel,
}

fn buttons() -> [Button; 18] {
    [
        Button { id: "undo", label: "Undo", icon: assets::UNDO, action: sel!(undoEdit:) },
        Button { id: "redo", label: "Redo", icon: assets::REDO, action: sel!(redoEdit:) },
        Button { id: "crop", label: "Crop", icon: assets::CROP, action: sel!(toggleCrop:) },
        Button { id: "uncrop", label: "Uncrop", icon: assets::EXPAND, action: sel!(uncrop:) },
        Button { id: "resize", label: "Resize", icon: assets::SCALING, action: sel!(resize:) },
        Button { id: "rasterize", label: "Rasterize", icon: assets::LAYERS, action: sel!(rasterize:) },
        Button { id: "addCapture", label: "Add Capture", icon: assets::IMAGE_PLUS, action: sel!(addCapture:) },
        Button { id: "backdrop", label: "Backdrop", icon: assets::WALLPAPER, action: sel!(showBackdrop:) },
        Button { id: "recognize", label: "Recognize Text", icon: assets::SCAN_TEXT, action: sel!(recognizeText:) },
        Button { id: "zoomOut", label: "Zoom Out", icon: assets::ZOOM_OUT, action: sel!(zoomOut:) },
        Button { id: "zoomFit", label: "Fit", icon: assets::MAXIMIZE, action: sel!(zoomToFit:) },
        Button { id: "zoomIn", label: "Zoom In", icon: assets::ZOOM_IN, action: sel!(zoomIn:) },
        Button { id: "copy", label: "Copy", icon: assets::COPY, action: sel!(copyImage:) },
        Button { id: "save", label: "Save", icon: assets::SAVE, action: sel!(saveImage:) },
        Button { id: "saveAs", label: "Save As", icon: assets::FOLDER_OPEN, action: sel!(saveImageAs:) },
        Button { id: "upload", label: "Upload", icon: assets::CLOUD_UPLOAD, action: sel!(uploadImage:) },
        Button { id: "exportGif", label: "Export GIF", icon: assets::FILM, action: sel!(exportGif:) },
        Button { id: "print", label: "Print", icon: assets::PRINTER, action: sel!(printImage:) },
    ]
}

/// Порядок кнопок, пустая строка разделяет группы.
const TOOLBAR_LAYOUT: [&str; 25] = [
    "undo",
    "redo",
    "",
    "crop",
    "uncrop",
    "resize",
    "",
    "color",
    "size",
    "style",
    "rasterize",
    "addCapture",
    "backdrop",
    "recognize",
    "",
    "zoomOut",
    "zoomFit",
    "zoomIn",
    "",
    "copy",
    "save",
    "saveAs",
    "upload",
    "exportGif",
    "print",
];

/// Что делает мышь на холсте.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pointer {
    Select,
    Draw(Tool),
    /// Пипетка: показывает цвет под мышью и копирует его по щелчку.
    Pick,
}

/// Инструменты на панели слева: что делает мышь, подсказка, иконка и клавиша.
const PALETTE: [(Pointer, &str, &[u8], u16); 15] = [
    (Pointer::Select, "Select (V)", assets::POINTER, keys::V),
    (Pointer::Draw(Tool::Arrow), "Arrow (A)", assets::ARROW, keys::A),
    (Pointer::Draw(Tool::Line), "Line (L)", assets::LINE, keys::L),
    (Pointer::Draw(Tool::Rect), "Rectangle (R)", assets::RECT, keys::R),
    (Pointer::Draw(Tool::Oval), "Oval (O)", assets::OVAL, keys::O),
    (Pointer::Draw(Tool::Pen), "Pen (P)", assets::PEN, keys::P),
    (Pointer::Draw(Tool::Highlighter), "Highlighter (H)", assets::HIGHLIGHTER, keys::H),
    (Pointer::Draw(Tool::Text), "Text (T)", assets::TEXT, keys::T),
    (Pointer::Draw(Tool::Counter), "Counter (N)", assets::COUNTER, keys::N),
    (Pointer::Draw(Tool::Blur), "Blur (B)", assets::BLUR, keys::B),
    (Pointer::Draw(Tool::Pixelate), "Pixelate (X)", assets::PIXELATE, keys::X),
    (Pointer::Draw(Tool::Spotlight), "Spotlight (S)", assets::SPOTLIGHT, keys::S),
    (Pointer::Draw(Tool::Magnifier), "Magnifier (M)", assets::MAGNIFIER, keys::M),
    (Pointer::Draw(Tool::Ruler), "Ruler (U), click to measure between edges", assets::RULER, keys::U),
    (Pointer::Pick, "Color Picker (I)", assets::PIPETTE, keys::I),
];

/// Что сейчас делает мышь.
enum Drag {
    /// Новая аннотация, ещё не в документе.
    Draw(Annotation),
    /// Перенос аннотации `index`. `current` - как она выглядит сейчас, `moved` - сдвинули ли её вообще,
    /// `snap` - к чему она прилипла.
    Move {
        index: usize,
        grip: Grip,
        from: Point,
        original: Box<Annotation>,
        current: Annotation,
        moved: bool,
        snap: Snap,
    },
}

/// Элементы окна подложки.
struct BackdropControls {
    enabled: Retained<NSButton>,
    kind: Retained<NSPopUpButton>,
    color: Retained<NSColorWell>,
    padding: Retained<NSSlider>,
    shadow: Retained<NSSlider>,
    radius: Retained<NSSlider>,
    outer_radius: Retained<NSSlider>,
    inset: Retained<NSSlider>,
    aspect: Retained<NSPopUpButton>,
    auto_crop: Retained<NSButton>,
}

struct EditorIvars {
    /// Основы документа и добавленные снимки. 0 - исходный снимок, основы появляются при впечатывании аннотаций.
    images: RefCell<Vec<CFRetained<CGImage>>>,
    /// Пикселей снимка на точку экрана, где его сняли. Нужен для DPI при экспорте и толщины линий.
    scale: f64,
    shot: Option<Shot>,
    history: RefCell<History<Doc>>,
    rendered: RefCell<CFRetained<CGImage>>,
    pointer: Cell<Pointer>,
    color: Cell<Color>,
    size: Cell<Size>,
    /// Выбранный стиль каждого инструмента, по `Tool::index`.
    styles: Cell<[usize; Tool::ALL.len()]>,
    /// Запись цвета пипетки, по номеру в `measure::FORMATS`.
    pick_format: Cell<usize>,
    /// Цвет под пипеткой и где он, в пикселях основы.
    hover: Cell<Option<(Point, [u8; 3])>>,
    /// Где снимок лежит на холсте с подложкой. Без подложки - во весь холст.
    layout: Cell<Layout>,
    /// Последние настройки подложки, чтобы включение возвращало их.
    last_backdrop: Cell<Backdrop>,
    backdrop_popover: OnceCell<Retained<NSPopover>>,
    backdrop_controls: OnceCell<BackdropControls>,
    selected: Cell<Option<usize>>,
    drag: RefCell<Option<Drag>>,
    /// Аннотация, которой нет на картинке холста, пока её тянут.
    hidden: Cell<Option<usize>>,
    window: OnceCell<Retained<NSWindow>>,
    scroll: OnceCell<Retained<NSScrollView>>,
    canvas: OnceCell<Retained<Canvas>>,
    image_view: OnceCell<Retained<NSImageView>>,
    marks: OnceCell<Retained<Marks>>,
    palette: OnceCell<Vec<Retained<NSButton>>>,
    color_well: OnceCell<Retained<NSColorWell>>,
    size_popup: OnceCell<Retained<NSPopUpButton>>,
    style_popup: OnceCell<Retained<NSPopUpButton>>,
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

        #[unsafe(method(rasterize:))]
        fn rasterize_action(&self, _sender: Option<&AnyObject>) {
            self.rasterize();
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

        #[unsafe(method(addCapture:))]
        fn add_capture_action(&self, _sender: Option<&AnyObject>) {
            self.add_capture();
        }

        #[unsafe(method(showBackdrop:))]
        fn show_backdrop_action(&self, sender: &NSToolbarItem) {
            self.show_backdrop(sender);
        }

        #[unsafe(method(backdropChanged:))]
        fn backdrop_changed_action(&self, sender: &AnyObject) {
            self.backdrop_changed(sender);
        }

        #[unsafe(method(randomizeBackdrop:))]
        fn randomize_backdrop(&self, _sender: Option<&AnyObject>) {
            self.change_fill(|fill| fill.randomized(backdrop::seed()));
        }

        #[unsafe(method(reverseBackdrop:))]
        fn reverse_backdrop(&self, _sender: Option<&AnyObject>) {
            self.change_fill(Fill::reversed);
        }

        #[unsafe(method(uploadImage:))]
        fn upload_image(&self, _sender: Option<&AnyObject>) {
            upload::upload(self.mtm(), self.rendered(), self.ivars().scale, self.ivars().shot.clone());
        }

        #[unsafe(method(recognizeText:))]
        fn recognize_text(&self, _sender: Option<&AnyObject>) {
            recognize::recognize_and_show(self.rendered());
        }

        #[unsafe(method(exportGif:))]
        fn export_gif_action(&self, _sender: Option<&AnyObject>) {
            self.export_gif();
        }

        #[unsafe(method(chooseTool:))]
        fn choose_tool_action(&self, sender: &NSButton) {
            if let Some((pointer, ..)) = PALETTE.get(sender.tag() as usize) {
                self.choose(*pointer);
            }
        }

        #[unsafe(method(colorChanged:))]
        fn color_changed(&self, sender: &NSColorWell) {
            if let Some(color) = color_of(&sender.color()) {
                self.ivars().color.set(color);
                self.restyle(|a| a.color = color);
            }
        }

        #[unsafe(method(sizeChanged:))]
        fn size_changed(&self, sender: &NSPopUpButton) {
            if let Some(size) = Size::ALL.get(sender.indexOfSelectedItem() as usize) {
                self.ivars().size.set(*size);
                self.restyle(|a| a.size = *size);
            }
        }

        #[unsafe(method(styleChanged:))]
        fn style_changed(&self, sender: &NSPopUpButton) {
            let style = sender.indexOfSelectedItem().max(0) as usize;
            if self.ivars().pointer.get() == Pointer::Pick && self.ivars().selected.get().is_none() {
                return self.ivars().pick_format.set(style);
            }
            let Some(tool) = self.styled_tool() else { return };
            let mut styles = self.ivars().styles.get();
            styles[tool.index()] = style;
            self.ivars().styles.set(styles);
            self.restyle(|a| a.style = style);
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
            let state = self.state();
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
    // SAFETY: системные константы - неизменяемые строки, живущие весь процесс.
    let (space, flexible) = unsafe { (NSToolbarSpaceItemIdentifier, NSToolbarFlexibleSpaceItemIdentifier) };
    let mut ids = Vec::new();
    for id in TOOLBAR_LAYOUT {
        if id.is_empty() {
            // Гибкий пробел в узком окне сжимается до нуля, поэтому рядом стоит постоянный. Иначе группы слипаются.
            ids.extend([space.copy(), flexible.copy()]);
        } else {
            ids.push(NSString::from_str(id));
        }
    }
    ids.push(flexible.copy());
    NSArray::from_retained_slice(&ids)
}

fn parse(field: &NSTextField) -> Option<f64> {
    field.stringValue().to_string().trim().parse::<f64>().ok().filter(|v| *v >= 1.0)
}

fn color_of(color: &NSColor) -> Option<Color> {
    let srgb = color.colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())?;
    Some(Color {
        red: srgb.redComponent(),
        green: srgb.greenComponent(),
        blue: srgb.blueComponent(),
        alpha: srgb.alphaComponent(),
    })
}

impl Editor {
    fn toolbar_item_for(&self, identifier: &NSString) -> Option<Retained<NSToolbarItem>> {
        let item = NSToolbarItem::initWithItemIdentifier(NSToolbarItem::alloc(self.mtm()), identifier);
        let ivars = self.ivars();
        let control: Option<(&str, Retained<NSView>)> = match identifier.to_string().as_str() {
            "color" => ivars.color_well.get().map(|c| ("Color", view(c))),
            "size" => ivars.size_popup.get().map(|c| ("Size", view(c))),
            "style" => ivars.style_popup.get().map(|c| ("Style", view(c))),
            _ => None,
        };
        if let Some((label, control)) = control {
            item.setLabel(&NSString::from_str(label));
            item.setToolTip(Some(&NSString::from_str(label)));
            // Панель ставит элементы вплотную по видимой рамке, поля есть только у обёртки.
            let padded = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&[control]), self.mtm());
            padded.setEdgeInsets(NSEdgeInsets { top: 0.0, left: 4.0, bottom: 0.0, right: 4.0 });
            item.setView(Some(&padded));
            return Some(item);
        }
        let button = buttons().into_iter().find(|b| identifier.to_string() == b.id)?;
        item.setLabel(&NSString::from_str(button.label));
        item.setToolTip(Some(&NSString::from_str(button.label)));
        item.setImage(assets::icon(button.icon, 18.0).as_deref());
        unsafe {
            item.setTarget(Some(self));
            item.setAction(Some(button.action));
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

    fn doc(&self) -> Doc {
        self.ivars().history.borrow().current().clone()
    }

    fn state(&self) -> State {
        self.ivars().history.borrow().current().state
    }

    fn base(&self, doc: &Doc) -> CFRetained<CGImage> {
        self.ivars().images.borrow()[doc.base].clone()
    }

    fn original_size(&self) -> (f64, f64) {
        let image = &self.ivars().images.borrow()[0];
        (CGImage::width(Some(image)) as f64, CGImage::height(Some(image)) as f64)
    }

    /// Применяет новый документ: запоминает его в истории и перерисовывает холст.
    /// Под новый размер холст подгоняется, если изменились обрезка, размер или подложка.
    fn apply(&self, doc: Doc) {
        let refit = doc.state != self.state();
        self.ivars().history.borrow_mut().push(doc);
        self.refresh(refit);
    }

    fn undo(&self) {
        let before = self.state();
        let moved = self.ivars().history.borrow_mut().undo();
        if moved { self.after_history(before) } else { NSBeep() }
    }

    fn redo(&self) {
        let before = self.state();
        let moved = self.ivars().history.borrow_mut().redo();
        if moved { self.after_history(before) } else { NSBeep() }
    }

    /// После отмены или повтора выделение может указывать на чужую или пропавшую аннотацию.
    fn after_history(&self, before: State) {
        self.ivars().drag.take();
        self.ivars().hidden.set(None);
        self.ivars().selected.set(None);
        self.sync_controls();
        self.sync_backdrop_controls();
        self.refresh(self.state() != before);
    }

    fn uncrop(&self) {
        let (width, height) = self.original_size();
        let doc = self.doc();
        if doc.state.is_cropped(width, height) {
            self.apply(Doc { state: doc.state.uncropped(width, height), ..doc });
        } else {
            NSBeep();
        }
    }

    /// Обрезает по выделению на холсте, переведённому в пиксели результата.
    fn crop_to(&self, selection: NSRect) {
        let doc = self.doc();
        let size = self.canvas().frame().size;
        let layout = self.ivars().layout.get();
        let factor = layout.width / size.width;
        let rect = PixelRect::new(
            selection.origin.x * factor - layout.content.0,
            // У холста начало внизу, у пикселей снимка - вверху.
            (size.height - selection.origin.y - selection.size.height) * factor - layout.content.1,
            selection.size.width * factor,
            selection.size.height * factor,
        );
        self.apply(Doc { state: doc.state.cropped(rect), ..doc });
    }

    /// Впечатывает аннотации в новую основу. Отмена возвращает и аннотации, и прежнюю основу.
    fn rasterize(&self) {
        let doc = self.doc();
        if doc.annotations.is_empty() {
            return NSBeep();
        }
        let Some(image) = document::rasterize(&self.ivars().images.borrow(), &doc, self.ivars().scale) else {
            return eprintln!("kadro: failed to rasterize annotations");
        };
        let base = self.keep(image);
        self.ivars().selected.set(None);
        self.sync_controls();
        self.apply(Doc { annotations: Vec::new(), base, ..doc });
    }

    /// Запоминает картинку в списке картинок редактора и возвращает её номер.
    fn keep(&self, image: CFRetained<CGImage>) -> usize {
        let mut images = self.ivars().images.borrow_mut();
        images.push(image);
        images.len() - 1
    }

    /// Перерисовывает результат и подгоняет под него холст.
    fn refresh(&self, refit: bool) {
        let doc = self.doc();
        let presented = present(&self.ivars().images.borrow(), &doc, self.ivars().hidden.get(), self.ivars().scale);
        let Some((image, layout)) = presented else {
            return eprintln!("kadro: failed to render the edited image");
        };
        let before = self.ivars().layout.replace(layout);
        let resized = (before.width, before.height) != (layout.width, layout.height);
        let size = self.points(layout.width, layout.height);
        let ns_image = NSImage::initWithCGImage_size(NSImage::alloc(), &image, size);
        *self.ivars().rendered.borrow_mut() = image;
        let image_view = self.ivars().image_view.get().expect("built with the window");
        image_view.setImage(Some(&ns_image));
        self.canvas().setFrameSize(size);
        self.window().setSubtitle(&NSString::from_str(&format!("{} × {}", layout.width, layout.height)));
        self.redraw_marks();
        if refit || resized {
            self.fit();
        }
    }

    fn redraw_marks(&self) {
        if let Some(marks) = self.ivars().marks.get() {
            marks.setNeedsDisplay(true);
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

    /// Точка холста в пикселях основы.
    fn to_base(&self, p: NSPoint) -> Point {
        let state = self.state();
        let size = self.canvas().frame().size;
        let layout = self.ivars().layout.get();
        let x = p.x * layout.width / size.width - layout.content.0;
        let y = (size.height - p.y) * layout.height / size.height - layout.content.1;
        state.to_source(x, y)
    }

    /// Пикселей основы на экранную точку при текущем масштабе.
    fn base_per_point(&self) -> f64 {
        let state = self.state();
        let layout = self.ivars().layout.get();
        let points_per_output = self.canvas().frame().size.width / layout.width;
        state.crop.width / (state.width * points_per_output * self.scroll().magnification())
    }

    fn choose(&self, pointer: Pointer) {
        self.canvas().set_cropping(false);
        self.ivars().pointer.set(pointer);
        if let Some(palette) = self.ivars().palette.get() {
            for (button, (entry, ..)) in palette.iter().zip(PALETTE) {
                button.setState(if entry == pointer { NSControlStateValueOn } else { NSControlStateValueOff });
            }
        }
        if pointer != Pointer::Pick {
            self.ivars().hover.set(None);
            NSCursor::arrowCursor().set();
        }
        self.select(None);
    }

    fn select(&self, index: Option<usize>) {
        self.ivars().selected.set(index);
        self.sync_controls();
        self.redraw_marks();
    }

    /// Инструмент, чей стиль показывают и меняют элементы управления: выделенной аннотации или текущий.
    fn styled_tool(&self) -> Option<Tool> {
        match self.ivars().selected.get() {
            Some(index) => self.ivars().history.borrow().current().annotations.get(index).map(|a| a.tool),
            None => match self.ivars().pointer.get() {
                Pointer::Draw(tool) => Some(tool),
                Pointer::Select | Pointer::Pick => None,
            },
        }
    }

    /// Показывает в цвете, размере и стиле выделенную аннотацию или настройки текущего инструмента.
    fn sync_controls(&self) {
        let ivars = self.ivars();
        let selected = ivars.selected.get().and_then(|i| ivars.history.borrow().current().annotations.get(i).cloned());
        let tool = self.styled_tool();
        let (color, size, style) = match &selected {
            Some(a) => (a.color, a.size, a.style),
            None => (ivars.color.get(), ivars.size.get(), tool.map_or(0, |t| ivars.styles.get()[t.index()])),
        };
        if let Some(well) = ivars.color_well.get() {
            well.setColor(&NSColor::colorWithSRGBRed_green_blue_alpha(color.red, color.green, color.blue, color.alpha));
        }
        if let Some(popup) = ivars.size_popup.get() {
            popup.selectItemAtIndex(Size::ALL.iter().position(|s| *s == size).unwrap_or(0) as isize);
        }
        if let Some(popup) = ivars.style_popup.get() {
            let picking = selected.is_none() && ivars.pointer.get() == Pointer::Pick;
            let (styles, style) = if picking {
                (&measure::FORMATS[..], ivars.pick_format.get())
            } else {
                (tool.map_or(&[][..], Tool::styles), style)
            };
            popup.removeAllItems();
            let titles: Vec<_> = styles.iter().map(|s| NSString::from_str(s)).collect();
            popup.addItemsWithTitles(&NSArray::from_retained_slice(&titles));
            popup.setEnabled(!styles.is_empty());
            if !styles.is_empty() {
                popup.selectItemAtIndex(style.min(styles.len() - 1) as isize);
            }
        }
    }

    /// Меняет выделенную аннотацию. Без выделения меняются только настройки для новых.
    fn restyle(&self, change: impl FnOnce(&mut Annotation)) {
        let Some(index) = self.ivars().selected.get() else { return };
        let mut doc = self.doc();
        let Some(annotation) = doc.annotations.get_mut(index) else { return };
        change(annotation);
        annotation.extent = paint::extent(annotation, self.ivars().scale);
        self.apply(doc);
    }

    fn new_annotation(&self, tool: Tool, at: Point) -> Annotation {
        let ivars = self.ivars();
        let mut annotation =
            Annotation::new(tool, at, ivars.color.get(), ivars.size.get(), ivars.styles.get()[tool.index()]);
        annotation.extent = paint::extent(&annotation, ivars.scale);
        annotation
    }

    /// Нажатие на холсте: берёт аннотацию, чтобы двигать, или начинает новую.
    fn press(&self, p: Point, clicks: isize) {
        let doc = self.doc();
        let tolerance = HIT_TOLERANCE * self.base_per_point();
        let hit = annotation::hit_test(&doc.annotations, p, tolerance);
        let text_hit = hit.map(|(i, _)| i).filter(|i| doc.annotations[*i].tool == Tool::Text);
        let pointer = self.ivars().pointer.get();
        if pointer == Pointer::Pick {
            return self.pick(p);
        }
        // Двойной щелчок по тексту правит его любым инструментом, а инструментом текста - и одиночный.
        if let Some(index) = text_hit
            && (clicks >= 2 || pointer == Pointer::Draw(Tool::Text))
        {
            return self.edit_text(index);
        }
        // Инструментом рисования берётся только выделенная аннотация, иначе рядом с другими нельзя было бы рисовать.
        let grab = match pointer {
            Pointer::Draw(_) => {
                self.ivars().selected.get().and_then(|i| doc.annotations[i].hit(p, tolerance).map(|g| (i, g)))
            }
            Pointer::Select | Pointer::Pick => hit,
        };
        if let Some((index, grip)) = grab {
            self.select(Some(index));
            let original = Box::new(doc.annotations[index].clone());
            let current = (*original).clone();
            *self.ivars().drag.borrow_mut() =
                Some(Drag::Move { index, grip, from: p, current, original, moved: false, snap: Snap::default() });
            return;
        }
        self.select(None);
        match pointer {
            Pointer::Select | Pointer::Pick => {}
            Pointer::Draw(Tool::Text) => self.add_text(p),
            Pointer::Draw(tool) => *self.ivars().drag.borrow_mut() = Some(Drag::Draw(self.new_annotation(tool, p))),
        }
        self.redraw_marks();
    }

    fn drag_to(&self, p: Point) {
        let mut started = None;
        let tolerance = HIT_TOLERANCE * self.base_per_point();
        let targets = |except: usize| {
            let doc = self.ivars().history.borrow();
            let doc = doc.current();
            let crop = doc.state.crop;
            let canvas = (Point::new(crop.x, crop.y), Point::new(crop.x + crop.width, crop.y + crop.height));
            let others = doc.annotations.iter().enumerate().filter(|(i, _)| *i != except).map(|(_, a)| a.bounds());
            std::iter::once(canvas).chain(others).collect::<Vec<_>>()
        };
        match self.ivars().drag.borrow_mut().as_mut() {
            None => return,
            Some(Drag::Draw(a)) => match a.tool {
                Tool::Pen | Tool::Highlighter => a.points.push(p),
                Tool::Text | Tool::Counter => a.points[0] = p,
                _ => a.points[1] = p,
            },
            Some(Drag::Move { index, grip, from, original, current, moved, snap }) => {
                *current = match *grip {
                    Grip::Body => {
                        let shifted = original.moved(p.x - from.x, p.y - from.y);
                        // Края и центр прилипают к краям и центрам холста и других аннотаций.
                        *snap = annotation::snap(shifted.bounds(), &targets(*index), tolerance);
                        shifted.moved(snap.dx, snap.dy)
                    }
                    Grip::Point(i) => original.with_point(i, p),
                };
                if !*moved {
                    *moved = true;
                    started = Some(*index);
                }
            }
        }
        // Картинку без переносимой аннотации достаточно нарисовать один раз, дальше её рисует слой поверх.
        if started.is_some() {
            self.ivars().hidden.set(started);
            self.refresh(false);
        }
        self.redraw_marks();
    }

    fn release(&self) {
        let Some(drag) = self.ivars().drag.take() else { return };
        let mut doc = self.doc();
        match drag {
            Drag::Draw(a) if a.tool == Tool::Ruler && a.is_degenerate() => {
                let rulers = self.edge_rulers(&a, &doc);
                if rulers.is_empty() {
                    return self.redraw_marks();
                }
                doc.annotations.extend(rulers);
            }
            Drag::Draw(a) if !a.is_degenerate() => {
                doc.annotations.push(a);
                self.ivars().selected.set(Some(doc.annotations.len() - 1));
            }
            Drag::Move { index, current, moved: true, .. } => {
                doc.annotations[index] = current;
                self.ivars().hidden.set(None);
            }
            _ => return self.redraw_marks(),
        }
        self.apply(doc);
        self.sync_controls();
    }

    /// Щелчок линейкой без протягивания: две линейки, поперёк и вдоль, между краями ровного участка под точкой.
    fn edge_rulers(&self, click: &Annotation, doc: &Doc) -> Vec<Annotation> {
        let base = self.base(doc);
        let crop = doc.state.crop;
        let (left, top) = (crop.x as usize, crop.y as usize);
        let (right, bottom) = (left + crop.width as usize, top + crop.height as usize);
        let p = click.points[0];
        let x = (p.x.floor().max(0.0) as usize).clamp(left, right - 1);
        let y = (p.y.floor().max(0.0) as usize).clamp(top, bottom - 1);
        let ruler = |from: Point, to: Point| Annotation { points: vec![from, to], ..click.clone() };
        let mut rulers = Vec::new();
        if let Some(row) = measure::row(&base, y) {
            let (start, end) = measure::span(&row[left..right], x - left);
            let y = y as f64 + 0.5;
            rulers.push(ruler(Point::new((left + start) as f64, y), Point::new((left + end + 1) as f64, y)));
        }
        if let Some(column) = measure::column(&base, x) {
            let (start, end) = measure::span(&column[top..bottom], y - top);
            let x = x as f64 + 0.5;
            rulers.push(ruler(Point::new(x, (top + start) as f64), Point::new(x, (top + end + 1) as f64)));
        }
        rulers
    }

    /// Пиксель результата под точкой основы.
    fn output_pixel(&self, p: Point) -> (usize, usize) {
        let state = self.state();
        let layout = self.ivars().layout.get();
        let x = (p.x - state.crop.x) * state.width / state.crop.width + layout.content.0;
        let y = (p.y - state.crop.y) * state.height / state.crop.height + layout.content.1;
        ((x.max(0.0) as usize).min(layout.width as usize - 1), (y.max(0.0) as usize).min(layout.height as usize - 1))
    }

    /// Пипетка над холстом: запоминает цвет под мышью для рамки рядом с ней.
    fn hover(&self, p: Option<Point>) {
        if self.ivars().pointer.get() != Pointer::Pick {
            return;
        }
        let sampled = p.and_then(|p| {
            let (x, y) = self.output_pixel(p);
            measure::pixel(&self.rendered(), x, y).map(|color| (p, color))
        });
        if sampled.is_some() {
            NSCursor::crosshairCursor().set();
        }
        self.ivars().hover.set(sampled);
        self.redraw_marks();
    }

    /// Копирует цвет пикселя под точкой в выбранной записи.
    fn pick(&self, p: Point) {
        let (x, y) = self.output_pixel(p);
        let Some(color) = measure::pixel(&self.rendered(), x, y) else { return NSBeep() };
        let text = measure::format(color, self.ivars().pick_format.get());
        let pasteboard = NSPasteboard::generalPasteboard();
        pasteboard.clearContents();
        // SAFETY: системная константа типа буфера обмена живёт весь процесс.
        pasteboard.setString_forType(&NSString::from_str(&text), unsafe { NSPasteboardTypeString });
        if let Some(screen) = self.window().screen() {
            preview::popup_on(self.mtm(), &format!("Copied {text}"), &screen);
        }
    }

    /// Ждёт снимок области и кладёт его на холст. Снимок не копируется и не сохраняется.
    fn add_capture(&self) {
        ADD_TARGET.set(Some(Weak::new(self)));
        output::route_next(output::Route::Editor);
        if !overlay::select(self.mtm(), overlay::Mode::Region) {
            output::cancel_route();
        }
    }

    /// Кладёт снимок в середину видимой части. Плотность пикселей подгоняется под основу,
    /// а слишком большой снимок уменьшается, чтобы поместиться.
    fn place(&self, image: CFRetained<CGImage>, capture_scale: f64) {
        let mut doc = self.doc();
        let crop = doc.state.crop;
        let density = self.ivars().scale / capture_scale;
        let (width, height) =
            (CGImage::width(Some(&image)) as f64 * density, CGImage::height(Some(&image)) as f64 * density);
        let fit = (crop.width * 0.9 / width).min(crop.height * 0.9 / height).min(1.0);
        let (width, height) = (width * fit, height * fit);
        let center = Point::new(crop.x + crop.width / 2.0, crop.y + crop.height / 2.0);
        let mut placed = self.new_annotation(Tool::Image, Point::new(center.x - width / 2.0, center.y - height / 2.0));
        placed.points[1] = Point::new(center.x + width / 2.0, center.y + height / 2.0);
        placed.image = self.keep(image);
        doc.annotations.push(placed);
        self.choose(Pointer::Select);
        self.ivars().selected.set(Some(doc.annotations.len() - 1));
        self.apply(doc);
        self.sync_controls();
        NSApplication::sharedApplication(self.mtm()).activate();
        self.window().makeKeyAndOrderFront(None);
    }

    /// GIF из двух кадров по секунде: с аннотациями и без них.
    fn export_gif(&self) {
        let doc = self.doc();
        let plain = Doc { annotations: Vec::new(), ..doc.clone() };
        let plain = present(&self.ivars().images.borrow(), &plain, None, self.ivars().scale);
        let (Some((plain, _)), false) = (plain, doc.annotations.is_empty()) else { return NSBeep() };
        let panel = NSSavePanel::savePanel(self.mtm());
        panel.setNameFieldStringValue(&NSString::from_str(&format!("{}.gif", self.window().title())));
        panel.setCanCreateDirectories(true);
        if panel.runModal() != NSModalResponseOK {
            return;
        }
        let Some(path) = panel.URL().and_then(|url| url.path()) else { return };
        let gif = output::encode_gif(&[&self.rendered(), &plain], 1.0);
        if let Err(err) = gif
            .ok_or_else(|| std::io::Error::other("encoding failed"))
            .and_then(|bytes| std::fs::write(path.to_string(), bytes))
        {
            let alert = NSAlert::new(self.mtm());
            alert.setMessageText(ns_string!("Couldn't export the GIF"));
            alert.setInformativeText(&NSString::from_str(&err.to_string()));
            alert.runModal();
        }
    }

    /// Показывает настройки подложки под кнопкой панели инструментов.
    fn show_backdrop(&self, item: &NSToolbarItem) {
        let popover = self.ivars().backdrop_popover.get_or_init(|| self.backdrop_popover());
        self.sync_backdrop_controls();
        popover.showRelativeToToolbarItem(item);
    }

    fn backdrop_popover(&self) -> Retained<NSPopover> {
        let mtm = self.mtm();
        let target: &AnyObject = self.as_ref();
        let changed = Some(sel!(backdropChanged:));
        let slider = |max: f64| {
            let slider = unsafe {
                NSSlider::sliderWithValue_minValue_maxValue_target_action(0.0, 0.0, max, Some(target), changed, mtm)
            };
            slider.setContinuous(true);
            slider.widthAnchor().constraintEqualToConstant(180.0).setActive(true);
            slider
        };
        let checkbox = |title: &str| unsafe {
            NSButton::checkboxWithTitle_target_action(&NSString::from_str(title), Some(target), changed, mtm)
        };
        let button = |title: &str, action: Sel| unsafe {
            NSButton::buttonWithTitle_target_action(&NSString::from_str(title), Some(target), Some(action), mtm)
        };
        let kind = popup(mtm, self, 120.0, &backdrop::KINDS, sel!(backdropChanged:));
        let aspects: Vec<_> = backdrop::ASPECTS.iter().map(|(title, _)| *title).collect();
        let aspect = popup(mtm, self, 120.0, &aspects, sel!(backdropChanged:));
        let color =
            NSColorWell::initWithFrame(NSColorWell::alloc(mtm), NSRect::new(NSPoint::ZERO, NSSize::new(38.0, 24.0)));
        color.setColorWellStyle(NSColorWellStyle::Minimal);
        unsafe {
            color.setTarget(Some(target));
            color.setAction(changed);
        }
        let controls = BackdropControls {
            enabled: checkbox("Show backdrop"),
            kind,
            color,
            padding: slider(160.0),
            shadow: slider(1.0),
            radius: slider(40.0),
            outer_radius: slider(40.0),
            inset: slider(80.0),
            aspect,
            auto_crop: checkbox("Trim plain edges"),
        };
        let fill_row = NSStackView::stackViewWithViews(
            &NSArray::from_retained_slice(&[
                view(&controls.kind),
                view(&controls.color),
                view(&button("Randomize", sel!(randomizeBackdrop:))),
                view(&button("Reverse", sel!(reverseBackdrop:))),
            ]),
            mtm,
        );
        let label = |text: &str| view(&NSTextField::labelWithString(&NSString::from_str(text), mtm));
        let empty = || view(&NSView::new(mtm));
        let rows = [
            [empty(), view(&controls.enabled)],
            [label("Background"), view(&fill_row)],
            [label("Padding"), view(&controls.padding)],
            [label("Shadow"), view(&controls.shadow)],
            [label("Corners"), view(&controls.radius)],
            [label("Outer corners"), view(&controls.outer_radius)],
            [label("Inset"), view(&controls.inset)],
            [label("Aspect ratio"), view(&controls.aspect)],
            [empty(), view(&controls.auto_crop)],
        ];
        let rows: Vec<_> = rows.iter().map(|row| NSArray::from_retained_slice(row)).collect();
        let grid = NSGridView::gridViewWithViews(&NSArray::from_retained_slice(&rows), mtm);
        grid.setRowSpacing(10.0);
        grid.setColumnSpacing(10.0);
        grid.setYPlacement(NSGridCellPlacement::Center);
        grid.columnAtIndex(0).setXPlacement(NSGridCellPlacement::Trailing);
        grid.setFrameSize(grid.fittingSize());
        let container = NSView::initWithFrame(
            NSView::alloc(mtm),
            NSRect::new(NSPoint::ZERO, NSSize::new(grid.fittingSize().width + 32.0, grid.fittingSize().height + 32.0)),
        );
        grid.setFrameOrigin(NSPoint::new(16.0, 16.0));
        container.addSubview(&grid);
        let _ = self.ivars().backdrop_controls.set(controls);

        let controller = NSViewController::new(mtm);
        controller.setView(&container);
        let popover = NSPopover::new(mtm);
        popover.setContentViewController(Some(&controller));
        popover.setBehavior(NSPopoverBehavior::Transient);
        popover
    }

    /// Показывает в окне подложки её текущие настройки или последние, если она выключена.
    fn sync_backdrop_controls(&self) {
        let Some(controls) = self.ivars().backdrop_controls.get() else { return };
        let current = self.doc().backdrop;
        let backdrop = current.unwrap_or(self.ivars().last_backdrop.get());
        let on = |flag: bool| if flag { NSControlStateValueOn } else { NSControlStateValueOff };
        controls.enabled.setState(on(current.is_some()));
        controls.kind.selectItemAtIndex(backdrop.fill.kind() as isize);
        if let Fill::Solid(c) = backdrop.fill {
            controls.color.setColor(&NSColor::colorWithSRGBRed_green_blue_alpha(c.red, c.green, c.blue, 1.0));
        }
        controls.color.setEnabled(backdrop.fill.kind() == Fill::Solid(Color::RED).kind());
        controls.padding.setDoubleValue(backdrop.padding);
        controls.shadow.setDoubleValue(backdrop.shadow);
        controls.radius.setDoubleValue(backdrop.radius);
        controls.outer_radius.setDoubleValue(backdrop.outer_radius);
        controls.inset.setDoubleValue(backdrop.inset);
        controls.aspect.selectItemAtIndex(backdrop.aspect as isize);
        controls.auto_crop.setState(on(backdrop.auto_crop));
    }

    /// Собирает подложку из элементов окна. Любая правка, кроме самого флажка, включает подложку.
    fn backdrop_changed(&self, sender: &AnyObject) {
        let Some(controls) = self.ivars().backdrop_controls.get() else { return };
        let toggled = std::ptr::eq(sender, controls.enabled.as_ref() as &AnyObject);
        let previous = self.doc().backdrop.unwrap_or(self.ivars().last_backdrop.get());
        let kind = controls.kind.indexOfSelectedItem().max(0) as usize;
        let white = Color { red: 1.0, green: 1.0, blue: 1.0, alpha: 1.0 };
        let solid = color_of(&controls.color.color()).unwrap_or(white);
        let fill = match previous.fill {
            Fill::Solid(_) if kind == previous.fill.kind() => Fill::Solid(solid),
            fill if kind == fill.kind() => fill,
            _ => Fill::of_kind(kind, solid),
        };
        let backdrop = Backdrop {
            fill,
            padding: controls.padding.doubleValue().round(),
            shadow: (controls.shadow.doubleValue() * 100.0).round() / 100.0,
            radius: controls.radius.doubleValue().round(),
            outer_radius: controls.outer_radius.doubleValue().round(),
            inset: controls.inset.doubleValue().round(),
            aspect: controls.aspect.indexOfSelectedItem().max(0) as usize,
            auto_crop: controls.auto_crop.state() == NSControlStateValueOn,
        };
        let enabled = !toggled || controls.enabled.state() == NSControlStateValueOn;
        self.set_backdrop(enabled.then_some(backdrop));
    }

    fn change_fill(&self, change: impl FnOnce(Fill) -> Fill) {
        let backdrop = self.doc().backdrop.unwrap_or(self.ivars().last_backdrop.get());
        self.set_backdrop(Some(Backdrop { fill: change(backdrop.fill), ..backdrop }));
    }

    /// Ставит подложку. Пока тянут ползунок, правка заменяет предыдущую, а не добавляет шаг отмены.
    fn set_backdrop(&self, backdrop: Option<Backdrop>) {
        if let Some(backdrop) = backdrop {
            self.ivars().last_backdrop.set(backdrop);
        }
        let doc = Doc { backdrop, ..self.doc() };
        let dragging = NSApplication::sharedApplication(self.mtm())
            .currentEvent()
            .is_some_and(|event| event.r#type() == NSEventType::LeftMouseDragged);
        if dragging {
            self.ivars().history.borrow_mut().amend(doc);
            self.refresh(false);
        } else {
            self.apply(doc);
        }
        self.sync_backdrop_controls();
    }

    fn delete_selected(&self) -> bool {
        let Some(index) = self.ivars().selected.get() else { return false };
        let mut doc = self.doc();
        if index < doc.annotations.len() {
            doc.annotations.remove(index);
        }
        self.select(None);
        self.apply(doc);
        true
    }

    fn add_text(&self, at: Point) {
        let Some(text) = self.ask_text("Add Text", "") else { return };
        let mut annotation = self.new_annotation(Tool::Text, at);
        annotation.text = text;
        annotation.extent = paint::extent(&annotation, self.ivars().scale);
        let mut doc = self.doc();
        doc.annotations.push(annotation);
        self.ivars().selected.set(Some(doc.annotations.len() - 1));
        self.apply(doc);
        self.sync_controls();
    }

    /// Правит текст. Пустой текст удаляет аннотацию.
    fn edit_text(&self, index: usize) {
        let mut doc = self.doc();
        let Some(current) = doc.annotations.get(index).map(|a| a.text.clone()) else { return };
        self.select(Some(index));
        let Some(text) = self.ask_text("Edit Text", &current) else { return };
        if text.trim().is_empty() {
            doc.annotations.remove(index);
            self.ivars().selected.set(None);
        } else {
            let annotation = &mut doc.annotations[index];
            annotation.text = text;
            annotation.extent = paint::extent(annotation, self.ivars().scale);
        }
        self.apply(doc);
        self.sync_controls();
    }

    /// Спрашивает текст. None - отмена или пустая строка там, где текста ещё не было.
    fn ask_text(&self, title: &str, initial: &str) -> Option<String> {
        let mtm = self.mtm();
        let field = NSTextField::textFieldWithString(&NSString::from_str(initial), mtm);
        field.setFrameSize(NSSize::new(280.0, 22.0));
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str(title));
        alert.setAccessoryView(Some(&field));
        alert.addButtonWithTitle(ns_string!("OK"));
        alert.addButtonWithTitle(ns_string!("Cancel"));
        alert.window().setInitialFirstResponder(Some(&field));
        if alert.runModal() != NSAlertFirstButtonReturn {
            return None;
        }
        let text = field.stringValue().to_string();
        if text.trim().is_empty() && initial.is_empty() { None } else { Some(text) }
    }

    /// Рисует в слое поверх холста аннотацию под мышью и рамку выделенной.
    /// `size` - размер слоя, его начало вверху слева.
    fn draw_marks(&self, size: NSSize) {
        let history = self.ivars().history.borrow();
        let doc = history.current();
        let (crop, state) = (doc.state.crop, doc.state);
        let layout = self.ivars().layout.get();
        // Точки слоя, пиксели холста с подложкой, пиксели снимка и пиксели основы.
        let transform = NSAffineTransform::transform();
        transform.scaleXBy_yBy(size.width / layout.width, size.height / layout.height);
        transform.translateXBy_yBy(layout.content.0, layout.content.1);
        transform.scaleXBy_yBy(state.width / crop.width, state.height / crop.height);
        transform.translateXBy_yBy(-crop.x, -crop.y);
        NSGraphicsContext::saveGraphicsState_class();
        transform.concat();

        let scale = self.ivars().scale;
        let images = self.ivars().images.borrow();
        let drag = self.ivars().drag.borrow();
        let marked = match drag.as_ref() {
            Some(Drag::Draw(a)) => {
                let number = doc.annotations.iter().filter(|a| a.tool == Tool::Counter).count() + 1;
                paint::draw_one(a, number, None, &images, scale, Mode::Live);
                None
            }
            Some(Drag::Move { index, current, moved, snap, .. }) => {
                if *moved {
                    let base = self.base(doc);
                    let number = paint::number(&doc.annotations, *index);
                    paint::draw_one(current, number, Some(&base), &images, scale, Mode::Final);
                    guides(*snap, crop, self.base_per_point());
                }
                Some(current)
            }
            None => self.ivars().selected.get().and_then(|i| doc.annotations.get(i)),
        };
        if let Some(annotation) = marked {
            handles(annotation, self.base_per_point(), scale);
        }
        if let Some((p, color)) = self.ivars().hover.get() {
            swatch(p, color, self.ivars().pick_format.get(), self.base_per_point());
        }
        NSGraphicsContext::restoreGraphicsState_class();
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
        let doc = self.doc();
        let state = doc.state;
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
                Some(value) => self.apply(Doc { state: state.resized(value), ..doc }),
                None => NSBeep(),
            }
        }
    }

    /// Esc: снимает выделение, а без него копирует, сохраняет или просто закрывает - как выбрано в настройках.
    fn escape(&self) {
        if self.ivars().selected.get().is_some() {
            return self.select(None);
        }
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

/// Рамка выделенной аннотации и ручки на её точках. Размеры - в экранных точках при любом масштабе.
fn handles(annotation: &Annotation, base_per_point: f64, scale: f64) {
    let (min, max) = annotation.bounds();
    let pad = 4.0 * base_per_point
        + if matches!(annotation.tool, Tool::Pen | Tool::Highlighter) {
            annotation.size.stroke() * scale * 2.0
        } else {
            0.0
        };
    let frame = NSRect::new(
        NSPoint::new(min.x - pad, min.y - pad),
        NSSize::new(max.x - min.x + 2.0 * pad, max.y - min.y + 2.0 * pad),
    );
    if !annotation.tool.is_two_point() || !matches!(annotation.tool, Tool::Arrow | Tool::Line) {
        let border = NSBezierPath::bezierPathWithRect(frame);
        border.setLineWidth(base_per_point);
        let dash = [4.0 * base_per_point, 3.0 * base_per_point];
        // SAFETY: массив из двух длин живёт до конца вызова.
        unsafe { border.setLineDash_count_phase(dash.as_ptr(), 2, 0.0) };
        NSColor::controlAccentColor().setStroke();
        border.stroke();
    }
    if annotation.tool.is_two_point() {
        let radius = 4.5 * base_per_point;
        for p in &annotation.points {
            let knob = NSBezierPath::bezierPathWithOvalInRect(NSRect::new(
                NSPoint::new(p.x - radius, p.y - radius),
                NSSize::new(2.0 * radius, 2.0 * radius),
            ));
            knob.setLineWidth(1.5 * base_per_point);
            NSColor::whiteColor().setFill();
            knob.fill();
            NSColor::controlAccentColor().setStroke();
            knob.stroke();
        }
    }
}

/// Линии, к которым прилипла переносимая аннотация, через всю видимую часть.
fn guides(snap: Snap, crop: PixelRect, base_per_point: f64) {
    let path = NSBezierPath::bezierPath();
    if let Some(x) = snap.x {
        path.moveToPoint(NSPoint::new(x, crop.y));
        path.lineToPoint(NSPoint::new(x, crop.y + crop.height));
    }
    if let Some(y) = snap.y {
        path.moveToPoint(NSPoint::new(crop.x, y));
        path.lineToPoint(NSPoint::new(crop.x + crop.width, y));
    }
    path.setLineWidth(base_per_point);
    NSColor::systemPinkColor().setStroke();
    path.stroke();
}

/// Квадрат с цветом под пипеткой и его запись рядом с мышью. Размеры в экранных точках.
fn swatch(p: Point, color: [u8; 3], format: usize, base_per_point: f64) {
    let [red, green, blue] = color.map(|c| f64::from(c) / 255.0);
    let side = 28.0 * base_per_point;
    let at = NSPoint::new(p.x + 16.0 * base_per_point, p.y + 16.0 * base_per_point);
    let square = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
        NSRect::new(at, NSSize::new(side, side)),
        4.0 * base_per_point,
        4.0 * base_per_point,
    );
    NSColor::colorWithSRGBRed_green_blue_alpha(red, green, blue, 1.0).setFill();
    square.fill();
    square.setLineWidth(2.0 * base_per_point);
    NSColor::whiteColor().setStroke();
    square.stroke();
    let font = objc2_app_kit::NSFont::monospacedSystemFontOfSize_weight(12.0 * base_per_point, 0.0);
    let white = NSColor::whiteColor();
    // SAFETY: ключи - системные константы атрибутов текста, они живут весь процесс.
    let keys = unsafe { [objc2_app_kit::NSFontAttributeName, objc2_app_kit::NSForegroundColorAttributeName] };
    let attributes = objc2_foundation::NSDictionary::<NSString, AnyObject>::from_slices(
        &keys,
        &[&*font as &AnyObject, &*white as &AnyObject],
    );
    let label = NSString::from_str(&measure::format(color, format));
    let size = unsafe { objc2_app_kit::NSStringDrawing::sizeWithAttributes(&*label, Some(&attributes)) };
    let pad = 4.0 * base_per_point;
    let plate = NSRect::new(
        NSPoint::new(at.x + side + 6.0 * base_per_point, at.y + (side - size.height) / 2.0 - pad / 2.0),
        NSSize::new(size.width + 2.0 * pad, size.height + pad),
    );
    NSColor::colorWithWhite_alpha(0.1, 0.85).setFill();
    NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(plate, pad, pad).fill();
    let text_at = NSPoint::new(plate.origin.x + pad, plate.origin.y + pad / 2.0);
    unsafe { objc2_app_kit::NSStringDrawing::drawAtPoint_withAttributes(&*label, text_at, Some(&attributes)) };
}

fn view(view: &NSView) -> Retained<NSView> {
    view.retain()
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
            } else if let Some(editor) = self.ivars().editor.load() {
                editor.press(editor.to_base(self.point(event)), event.clickCount());
            }
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            if let Some(editor) = self.ivars().editor.load() {
                editor.hover(Some(editor.to_base(self.point(event))));
            }
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            if let Some(editor) = self.ivars().editor.load() {
                editor.hover(None);
            }
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let Some(start) = self.ivars().start.get() else {
                if let Some(editor) = self.ivars().editor.load() {
                    editor.drag_to(editor.to_base(self.point(event)));
                }
                return;
            };
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
            if self.ivars().start.take().is_none()
                && let Some(editor) = self.ivars().editor.load()
            {
                editor.release();
            }
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let Some(editor) = self.ivars().editor.load() else { return };
            let plain = !event.modifierFlags().intersects(
                NSEventModifierFlags::Command | NSEventModifierFlags::Control | NSEventModifierFlags::Option,
            );
            let key = event.keyCode();
            match key {
                keys::ESCAPE if self.ivars().cropping.get() => self.set_cropping(false),
                keys::ESCAPE => editor.escape(),
                keys::RETURN | keys::ENTER if self.ivars().cropping.get() => {
                    if let Some(selection) = self.ivars().selection.get().filter(|s| s.size.width >= 1.0 && s.size.height >= 1.0) {
                        self.set_cropping(false);
                        editor.crop_to(selection);
                    }
                }
                keys::DELETE | keys::FORWARD_DELETE if editor.delete_selected() => {}
                keys::C if plain => self.set_cropping(!self.ivars().cropping.get()),
                _ if plain && PALETTE.iter().any(|(.., k)| *k == key) => {
                    if let Some((pointer, ..)) = PALETTE.iter().find(|(.., k)| *k == key) {
                        editor.choose(*pointer);
                    }
                }
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

struct MarksIvars {
    editor: Weak<Editor>,
}

define_class!(
    /// Аннотация под мышью и рамка выделенной поверх картинки. Мышь проходит сквозь него к холсту.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = MarksIvars]
    struct Marks;

    impl Marks {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            if let Some(editor) = self.ivars().editor.load() {
                editor.draw_marks(self.bounds().size);
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

/// Колонка инструментов слева от холста.
fn palette(mtm: MainThreadMarker, editor: &Editor, height: f64) -> (Retained<NSStackView>, Vec<Retained<NSButton>>) {
    let buttons: Vec<_> = PALETTE
        .iter()
        .enumerate()
        .filter_map(|(index, (pointer, tip, icon, _))| {
            let image = assets::icon(icon, 16.0)?;
            let button =
                unsafe { NSButton::buttonWithImage_target_action(&image, Some(editor), Some(sel!(chooseTool:)), mtm) };
            button.setButtonType(NSButtonType::PushOnPushOff);
            // У Toolbar высота рамки постоянная и меньше иконки. FlexiblePush тянет рамку по размеру кнопки.
            button.setBezelStyle(NSBezelStyle::FlexiblePush);
            button.widthAnchor().constraintEqualToConstant(PALETTE_BUTTON).setActive(true);
            button.heightAnchor().constraintEqualToConstant(PALETTE_BUTTON).setActive(true);
            button.setTag(index as isize);
            button.setToolTip(Some(&NSString::from_str(tip)));
            button.setState(if *pointer == Pointer::Select { NSControlStateValueOn } else { NSControlStateValueOff });
            Some(button)
        })
        .collect();
    let stack = NSStackView::initWithFrame(
        NSStackView::alloc(mtm),
        NSRect::new(NSPoint::ZERO, NSSize::new(PALETTE_WIDTH, height)),
    );
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    stack.setSpacing(4.0);
    stack.setEdgeInsets(NSEdgeInsets { top: 8.0, left: 0.0, bottom: 8.0, right: 0.0 });
    for button in &buttons {
        stack.addView_inGravity(button, NSStackViewGravity::Top);
    }
    stack.setAutoresizingMask(NSAutoresizingMaskOptions::ViewHeightSizable);
    (stack, buttons)
}

fn popup(mtm: MainThreadMarker, editor: &Editor, width: f64, titles: &[&str], action: Sel) -> Retained<NSPopUpButton> {
    let popup = NSPopUpButton::initWithFrame_pullsDown(
        NSPopUpButton::alloc(mtm),
        NSRect::new(NSPoint::ZERO, NSSize::new(width, 24.0)),
        false,
    );
    let titles: Vec<_> = titles.iter().map(|t| NSString::from_str(t)).collect();
    popup.addItemsWithTitles(&NSArray::from_retained_slice(&titles));
    unsafe {
        popup.setTarget(Some(editor));
        popup.setAction(Some(action));
    }
    popup
}

/// Открывает снимок в редакторе. Без `shot` - картинка из файла или буфера.
pub fn open(mtm: MainThreadMarker, image: CFRetained<CGImage>, scale: f64, shot: Option<Shot>, title: &str) {
    let (width, height) = (CGImage::width(Some(&image)) as f64, CGImage::height(Some(&image)) as f64);
    let ivars = EditorIvars {
        rendered: RefCell::new(image.clone()),
        images: RefCell::new(vec![image]),
        scale,
        shot,
        history: RefCell::new(History::new(Doc {
            state: State::original(width, height),
            annotations: Vec::new(),
            base: 0,
            backdrop: None,
        })),
        pointer: Cell::new(Pointer::Select),
        color: Cell::new(Color::RED),
        size: Cell::new(Size::Medium),
        styles: Cell::new([0; Tool::ALL.len()]),
        pick_format: Cell::new(0),
        hover: Cell::new(None),
        layout: Cell::new(Layout { width, height, card: (0.0, 0.0, width, height), content: (0.0, 0.0) }),
        last_backdrop: Cell::new(Backdrop::default()),
        backdrop_popover: OnceCell::new(),
        backdrop_controls: OnceCell::new(),
        selected: Cell::new(None),
        drag: RefCell::new(None),
        hidden: Cell::new(None),
        window: OnceCell::new(),
        scroll: OnceCell::new(),
        canvas: OnceCell::new(),
        image_view: OnceCell::new(),
        marks: OnceCell::new(),
        palette: OnceCell::new(),
        color_well: OnceCell::new(),
        size_popup: OnceCell::new(),
        style_popup: OnceCell::new(),
        size_fields: RefCell::new(None),
    };
    let editor: Retained<Editor> = unsafe { msg_send![super(Editor::alloc(mtm).set_ivars(ivars)), init] };
    let settings = Settings::load();

    let screen = NSScreen::mainScreen(mtm);
    let backing = screen.as_ref().map_or(2.0, |s| s.backingScaleFactor());
    let visible = screen.as_ref().map_or(NSRect::new(NSPoint::ZERO, NSSize::new(1440.0, 900.0)), |s| s.visibleFrame());
    let natural = NSSize::new(width / backing, height / backing);
    let content = NSSize::new(
        (natural.width + PALETTE_WIDTH).min(visible.size.width * MAX_SCREEN_SHARE).max(MIN_WINDOW.width),
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

    // Элементы стиля нужны делегату панели инструментов, поэтому создаются до неё.
    let color_well =
        NSColorWell::initWithFrame(NSColorWell::alloc(mtm), NSRect::new(NSPoint::ZERO, NSSize::new(38.0, 24.0)));
    color_well.setColorWellStyle(NSColorWellStyle::Minimal);
    color_well.widthAnchor().constraintEqualToConstant(38.0).setActive(true);
    color_well.heightAnchor().constraintEqualToConstant(24.0).setActive(true);
    unsafe {
        color_well.setTarget(Some(&*editor));
        color_well.setAction(Some(sel!(colorChanged:)));
    }
    let _ = editor.ivars().color_well.set(color_well);
    let sizes: Vec<_> = Size::ALL.iter().map(|s| s.title()).collect();
    let _ = editor.ivars().size_popup.set(popup(mtm, &editor, 64.0, &sizes, sel!(sizeChanged:)));
    let _ = editor.ivars().style_popup.set(popup(mtm, &editor, 140.0, &[], sel!(styleChanged:)));
    editor.sync_controls();

    let toolbar = NSToolbar::initWithIdentifier(NSToolbar::alloc(mtm), ns_string!("KadroEditor"));
    toolbar.setDisplayMode(NSToolbarDisplayMode::IconOnly);
    toolbar.setDelegate(Some(ProtocolObject::from_ref(&*editor)));
    window.setToolbar(Some(&toolbar));
    window.setToolbarStyle(NSWindowToolbarStyle::Unified);

    let root = NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::ZERO, content));
    let (palette, palette_buttons) = palette(mtm, &editor, content.height);
    root.addSubview(&palette);
    let _ = editor.ivars().palette.set(palette_buttons);

    let scroll_frame =
        NSRect::new(NSPoint::new(PALETTE_WIDTH, 0.0), NSSize::new(content.width - PALETTE_WIDTH, content.height));
    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), scroll_frame);
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
    let tracking = unsafe {
        NSTrackingArea::initWithRect_options_owner_userInfo(
            NSTrackingArea::alloc(),
            NSRect::ZERO,
            NSTrackingAreaOptions::MouseMoved
                | NSTrackingAreaOptions::MouseEnteredAndExited
                | NSTrackingAreaOptions::ActiveInKeyWindow
                | NSTrackingAreaOptions::InVisibleRect,
            Some(&canvas),
            None,
        )
    };
    canvas.addTrackingArea(&tracking);
    let sizable = NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable;
    let image_view = NSImageView::initWithFrame(NSImageView::alloc(mtm), NSRect::new(NSPoint::ZERO, natural));
    image_view.setImageScaling(NSImageScaling::ScaleAxesIndependently);
    image_view.setAutoresizingMask(sizable);
    canvas.addSubview(&image_view);
    let marks: Retained<Marks> = unsafe {
        msg_send![
            super(Marks::alloc(mtm).set_ivars(MarksIvars { editor: Weak::from_retained(&editor) })),
            initWithFrame: NSRect::new(NSPoint::ZERO, natural)
        ]
    };
    marks.setAutoresizingMask(sizable);
    canvas.addSubview(&marks);
    let overlay: Retained<CropOverlay> = unsafe {
        msg_send![
            super(CropOverlay::alloc(mtm).set_ivars(OverlayIvars { canvas: Weak::from_retained(&canvas) })),
            initWithFrame: NSRect::new(NSPoint::ZERO, natural)
        ]
    };
    overlay.setAutoresizingMask(sizable);
    canvas.addSubview(&overlay);
    let _ = canvas.ivars().overlay.set(overlay);
    scroll.setDocumentView(Some(&canvas));
    root.addSubview(&scroll);
    window.setContentView(Some(&root));
    window.makeFirstResponder(Some(&canvas));

    let _ = editor.ivars().window.set(window.clone());
    let _ = editor.ivars().scroll.set(scroll);
    let _ = editor.ivars().canvas.set(canvas);
    let _ = editor.ivars().image_view.set(image_view);
    let _ = editor.ivars().marks.set(marks);
    editor.refresh(false);
    window.center();
    NSApplication::sharedApplication(mtm).activate();
    window.makeKeyAndOrderFront(None);
    editor.initial_zoom();
    EDITORS.with_borrow_mut(|editors| editors.push(editor));
}

/// Кладёт снимок на холст редактора, который его ждал. Если редактор закрыли, снимок пропадает.
pub fn place_capture(image: CFRetained<CGImage>, scale: f64) {
    if let Some(editor) = ADD_TARGET.take().and_then(|weak| weak.load()) {
        editor.place(image, scale);
    }
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
