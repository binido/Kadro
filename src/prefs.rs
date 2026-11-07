//! Окно настроек: вкладки с параметрами снимков, редактора, загрузки и сочетаниями клавиш.

use std::cell::{Cell, OnceCell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSBeep, NSBezelStyle, NSButton, NSCellImagePosition, NSColor,
    NSColorSpace, NSColorWell, NSColorWellStyle, NSControl, NSControlStateValueOff, NSControlStateValueOn,
    NSControlTextEditingDelegate, NSEvent, NSEventModifierFlags, NSGridCell, NSGridCellPlacement, NSGridView,
    NSLineBreakMode, NSModalResponseOK, NSOpenPanel, NSPopUpButton, NSResponder, NSSecureTextField, NSStackView,
    NSTabView, NSTabViewController, NSTabViewControllerTabStyle, NSTabViewItem, NSTextField, NSTextFieldDelegate,
    NSUserInterfaceItemIdentification, NSView, NSViewController, NSWindow, NSWindowStyleMask, NSWindowToolbarStyle,
    NSWorkspace,
};
use objc2_foundation::{
    NSArray, NSLocale, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRange, NSRect, NSSize, NSString, NSURL,
    ns_string,
};
use objc2_service_management::{SMAppService, SMAppServiceStatus};

use crate::hotkeys::{self, Action, Shortcut};
use crate::settings::{
    self, AfterCapture, Choice, Confirmation, Cursor, DEFAULT_FILE_NAME_TEMPLATE, DefaultZoom, EscAction, Format,
    LibraryLimit, LinkExpiry, LinkKind, LinkPolicy, PreviewHiding, ScrollLimit, ScrollSpeed, Settings,
    WindowBackground,
};
use crate::{assets, keys, library, output, recognize, updates, upload};

const MARGIN: f64 = 24.0;
const FIELD_WIDTH: f64 = 240.0;
const SHORTCUT_WIDTH: f64 = 140.0;
const KEYBOARD_SETTINGS_URL: &str = "x-apple.systempreferences:com.apple.Keyboard-Settings.extension";
const FONT_SIZE: f64 = 13.0;

thread_local! {
    static WINDOW: RefCell<Option<Retained<NSWindow>>> = const { RefCell::new(None) };
    static CONTROLLER: OnceCell<Retained<Controller>> = const { OnceCell::new() };
}

#[derive(Default)]
struct ControllerIvars {
    /// Радиокнопка "Nothing" в выборе, что показать после снимка.
    show_nothing: OnceCell<Retained<NSButton>>,
    copy: OnceCell<Retained<NSButton>>,
    save: OnceCell<Retained<NSButton>>,
    directory: OnceCell<Retained<NSButton>>,
    /// Итог проверки подключения к хранилищу.
    test_status: OnceCell<Retained<NSTextField>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ControllerIvars]
    struct Controller;

    unsafe impl NSObjectProtocol for Controller {}

    unsafe impl NSControlTextEditingDelegate for Controller {
        // Поля сохраняются на каждое изменение, отдельной кнопки нет. Идентификатор поля - ключ
        // настройки, у ключей доступа - имя записи в Связке ключей, у шаблона имени его нет.
        #[unsafe(method(controlTextDidChange:))]
        fn control_text_did_change(&self, notification: &NSNotification) {
            let Some(field) = notification.object().and_then(|o| o.downcast::<NSTextField>().ok()) else { return };
            let value = field.stringValue().to_string();
            match field.identifier().map(|id| id.to_string()) {
                Some(id) if id == upload::ACCESS_KEY || id == upload::SECRET_KEY => upload::set_secret(&id, value.trim()),
                Some(key) => settings::set_string(&key, &value),
                None => settings::set_string(settings::FILE_NAME_TEMPLATE, &value),
            }
        }
    }

    unsafe impl NSTextFieldDelegate for Controller {}

    impl Controller {
        #[unsafe(method(toggleDestination:))]
        fn toggle_destination(&self, _sender: &NSButton) {
            let [copy, save] = self.destinations();
            settings::set_flag(settings::COPY_TO_CLIPBOARD, is_on(copy));
            settings::set_flag(settings::SAVE_TO_FILE, is_on(save));
            self.update_destinations();
        }

        #[unsafe(method(changeAfterCapture:))]
        fn change_after_capture(&self, sender: &NSButton) {
            store_choice::<AfterCapture>(sender);
            self.update_destinations();
        }

        #[unsafe(method(changeEscAction:))]
        fn change_esc_action(&self, sender: &NSButton) {
            store_choice::<EscAction>(sender);
        }

        #[unsafe(method(changeDefaultZoom:))]
        fn change_default_zoom(&self, sender: &NSButton) {
            store_choice::<DefaultZoom>(sender);
        }

        #[unsafe(method(toggleEditorOnTop:))]
        fn toggle_editor_on_top(&self, sender: &NSButton) {
            settings::set_flag(settings::EDITOR_ON_TOP, is_on(sender));
        }

        #[unsafe(method(toggleScrollZoom:))]
        fn toggle_scroll_zoom(&self, sender: &NSButton) {
            settings::set_flag(settings::EDITOR_SCROLL_ZOOM, is_on(sender));
        }

        #[unsafe(method(changePreviewHiding:))]
        fn change_preview_hiding(&self, sender: &NSButton) {
            store_choice::<PreviewHiding>(sender);
        }

        #[unsafe(method(changeConfirmation:))]
        fn change_confirmation(&self, sender: &NSButton) {
            store_choice::<Confirmation>(sender);
        }

        #[unsafe(method(changeWindowBackground:))]
        fn change_window_background(&self, sender: &NSButton) {
            store_choice::<WindowBackground>(sender);
        }

        #[unsafe(method(changeRecognitionLanguage:))]
        fn change_recognition_language(&self, sender: &NSPopUpButton) {
            // Первый пункт - языки системы, остальные идут в порядке `recognize::languages`.
            let index = sender.indexOfSelectedItem();
            let language = usize::try_from(index - 1).ok().and_then(|i| recognize::languages().get(i).cloned());
            settings::set_string(settings::RECOGNITION_LANGUAGE, language.as_deref().unwrap_or(""));
        }

        #[unsafe(method(changeScrollLimit:))]
        fn change_scroll_limit(&self, sender: &NSButton) {
            store_choice::<ScrollLimit>(sender);
        }

        #[unsafe(method(changeScrollSpeed:))]
        fn change_scroll_speed(&self, sender: &NSButton) {
            store_choice::<ScrollSpeed>(sender);
        }

        #[unsafe(method(toggleScrollReverse:))]
        fn toggle_scroll_reverse(&self, sender: &NSButton) {
            settings::set_flag(settings::SCROLL_REVERSE, is_on(sender));
        }

        #[unsafe(method(changeLinkKind:))]
        fn change_link_kind(&self, sender: &NSButton) {
            store_choice::<LinkKind>(sender);
        }

        #[unsafe(method(changeLinkExpiry:))]
        fn change_link_expiry(&self, sender: &NSButton) {
            store_choice::<LinkExpiry>(sender);
        }

        // Проверка идёт в сети, поэтому вне главного потока, а итог пишется под кнопкой.
        #[unsafe(method(testUpload:))]
        fn test_upload(&self, _sender: &NSButton) {
            let Some(status) = self.ivars().test_status.get() else { return };
            let storage = upload::Storage::load();
            if !storage.is_configured() {
                return status.setStringValue(ns_string!("Fill in the bucket and both keys first."));
            }
            status.setStringValue(ns_string!("Testing…"));
            output::in_background(move || {
                let result = storage.test();
                dispatch2::DispatchQueue::main().exec_async(move || {
                    let text = match result {
                        Ok(()) => "Connection works: Kadro can upload to this bucket.".to_string(),
                        Err(message) => message,
                    };
                    CONTROLLER.with(|c| {
                        if let Some(status) = c.get().and_then(|c| c.ivars().test_status.get()) {
                            status.setStringValue(&NSString::from_str(&text));
                        }
                    });
                });
            });
        }

        #[unsafe(method(toggleHideIcon:))]
        fn toggle_hide_icon(&self, sender: &NSButton) {
            settings::set_flag(settings::HIDE_ICON, is_on(sender));
            crate::set_icon_visible(!is_on(sender));
        }

        #[unsafe(method(changeLinkPolicy:))]
        fn change_link_policy(&self, sender: &NSButton) {
            store_choice::<LinkPolicy>(sender);
        }

        #[unsafe(method(toggleCheckUpdates:))]
        fn toggle_check_updates(&self, sender: &NSButton) {
            settings::set_flag(settings::CHECK_UPDATES, is_on(sender));
        }

        #[unsafe(method(checkUpdatesNow:))]
        fn check_updates_now(&self, _sender: &NSButton) {
            updates::check(true);
        }

        #[unsafe(method(toggleLibrary:))]
        fn toggle_library(&self, sender: &NSButton) {
            settings::set_flag(settings::LIBRARY, is_on(sender));
        }

        #[unsafe(method(changeLibraryLimit:))]
        fn change_library_limit(&self, sender: &NSButton) {
            store_choice::<LibraryLimit>(sender);
        }

        #[unsafe(method(toggleSingleLine:))]
        fn toggle_single_line(&self, sender: &NSButton) {
            settings::set_flag(settings::RECOGNITION_SINGLE_LINE, is_on(sender));
        }

        #[unsafe(method(changeWindowColor:))]
        fn change_window_color(&self, sender: &NSColorWell) {
            if let Some(rgb) = sender.color().colorUsingColorSpace(&NSColorSpace::sRGBColorSpace()) {
                let byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                let hex = format!("#{:02X}{:02X}{:02X}", byte(rgb.redComponent()), byte(rgb.greenComponent()), byte(rgb.blueComponent()));
                settings::set_string(settings::WINDOW_COLOR, &hex);
            }
        }

        #[unsafe(method(changeFormat:))]
        fn change_format(&self, sender: &NSButton) {
            store_choice::<Format>(sender);
        }

        #[unsafe(method(changeCursor:))]
        fn change_cursor(&self, sender: &NSButton) {
            store_choice::<Cursor>(sender);
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
                offer_reindex(self.mtm(), std::path::PathBuf::from(path.to_string()));
            }
        }

        #[unsafe(method(openKeyboardSettings:))]
        fn open_keyboard_settings(&self, _sender: &NSButton) {
            if let Some(url) = NSURL::URLWithString(ns_string!(KEYBOARD_SETTINGS_URL)) {
                NSWorkspace::sharedWorkspace().openURL(&url);
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

    fn destinations(&self) -> [&NSButton; 2] {
        let ivars = self.ivars();
        [&ivars.copy, &ivars.save].map(|cell| &**cell.get().expect("built with the window"))
    }

    /// Снимок должен куда-то попасть: в превью, редактор, буфер или файл.
    /// Поэтому последнее включённое из них выключить нельзя.
    fn update_destinations(&self) {
        let buttons = self.destinations();
        let shows = Settings::load().after_capture != AfterCapture::Nothing;
        let on = buttons.iter().filter(|b| is_on(b)).count();
        for button in buttons {
            button.setEnabled(shows || !(is_on(button) && on == 1));
        }
        if let Some(nothing) = self.ivars().show_nothing.get() {
            nothing.setEnabled(on > 0);
        }
    }
}

struct ShortcutIvars {
    action: Action,
    recording: Cell<bool>,
    /// Подпись под таблицей с подсказкой или причиной, почему сочетание не подошло.
    message: Retained<NSTextField>,
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
        let ivars = ShortcutIvars { action, recording: Cell::new(false), message: message.retain() };
        let this = Self::alloc(mtm).set_ivars(ivars);
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        unsafe {
            this.setTarget(Some(&this));
            this.setAction(Some(sel!(startRecording:)));
        }
        this.setBezelStyle(NSBezelStyle::Push);
        this.setFont(Some(&assets::font(FONT_SIZE)));
        this.refresh_title();
        this
    }

    fn action(&self) -> Action {
        self.ivars().action
    }

    fn refresh_title(&self) {
        let title = self.action().shortcut().map_or_else(|| "Record Shortcut".into(), Shortcut::display);
        self.setTitle(&NSString::from_str(&title));
    }

    fn show_message(&self, text: &str) {
        Self::show(&self.ivars().message, text);
    }

    /// Пишет под таблицей, почему сочетание не подошло, или подсказку, если `text` пустой.
    fn show(message: &NSTextField, text: &str) {
        let (text, color) = if text.is_empty() {
            ("Click a shortcut and press new keys. ⌫ clears it, Esc cancels.", NSColor::secondaryLabelColor())
        } else {
            (text, NSColor::systemRedColor())
        };
        message.setStringValue(&NSString::from_str(text));
        message.setTextColor(Some(&color));
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
        if plain && key_code == keys::ESCAPE {
            self.show_message("");
            return self.stop_recording();
        }
        if plain && [keys::DELETE, keys::FORWARD_DELETE].contains(&key_code) {
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

define_class!(
    /// Вкладки настроек, которые запоминают последнюю открытую.
    #[unsafe(super(NSTabViewController, NSViewController, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    struct Tabs;

    impl Tabs {
        #[unsafe(method(tabView:didSelectTabViewItem:))]
        fn did_select(&self, tab_view: &NSTabView, item: Option<&NSTabViewItem>) {
            unsafe {
                let _: () = msg_send![super(self), tabView: tab_view, didSelectTabViewItem: item];
            }
            settings::set_string(settings::SETTINGS_TAB, &self.selectedTabViewItemIndex().to_string());
        }
    }
);

fn build_window(mtm: MainThreadMarker) -> Retained<NSWindow> {
    let controller = CONTROLLER.with(|c| c.get_or_init(|| Controller::new(mtm)).clone());
    // Читается до вкладок, потому что первая добавленная вкладка выбирается и записывает свой номер.
    let last = settings::string(settings::SETTINGS_TAB).and_then(|t| t.parse::<isize>().ok()).unwrap_or(0);
    let tabs: Retained<Tabs> = unsafe { msg_send![Tabs::alloc(mtm), init] };
    tabs.setTabStyle(NSTabViewControllerTabStyle::Toolbar);
    for (title, icon, view) in [
        ("General", assets::SLIDERS, general_tab(mtm, &controller)),
        ("Capture", assets::CAMERA, capture_tab(mtm, &controller)),
        ("Saving", assets::FOLDER_DOWN, saving_tab(mtm, &controller)),
        ("Editor", assets::SQUARE_PEN, editor_tab(mtm, &controller)),
        ("Upload", assets::CLOUD_UPLOAD, upload_tab(mtm, &controller)),
        ("Shortcuts", assets::KEYBOARD, shortcuts_tab(mtm, &controller)),
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
    if (0..tabs.tabViewItems().len() as isize).contains(&last) {
        tabs.setSelectedTabViewItemIndex(last);
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
    Field(&'static str, Retained<NSView>),
    /// Заголовок группы строк.
    Heading(&'static str),
    Note(Retained<NSView>),
}

/// Запуск, строка меню, ссылки и обновления.
fn general_tab(mtm: MainThreadMarker, controller: &Controller) -> Retained<NSView> {
    let current = Settings::load();
    let login = checkbox(mtm, "Launch at login", launches_at_login(), controller, sel!(toggleLaunchAtLogin:));
    let hide_icon = checkbox(mtm, "Hide the menu bar icon", current.hide_icon, controller, sel!(toggleHideIcon:));
    let hide_hint = label(mtm, "Open Kadro again from Applications to get back to Settings.");
    hide_hint.setTextColor(Some(&NSColor::secondaryLabelColor()));
    let links = radios(mtm, current.links, controller, sel!(changeLinkPolicy:));
    let updates_on =
        checkbox(mtm, "Check for updates once a day", current.check_updates, controller, sel!(toggleCheckUpdates:));
    let check_now = unsafe {
        NSButton::buttonWithTitle_target_action(
            ns_string!("Check Now"),
            Some(controller),
            Some(sel!(checkUpdatesNow:)),
            mtm,
        )
    };
    grid(
        mtm,
        &[
            Row::Field("Autostart", view(&login)),
            Row::Field("Menu bar", view(&hide_icon)),
            Row::Field("", view(&hide_hint)),
            Row::Field("kadro:// links", inline(mtm, &links)),
            Row::Field("Updates", inline(mtm, &[view(&updates_on), view(&check_now)])),
        ],
    )
}

/// Что происходит после снимка, что попадает в кадр и снимок с прокруткой.
fn capture_tab(mtm: MainThreadMarker, controller: &Controller) -> Retained<NSView> {
    let current = Settings::load();
    let after_capture = radios(mtm, current.after_capture, controller, sel!(changeAfterCapture:));
    if let Some(nothing) = after_capture.last().and_then(|v| v.clone().downcast::<NSButton>().ok()) {
        let _ = controller.ivars().show_nothing.set(nothing);
    }
    let copy = checkbox(mtm, "Copy", current.copy_to_clipboard, controller, sel!(toggleDestination:));
    let save = checkbox(mtm, "Save", current.save_to_file, controller, sel!(toggleDestination:));
    let hiding = radios(mtm, current.preview_hiding, controller, sel!(changePreviewHiding:));
    let confirmation = radios(mtm, current.confirmation, controller, sel!(changeConfirmation:));
    let _ = controller.ivars().copy.set(copy.clone());
    let _ = controller.ivars().save.set(save.clone());
    controller.update_destinations();

    let cursor = radios(mtm, current.cursor, controller, sel!(changeCursor:));
    let mut window_background = radios(mtm, current.window_background, controller, sel!(changeWindowBackground:));
    let window_color =
        NSColorWell::initWithFrame(NSColorWell::alloc(mtm), NSRect::new(NSPoint::ZERO, NSSize::new(38.0, 24.0)));
    window_color.setColorWellStyle(NSColorWellStyle::Minimal);
    let c = current.window_color;
    window_color.setColor(&NSColor::colorWithSRGBRed_green_blue_alpha(c.red, c.green, c.blue, 1.0));
    unsafe {
        window_color.setTarget(Some(controller));
        window_color.setAction(Some(sel!(changeWindowColor:)));
    }
    window_background.push(view(&window_color));

    let scroll_limit = radios(mtm, current.scroll_limit, controller, sel!(changeScrollLimit:));
    let scroll_speed = radios(mtm, current.scroll_speed, controller, sel!(changeScrollSpeed:));
    let scroll_reverse = checkbox(
        mtm,
        "Reverse direction, for apps that scroll the other way",
        current.scroll_reverse,
        controller,
        sel!(toggleScrollReverse:),
    );

    grid(
        mtm,
        &[
            Row::Heading("After capture"),
            Row::Field("Show", inline(mtm, &after_capture)),
            Row::Field("Screenshot goes to", inline(mtm, &[view(&copy), view(&save)])),
            Row::Field("Hide preview", inline(mtm, &hiding)),
            Row::Field("Confirm without preview", inline(mtm, &confirmation)),
            Row::Heading("Image"),
            Row::Field("Cursor", inline(mtm, &cursor)),
            Row::Field("Behind windows", inline(mtm, &window_background)),
            Row::Heading("Scrolling capture"),
            Row::Field("Capture up to", inline(mtm, &scroll_limit)),
            Row::Field("Scroll speed", inline(mtm, &scroll_speed)),
            Row::Field("", view(&scroll_reverse)),
        ],
    )
}

/// Куда и как сохраняются снимки, библиотека и распознавание текста.
fn saving_tab(mtm: MainThreadMarker, controller: &Controller) -> Retained<NSView> {
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

    let file_name = NSTextField::textFieldWithString(&NSString::from_str(&current.file_name_template), mtm);
    file_name.setPlaceholderString(Some(&NSString::from_str(DEFAULT_FILE_NAME_TEMPLATE)));
    file_name.setFont(Some(&assets::font(FONT_SIZE)));
    file_name.widthAnchor().constraintEqualToConstant(FIELD_WIDTH).setActive(true);
    // Делегат не удерживается полем, контроллер живёт в CONTROLLER.
    unsafe { file_name.setDelegate(Some(ProtocolObject::from_ref(controller))) };
    let tokens = label(mtm, "{date} {time} {mode} {app}");
    tokens.setTextColor(Some(&NSColor::secondaryLabelColor()));
    let formats = radios(mtm, current.format, controller, sel!(changeFormat:));
    let downscale =
        checkbox(mtm, "Downscale to 1× when saving", current.downscale_retina, controller, sel!(toggleDownscale:));

    let library_on =
        checkbox(mtm, "Keep a library of saved screenshots", current.library, controller, sel!(toggleLibrary:));
    let library_limit = radios(mtm, current.library_limit, controller, sel!(changeLibraryLimit:));
    let language = recognition_languages(mtm, current.recognition_language.as_deref(), controller);
    let single_line = checkbox(mtm, "Join lines into one", current.single_line, controller, sel!(toggleSingleLine:));

    grid(
        mtm,
        &[
            Row::Heading("Files"),
            Row::Field("Screenshots folder", view(&directory)),
            Row::Field("File name", view(&file_name)),
            Row::Field("", view(&tokens)),
            Row::Field("Format", inline(mtm, &formats)),
            Row::Field("Retina screenshots", view(&downscale)),
            Row::Heading("Library"),
            Row::Field("", view(&library_on)),
            Row::Field("Remember up to", inline(mtm, &library_limit)),
            Row::Heading("Text recognition"),
            Row::Field("Language", view(&language)),
            Row::Field("", view(&single_line)),
        ],
    )
}

fn editor_tab(mtm: MainThreadMarker, controller: &Controller) -> Retained<NSView> {
    let current = Settings::load();
    let esc = radios(mtm, current.esc_action, controller, sel!(changeEscAction:));
    let zoom = radios(mtm, current.default_zoom, controller, sel!(changeDefaultZoom:));
    let on_top = checkbox(
        mtm,
        "Keep the editor above other windows",
        current.editor_on_top,
        controller,
        sel!(toggleEditorOnTop:),
    );
    let scroll = checkbox(mtm, "Zoom with the scroll wheel", current.scroll_zoom, controller, sel!(toggleScrollZoom:));
    let scroll_hint = label(mtm, "Otherwise hold ⌘ while scrolling, or pinch.");
    scroll_hint.setTextColor(Some(&NSColor::secondaryLabelColor()));
    grid(
        mtm,
        &[
            Row::Field("Esc in the editor", inline(mtm, &esc)),
            Row::Field("Open screenshots", inline(mtm, &zoom)),
            Row::Field("Window", view(&on_top)),
            Row::Field("Zoom", view(&scroll)),
            Row::Field("", view(&scroll_hint)),
        ],
    )
}

/// Хранилище для загрузки: адрес, бакет, ключи, вид ссылки и проверка подключения.
fn upload_tab(mtm: MainThreadMarker, controller: &Controller) -> Retained<NSView> {
    let current = upload::Storage::load();
    let field = |value: &str, placeholder: &str, id: &str, secure: bool| {
        let field: Retained<NSTextField> = if secure {
            let secure: Retained<NSSecureTextField> =
                unsafe { msg_send![NSSecureTextField::alloc(mtm), initWithFrame: NSRect::ZERO] };
            secure.setStringValue(&NSString::from_str(value));
            Retained::into_super(secure)
        } else {
            NSTextField::textFieldWithString(&NSString::from_str(value), mtm)
        };
        field.setPlaceholderString(Some(&NSString::from_str(placeholder)));
        field.setFont(Some(&assets::font(FONT_SIZE)));
        field.setIdentifier(Some(&NSString::from_str(id)));
        field.widthAnchor().constraintEqualToConstant(FIELD_WIDTH * 1.5).setActive(true);
        // Делегат не удерживается полем, контроллер живёт в CONTROLLER.
        unsafe { field.setDelegate(Some(ProtocolObject::from_ref(controller))) };
        view(&field)
    };
    let text = |key: &str| settings::string(key).unwrap_or_default();
    let endpoint = field(
        &text(settings::UPLOAD_ENDPOINT),
        "Empty for Amazon S3, or https://… for R2, B2 and others",
        settings::UPLOAD_ENDPOINT,
        false,
    );
    let region = field(&text(settings::UPLOAD_REGION), "us-east-1, or auto for R2", settings::UPLOAD_REGION, false);
    let bucket = field(&text(settings::UPLOAD_BUCKET), "my-screenshots", settings::UPLOAD_BUCKET, false);
    let prefix =
        field(&text(settings::UPLOAD_PREFIX), "Folder inside the bucket, optional", settings::UPLOAD_PREFIX, false);
    let access = field(&current.access_key, "Stored in the Keychain", upload::ACCESS_KEY, false);
    let secret = field(&current.secret_key, "Stored in the Keychain", upload::SECRET_KEY, true);
    let links = radios(mtm, current.links, controller, sel!(changeLinkKind:));
    let expiry = radios(mtm, current.expiry, controller, sel!(changeLinkExpiry:));
    let public_url = field(
        &text(settings::UPLOAD_PUBLIC_URL),
        "https://img.example.com, optional",
        settings::UPLOAD_PUBLIC_URL,
        false,
    );
    let test = unsafe {
        NSButton::buttonWithTitle_target_action(
            ns_string!("Test Connection"),
            Some(controller),
            Some(sel!(testUpload:)),
            mtm,
        )
    };
    let status = label(mtm, "");
    status.setTextColor(Some(&NSColor::secondaryLabelColor()));
    status.setLineBreakMode(NSLineBreakMode::ByWordWrapping);
    status.setPreferredMaxLayoutWidth(FIELD_WIDTH);
    let _ = controller.ivars().test_status.set(status.clone());
    grid(
        mtm,
        &[
            Row::Heading("Storage"),
            Row::Field("Endpoint", endpoint),
            Row::Field("Region", region),
            Row::Field("Bucket", bucket),
            Row::Field("Path prefix", prefix),
            Row::Heading("Keys"),
            Row::Field("Access key", access),
            Row::Field("Secret key", secret),
            Row::Field("", inline(mtm, &[view(&test), view(&status)])),
            Row::Heading("Links"),
            Row::Field("Copy after upload", inline(mtm, &links)),
            Row::Field("Temporary links last", inline(mtm, &expiry)),
            Row::Field("Public address", public_url),
        ],
    )
}

fn shortcuts_tab(mtm: MainThreadMarker, controller: &Controller) -> Retained<NSView> {
    let how = label(mtm, "To use ⇧⌘3 and ⇧⌘4, turn them off in Keyboard Shortcuts → Screenshots.");
    let open_settings = unsafe {
        NSButton::buttonWithTitle_target_action(
            ns_string!("Open Keyboard Settings"),
            Some(controller),
            Some(sel!(openKeyboardSettings:)),
            mtm,
        )
    };
    open_settings.setFont(Some(&assets::font(FONT_SIZE)));

    let message = label(mtm, "");
    let shortcut = |action: &Action| {
        let button = ShortcutButton::new(mtm, *action, &message);
        button.widthAnchor().constraintEqualToConstant(SHORTCUT_WIDTH).setActive(true);
        Row::Field(action.title(), view(&button))
    };
    let group = |title, actions: &[Action]| {
        let rows: Vec<_> = std::iter::once(Row::Heading(title)).chain(actions.iter().map(&shortcut)).collect();
        table(mtm, &rows)
    };
    // Первые семь действий снимают экран, остальные работают с готовыми снимками.
    let (capture, screenshots) = Action::ALL.split_at(7);
    let columns = side_by_side(mtm, &group("Capture", capture), &group("Screenshots", screenshots));
    let rows = grid(
        mtm,
        &[Row::Note(view(&how)), Row::Note(view(&open_settings)), Row::Note(columns), Row::Note(view(&message))],
    );
    ShortcutButton::show(&message, "");
    rows
}

/// Таблица настроек с отступами до краёв окна.
fn grid(mtm: MainThreadMarker, rows: &[Row]) -> Retained<NSView> {
    let grid = table(mtm, rows);
    let size = grid.fittingSize();
    let container = NSView::initWithFrame(
        NSView::alloc(mtm),
        NSRect::new(NSPoint::ZERO, NSSize::new(size.width + 2.0 * MARGIN, size.height + 2.0 * MARGIN)),
    );
    grid.setFrame(NSRect::new(NSPoint::new(MARGIN, MARGIN), size));
    container.addSubview(&grid);
    container
}

/// Две таблицы рядом, выровненные по верху.
fn side_by_side(mtm: MainThreadMarker, left: &NSGridView, right: &NSGridView) -> Retained<NSView> {
    const GAP: f64 = 40.0;
    let (l, r) = (left.fittingSize(), right.fittingSize());
    let size = NSSize::new(l.width + GAP + r.width, l.height.max(r.height));
    // Рамки ставятся вручную. В стеке короткая таблица растягивается до высоты длинной и разводит строки.
    let pair = NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::ZERO, size));
    left.setFrame(NSRect::new(NSPoint::new(0.0, size.height - l.height), l));
    right.setFrame(NSRect::new(NSPoint::new(l.width + GAP, size.height - r.height), r));
    pair.addSubview(left);
    pair.addSubview(right);
    pair.widthAnchor().constraintEqualToConstant(size.width).setActive(true);
    pair.heightAnchor().constraintEqualToConstant(size.height).setActive(true);
    pair
}

/// Подписи справа налево к контролам, заголовки и пояснения во всю ширину.
fn table(mtm: MainThreadMarker, rows: &[Row]) -> Retained<NSGridView> {
    let cells: Vec<_> = rows
        .iter()
        .map(|row| {
            let empty = NSGridCell::emptyContentView(mtm);
            let pair = match row {
                Row::Field(title, control) => [view(&label(mtm, title)), control.clone()],
                Row::Heading(title) => {
                    let heading = label(mtm, title);
                    heading.setFont(Some(&assets::semibold_font(FONT_SIZE)));
                    [view(&heading), empty]
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
        if let Row::Field(..) = row {
            continue;
        }
        grid.mergeCellsInHorizontalRange_verticalRange(NSRange::new(0, 2), NSRange::new(index, 1));
        grid.cellAtColumnIndex_rowIndex(0, index as isize).setXPlacement(NSGridCellPlacement::Leading);
        if let (Row::Heading(_), 1..) = (row, index) {
            grid.rowAtIndex(index as isize).setTopPadding(10.0);
        }
    }

    grid
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

fn checkbox(mtm: MainThreadMarker, title: &str, on: bool, target: &Controller, action: Sel) -> Retained<NSButton> {
    let button = unsafe {
        NSButton::checkboxWithTitle_target_action(&NSString::from_str(title), Some(target), Some(action), mtm)
    };
    button.setState(state(on));
    button.setFont(Some(&assets::font(FONT_SIZE)));
    button
}

/// Радиокнопки всех вариантов настройки в одну строку. Tag каждой - её номер в [`Choice::ALL`].
fn radios<T: Choice>(mtm: MainThreadMarker, selected: T, target: &Controller, action: Sel) -> Vec<Retained<NSView>> {
    T::ALL
        .iter()
        .enumerate()
        .map(|(index, choice)| {
            // Радиокнопки с общим action и общим родителем AppKit сам связывает в группу.
            let radio = unsafe {
                NSButton::radioButtonWithTitle_target_action(
                    &NSString::from_str(choice.title()),
                    Some(target),
                    Some(action),
                    mtm,
                )
            };
            radio.setTag(index as isize);
            radio.setState(state(*choice == selected));
            radio.setFont(Some(&assets::font(FONT_SIZE)));
            view(&radio)
        })
        .collect()
}

/// Сохраняет вариант настройки, выбранный радиокнопкой из [`radios`].
fn store_choice<T: Choice>(radio: &NSButton) {
    if let Some(choice) = usize::try_from(radio.tag()).ok().and_then(|index| T::ALL.get(index)) {
        settings::set_choice(*choice);
    }
}

fn view(view: &NSView) -> Retained<NSView> {
    view.retain()
}

/// После смены папки снимков предлагает добавить её снимки в библиотеку.
fn offer_reindex(mtm: MainThreadMarker, directory: std::path::PathBuf) {
    if !Settings::load().library {
        return;
    }
    let alert = NSAlert::new(mtm);
    alert.setMessageText(ns_string!("Add screenshots from the new folder to the library?"));
    alert.setInformativeText(ns_string!(
        "Kadro will index the images there and recognize their text in the background."
    ));
    alert.addButtonWithTitle(ns_string!("Add"));
    alert.addButtonWithTitle(ns_string!("Not Now"));
    if alert.runModal() == NSAlertFirstButtonReturn {
        output::in_background(move || library::import(&directory));
    }
}

/// Выбор языка распознавания: языки системы или один из языков Vision с английским названием.
fn recognition_languages(
    mtm: MainThreadMarker,
    selected: Option<&str>,
    target: &Controller,
) -> Retained<NSPopUpButton> {
    let popup = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
    let english = NSLocale::localeWithLocaleIdentifier(ns_string!("en"));
    let languages = recognize::languages();
    let titles = std::iter::once("System languages".to_string()).chain(languages.iter().map(|code| {
        let name = english.localizedStringForLocaleIdentifier(&NSString::from_str(code)).to_string();
        format!("{name} ({code})")
    }));
    for title in titles {
        popup.addItemWithTitle(&NSString::from_str(&title));
    }
    let index = selected.and_then(|code| languages.iter().position(|l| l == code)).map_or(0, |i| i + 1);
    popup.selectItemAtIndex(index as isize);
    popup.setFont(Some(&assets::font(FONT_SIZE)));
    unsafe {
        popup.setTarget(Some(target));
        popup.setAction(Some(sel!(changeRecognitionLanguage:)));
    }
    popup
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

/// Путь для подписи кнопки: домашняя папка сокращается до `~`.
fn display_path(path: &str) -> String {
    let home = std::env::var("HOME").ok();
    match home.as_deref().and_then(|home| path.strip_prefix(home)) {
        Some(rest) => format!("~{rest}"),
        None => path.to_string(),
    }
}
