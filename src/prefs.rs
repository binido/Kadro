use std::cell::{Cell, OnceCell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly, Message};
use objc2_app_kit::{
    NSAlert, NSApplication, NSBeep, NSBox, NSBoxType, NSButton, NSCellImagePosition, NSColor, NSControl,
    NSControlStateValueOff, NSControlStateValueOn, NSEvent, NSEventModifierFlags, NSGridCell,
    NSGridCellPlacement, NSGridView, NSLineBreakMode, NSModalResponseOK, NSOpenPanel, NSResponder,
    NSStackView, NSTabViewController, NSTabViewControllerTabStyle, NSTabViewItem, NSTextField, NSView,
    NSViewController, NSWindow, NSWindowStyleMask, NSWindowToolbarStyle,
};
use objc2_foundation::{ns_string, NSArray, NSObject, NSPoint, NSRange, NSRect, NSSize, NSString, NSURL};
use objc2_service_management::{SMAppService, SMAppServiceStatus};

use crate::assets;
use crate::hotkeys::{self, Action, Shortcut};
use crate::settings::{self, Format, Settings};

const MARGIN: f64 = 24.0;
const FIELD_WIDTH: f64 = 240.0;
const SHORTCUT_WIDTH: f64 = 140.0;
const FONT_SIZE: f64 = 13.0;
const ESCAPE_KEY: u16 = 53;
const DELETE_KEYS: [u16; 2] = [51, 117];

thread_local! {
    static WINDOW: RefCell<Option<Retained<NSWindow>>> = const { RefCell::new(None) };
    static CONTROLLER: OnceCell<Retained<Controller>> = const { OnceCell::new() };
}

#[derive(Default)]
struct ControllerIvars {
    copy: OnceCell<Retained<NSButton>>,
    save: OnceCell<Retained<NSButton>>,
    directory: OnceCell<Retained<NSButton>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ControllerIvars]
    struct Controller;

    impl Controller {
        #[unsafe(method(toggleDestination:))]
        fn toggle_destination(&self, _sender: &NSButton) {
            let (copy, save) = self.destinations();
            settings::set_flag(settings::COPY_TO_CLIPBOARD, is_on(copy));
            settings::set_flag(settings::SAVE_TO_FILE, is_on(save));
            self.update_destinations();
        }

        #[unsafe(method(changeFormat:))]
        fn change_format(&self, sender: &NSButton) {
            if let Some(format) = Format::ALL.get(sender.tag() as usize) {
                settings::set_string(settings::FORMAT, format.key());
            }
        }

        #[unsafe(method(chooseDirectory:))]
        fn choose_directory(&self, _sender: &NSButton) {
            let panel = NSOpenPanel::openPanel(self.mtm());
            panel.setCanChooseDirectories(true);
            panel.setCanChooseFiles(false);
            panel.setCanCreateDirectories(true);
            let current = Settings::load().directory;
            panel.setDirectoryURL(Some(&NSURL::fileURLWithPath(&NSString::from_str(&current.to_string_lossy()))));
            if panel.runModal() != NSModalResponseOK {
                return;
            }
            if let Some(path) = panel.URL().and_then(|url| url.path()) {
                settings::set_string(settings::DIRECTORY, &path.to_string());
                if let Some(button) = self.ivars().directory.get() {
                    button.setTitle(&NSString::from_str(&display_path(&path.to_string())));
                }
            }
        }

        #[unsafe(method(toggleDownscale:))]
        fn toggle_downscale(&self, sender: &NSButton) {
            settings::set_flag(settings::DOWNSCALE_RETINA, is_on(sender));
        }

        #[unsafe(method(toggleLaunchAtLogin:))]
        fn toggle_launch_at_login(&self, sender: &NSButton) {
            let service = unsafe { SMAppService::mainAppService() };
            let result = if is_on(sender) {
                unsafe { service.registerAndReturnError() }
            } else {
                unsafe { service.unregisterAndReturnError() }
            };
            if let Err(error) = result {
                sender.setState(state(launches_at_login()));
                let alert = NSAlert::new(self.mtm());
                alert.setMessageText(ns_string!("Couldn't change Launch at Login"));
                alert.setInformativeText(&error.localizedDescription());
                alert.runModal();
            }
        }
    }
);

impl Controller {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ControllerIvars::default());
        unsafe { msg_send![super(this), init] }
    }

    fn destinations(&self) -> (&NSButton, &NSButton) {
        let ivars = self.ivars();
        (ivars.copy.get().expect("built with the window"), ivars.save.get().expect("built with the window"))
    }

    /// Последнее включённое назначение нельзя выключить, иначе снимок пропадёт.
    fn update_destinations(&self) {
        let (copy, save) = self.destinations();
        copy.setEnabled(!is_on(copy) || is_on(save));
        save.setEnabled(!is_on(save) || is_on(copy));
    }
}

#[derive(Default)]
struct ShortcutIvars {
    action: Cell<usize>,
    recording: Cell<bool>,
    message: OnceCell<Retained<NSTextField>>,
}

define_class!(
    #[unsafe(super(NSButton, NSControl, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ShortcutIvars]
    struct ShortcutButton;

    impl ShortcutButton {
        #[unsafe(method(startRecording:))]
        fn start_recording(&self, _sender: Option<&AnyObject>) {
            hotkeys::unregister_all();
            self.ivars().recording.set(true);
            self.setTitle(ns_string!("Type Shortcut…"));
            if let Some(window) = self.window() {
                window.makeFirstResponder(Some(self));
            }
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        // Сочетания с ⌘ приходят сюда, а не в keyDown.
        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> bool {
            if self.ivars().recording.get() {
                self.record(event);
                true
            } else {
                unsafe { msg_send![super(self), performKeyEquivalent: event] }
            }
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            if self.ivars().recording.get() {
                self.record(event);
            } else {
                unsafe { msg_send![super(self), keyDown: event] }
            }
        }

        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            self.stop_recording();
            unsafe { msg_send![super(self), resignFirstResponder] }
        }
    }
);

impl ShortcutButton {
    fn new(mtm: MainThreadMarker, action: Action, message: &NSTextField) -> Retained<Self> {
        let ivars = ShortcutIvars::default();
        ivars.action.set(Action::ALL.iter().position(|a| *a == action).unwrap_or(0));
        let _ = ivars.message.set(message.retain());
        let this = Self::alloc(mtm).set_ivars(ivars);
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        unsafe {
            this.setTarget(Some(&this));
            this.setAction(Some(sel!(startRecording:)));
        }
        this.setBezelStyle(objc2_app_kit::NSBezelStyle::Push);
        this.setFont(Some(&assets::font(FONT_SIZE)));
        this.refresh_title();
        this
    }

    fn action(&self) -> Action {
        Action::ALL[self.ivars().action.get()]
    }

    fn refresh_title(&self) {
        let title = self.action().shortcut().map_or_else(|| "Record Shortcut".into(), Shortcut::display);
        self.setTitle(&NSString::from_str(&title));
    }

    fn show_message(&self, text: &str) {
        if let Some(label) = self.ivars().message.get() {
            label.setStringValue(&NSString::from_str(text));
        }
    }

    fn stop_recording(&self) {
        if self.ivars().recording.replace(false) {
            self.refresh_title();
            hotkeys::register_all();
        }
    }

    fn record(&self, event: &NSEvent) {
        let key_code = event.keyCode();
        let flags = event.modifierFlags();
        let plain = !flags.intersects(
            NSEventModifierFlags::Command
                | NSEventModifierFlags::Option
                | NSEventModifierFlags::Control
                | NSEventModifierFlags::Shift,
        );
        if plain && key_code == ESCAPE_KEY {
            self.show_message("");
            return self.stop_recording();
        }
        if plain && DELETE_KEYS.contains(&key_code) {
            self.action().set_shortcut(None);
            self.show_message("");
            return self.stop_recording();
        }

        let shortcut = Shortcut::from_event(key_code, flags);
        if !shortcut.is_usable() {
            NSBeep();
            return self.show_message("Add ⌘, ⌃ or ⌥ to the shortcut.");
        }
        match hotkeys::conflict(self.action(), shortcut) {
            Some(conflict) => {
                NSBeep();
                self.show_message(&format!("{} {conflict}", shortcut.display()));
            }
            None => {
                self.action().set_shortcut(Some(shortcut));
                self.show_message("");
                self.stop_recording();
            }
        }
    }
}

/// Показывает окно настроек, создавая его при первом открытии.
pub fn show(mtm: MainThreadMarker) {
    let window = WINDOW.with_borrow_mut(|slot| slot.get_or_insert_with(|| build_window(mtm)).clone());
    NSApplication::sharedApplication(mtm).activate();
    window.makeKeyAndOrderFront(None);
}

fn build_window(mtm: MainThreadMarker) -> Retained<NSWindow> {
    let controller = CONTROLLER.with(|c| c.get_or_init(|| Controller::new(mtm)).clone());
    let tabs = NSTabViewController::new(mtm);
    tabs.setTabStyle(NSTabViewControllerTabStyle::Toolbar);
    for (title, icon, view) in [
        ("General", assets::SLIDERS, general_tab(mtm, &controller)),
        ("Shortcuts", assets::KEYBOARD, shortcuts_tab(mtm)),
    ] {
        let page = NSViewController::new(mtm);
        page.setView(&view);
        // Иначе окно берёт размер по умолчанию и прижимает содержимое к низу.
        page.setPreferredContentSize(view.frame().size);
        page.setTitle(Some(&NSString::from_str(title)));
        let item = NSTabViewItem::tabViewItemWithViewController(&page);
        item.setImage(assets::icon(icon, 20.0).as_deref());
        tabs.addTabViewItem(&item);
    }

    let window = NSWindow::windowWithContentViewController(&tabs);
    window.setStyleMask(NSWindowStyleMask::Titled | NSWindowStyleMask::Closable);
    window.setToolbarStyle(NSWindowToolbarStyle::Preference);
    // Окно переиспользуется при следующем открытии, его держит WINDOW.
    unsafe { window.setReleasedWhenClosed(false) };
    window.center();
    window
}

/// Строка таблицы настроек.
enum Row {
    Field(String, Retained<NSView>),
    Separator,
    Note(Retained<NSView>),
}

fn general_tab(mtm: MainThreadMarker, controller: &Controller) -> Retained<NSView> {
    let current = Settings::load();

    let directory = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str(&display_path(&current.directory.to_string_lossy())),
            Some(controller),
            Some(sel!(chooseDirectory:)),
            mtm,
        )
    };
    directory.setImage(assets::icon(assets::FOLDER_OPEN, 14.0).as_deref());
    directory.setImagePosition(NSCellImagePosition::ImageLeading);
    directory.setLineBreakMode(NSLineBreakMode::ByTruncatingMiddle);
    directory.setFont(Some(&assets::font(FONT_SIZE)));
    directory.widthAnchor().constraintEqualToConstant(FIELD_WIDTH).setActive(true);
    let _ = controller.ivars().directory.set(directory.clone());

    let formats: Vec<_> = Format::ALL
        .iter()
        .enumerate()
        .map(|(index, format)| {
            // Радиокнопки с общим action и общим родителем AppKit сам связывает в группу.
            let radio = unsafe {
                NSButton::radioButtonWithTitle_target_action(
                    &NSString::from_str(format.title()),
                    Some(controller),
                    Some(sel!(changeFormat:)),
                    mtm,
                )
            };
            radio.setTag(index as isize);
            radio.setState(state(format.key() == current.format.key()));
            radio.setFont(Some(&assets::font(FONT_SIZE)));
            view(&radio)
        })
        .collect();

    let copy = checkbox(mtm, "Copy", current.copy_to_clipboard, controller, sel!(toggleDestination:));
    let save = checkbox(mtm, "Save", current.save_to_file, controller, sel!(toggleDestination:));
    let _ = controller.ivars().copy.set(copy.clone());
    let _ = controller.ivars().save.set(save.clone());
    controller.update_destinations();

    let downscale = checkbox(mtm, "Downscale to 1× when saving", current.downscale_retina, controller, sel!(toggleDownscale:));
    let login = checkbox(mtm, "Launch at login", launches_at_login(), controller, sel!(toggleLaunchAtLogin:));

    grid(mtm, vec![
        Row::Field("Screenshots folder".into(), view(&directory)),
        Row::Field("Save format".into(), inline(mtm, &formats)),
        Row::Field("Resize retina screenshots".into(), view(&downscale)),
        Row::Separator,
        Row::Field("Autostart".into(), view(&login)),
        Row::Field("After screenshot".into(), inline(mtm, &[view(&copy), view(&save)])),
    ])
}

fn shortcuts_tab(mtm: MainThreadMarker) -> Retained<NSView> {
    let message = label(mtm, "");
    message.setTextColor(Some(&NSColor::systemRedColor()));
    let hint = label(mtm, "Click a shortcut and press new keys. ⌫ clears it, Esc cancels.");
    hint.setTextColor(Some(&NSColor::secondaryLabelColor()));

    let mut rows: Vec<Row> = Action::ALL
        .iter()
        .map(|action| {
            let button = ShortcutButton::new(mtm, *action, &message);
            button.widthAnchor().constraintEqualToConstant(SHORTCUT_WIDTH).setActive(true);
            Row::Field(action.title().into(), view(&button))
        })
        .collect();
    rows.extend([Row::Separator, Row::Note(view(&hint)), Row::Note(view(&message))]);
    grid(mtm, rows)
}

/// Таблица настроек: подписи справа налево к контролам, разделители и пояснения во всю ширину.
fn grid(mtm: MainThreadMarker, rows: Vec<Row>) -> Retained<NSView> {
    let cells: Vec<_> = rows
        .iter()
        .map(|row| {
            let empty = NSGridCell::emptyContentView(mtm);
            let pair = match row {
                Row::Field(title, control) => [view(&label(mtm, title)), control.clone()],
                Row::Separator => {
                    let line = NSBox::new(mtm);
                    line.setBoxType(NSBoxType::Separator);
                    [view(&line), empty]
                }
                Row::Note(note) => [note.clone(), empty],
            };
            NSArray::from_retained_slice(&pair)
        })
        .collect();
    let grid = NSGridView::gridViewWithViews(&NSArray::from_retained_slice(&cells), mtm);
    grid.setRowSpacing(12.0);
    grid.setColumnSpacing(12.0);
    grid.setYPlacement(NSGridCellPlacement::Center);
    grid.columnAtIndex(0).setXPlacement(NSGridCellPlacement::Trailing);
    for (index, row) in rows.iter().enumerate() {
        let placement = match row {
            Row::Field(..) => continue,
            Row::Separator => NSGridCellPlacement::Fill,
            Row::Note(_) => NSGridCellPlacement::Leading,
        };
        grid.mergeCellsInHorizontalRange_verticalRange(NSRange::new(0, 2), NSRange::new(index, 1));
        grid.cellAtColumnIndex_rowIndex(0, index as isize).setXPlacement(placement);
        if let Row::Separator = row {
            let separator = grid.rowAtIndex(index as isize);
            separator.setTopPadding(4.0);
            separator.setBottomPadding(4.0);
        }
    }

    let size = grid.fittingSize();
    let container = NSView::initWithFrame(
        NSView::alloc(mtm),
        NSRect::new(NSPoint::ZERO, NSSize::new(size.width + 2.0 * MARGIN, size.height + 2.0 * MARGIN)),
    );
    grid.setFrame(NSRect::new(NSPoint::new(MARGIN, MARGIN), size));
    container.addSubview(&grid);
    container
}

/// Контролы в одну строку, как Copy и Save.
fn inline(mtm: MainThreadMarker, views: &[Retained<NSView>]) -> Retained<NSView> {
    let stack = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(views), mtm);
    stack.setSpacing(16.0);
    view(&stack)
}

fn label(mtm: MainThreadMarker, text: &str) -> Retained<NSTextField> {
    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    label.setFont(Some(&assets::font(FONT_SIZE)));
    label
}

fn checkbox(mtm: MainThreadMarker, title: &str, on: bool, target: &Controller, action: objc2::runtime::Sel) -> Retained<NSButton> {
    let button = unsafe { NSButton::checkboxWithTitle_target_action(&NSString::from_str(title), Some(target), Some(action), mtm) };
    button.setState(state(on));
    button.setFont(Some(&assets::font(FONT_SIZE)));
    button
}

fn view(view: &NSView) -> Retained<NSView> {
    view.retain()
}

fn is_on(button: &NSButton) -> bool {
    button.state() == NSControlStateValueOn
}

fn state(on: bool) -> isize {
    if on { NSControlStateValueOn } else { NSControlStateValueOff }
}

/// Запуск при входе работает только у приложения, собранного в .app и лежащего в Программах.
fn launches_at_login() -> bool {
    let status = unsafe { SMAppService::mainAppService().status() };
    status == SMAppServiceStatus::Enabled
}

fn display_path(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if path.starts_with(&home) => format!("~{}", &path[home.len()..]),
        _ => path.to_string(),
    }
}
