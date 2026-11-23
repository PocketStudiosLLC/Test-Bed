//! Browsing a phone's storage: pick files and folders to receive on this
//! PC (and where to save them), or pick the phone folder that sent files go
//! to.

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use ac_core::SlotId;
use ac_device::transfer::{human_bytes, receive_root};
use ac_device::{RemoteEntry, RemoteItem, UiCommand};
use adw::prelude::*;
use gtk::glib;

use crate::window::App;

const START: &str = "/sdcard";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Choose files and folders to copy to this PC.
    Receive,
    /// Choose the folder sent files go to.
    SendDir,
}

pub struct PhoneFiles {
    pub dialog: adw::Dialog,
    pub slot: SlotId,
    app: Weak<App>,
    mode: Mode,
    toasts: adw::ToastOverlay,
    path_entry: gtk::Entry,
    up: gtk::Button,
    hidden: gtk::ToggleButton,
    stack: gtk::Stack,
    status: adw::StatusPage,
    list: gtk::ListBox,
    action: gtk::Button,
    dest_row: adw::ActionRow,
    /// The folder shown (or being loaded).
    path: RefCell<String>,
    loaded: Cell<bool>,
    entries: RefCell<Vec<RemoteEntry>>,
    /// Names picked in the shown folder.
    selected: RefCell<BTreeSet<String>>,
    dest: RefCell<PathBuf>,
}

/// `/a/b/` → `/a/b`; `/` stays.
fn clean(path: &str) -> String {
    let t = path.trim();
    let t = t.trim_end_matches('/');
    match t {
        "" => "/".into(),
        t if t.starts_with('/') => t.into(),
        t => format!("/{t}"),
    }
}

fn parent(path: &str) -> Option<String> {
    let p = clean(path);
    if p == "/" {
        return None;
    }
    let (head, _) = p.rsplit_once('/')?;
    Some(if head.is_empty() { "/".into() } else { head.into() })
}

fn join(dir: &str, name: &str) -> String {
    let d = clean(dir);
    if d == "/" { format!("/{name}") } else { format!("{d}/{name}") }
}

fn when(mtime: u64) -> String {
    glib::DateTime::from_unix_local(mtime as i64).and_then(|d| d.format("%Y-%m-%d %H:%M")).map(|s| s.to_string()).unwrap_or_default()
}

/// `~/Downloads` for paths in the home folder.
fn show_local(p: &std::path::Path) -> String {
    match directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf()) {
        Some(home) if p.starts_with(&home) => format!("~/{}", p.strip_prefix(&home).unwrap().display()),
        _ => p.display().to_string(),
    }
}

impl PhoneFiles {
    /// `start`: the folder to open (`/sdcard` when `None`).
    pub fn new(app: &Rc<App>, slot: SlotId, title: &str, mode: Mode, start: Option<String>) -> Rc<Self> {
        let heading = match mode {
            Mode::Receive => format!("Receive from {title}"),
            Mode::SendDir => format!("Folder on {title}"),
        };
        let dialog = adw::Dialog::builder().title(&heading).content_width(520).content_height(640).build();
        let header = adw::HeaderBar::new();
        let up = gtk::Button::from_icon_name("go-up-symbolic");
        up.set_tooltip_text(Some("Parent folder (Alt+Up)"));
        let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
        refresh.set_tooltip_text(Some("Refresh"));
        let hidden = gtk::ToggleButton::builder().icon_name("view-reveal-symbolic").tooltip_text("Show hidden files").build();
        header.pack_start(&up);
        header.pack_start(&refresh);
        header.pack_end(&hidden);

        let path_entry = gtk::Entry::builder()
            .placeholder_text("/sdcard")
            .primary_icon_name("phone-symbolic")
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(6)
            .build();
        path_entry.update_property(&[gtk::accessible::Property::Label("Folder on the phone")]);

        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
        list.add_css_class("boxed-list");
        let clamp = adw::Clamp::builder().child(&list).margin_top(6).margin_bottom(12).margin_start(12).margin_end(12).build();
        let scroll = gtk::ScrolledWindow::builder().child(&clamp).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build();
        let status = adw::StatusPage::builder().icon_name("folder-symbolic").build();
        status.add_css_class("compact");
        let spinner = adw::Spinner::builder().width_request(32).height_request(32).halign(gtk::Align::Center).valign(gtk::Align::Center).build();
        let stack = gtk::Stack::new();
        stack.add_named(&scroll, Some("list"));
        stack.add_named(&status, Some("status"));
        stack.add_named(&spinner, Some("loading"));

        // Receive: where the files go on this PC.
        let dest = receive_root(&app.config().transfer.receive_dir);
        let dest_row = adw::ActionRow::builder().title("Save to").subtitle(show_local(&dest)).build();
        dest_row.set_subtitle_selectable(true);
        dest_row.add_prefix(&gtk::Image::from_icon_name("folder-download-symbolic"));
        let change = gtk::Button::builder().label("Change…").valign(gtk::Align::Center).build();
        dest_row.add_suffix(&change);
        let dest_list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
        dest_list.add_css_class("boxed-list");
        dest_list.append(&dest_row);

        let action = gtk::Button::builder().label(if mode == Mode::Receive { "Receive" } else { "Send Here" }).build();
        action.add_css_class("suggested-action");
        action.add_css_class("pill");
        let cancel = gtk::Button::builder().label("Cancel").build();
        cancel.add_css_class("pill");
        let buttons = gtk::Box::builder().spacing(12).halign(gtk::Align::End).build();
        buttons.append(&cancel);
        buttons.append(&action);
        let bottom = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).margin_top(12).margin_bottom(12).margin_start(12).margin_end(12).build();
        if mode == Mode::Receive {
            bottom.append(&dest_list);
        } else {
            let hint = gtk::Label::builder()
                .label("Files you send to this phone are saved in the folder shown above. Type a path to use a new folder.")
                .wrap(true)
                .xalign(0.0)
                .build();
            hint.add_css_class("dim-label");
            hint.add_css_class("caption");
            bottom.append(&hint);
        }
        bottom.append(&buttons);

        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.add_top_bar(&path_entry);
        view.set_content(Some(&stack));
        view.add_bottom_bar(&bottom);
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&view));
        dialog.set_child(Some(&toasts));

        let this = Rc::new(Self {
            dialog,
            slot,
            app: Rc::downgrade(app),
            mode,
            toasts,
            path_entry,
            up,
            hidden,
            stack,
            status,
            list,
            action,
            dest_row,
            path: RefCell::new(String::new()),
            loaded: Cell::new(false),
            entries: RefCell::default(),
            selected: RefCell::default(),
            dest: RefCell::new(dest),
        });

        let weak = Rc::downgrade(&this);
        let with = move |f: fn(&Rc<Self>)| {
            let weak = weak.clone();
            move || {
                if let Some(t) = weak.upgrade() {
                    f(&t);
                }
            }
        };
        let go_up = with(|t| {
            if let Some(p) = parent(&t.path.borrow().clone()) {
                t.open(&p);
            }
        });
        this.up.connect_clicked({
            let go_up = go_up.clone();
            move |_| go_up()
        });
        let reload = with(|t| t.open(&t.path.borrow().clone()));
        refresh.connect_clicked(move |_| reload());
        let typed = with(|t| t.open(&t.path_entry.text()));
        this.path_entry.connect_activate(move |_| typed());
        let rerender = with(|t| t.render());
        this.hidden.connect_toggled(move |_| rerender());
        let act = with(|t| t.act());
        this.action.connect_clicked(move |_| act());
        let choose = with(|t| t.choose_dest());
        change.connect_clicked(move |_| choose());
        {
            let dialog = this.dialog.clone();
            cancel.connect_clicked(move |_| {
                dialog.close();
            });
        }
        {
            let keys = gtk::EventControllerKey::new();
            keys.connect_key_pressed(move |_, key, _, mods| {
                if mods.contains(gtk::gdk::ModifierType::ALT_MASK) && key == gtk::gdk::Key::Up {
                    go_up();
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
            this.dialog.add_controller(keys);
        }
        {
            let app = Rc::downgrade(app);
            this.dialog.connect_closed(move |_| {
                if let Some(app) = app.upgrade() {
                    app.phone_files_closed(slot);
                }
            });
        }
        this.open(&start.unwrap_or_else(|| START.into()));
        this
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn path(&self) -> String {
        self.path.borrow().clone()
    }

    fn open(&self, path: &str) {
        let path = clean(path);
        *self.path.borrow_mut() = path.clone();
        self.path_entry.set_text(&path);
        self.up.set_sensitive(path != "/");
        self.selected.borrow_mut().clear();
        self.loaded.set(false);
        self.stack.set_visible_child_name("loading");
        self.update_action();
        if let Some(app) = self.app.upgrade() {
            app.send(UiCommand::ListRemote { slot: self.slot, path });
        }
    }

    /// The manager's answer to `ListRemote`; stale answers are ignored.
    pub fn show_listing(self: &Rc<Self>, path: &str, entries: Result<Vec<RemoteEntry>, String>) {
        if clean(path) != *self.path.borrow() {
            return;
        }
        match entries {
            Ok(e) => {
                *self.entries.borrow_mut() = e;
                self.loaded.set(true);
                self.render();
            }
            Err(why) => {
                self.entries.borrow_mut().clear();
                self.status.set_icon_name(Some("dialog-warning-symbolic"));
                self.status.set_title("Can't open this folder");
                self.status.set_description(Some(&glib::markup_escape_text(&why)));
                self.stack.set_visible_child_name("status");
            }
        }
        self.update_action();
    }

    fn render(self: &Rc<Self>) {
        while let Some(row) = self.list.first_child() {
            self.list.remove(&row);
        }
        let show_hidden = self.hidden.is_active();
        let entries = self.entries.borrow();
        let shown: Vec<&RemoteEntry> = entries
            .iter()
            .filter(|e| show_hidden || !e.name.starts_with('.'))
            .filter(|e| self.mode == Mode::Receive || e.is_dir || e.is_link)
            .collect();
        if !self.loaded.get() {
            return;
        }
        if shown.is_empty() {
            self.status.set_icon_name(Some("folder-symbolic"));
            self.status.set_title("Empty folder");
            let hidden = entries.len() - shown.len();
            self.status.set_description(match (hidden, self.mode) {
                (0, _) => None,
                (_, Mode::Receive) => Some("Only hidden files are here: show them with the eye button"),
                (_, Mode::SendDir) => Some("No folders here"),
            });
            self.stack.set_visible_child_name("status");
            return;
        }
        for e in shown {
            let row = adw::ActionRow::builder().title(&e.name).use_markup(false).activatable(true).build();
            let icon = match (e.is_dir, e.is_link) {
                (true, _) => "folder-symbolic",
                (_, true) => "emblem-symbolic-link-symbolic",
                _ => "text-x-generic-symbolic",
            };
            row.add_prefix(&gtk::Image::from_icon_name(icon));
            let date = when(e.mtime);
            let sub = match (e.is_dir, e.is_link) {
                (true, _) => date,
                (_, true) => "Link".into(),
                _ if date.is_empty() => human_bytes(e.size),
                _ => format!("{} · {date}", human_bytes(e.size)),
            };
            row.set_subtitle(&sub);
            let navigable = e.is_dir || e.is_link;
            let check = (self.mode == Mode::Receive && !e.is_link).then(|| {
                let c = gtk::CheckButton::builder().valign(gtk::Align::Center).build();
                c.update_property(&[gtk::accessible::Property::Label(&format!("Receive {}", e.name))]);
                let (weak, name) = (Rc::downgrade(self), e.name.clone());
                c.connect_toggled(move |c| {
                    if let Some(t) = weak.upgrade() {
                        if c.is_active() {
                            t.selected.borrow_mut().insert(name.clone());
                        } else {
                            t.selected.borrow_mut().remove(&name);
                        }
                        t.update_action();
                    }
                });
                c
            });
            if let Some(c) = &check {
                row.add_prefix(c);
            }
            if navigable {
                row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            }
            let (weak, name) = (Rc::downgrade(self), e.name.clone());
            row.connect_activated(move |_| {
                let Some(t) = weak.upgrade() else { return };
                if navigable {
                    let p = join(&t.path(), &name);
                    t.open(&p);
                } else if let Some(c) = &check {
                    c.set_active(!c.is_active());
                }
            });
            self.list.append(&row);
        }
        self.stack.set_visible_child_name("list");
    }

    fn update_action(&self) {
        match self.mode {
            Mode::Receive => {
                let n = self.selected.borrow().len();
                self.action.set_sensitive(n > 0);
                self.action.set_label(&match n {
                    0 | 1 => "Receive".to_owned(),
                    n => format!("Receive {n} Items"),
                });
            }
            // A typed folder that doesn't exist yet is fine: it is created.
            Mode::SendDir => self.action.set_sensitive(true),
        }
    }

    fn toast(&self, msg: &str) {
        self.toasts.add_toast(adw::Toast::new(msg));
    }

    fn act(&self) {
        let Some(app) = self.app.upgrade() else { return };
        match self.mode {
            Mode::Receive => {
                let dir = self.path();
                let entries = self.entries.borrow();
                let items: Vec<RemoteItem> = self
                    .selected
                    .borrow()
                    .iter()
                    .filter_map(|name| entries.iter().find(|e| &e.name == name))
                    .map(|e| RemoteItem { path: join(&dir, &e.name), is_dir: e.is_dir })
                    .collect();
                if items.is_empty() {
                    return;
                }
                app.send(UiCommand::ReceiveFiles { slot: self.slot, items, dest: self.dest.borrow().clone() });
            }
            Mode::SendDir => {
                // The typed path, even if not opened with Enter.
                let dir = clean(&self.path_entry.text());
                if let Some(why) = ac_config::check_target_dir(&dir) {
                    self.toast(&format!("The folder {why}"));
                    return;
                }
                app.set_send_dir(self.slot, dir);
            }
        }
        self.dialog.close();
    }

    fn choose_dest(self: &Rc<Self>) {
        let Some(app) = self.app.upgrade() else { return };
        let dialog = gtk::FileDialog::builder().title("Save Received Files To").accept_label("Select").modal(true).build();
        let current = self.dest.borrow().clone();
        if current.is_dir() {
            dialog.set_initial_folder(Some(&gtk::gio::File::for_path(&current)));
        }
        let weak = Rc::downgrade(self);
        dialog.select_folder(Some(app.window()), gtk::gio::Cancellable::NONE, move |r| {
            let (Some(t), Ok(f)) = (weak.upgrade(), r) else { return };
            let Some(p) = f.path() else { return };
            t.dest_row.set_subtitle(&show_local(&p));
            *t.dest.borrow_mut() = p.clone();
            // Remembered for next time.
            if let Some(app) = t.app.upgrade() {
                let mut cfg = app.config();
                cfg.transfer.receive_dir = show_local(&p);
                if let Err(e) = app.apply_config(cfg) {
                    t.toast(&format!("Used this time only: {e}"));
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(clean("/sdcard/"), "/sdcard");
        assert_eq!(clean(" sdcard/DCIM "), "/sdcard/DCIM");
        assert_eq!(clean("/"), "/");
        assert_eq!(parent("/sdcard/DCIM").as_deref(), Some("/sdcard"));
        assert_eq!(parent("/sdcard").as_deref(), Some("/"));
        assert_eq!(parent("/"), None);
        assert_eq!(join("/", "sdcard"), "/sdcard");
        assert_eq!(join("/sdcard/", "a b"), "/sdcard/a b");
    }
}
