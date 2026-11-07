//! Окно библиотеки: поиск и снимки плитками трёх размеров или списком.
//!
//! Сетка рисует только видимые ячейки, а миниатюры готовятся в фоне и кешируются на диске,
//! поэтому тысячи снимков открываются сразу. Закрытое окно ничего не держит в памяти.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSAutoresizingMaskOptions, NSBackingStoreType, NSBezierPath,
    NSColor, NSCompositingOperation, NSEvent, NSFont, NSFontAttributeName, NSForegroundColorAttributeName, NSImage,
    NSLineBreakMode, NSMenu, NSMenuItem, NSMutableParagraphStyle, NSParagraphStyleAttributeName, NSResponder,
    NSScrollView, NSSearchField, NSSegmentSwitchTracking, NSSegmentedControl, NSStringDrawingOptions,
    NSStringNSExtendedStringDrawing, NSTextField, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask, NSWorkspace,
};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_graphics::CGImage;
use objc2_foundation::{
    NSArray, NSDate, NSDateFormatter, NSDictionary, NSFileManager, NSNotification, NSObject, NSObjectProtocol, NSPoint,
    NSRect, NSSize, NSString, NSTimer, NSURL, ns_string,
};
use objc2_image_io::{
    CGImageSource, kCGImageSourceCreateThumbnailFromImageAlways, kCGImageSourceCreateThumbnailWithTransform,
    kCGImageSourceThumbnailMaxPixelSize,
};

use crate::library::{self, Entry};
use crate::settings::{self, Settings};
use crate::{editor, keys, output, sigv4, upload};

/// Миниатюры не больше этого числа пикселей по длинной стороне.
const THUMBNAIL_PIXELS: f64 = 520.0;
/// Сколько миниатюр держать в памяти. Дальние от экрана вытесняются первыми загруженными.
const CACHED_THUMBNAILS: usize = 240;
const SPACING: f64 = 12.0;
const CAPTION: f64 = 34.0;
const LIST_ROW: f64 = 52.0;
const BAR_HEIGHT: f64 = 44.0;

unsafe extern "C" {
    fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
}

thread_local! {
    static BROWSER: RefCell<Option<Retained<Browser>>> = const { RefCell::new(None) };
    // Закрытое окно живёт до конца обработчика закрытия, освобождать его посреди него нельзя.
    static RETIRED: RefCell<Option<Retained<Browser>>> = const { RefCell::new(None) };
}

/// Вид снимков.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Small,
    Medium,
    Large,
    List,
}

impl View {
    const ALL: [View; 4] = [View::Small, View::Medium, View::Large, View::List];

    fn tile(self) -> f64 {
        match self {
            View::Small => 120.0,
            View::Medium => 180.0,
            View::Large => 260.0,
            View::List => 0.0,
        }
    }
}

/// Сколько колонок и какого размера ячейки при ширине `width`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Grid {
    pub columns: usize,
    pub cell: NSSize,
}

pub fn grid(view: View, width: f64) -> Grid {
    if view == View::List {
        return Grid { columns: 1, cell: NSSize::new(width, LIST_ROW) };
    }
    let tile = view.tile();
    let columns = (((width - SPACING) / (tile + SPACING)).floor() as usize).max(1);
    // Лишняя ширина делится между колонками, чтобы ряд доходил до правого края.
    let cell_width = (width - SPACING) / columns as f64 - SPACING;
    Grid { columns, cell: NSSize::new(cell_width, cell_width * 0.66 + CAPTION) }
}

impl Grid {
    /// Высота сетки из `count` снимков.
    pub fn height(self, count: usize) -> f64 {
        let rows = count.div_ceil(self.columns);
        rows as f64 * (self.cell.height + SPACING) + SPACING
    }

    /// Ячейка снимка `index`, начало вверху слева.
    pub fn rect(self, index: usize) -> NSRect {
        let (row, column) = (index / self.columns, index % self.columns);
        NSRect::new(
            NSPoint::new(
                SPACING + column as f64 * (self.cell.width + SPACING),
                SPACING + row as f64 * (self.cell.height + SPACING),
            ),
            self.cell,
        )
    }

    /// Снимок под точкой, если она попала в ячейку, а не в зазор.
    pub fn index_at(self, point: NSPoint, count: usize) -> Option<usize> {
        let column = ((point.x - SPACING) / (self.cell.width + SPACING)).floor();
        let row = ((point.y - SPACING) / (self.cell.height + SPACING)).floor();
        if column < 0.0 || row < 0.0 || column as usize >= self.columns {
            return None;
        }
        let index = row as usize * self.columns + column as usize;
        let rect = self.rect(index);
        let inside = point.x <= rect.origin.x + rect.size.width && point.y <= rect.origin.y + rect.size.height;
        (inside && index < count).then_some(index)
    }

    /// Снимки, чьи ячейки пересекают полосу по высоте.
    pub fn visible(self, top: f64, bottom: f64, count: usize) -> std::ops::Range<usize> {
        let row_height = self.cell.height + SPACING;
        let first = ((top - SPACING) / row_height).floor().max(0.0) as usize;
        let last = ((bottom - SPACING) / row_height).ceil().max(0.0) as usize;
        (first * self.columns).min(count)..((last + 1) * self.columns).min(count)
    }
}

/// Снимок в окне: запись индекса и её дата для показа и поиска.
struct Item {
    entry: Entry,
    date: String,
    time: String,
}

struct BrowserIvars {
    items: RefCell<Vec<Item>>,
    /// Номера показанных снимков в `items` после поиска.
    shown: RefCell<Vec<usize>>,
    view: Cell<View>,
    selected: Cell<Option<usize>>,
    thumbnails: RefCell<HashMap<PathBuf, Retained<NSImage>>>,
    /// Порядок загрузки миниатюр, чтобы вытеснять самые давние.
    loaded: RefCell<VecDeque<PathBuf>>,
    loading: RefCell<HashSet<PathBuf>>,
    window: OnceCell<Retained<NSWindow>>,
    scroll: OnceCell<Retained<NSScrollView>>,
    tiles: OnceCell<Retained<Tiles>>,
    search: OnceCell<Retained<NSSearchField>>,
    count: OnceCell<Retained<NSTextField>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = BrowserIvars]
    struct Browser;

    impl Browser {
        #[unsafe(method(searchChanged:))]
        fn search_changed(&self, _sender: Option<&AnyObject>) {
            self.reload();
        }

        #[unsafe(method(viewChanged:))]
        fn view_changed(&self, sender: &NSSegmentedControl) {
            let view = View::ALL.get(sender.selectedSegment().max(0) as usize).copied().unwrap_or(View::Medium);
            self.ivars().view.set(view);
            settings::set_string(VIEW_KEY, &sender.selectedSegment().to_string());
            self.relayout();
        }

        #[unsafe(method(openSelected:))]
        fn open_selected(&self, _sender: Option<&AnyObject>) {
            if let Some(path) = self.selected_path() {
                editor::open_file(self.mtm(), &path);
            }
        }

        #[unsafe(method(copySelected:))]
        fn copy_selected(&self, _sender: Option<&AnyObject>) {
            if let Some((image, scale)) = self.selected_image() {
                output::copy_in_background(image, scale);
            }
        }

        #[unsafe(method(revealSelected:))]
        fn reveal_selected(&self, _sender: Option<&AnyObject>) {
            if let Some(path) = self.selected_path() {
                let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
                NSWorkspace::sharedWorkspace().activateFileViewerSelectingURLs(&NSArray::from_retained_slice(&[url]));
            }
        }

        #[unsafe(method(uploadSelected:))]
        fn upload_selected(&self, _sender: Option<&AnyObject>) {
            if let Some((image, scale)) = self.selected_image() {
                upload::upload(self.mtm(), image, scale, None);
            }
        }

        #[unsafe(method(trashSelected:))]
        fn trash_selected(&self, _sender: Option<&AnyObject>) {
            self.trash();
        }
    }

    unsafe impl NSObjectProtocol for Browser {}

    unsafe impl NSWindowDelegate for Browser {
        #[unsafe(method(windowDidResize:))]
        fn window_did_resize(&self, _notification: &NSNotification) {
            self.relayout();
        }

        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            if let Some(browser) = BROWSER.take() {
                RETIRED.set(Some(browser));
                DispatchQueue::main().exec_async(|| drop(RETIRED.take()));
                // Миниатюры освобождаются, но malloc держит их память про запас. Через секунду
                // Core Animation отпускает свои копии, и свободное возвращается системе.
                let relieve = RcBlock::new(|_timer: NonNull<NSTimer>| {
                    // SAFETY: null означает все зоны malloc, функция возвращает системе только свободное.
                    unsafe { malloc_zone_pressure_relief(std::ptr::null_mut(), 0) };
                });
                unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(1.0, false, &relieve) };
            }
        }
    }
);

/// Ключ настроек для вида снимков.
const VIEW_KEY: &str = "libraryView";

impl Browser {
    fn tiles(&self) -> &Tiles {
        self.ivars().tiles.get().expect("built with the window")
    }

    /// Перечитывает индекс и применяет поиск.
    fn reload(&self) {
        let date_formatter = NSDateFormatter::new();
        date_formatter.setDateFormat(Some(ns_string!("yyyy-MM-dd")));
        let time_formatter = NSDateFormatter::new();
        time_formatter.setDateFormat(Some(ns_string!("HH:mm")));
        let items: Vec<Item> = library::entries()
            .into_iter()
            .map(|entry| {
                let date = NSDate::dateWithTimeIntervalSince1970(entry.modified as f64);
                Item {
                    date: date_formatter.stringFromDate(&date).to_string(),
                    time: time_formatter.stringFromDate(&date).to_string(),
                    entry,
                }
            })
            .collect();
        let query = self.ivars().search.get().map(|s| s.stringValue().to_string()).unwrap_or_default();
        let shown: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.entry.matches(&query, &item.date))
            .map(|(i, _)| i)
            .collect();
        if let Some(count) = self.ivars().count.get() {
            let text = match (shown.len(), items.len()) {
                (shown, total) if shown == total => format!("{total} screenshots"),
                (shown, total) => format!("{shown} of {total}"),
            };
            count.setStringValue(&NSString::from_str(&text));
        }
        *self.ivars().items.borrow_mut() = items;
        *self.ivars().shown.borrow_mut() = shown;
        self.ivars().selected.set(None);
        self.relayout();
    }

    /// Подгоняет высоту сетки под ширину окна и число снимков.
    fn relayout(&self) {
        let Some(scroll) = self.ivars().scroll.get() else { return };
        let width = scroll.contentSize().width;
        let layout = grid(self.ivars().view.get(), width);
        let height = layout.height(self.ivars().shown.borrow().len()).max(scroll.contentSize().height);
        self.tiles().setFrameSize(NSSize::new(width, height));
        self.tiles().setNeedsDisplay(true);
    }

    fn selected_path(&self) -> Option<PathBuf> {
        let index = self.ivars().selected.get()?;
        let item = *self.ivars().shown.borrow().get(index)?;
        self.ivars().items.borrow().get(item).map(|i| i.entry.path.clone())
    }

    fn selected_image(&self) -> Option<(CFRetained<CGImage>, f64)> {
        std::fs::read(self.selected_path()?).ok().and_then(|bytes| output::decode(&bytes))
    }

    /// Переносит выбранный снимок в Корзину. Оттуда его можно вернуть, поэтому без вопроса.
    fn trash(&self) {
        let Some(path) = self.selected_path() else { return };
        let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        match NSFileManager::defaultManager().trashItemAtURL_resultingItemURL_error(&url, None) {
            Ok(()) => {
                library::forget(&path);
                self.reload();
            }
            Err(err) => {
                let alert = NSAlert::new(self.mtm());
                alert.setMessageText(ns_string!("Couldn't move the screenshot to the Trash"));
                alert.setInformativeText(&err.localizedDescription());
                alert.runModal();
            }
        }
    }

    /// Миниатюра из кеша в памяти. Если её нет, начинает готовить в фоне и возвращает None.
    fn thumbnail(&self, path: &Path, modified: u64) -> Option<Retained<NSImage>> {
        if let Some(image) = self.ivars().thumbnails.borrow().get(path) {
            return Some(image.clone());
        }
        if !self.ivars().loading.borrow_mut().insert(path.to_path_buf()) {
            return None;
        }
        let path = path.to_path_buf();
        let weak = Weak::from_retained(&BROWSER.with_borrow(Clone::clone)?);
        let weak = Sendable(weak);
        output::in_background(move || {
            let image = cached_thumbnail(&path, modified).map(Sendable);
            DispatchQueue::main().exec_async(move || {
                let Some(browser) = weak.into_inner().load() else { return };
                browser.ivars().loading.borrow_mut().remove(&path);
                let Some(image) = image else { return };
                let image = image.into_inner();
                let size = NSSize::new(CGImage::width(Some(&image)) as f64, CGImage::height(Some(&image)) as f64);
                let ns_image = NSImage::initWithCGImage_size(NSImage::alloc(), &image, size);
                let mut loaded = browser.ivars().loaded.borrow_mut();
                let mut thumbnails = browser.ivars().thumbnails.borrow_mut();
                while loaded.len() >= CACHED_THUMBNAILS {
                    if let Some(oldest) = loaded.pop_front() {
                        thumbnails.remove(&oldest);
                    }
                }
                loaded.push_back(path.clone());
                thumbnails.insert(path, ns_image);
                drop((loaded, thumbnails));
                browser.tiles().setNeedsDisplay(true);
            });
        });
        None
    }
}

/// Обёртка, чтобы передать объект на главный поток, где с ним и работают.
struct Sendable<T>(T);
// SAFETY: значение только перевозится между потоками, а используется на главном или в фоне без общего доступа.
unsafe impl<T> Send for Sendable<T> {}

impl<T> Sendable<T> {
    /// Метод, а не разбор поля: замыкание забирает обёртку целиком, а не её непересылаемое поле.
    fn into_inner(self) -> T {
        self.0
    }
}

fn thumbnails_directory() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join("Library/Caches/Kadro/Thumbnails")
}

/// Миниатюра с диска или новая, которая тут же кладётся в кеш. Имя кеша зависит от пути
/// и времени изменения, так что изменённый файл получает новую миниатюру.
fn cached_thumbnail(path: &Path, modified: u64) -> Option<CFRetained<CGImage>> {
    let key = sigv4::hex(&sigv4::sha256(format!("{}|{modified}", path.display()).as_bytes()));
    let cache = thumbnails_directory().join(format!("{}.jpg", &key[..24]));
    if let Some((image, _)) = std::fs::read(&cache).ok().and_then(|bytes| output::decode(&bytes)) {
        return Some(image);
    }
    let bytes = std::fs::read(path).ok()?;
    let source = unsafe { CGImageSource::with_data(&objc2_core_foundation::CFData::from_bytes(&bytes), None) }?;
    let size = CFNumber::new_f64(THUMBNAIL_PIXELS);
    let yes = CFBoolean::new(true);
    // SAFETY: ключи - константы ImageIO, они живут весь процесс.
    let keys = unsafe {
        [
            kCGImageSourceCreateThumbnailFromImageAlways,
            kCGImageSourceCreateThumbnailWithTransform,
            kCGImageSourceThumbnailMaxPixelSize,
        ]
    };
    let values: [&CFType; 3] = [yes.as_ref(), yes.as_ref(), size.as_ref()];
    let options = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);
    let thumbnail = unsafe { source.thumbnail_at_index(0, Some(options.as_opaque())) }?;
    let written = std::fs::create_dir_all(thumbnails_directory())
        .and_then(|()| output::write_to(&cache.with_extension("jpg"), &thumbnail, 1.0));
    if let Err(err) = written {
        eprintln!("kadro: failed to cache a thumbnail: {err}");
    }
    Some(thumbnail)
}

struct TilesIvars {
    browser: Weak<Browser>,
}

define_class!(
    /// Сетка снимков. Рисует только видимые ячейки.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = TilesIvars]
    struct Tiles;

    impl Tiles {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, dirty: NSRect) {
            if let Some(browser) = self.ivars().browser.load() {
                draw(&browser, self.bounds().size.width, dirty);
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let Some(browser) = self.ivars().browser.load() else { return };
            self.select_at(&browser, event);
            if event.clickCount() >= 2 {
                unsafe { msg_send![&*browser, openSelected: std::ptr::null::<AnyObject>()] }
            }
        }

        #[unsafe(method_id(menuForEvent:))]
        fn menu_for_event(&self, event: &NSEvent) -> Option<Retained<NSMenu>> {
            self.menu_at(event)
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let Some(browser) = self.ivars().browser.load() else { return };
            let command = event.modifierFlags().contains(objc2_app_kit::NSEventModifierFlags::Command);
            match event.keyCode() {
                keys::RETURN | keys::ENTER => unsafe { msg_send![&*browser, openSelected: std::ptr::null::<AnyObject>()] },
                keys::DELETE if command => browser.trash(),
                keys::C if command => unsafe { msg_send![&*browser, copySelected: std::ptr::null::<AnyObject>()] },
                _ => unsafe { msg_send![super(self), keyDown: event] },
            }
        }
    }
);

impl Tiles {
    /// Меню выбранного снимка под указателем. Мимо снимков меню нет.
    fn menu_at(&self, event: &NSEvent) -> Option<Retained<NSMenu>> {
        let browser = self.ivars().browser.load()?;
        self.select_at(&browser, event);
        browser.ivars().selected.get()?;
        Some(context_menu(self.mtm(), &browser))
    }

    fn select_at(&self, browser: &Browser, event: &NSEvent) {
        let point = self.convertPoint_fromView(event.locationInWindow(), None);
        let count = browser.ivars().shown.borrow().len();
        let index = grid(browser.ivars().view.get(), self.bounds().size.width).index_at(point, count);
        browser.ivars().selected.set(index);
        self.setNeedsDisplay(true);
    }
}

fn context_menu(mtm: MainThreadMarker, browser: &Browser) -> Retained<NSMenu> {
    let menu = NSMenu::new(mtm);
    for (title, action) in [
        ("Open in Editor", sel!(openSelected:)),
        ("Copy", sel!(copySelected:)),
        ("Show in Finder", sel!(revealSelected:)),
        ("Upload", sel!(uploadSelected:)),
        ("Move to Trash", sel!(trashSelected:)),
    ] {
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(title),
                Some(action),
                ns_string!(""),
            )
        };
        unsafe { item.setTarget(Some(browser)) };
        menu.addItem(&item);
    }
    menu
}

/// Рисует видимые ячейки: миниатюру по центру своего места, подпись и рамку выбранной.
fn draw(browser: &Browser, width: f64, dirty: NSRect) {
    let ivars = browser.ivars();
    let view = ivars.view.get();
    let layout = grid(view, width);
    let shown = ivars.shown.borrow();
    let items = ivars.items.borrow();
    let range = layout.visible(dirty.origin.y, dirty.origin.y + dirty.size.height, shown.len());
    let paragraph = NSMutableParagraphStyle::new();
    paragraph.setLineBreakMode(NSLineBreakMode::ByTruncatingMiddle);
    let text = |size: f64, color: &NSColor| {
        let font = NSFont::systemFontOfSize(size);
        // SAFETY: ключи - системные константы атрибутов текста, они живут весь процесс.
        let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName, NSParagraphStyleAttributeName] };
        NSDictionary::<NSString, AnyObject>::from_slices(
            &keys,
            &[&*font as &AnyObject, color as &AnyObject, &*paragraph as &AnyObject],
        )
    };
    let name_attributes = text(12.0, &NSColor::labelColor());
    let detail_attributes = text(11.0, &NSColor::secondaryLabelColor());
    for (position, &index) in shown[range.clone()].iter().enumerate() {
        let position = range.start + position;
        let Some(item) = items.get(index) else { continue };
        let cell = layout.rect(position);
        let (picture, caption) = if view == View::List {
            let side = LIST_ROW - 12.0;
            (
                NSRect::new(NSPoint::new(cell.origin.x + 6.0, cell.origin.y + 6.0), NSSize::new(side * 1.5, side)),
                NSRect::new(
                    NSPoint::new(cell.origin.x + side * 1.5 + 18.0, cell.origin.y + 8.0),
                    NSSize::new(cell.size.width - side * 1.5 - 24.0, 36.0),
                ),
            )
        } else {
            (
                NSRect::new(cell.origin, NSSize::new(cell.size.width, cell.size.height - CAPTION)),
                NSRect::new(
                    NSPoint::new(cell.origin.x, cell.origin.y + cell.size.height - CAPTION + 4.0),
                    NSSize::new(cell.size.width, CAPTION - 4.0),
                ),
            )
        };
        if ivars.selected.get() == Some(position) {
            NSColor::selectedContentBackgroundColor().colorWithAlphaComponent(0.25).setFill();
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(cell, 8.0, 8.0).fill();
        }
        match browser.thumbnail(&item.entry.path, item.entry.modified) {
            Some(image) => {
                let size = image.size();
                let fit = (picture.size.width / size.width).min(picture.size.height / size.height);
                let (w, h) = (size.width * fit, size.height * fit);
                let rect = NSRect::new(
                    NSPoint::new(
                        picture.origin.x + (picture.size.width - w) / 2.0,
                        picture.origin.y + (picture.size.height - h) / 2.0,
                    ),
                    NSSize::new(w, h),
                );
                unsafe {
                    image.drawInRect_fromRect_operation_fraction_respectFlipped_hints(
                        rect,
                        NSRect::ZERO,
                        NSCompositingOperation::SourceOver,
                        1.0,
                        true,
                        None,
                    );
                }
            }
            None => {
                NSColor::quaternaryLabelColor().setFill();
                NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(picture, 6.0, 6.0).fill();
            }
        }
        let name = NSString::from_str(&item.entry.name());
        let detail = NSString::from_str(&format!("{} {}", item.date, item.time));
        let line = NSRect::new(caption.origin, NSSize::new(caption.size.width, 16.0));
        let below =
            NSRect::new(NSPoint::new(caption.origin.x, caption.origin.y + 16.0), NSSize::new(caption.size.width, 15.0));
        let options = NSStringDrawingOptions::UsesLineFragmentOrigin | NSStringDrawingOptions::TruncatesLastVisibleLine;
        unsafe {
            name.drawWithRect_options_attributes_context(line, options, Some(&name_attributes), None);
            detail.drawWithRect_options_attributes_context(below, options, Some(&detail_attributes), None);
        }
    }
}

/// Открывает библиотеку. При первом открытии забирает снимки из папки, при каждом убирает пропавшие.
pub fn show(mtm: MainThreadMarker) {
    let settings = Settings::load();
    if !settings.library {
        let alert = NSAlert::new(mtm);
        alert.setMessageText(ns_string!("The library is turned off"));
        alert.setInformativeText(ns_string!("Turn it on to find saved screenshots by name, date and text."));
        alert.addButtonWithTitle(ns_string!("Turn On"));
        alert.addButtonWithTitle(ns_string!("Cancel"));
        NSApplication::sharedApplication(mtm).activate();
        if alert.runModal() != NSAlertFirstButtonReturn {
            return;
        }
        settings::set_flag(settings::LIBRARY, true);
    }
    if let Some(browser) = BROWSER.with_borrow(Clone::clone) {
        NSApplication::sharedApplication(mtm).activate();
        if let Some(window) = browser.ivars().window.get() {
            window.makeKeyAndOrderFront(None);
        }
        return;
    }
    let browser = build(mtm);
    BROWSER.set(Some(browser.clone()));
    browser.reload();
    let first = !library::exists();
    let directory = settings.directory;
    output::in_background(move || {
        if first {
            library::import(&directory);
        } else {
            library::prune();
            library::recognize_pending();
        }
        DispatchQueue::main().exec_async(|| {
            if let Some(browser) = BROWSER.with_borrow(Clone::clone) {
                browser.reload();
            }
        });
    });
}

fn build(mtm: MainThreadMarker) -> Retained<Browser> {
    let view = settings::string(VIEW_KEY)
        .and_then(|text| text.parse::<usize>().ok())
        .and_then(|index| View::ALL.get(index).copied())
        .unwrap_or(View::Medium);
    let ivars = BrowserIvars {
        items: RefCell::new(Vec::new()),
        shown: RefCell::new(Vec::new()),
        view: Cell::new(view),
        selected: Cell::new(None),
        thumbnails: RefCell::new(HashMap::new()),
        loaded: RefCell::new(VecDeque::new()),
        loading: RefCell::new(HashSet::new()),
        window: OnceCell::new(),
        scroll: OnceCell::new(),
        tiles: OnceCell::new(),
        search: OnceCell::new(),
        count: OnceCell::new(),
    };
    let browser: Retained<Browser> = unsafe { msg_send![super(Browser::alloc(mtm).set_ivars(ivars)), init] };
    let size = NSSize::new(900.0, 620.0);
    let window: Retained<NSWindow> = unsafe {
        msg_send![
            NSWindow::alloc(mtm),
            initWithContentRect: NSRect::new(NSPoint::ZERO, size),
            styleMask: NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable | NSWindowStyleMask::Resizable,
            backing: NSBackingStoreType::Buffered,
            defer: false,
        ]
    };
    // Окно держит BROWSER через контроллер, иначе close() освободит его второй раз.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(ns_string!("Library"));
    window.setContentMinSize(NSSize::new(420.0, 300.0));
    window.setDelegate(Some(ProtocolObject::from_ref(&*browser)));

    let root = NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::ZERO, size));
    let search = NSSearchField::initWithFrame(
        NSSearchField::alloc(mtm),
        NSRect::new(NSPoint::new(12.0, size.height - BAR_HEIGHT + 10.0), NSSize::new(320.0, 24.0)),
    );
    search.setPlaceholderString(Some(ns_string!("Search names, dates and text")));
    search.setSendsSearchStringImmediately(true);
    unsafe {
        search.setTarget(Some(&browser));
        search.setAction(Some(sel!(searchChanged:)));
    }
    search.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
    let titles: Vec<_> = ["Small", "Medium", "Large", "List"].iter().map(|t| NSString::from_str(t)).collect();
    let segments = unsafe {
        NSSegmentedControl::segmentedControlWithLabels_trackingMode_target_action(
            &NSArray::from_retained_slice(&titles),
            NSSegmentSwitchTracking::SelectOne,
            Some(&browser),
            Some(sel!(viewChanged:)),
            mtm,
        )
    };
    segments.setSelectedSegment(View::ALL.iter().position(|v| *v == view).unwrap_or(1) as isize);
    let segments_size = segments.fittingSize();
    segments.setFrame(NSRect::new(
        NSPoint::new(size.width - segments_size.width - 12.0, size.height - BAR_HEIGHT + 10.0),
        segments_size,
    ));
    segments.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinXMargin | NSAutoresizingMaskOptions::ViewMinYMargin);
    let count = NSTextField::labelWithString(ns_string!(""), mtm);
    count.setTextColor(Some(&NSColor::secondaryLabelColor()));
    count.setFrame(NSRect::new(NSPoint::new(344.0, size.height - BAR_HEIGHT + 13.0), NSSize::new(200.0, 18.0)));
    count.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);

    let scroll = NSScrollView::initWithFrame(
        NSScrollView::alloc(mtm),
        NSRect::new(NSPoint::ZERO, NSSize::new(size.width, size.height - BAR_HEIGHT)),
    );
    scroll.setHasVerticalScroller(true);
    scroll.setDrawsBackground(false);
    scroll.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    let tiles: Retained<Tiles> = unsafe {
        msg_send![
            super(Tiles::alloc(mtm).set_ivars(TilesIvars { browser: Weak::from_retained(&browser) })),
            initWithFrame: NSRect::new(NSPoint::ZERO, scroll.contentSize())
        ]
    };
    scroll.setDocumentView(Some(&tiles));
    for subview in [&*search as &NSView, &segments, &count, &scroll] {
        root.addSubview(subview);
    }
    window.setContentView(Some(&root));
    window.center();
    NSApplication::sharedApplication(mtm).activate();
    window.makeKeyAndOrderFront(None);
    window.makeFirstResponder(Some(&tiles));

    let _ = browser.ivars().window.set(window);
    let _ = browser.ivars().scroll.set(scroll);
    let _ = browser.ivars().tiles.set(tiles);
    let _ = browser.ivars().search.set(search);
    let _ = browser.ivars().count.set(count);
    browser
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_fills_the_width_and_finds_cells_under_the_pointer() {
        // 3 колонки по 180 и зазоры по 12 помещаются в 600, излишек делится между ними
        let medium = grid(View::Medium, 600.0);
        assert_eq!(medium.columns, 3);
        assert!((medium.cell.width - (600.0 - 12.0) / 3.0 + 12.0).abs() < 1e-9);
        let second_row = medium.rect(4);
        assert_eq!(second_row.origin.x, 12.0 + medium.cell.width + 12.0);
        let center = NSPoint::new(second_row.origin.x + 10.0, second_row.origin.y + 10.0);
        assert_eq!(medium.index_at(center, 10), Some(4));
        // зазор между ячейками и пустое место после последнего снимка
        assert_eq!(medium.index_at(NSPoint::new(5.0, 5.0), 10), None);
        assert_eq!(medium.index_at(center, 4), None);
        // узкое окно - одна колонка, список - всегда одна
        assert_eq!(grid(View::Large, 200.0).columns, 1);
        assert_eq!(grid(View::List, 900.0), Grid { columns: 1, cell: NSSize::new(900.0, LIST_ROW) });
    }

    #[test]
    fn only_rows_in_the_visible_band_are_drawn() {
        let list = grid(View::List, 500.0);
        assert_eq!(list.height(3), 3.0 * (LIST_ROW + 12.0) + 12.0);
        // полоса от 200 до 330 задевает ряды 2 - 5
        assert_eq!(list.visible(200.0, 330.0, 1000), 2..6);
        assert_eq!(list.visible(200.0, 330.0, 4), 2..4);
    }
}
