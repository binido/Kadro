mod capture;
mod output;
mod overlay;
mod settings;

use std::cell::OnceCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSImage, NSMenu,
    NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{ns_string, NSNotification, NSObject, NSObjectProtocol, NSString};

#[derive(Default)]
struct Ivars {
    // Иконка исчезает из строки меню, как только NSStatusItem освобождается.
    status_item: OnceCell<Retained<NSStatusItem>>,
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
        }
    }

    impl AppDelegate {
        #[unsafe(method(captureRegion:))]
        fn capture_region(&self, _sender: Option<&AnyObject>) {
            overlay::select(self.mtm(), overlay::Mode::Region);
        }

        #[unsafe(method(captureWindow:))]
        fn capture_window(&self, _sender: Option<&AnyObject>) {
            overlay::select(self.mtm(), overlay::Mode::Window);
        }

        #[unsafe(method(captureScreen:))]
        fn capture_screen(&self, _sender: Option<&AnyObject>) {
            capture::capture_screen();
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
            let icon = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                ns_string!("camera.viewfinder"),
                Some(ns_string!("Kadro")),
            );
            button.setImage(icon.as_deref());
        }

        let menu = NSMenu::new(mtm);
        self.add_menu_item(&menu, "Capture Region", sel!(captureRegion:));
        self.add_menu_item(&menu, "Capture Full Screen", sel!(captureScreen:));
        self.add_menu_item(&menu, "Capture Window", sel!(captureWindow:));
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
        let _ = self.ivars().status_item.set(item);
    }

    fn add_menu_item(&self, menu: &NSMenu, title: &str, action: Sel) {
        let item = unsafe {
            menu.addItemWithTitle_action_keyEquivalent(
                &NSString::from_str(title),
                Some(action),
                ns_string!(""),
            )
        };
        unsafe { item.setTarget(Some(self)) };
    }
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
