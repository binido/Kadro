//! Kadro - снимки экрана из строки меню macOS.
//!
//! Здесь живут точка входа и пункт в строке меню. Захват, вывод и окна разнесены по модулям.

mod annotation;
mod assets;
mod backdrop;
mod bridge;
mod browser;
mod capture;
mod document;
mod editor;
mod geometry;
mod hotkeys;
mod keys;
mod library;
mod links;
mod measure;
mod output;
mod overlay;
mod paint;
mod pin;
mod prefs;
mod preview;
mod recognize;
mod scroll;
mod settings;
mod sigv4;
mod stitch;
mod updates;
mod upload;

use std::cell::OnceCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSCellImagePosition, NSEvent,
    NSEventModifierFlags, NSMenu, NSMenuDelegate, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{NSArray, NSNotification, NSObject, NSObjectProtocol, NSString, NSURL, ns_string};

use hotkeys::Action;

thread_local! {
    // Иконка исчезает из строки меню, как только NSStatusItem освобождается.
    static STATUS_ITEM: OnceCell<Retained<NSStatusItem>> = const { OnceCell::new() };
    // Меню иконки. Сочетание Show Kadro Menu показывает его у курсора, даже если иконка скрыта.
    static MENU: OnceCell<Retained<NSMenu>> = const { OnceCell::new() };
}

/// Показывает или прячет иконку в строке меню.
pub fn set_icon_visible(visible: bool) {
    STATUS_ITEM.with(|item| {
        if let Some(item) = item.get() {
            item.setVisible(visible);
        }
    });
}

/// Открывает меню Kadro у курсора.
pub fn show_menu(mtm: MainThreadMarker) {
    MENU.with(|menu| {
        if let Some(menu) = menu.get() {
            NSApplication::sharedApplication(mtm).activate();
            menu.popUpMenuPositioningItem_atLocation_inView(None, NSEvent::mouseLocation(), None);
        }
    });
}

/// Показывает секунды отсчёта рядом с иконкой в строке меню, None - убирает.
pub fn show_countdown(seconds: Option<u32>) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    STATUS_ITEM.with(|item| {
        if let Some(button) = item.get().and_then(|item| item.button(mtm)) {
            let title = seconds.map(|s| format!(" {s}")).unwrap_or_default();
            button.setTitle(&NSString::from_str(&title));
        }
    });
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            self.install_status_item();
            hotkeys::install();
            updates::check_on_launch();
        }

        // Картинки, открытые через Kadro из Finder или перетащенные на его значок, и ссылки kadro://.
        #[unsafe(method(application:openURLs:))]
        fn open_urls(&self, _application: &NSApplication, urls: &NSArray<NSURL>) {
            for url in urls {
                if url.scheme().is_some_and(|scheme| scheme.to_string() == links::SCHEME) {
                    let text = url.absoluteString().map(|s| s.to_string()).unwrap_or_default();
                    links::handle(self.mtm(), &text);
                } else if let Some(path) = url.path() {
                    editor::open_file(self.mtm(), std::path::Path::new(&path.to_string()));
                }
            }
        }

        // Повторный запуск уже открытого Kadro - путь к настройкам, когда иконка скрыта.
        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn should_handle_reopen(&self, _application: &NSApplication, _visible: bool) -> bool {
            prefs::show(self.mtm());
            true
        }
    }

    unsafe impl NSMenuDelegate for AppDelegate {
        // Сочетания могли поменяться в настройках, поэтому подписи обновляются при каждом открытии.
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &NSMenu) {
            for item in menu.itemArray() {
                let Some(action) = action_for_tag(item.tag()) else { continue };
                match action.shortcut() {
                    Some(shortcut) => {
                        let key = shortcut.key_name().filter(|k| k.chars().count() == 1).unwrap_or("");
                        item.setKeyEquivalent(&NSString::from_str(&key.to_lowercase()));
                        item.setKeyEquivalentModifierMask(shortcut.menu_modifiers());
                    }
                    None => {
                        item.setKeyEquivalent(ns_string!(""));
                        item.setKeyEquivalentModifierMask(NSEventModifierFlags::empty());
                    }
                }
            }
        }
    }

    impl AppDelegate {
        #[unsafe(method(runAction:))]
        fn run_action(&self, sender: &NSMenuItem) {
            if let Some(action) = action_for_tag(sender.tag()) {
                action.run(self.mtm());
            }
        }

        #[unsafe(method(showSettings:))]
        fn show_settings(&self, _sender: Option<&AnyObject>) {
            prefs::show(self.mtm());
        }

        #[unsafe(method(checkForUpdates:))]
        fn check_for_updates(&self, _sender: Option<&AnyObject>) {
            updates::check(true);
        }

        #[unsafe(method(openFile:))]
        fn open_file(&self, _sender: Option<&AnyObject>) {
            editor::choose_file(self.mtm());
        }

        #[unsafe(method(openClipboard:))]
        fn open_clipboard(&self, _sender: Option<&AnyObject>) {
            editor::open_clipboard(self.mtm());
        }
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }

    fn install_status_item(&self) {
        let mtm = self.mtm();
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        if let Some(button) = item.button(mtm) {
            button.setImage(assets::icon(assets::APERTURE, 18.0).as_deref());
            // Иначе секунды отсчёта заменили бы иконку, а не встали рядом.
            button.setImagePosition(NSCellImagePosition::ImageLeading);
        }

        let menu = NSMenu::new(mtm);
        menu.setDelegate(Some(ProtocolObject::from_ref(self)));
        for (index, action) in Action::ALL.into_iter().enumerate() {
            // Это сочетание и так открывает само меню.
            if action == Action::ShowMenu {
                continue;
            }
            let item = self.add_item(&menu, action.title(), sel!(runAction:), "", action_icon(action));
            item.setTag(tag_for_action(index));
        }
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        self.add_item(&menu, "Open File…", sel!(openFile:), "o", assets::FOLDER_OPEN);
        self.add_item(&menu, "Open from Clipboard", sel!(openClipboard:), "", assets::CLIPBOARD_PASTE);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        self.add_item(&menu, "Check for Updates…", sel!(checkForUpdates:), "", assets::REPEAT);
        self.add_item(&menu, "Settings…", sel!(showSettings:), ",", assets::SETTINGS);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        // Без target действие уходит по цепочке ответчиков и попадает в NSApplication.
        unsafe {
            menu.addItemWithTitle_action_keyEquivalent(
                ns_string!("Quit Kadro"),
                Some(sel!(terminate:)),
                ns_string!("q"),
            )
        };
        item.setMenu(Some(&menu));
        item.setVisible(!settings::Settings::load().hide_icon);
        STATUS_ITEM.with(|slot| {
            let _ = slot.set(item);
        });
        MENU.with(|slot| {
            let _ = slot.set(menu);
        });
    }

    fn add_item(&self, menu: &NSMenu, title: &str, action: Sel, key: &str, icon: &[u8]) -> Retained<NSMenuItem> {
        let item = unsafe {
            menu.addItemWithTitle_action_keyEquivalent(
                &NSString::from_str(title),
                Some(action),
                &NSString::from_str(key),
            )
        };
        unsafe { item.setTarget(Some(self)) };
        item.setImage(assets::icon(icon, 16.0).as_deref());
        item
    }
}

fn action_icon(action: Action) -> &'static [u8] {
    match action {
        Action::Region => assets::SQUARE_DASHED,
        Action::RepeatRegion => assets::REPEAT,
        Action::Screen => assets::MONITOR,
        Action::DelayedScreen => assets::TIMER,
        Action::Window => assets::APP_WINDOW,
        Action::ActiveWindow => assets::APP_WINDOW_MAC,
        Action::PinLast => assets::PIN,
        Action::EditLast => assets::SQUARE_PEN,
        Action::RecognizeText => assets::SCAN_TEXT,
        Action::ScrollCapture => assets::SCROLL,
        Action::UploadLast => assets::CLOUD_UPLOAD,
        Action::OpenLibrary => assets::LIBRARY,
        Action::ShowMenu => assets::APERTURE,
    }
}

/// Tag пункта меню для действия с этим номером в [`Action::ALL`].
/// Номер сдвинут на единицу: у остальных пунктов меню tag 0.
fn tag_for_action(index: usize) -> isize {
    isize::try_from(index).map_or(0, |index| index + 1)
}

/// Действие пункта меню по его tag, обратно к [`tag_for_action`].
fn action_for_tag(tag: isize) -> Option<Action> {
    Action::ALL.get(usize::try_from(tag - 1).ok()?).copied()
}

fn main() {
    // Тот же исполняемый файл распознаёт текст в отдельном процессе и сразу выходит.
    if recognize::run_child() {
        return;
    }
    // Rust вызывает main на главном потоке процесса, поэтому маркер есть всегда.
    let mtm = MainThreadMarker::new().expect("main runs on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    // Только иконка в строке меню, без значка в Dock.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let delegate = AppDelegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.run();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_is_found_by_its_menu_tag() {
        for (index, action) in Action::ALL.into_iter().enumerate() {
            assert_eq!(action_for_tag(tag_for_action(index)), Some(action));
        }
        // Tag 0 - у пунктов без действия, вроде Settings и Quit.
        assert_eq!(action_for_tag(0), None);
    }
}
