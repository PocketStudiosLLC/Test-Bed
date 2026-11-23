//! Receive files: a browser of a remote PC's folders, opened from its
//! viewer. The PC lists one folder at a time; the files and folders ticked
//! are copied by the viewer to the folder chosen at the bottom (Downloads
//! unless changed; the choice is remembered).

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::rc::Rc;

use ac_remote::proto::{DirEntry, Listing};
use adw::prelude::*;
use gtk::glib;

/// Copy these entries of folder `dir` (name, is a folder) into a folder on
/// this PC.
pub type Take = Box<dyn Fn(String, Vec<(String, bool)>, PathBuf)>;

/// What the browser asks of its viewer.
pub struct Ask {
    /// List folder `dir` (home if empty), or its subfolder `name`; the
    /// answer comes back through [`Browser::listing`] with the same id.
    pub list: Box<dyn Fn(u64, String, Option<String>)>,
    pub take: Take,
    /// The dialog closed.
    pub closed: Box<dyn Fn()>,
}

#[derive(Default)]
struct State {
    /// The listing asked for last; older answers are ignored.
    waiting: u64,
    next_id: u64,
    listing: Option<Listing>,
    /// Rows shown, in order (hidden entries may be left out).
    shown: Vec<DirEntry>,
    picked: BTreeSet<String>,
    /// `Place` paths, in the dropdown's order (after "Go to…").
    places: Vec<String>,
}

pub struct Browser {
    dialog: adw::Dialog,
    toasts: adw::ToastOverlay,
    stack: gtk::Stack,
    status: adw::StatusPage,
    list: gtk::ListBox,
    path: gtk::Label,
    up: gtk::Button,
    places: gtk::DropDown,
    spinner: adw::Spinner,
    receive: gtk::Button,
    show_hidden: gtk::CheckButton,
    omitted: gtk::Label,
    dest_label: gtk::Label,
    dest: RefCell<PathBuf>,
    /// Setting the dropdown from code, not the user.
    quiet: Cell<bool>,
    state: RefCell<State>,
    ask: Ask,
}

impl Browser {
    pub fn open(parent: &impl IsA<gtk::Widget>, host_name: &str, ask: Ask) -> Rc<Self> {
        let up = gtk::Button::builder().icon_name("go-up-symbolic").tooltip_text("Parent folder").sensitive(false).build();
        let places = gtk::DropDown::from_strings(&["Go to…"]);
        places.set_tooltip_text(Some("Folders on this PC"));
        let receive = gtk::Button::builder().label("Receive").css_classes(["suggested-action"]).sensitive(false).build();
        let spinner = adw::Spinner::builder().visible(false).build();
        let header = adw::HeaderBar::new();
        header.pack_start(&up);
        header.pack_start(&places);
        header.pack_end(&receive);
        header.pack_end(&spinner);

        let path = gtk::Label::builder()
            .xalign(0.0)
            .selectable(true)
            .ellipsize(gtk::pango::EllipsizeMode::Start)
            .margin_start(12)
            .margin_end(12)
            .margin_top(6)
            .margin_bottom(6)
            .css_classes(["dim-label", "monospace"])
            .build();
        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["navigation-sidebar"]).build();
        let scroller = gtk::ScrolledWindow::builder().child(&list).vexpand(true).hscrollbar_policy(gtk::PolicyType::Never).build();
        let status = adw::StatusPage::builder().icon_name("folder-open-symbolic").title("Loading…").build();
        let stack = gtk::Stack::new();
        stack.add_named(&scroller, Some("list"));
        stack.add_named(&status, Some("status"));
        stack.set_visible_child_name("status");

        let show_hidden = gtk::CheckButton::with_label("Show hidden files");
        let omitted = gtk::Label::builder().hexpand(true).xalign(1.0).css_classes(["dim-label"]).build();
        let footer = gtk::Box::builder().spacing(12).margin_start(12).margin_end(12).margin_top(6).margin_bottom(6).build();
        footer.append(&show_hidden);
        footer.append(&omitted);

        // Where the files go on this PC.
        let dest = receive_dir();
        let dest_label = gtk::Label::builder()
            .label(dest.display().to_string())
            .tooltip_text(dest.display().to_string())
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::Start)
            .css_classes(["monospace"])
            .build();
        let change = gtk::Button::builder().label("Change…").tooltip_text("Choose the folder on this PC the files are saved in").build();
        let save_row = gtk::Box::builder().spacing(12).margin_start(12).margin_end(12).margin_top(6).margin_bottom(6).build();
        save_row.append(&gtk::Image::from_icon_name("folder-download-symbolic"));
        save_row.append(&gtk::Label::builder().label("Save to").css_classes(["dim-label"]).build());
        save_row.append(&dest_label);
        save_row.append(&change);

        let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
        body.append(&path);
        body.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        body.append(&stack);
        body.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        body.append(&footer);
        body.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        body.append(&save_row);
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&body));
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&toasts));
        let dialog = adw::Dialog::builder()
            .title(format!("Receive files from {host_name}"))
            .content_width(600)
            .content_height(600)
            .child(&view)
            .build();

        let b = Rc::new(Self {
            dialog,
            toasts,
            stack,
            status,
            list,
            path,
            up,
            places,
            spinner,
            receive,
            show_hidden,
            omitted,
            dest_label,
            dest: RefCell::new(dest),
            quiet: Cell::new(false),
            state: RefCell::default(),
            ask,
        });

        let weak = Rc::downgrade(&b);
        b.up.connect_clicked(move |_| {
            if let Some(b) = weak.upgrade() {
                let parent = b.state.borrow().listing.as_ref().and_then(|l| l.parent.clone());
                if let Some(p) = parent {
                    b.request(p, None);
                }
            }
        });
        let weak = Rc::downgrade(&b);
        b.places.connect_selected_notify(move |d| {
            let Some(b) = weak.upgrade() else { return };
            if b.quiet.get() || d.selected() == 0 {
                return;
            }
            let path = b.state.borrow().places.get(d.selected() as usize - 1).cloned();
            if let Some(p) = path {
                b.request(p, None);
            }
            b.reset_places();
        });
        let weak = Rc::downgrade(&b);
        b.list.connect_row_activated(move |_, row| {
            let Some(b) = weak.upgrade() else { return };
            let entry = b.state.borrow().shown.get(row.index() as usize).cloned();
            match entry {
                Some(e) if e.dir => {
                    let dir = b.state.borrow().listing.as_ref().map(|l| l.path.clone()).unwrap_or_default();
                    b.request(dir, Some(e.name));
                }
                Some(_) => {
                    if let Some(check) = row.child().and_then(|c| c.last_child()).and_downcast::<gtk::CheckButton>() {
                        check.set_active(!check.is_active());
                    }
                }
                None => {}
            }
        });
        let weak = Rc::downgrade(&b);
        b.show_hidden.connect_toggled(move |_| {
            if let Some(b) = weak.upgrade() {
                b.fill();
            }
        });
        let weak = Rc::downgrade(&b);
        b.receive.connect_clicked(move |_| {
            let Some(b) = weak.upgrade() else { return };
            let (dir, items) = {
                let s = b.state.borrow();
                let entries = s.listing.as_ref().map(|l| l.entries.as_slice()).unwrap_or_default();
                let items: Vec<(String, bool)> =
                    s.picked.iter().filter_map(|n| entries.iter().find(|e| &e.name == n)).map(|e| (e.name.clone(), e.dir)).collect();
                (s.listing.as_ref().map(|l| l.path.clone()).unwrap_or_default(), items)
            };
            if !items.is_empty() {
                (b.ask.take)(dir, items, b.dest.borrow().clone());
                b.dialog.close();
            }
        });
        let weak = Rc::downgrade(&b);
        change.connect_clicked(move |_| {
            if let Some(b) = weak.upgrade() {
                b.choose_dest();
            }
        });
        let weak = Rc::downgrade(&b);
        b.dialog.connect_closed(move |_| {
            if let Some(b) = weak.upgrade() {
                (b.ask.closed)();
            }
        });

        b.dialog.present(Some(parent));
        b.request(String::new(), None);
        b
    }

    /// Asks for the folder on this PC to save in, and remembers it.
    fn choose_dest(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder().title("Save Received Files To").accept_label("Select").modal(true).build();
        let current = self.dest.borrow().clone();
        if current.is_dir() {
            dialog.set_initial_folder(Some(&gtk::gio::File::for_path(&current)));
        }
        let parent = self.dialog.root().and_downcast::<gtk::Window>();
        let weak = Rc::downgrade(self);
        dialog.select_folder(parent.as_ref(), gtk::gio::Cancellable::NONE, move |r| {
            let (Some(b), Ok(f)) = (weak.upgrade(), r) else { return };
            let Some(p) = f.path() else { return };
            b.dest_label.set_label(&p.display().to_string());
            b.dest_label.set_tooltip_text(Some(&p.display().to_string()));
            *b.dest.borrow_mut() = p.clone();
            let mut cfg = ac_remote::config::ClientConfig::load().unwrap_or_default();
            cfg.receive_dir = p.display().to_string();
            if let Err(e) = cfg.save() {
                b.toasts.add_toast(adw::Toast::new(&format!("Used this time only: {e:#}")));
            }
        });
    }

    pub fn close(&self) {
        self.dialog.close();
    }

    fn request(&self, dir: String, name: Option<String>) {
        let id = {
            let mut s = self.state.borrow_mut();
            s.next_id += 1;
            s.waiting = s.next_id;
            s.waiting
        };
        self.spinner.set_visible(true);
        (self.ask.list)(id, dir, name);
    }

    /// The PC's answer to a listing request.
    pub fn listing(self: &Rc<Self>, id: u64, result: Result<Listing, String>) {
        if id != self.state.borrow().waiting {
            return;
        }
        self.spinner.set_visible(false);
        let listing = match result {
            Ok(l) => l,
            Err(e) => {
                if self.state.borrow().listing.is_some() {
                    self.toasts.add_toast(adw::Toast::builder().title(glib::markup_escape_text(&e)).timeout(5).build());
                } else {
                    self.status.set_icon_name(Some("dialog-warning-symbolic"));
                    self.status.set_title("Can't show this PC's files");
                    self.status.set_description(Some(&glib::markup_escape_text(&e)));
                    self.stack.set_visible_child_name("status");
                }
                return;
            }
        };
        if self.state.borrow().places.is_empty() && !listing.places.is_empty() {
            let names: Vec<&str> = std::iter::once("Go to…").chain(listing.places.iter().map(|p| p.name.as_str())).collect();
            self.quiet.set(true);
            self.places.set_model(Some(&gtk::StringList::new(&names)));
            self.quiet.set(false);
            self.state.borrow_mut().places = listing.places.iter().map(|p| p.path.clone()).collect();
        }
        self.path.set_label(&listing.path);
        self.path.set_tooltip_text(Some(&listing.path));
        self.up.set_sensitive(listing.parent.is_some());
        self.omitted.set_label(&match listing.omitted {
            0 => String::new(),
            n => format!("{n} more not shown"),
        });
        {
            let mut s = self.state.borrow_mut();
            s.listing = Some(listing);
            s.picked.clear();
        }
        self.fill();
    }

    /// Back to "Go to…", once the dropdown's handler has returned.
    fn reset_places(&self) {
        let d = self.places.clone();
        glib::idle_add_local_once(move || d.set_selected(0));
    }

    fn update_receive(&self) {
        let n = self.state.borrow().picked.len();
        self.receive.set_sensitive(n > 0);
        self.receive.set_label(&match n {
            0 | 1 => "Receive".to_string(),
            n => format!("Receive {n} items"),
        });
    }

    /// Rows for the current listing.
    fn fill(self: &Rc<Self>) {
        while let Some(row) = self.list.first_child() {
            self.list.remove(&row);
        }
        let hidden = self.show_hidden.is_active();
        let shown: Vec<DirEntry> = {
            let s = self.state.borrow();
            s.listing.iter().flat_map(|l| l.entries.iter()).filter(|e| hidden || !e.hidden).cloned().collect()
        };
        // Ticks on files no longer shown don't count.
        self.state.borrow_mut().picked.retain(|n| shown.iter().any(|e| &e.name == n));
        for e in &shown {
            self.list.append(&self.row(e));
        }
        let empty = shown.is_empty();
        self.state.borrow_mut().shown = shown;
        if empty {
            self.status.set_icon_name(Some("folder-symbolic"));
            self.status.set_title("Nothing here");
            self.status.set_description(None);
            self.stack.set_visible_child_name("status");
        } else {
            self.stack.set_visible_child_name("list");
        }
        self.update_receive();
    }

    fn row(self: &Rc<Self>, e: &DirEntry) -> gtk::ListBoxRow {
        let icon = gtk::Image::from_icon_name(if e.dir { "folder-symbolic" } else { "text-x-generic-symbolic" });
        let name = gtk::Label::builder().label(&e.name).xalign(0.0).hexpand(true).ellipsize(gtk::pango::EllipsizeMode::Middle).build();
        if e.hidden {
            name.add_css_class("dim-label");
        }
        let when = e
            .modified
            .and_then(|t| glib::DateTime::from_unix_local(t as i64).ok())
            .and_then(|d| d.format("%e %b %Y").ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let detail = if e.dir { when } else { format!("{} · {when}", glib::format_size(e.size)) };
        let detail = gtk::Label::builder().label(detail.trim_end_matches(" · ")).css_classes(["dim-label", "numeric"]).build();
        let line = gtk::Box::builder().spacing(10).margin_top(4).margin_bottom(4).margin_start(4).margin_end(4).build();
        line.append(&icon);
        line.append(&name);
        line.append(&detail);
        {
            // Folders can be ticked too: they come with everything in them.
            let check = gtk::CheckButton::new();
            check.set_tooltip_text(Some(if e.dir { "Receive this folder and everything in it" } else { "Receive this file" }));
            check.update_property(&[gtk::accessible::Property::Label(&format!("Receive {}", e.name))]);
            check.set_active(self.state.borrow().picked.contains(&e.name));
            let file = e.name.clone();
            let weak = Rc::downgrade(self);
            check.connect_toggled(move |c| {
                let Some(b) = weak.upgrade() else { return };
                {
                    let mut s = b.state.borrow_mut();
                    if c.is_active() {
                        s.picked.insert(file.clone());
                    } else {
                        s.picked.remove(&file);
                    }
                }
                b.update_receive();
            });
            line.append(&check);
        }
        if e.dir {
            line.append(&gtk::Image::from_icon_name("go-next-symbolic"));
        }
        gtk::ListBoxRow::builder().child(&line).activatable(true).build()
    }
}

/// The folder received files go to: the one chosen last, or Downloads.
pub fn receive_dir() -> PathBuf {
    let chosen = ac_remote::config::ClientConfig::load().map(|c| c.receive_dir).unwrap_or_default();
    let chosen = chosen.trim();
    if chosen.is_empty() { ac_remote::files::downloads_dir() } else { PathBuf::from(chosen) }
}
