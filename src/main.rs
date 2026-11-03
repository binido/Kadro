mod assets;
mod capture;
mod hotkeys;
mod output;
mod overlay;
mod prefs;
mod preview;
mod settings;

use std::cell::OnceCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSCellImagePosition, NSApplicationDelegate, NSEventModifierFlags, NSMenu,
    NSMenuDelegate, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{ns_string, NSNotification, NSObject, NSObjectProtocol, NSString};

use hotkeys::Action;

#[derive(Default)]
struct Ivars {}

thread_local! {
    // Иконка исчезает из строки меню, как только NSStatusItem освобождается.
    static STATUS_ITEM: OnceCell<Retained<NSStatusItem>> = const { OnceCell::new() };
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
    #[ivars = Ivars]
    struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            self.install_status_item();
            hotkeys::install();
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
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars::default());
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
        for (index, action) in Action::ALL.iter().enumerate() {
            let icon = match action {
                Action::Region => assets::SQUARE_DASHED,
                Action::RepeatRegion => assets::REPEAT,
                Action::Screen => assets::MONITOR,
                Action::DelayedScreen => assets::TIMER,
                Action::Window => assets::APP_WINDOW,
                Action::ActiveWindow => assets::APP_WINDOW_MAC,
            };
            let item = self.add_item(&menu, action.title(), sel!(runAction:), "", icon);
            item.setTag(index as isize + 1);
        }
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        self.add_item(&menu, "Settings…", sel!(showSettings:), ",", assets::SETTINGS);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        // Без target действие уходит по цепочке ответчиков и попадает в NSApplication.
        unsafe {
            menu.addItemWithTitle_action_keyEquivalent(ns_string!("Quit Kadro"), Some(sel!(terminate:)), ns_string!("q"))
        };
        item.setMenu(Some(&menu));
        STATUS_ITEM.with(|slot| {
            let _ = slot.set(item);
        });
    }

    fn add_item(&self, menu: &NSMenu, title: &str, action: objc2::runtime::Sel, key: &str, icon: &[u8]) -> Retained<NSMenuItem> {
        let item = unsafe {
            menu.addItemWithTitle_action_keyEquivalent(&NSString::from_str(title), Some(action), &NSString::from_str(key))
        };
        unsafe { item.setTarget(Some(self)) };
        item.setImage(assets::icon(icon, 16.0).as_deref());
        item
    }
}

/// Пункты действий помечены tag = индекс + 1, у остальных пунктов tag 0.
fn action_for_tag(tag: isize) -> Option<Action> {
    Action::ALL.get(usize::try_from(tag - 1).ok()?).copied()
}

fn main() {
    let mtm = MainThreadMarker::new().expect("AppKit starts on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    // Только иконка в строке меню, без значка в Dock.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let delegate = AppDelegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.run();
}
