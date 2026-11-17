//! quaked's menu bar icon: the Quake's state at a glance (a count when a Claude session
//! needs you) and a menu with status, pairing, the settings page, a page switcher for
//! the board, Restart and Quit. AppKit through objc2; main thread only.

use objc2::declare::ClassBuilder;
use objc2::rc::autoreleasepool;
use objc2::runtime::{AnyClass, AnyObject, Bool, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::NSString;
use std::sync::{Mutex, OnceLock};

/// What a menu item asks the app to do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    Settings,
    Restart,
    Quit,
    Pair(bool),
    /// Index into the page list the menu was built with.
    Page(usize),
}

/// What the menu shows; it's rebuilt only when this changes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Model {
    /// The Quake is attached somewhere (filled icon) or nowhere (outline).
    pub board: bool,
    /// Claude sessions waiting on you (shown next to the icon).
    pub waiting: usize,
    /// Status lines at the top of the menu (not clickable).
    pub lines: Vec<String>,
    /// A Mac asking to pair.
    pub pair: Option<String>,
    /// The board's pages, with the current one checked; empty when the board isn't here.
    pub pages: Vec<(String, bool)>,
}

type Sender = Box<dyn Fn(Action) + Send>;
static SEND: OnceLock<Mutex<Sender>> = OnceLock::new();

fn send(a: Action) {
    if let Some(s) = SEND.get() {
        (s.lock().unwrap())(a);
    }
}

extern "C" fn on_settings(_: *mut AnyObject, _: Sel, _: *mut AnyObject) {
    send(Action::Settings);
}
extern "C" fn on_restart(_: *mut AnyObject, _: Sel, _: *mut AnyObject) {
    send(Action::Restart);
}
extern "C" fn on_quit(_: *mut AnyObject, _: Sel, _: *mut AnyObject) {
    send(Action::Quit);
}
extern "C" fn on_allow(_: *mut AnyObject, _: Sel, _: *mut AnyObject) {
    send(Action::Pair(true));
}
extern "C" fn on_refuse(_: *mut AnyObject, _: Sel, _: *mut AnyObject) {
    send(Action::Pair(false));
}
extern "C" fn on_page(_: *mut AnyObject, _: Sel, sender: *mut AnyObject) {
    if sender.is_null() {
        return;
    }
    let tag: isize = unsafe { msg_send![sender, tag] };
    if tag >= 0 {
        send(Action::Page(tag as usize));
    }
}

/// The Objective-C object menu items call back into.
fn target_class() -> &'static AnyClass {
    static CLASS: OnceLock<&'static AnyClass> = OnceLock::new();
    CLASS.get_or_init(|| {
        let mut b = ClassBuilder::new("QuakedMenuTarget", class!(NSObject)).expect("menu target class");
        unsafe {
            b.add_method(sel!(openSettings:), on_settings as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject));
            b.add_method(sel!(restartQuaked:), on_restart as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject));
            b.add_method(sel!(quitQuaked:), on_quit as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject));
            b.add_method(sel!(allowPair:), on_allow as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject));
            b.add_method(sel!(refusePair:), on_refuse as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject));
            b.add_method(sel!(showPage:), on_page as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject));
        }
        b.register()
    })
}

fn symbol(name: &str) -> *mut AnyObject {
    let ns = NSString::from_str(name);
    let img: *mut AnyObject = unsafe { msg_send![class!(NSImage), imageWithSystemSymbolName: &*ns, accessibilityDescription: std::ptr::null_mut::<AnyObject>()] };
    if !img.is_null() {
        let _: () = unsafe { msg_send![img, setTemplate: Bool::YES] };
    }
    img
}

pub struct MenuBar {
    item: *mut AnyObject,
    target: *mut AnyObject,
    shown: Option<Model>,
}

impl MenuBar {
    /// Adds the icon to the menu bar. `on_action` runs on the main thread when an item is
    /// chosen.
    pub fn new(on_action: impl Fn(Action) + Send + 'static) -> Option<MenuBar> {
        let _ = SEND.set(Mutex::new(Box::new(on_action)));
        autoreleasepool(|_| unsafe {
            let bar: *mut AnyObject = msg_send![class!(NSStatusBar), systemStatusBar];
            // NSVariableStatusItemLength
            let item: *mut AnyObject = msg_send![bar, statusItemWithLength: -1.0f64];
            if item.is_null() {
                return None;
            }
            let _: *mut AnyObject = msg_send![item, retain];
            let target: *mut AnyObject = msg_send![target_class(), new];
            let mut m = MenuBar { item, target, shown: None };
            m.update(&Model::default());
            Some(m)
        })
    }

    /// Redraws the icon and rebuilds the menu if anything changed.
    pub fn update(&mut self, model: &Model) {
        if self.shown.as_ref() == Some(model) {
            return;
        }
        autoreleasepool(|_| unsafe {
            let button: *mut AnyObject = msg_send![self.item, button];
            if !button.is_null() {
                let img = symbol(if model.board { "dial.medium.fill" } else { "dial.medium" });
                let _: () = msg_send![button, setImage: img];
                let _: () = msg_send![button, setImagePosition: 2isize]; // NSImageLeft
                let title = NSString::from_str(&if model.waiting > 0 { format!(" {}", model.waiting) } else { String::new() });
                let _: () = msg_send![button, setTitle: &*title];
                let tip = NSString::from_str(&model.lines.join("\n"));
                let _: () = msg_send![button, setToolTip: &*tip];
            }
            let menu = self.build(model);
            let _: () = msg_send![self.item, setMenu: menu];
            let _: () = msg_send![menu, release];
        });
        self.shown = Some(model.clone());
    }

    unsafe fn add(&self, menu: *mut AnyObject, title: &str, action: Option<Sel>, key: &str, tag: isize, checked: bool, icon: Option<&str>) -> *mut AnyObject {
        let t = NSString::from_str(title);
        let k = NSString::from_str(key);
        let it: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        let it: *mut AnyObject = msg_send![it, initWithTitle: &*t, action: action, keyEquivalent: &*k];
        if action.is_some() {
            let _: () = msg_send![it, setTarget: self.target];
        }
        let _: () = msg_send![it, setEnabled: Bool::new(action.is_some())];
        let _: () = msg_send![it, setTag: tag];
        if checked {
            let _: () = msg_send![it, setState: 1isize];
        }
        if let Some(name) = icon {
            let img = symbol(name);
            if !img.is_null() {
                let _: () = msg_send![it, setImage: img];
            }
        }
        let _: () = msg_send![menu, addItem: it];
        let _: () = msg_send![it, release];
        it
    }

    unsafe fn separator(menu: *mut AnyObject) {
        let sep: *mut AnyObject = msg_send![class!(NSMenuItem), separatorItem];
        let _: () = msg_send![menu, addItem: sep];
    }

    unsafe fn build(&self, model: &Model) -> *mut AnyObject {
        let menu: *mut AnyObject = msg_send![class!(NSMenu), new];
        let _: () = msg_send![menu, setAutoenablesItems: Bool::NO];
        for line in &model.lines {
            self.add(menu, line, None, "", -1, false, None);
        }
        if let Some(who) = &model.pair {
            Self::separator(menu);
            self.add(menu, &format!("{who} wants to pair"), None, "", -1, false, Some("laptopcomputer.and.iphone"));
            self.add(menu, "Allow", Some(sel!(allowPair:)), "", -1, false, Some("checkmark.circle"));
            self.add(menu, "Refuse", Some(sel!(refusePair:)), "", -1, false, Some("xmark.circle"));
        }
        Self::separator(menu);
        self.add(menu, "Open Settings…", Some(sel!(openSettings:)), ",", -1, false, Some("gearshape"));
        if !model.pages.is_empty() {
            let sub: *mut AnyObject = msg_send![class!(NSMenu), new];
            let _: () = msg_send![sub, setAutoenablesItems: Bool::NO];
            for (i, (label, current)) in model.pages.iter().enumerate() {
                self.add(sub, label, Some(sel!(showPage:)), "", i as isize, *current, None);
            }
            let parent = self.add(menu, "Show on the Quake", None, "", -1, false, Some("dial.medium"));
            let _: () = msg_send![parent, setEnabled: Bool::YES];
            let _: () = msg_send![parent, setSubmenu: sub];
            let _: () = msg_send![sub, release];
        }
        Self::separator(menu);
        self.add(menu, "Restart quaked", Some(sel!(restartQuaked:)), "", -1, false, Some("arrow.clockwise"));
        self.add(menu, "Quit quaked", Some(sel!(quitQuaked:)), "q", -1, false, Some("power"));
        menu
    }
}

impl Drop for MenuBar {
    fn drop(&mut self) {
        unsafe {
            let bar: *mut AnyObject = msg_send![class!(NSStatusBar), systemStatusBar];
            let _: () = msg_send![bar, removeStatusItem: self.item];
            let _: () = msg_send![self.item, release];
            let _: () = msg_send![self.target, release];
        }
    }
}
