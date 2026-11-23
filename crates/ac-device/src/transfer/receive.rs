//! Browsing the phone's storage and copying files and folders from it to
//! this PC, over ADB's `sync:` service (`LIST` and `RECV`).
//!
//! Phones reached only through the helper (no ADB) can't be browsed: the
//! helper has no way to read other apps' files.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ac_adb::sync::{DirEntry, SyncSession};
use ac_core::SlotId;
use tokio::net::TcpStream;

use super::{Ctx, Link, REPORT_EVERY, Transfers, human_bytes};
use crate::api::{RemoteEntry, RemoteItem, TransferView, UiEvent};

const NO_ADB: &str = "Browsing the phone's files needs a wireless-debugging (ADB) connection to it";
const NOT_CONNECTED: &str = "Connect this phone first";
/// Folders nested deeper than this are not followed (a guard against loops).
const MAX_DEPTH: usize = 64;

/// Where received files go: `configured` (`~/` is the home folder), or the
/// Downloads folder when empty.
pub fn receive_root(configured: &str) -> PathBuf {
    let configured = configured.trim();
    if !configured.is_empty() {
        if let Some(rest) = configured.strip_prefix("~/")
            && let Some(home) = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf())
        {
            return home.join(rest);
        }
        return PathBuf::from(configured);
    }
    let dirs = directories::UserDirs::new();
    match dirs.as_ref().and_then(|d| d.download_dir()) {
        Some(d) => d.to_path_buf(),
        None => dirs.map_or_else(|| PathBuf::from("Downloads"), |d| d.home_dir().join("Downloads")),
    }
}

/// `/a/b/` → `/a/b`, keeping `/` itself.
fn trim_dir(path: &str) -> &str {
    let t = path.trim_end_matches('/');
    if t.is_empty() { "/" } else { t }
}

fn child(dir: &str, name: &str) -> String {
    let d = trim_dir(dir);
    if d == "/" { format!("/{name}") } else { format!("{d}/{name}") }
}

/// A folder path for `LIST`: the trailing `/` makes adbd follow a symlink
/// such as `/sdcard`.
fn list_path(dir: &str) -> String {
    let d = trim_dir(dir);
    if d == "/" { "/".into() } else { format!("{d}/") }
}

fn entry(e: DirEntry) -> RemoteEntry {
    RemoteEntry { is_dir: e.is_dir(), is_link: e.is_symlink(), size: e.size as u64, mtime: e.mtime as u64, name: e.name }
}

/// Folders first, then by name, ignoring case.
pub fn sort_entries(entries: &mut [RemoteEntry]) {
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())).then_with(|| a.name.cmp(&b.name)));
}

/// `name`, or `name (1)`, `name (2)`, … whichever is free in `dir`.
fn free_local(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if std::fs::symlink_metadata(&first).is_err() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (1..).map(|n| dir.join(format!("{stem} ({n}){ext}"))).find(|p| std::fs::symlink_metadata(p).is_err()).expect("unbounded")
}

/// One file to copy.
struct Pull {
    remote: String,
    local: PathBuf,
    size: u64,
    mtime: u32,
}

/// What a receive will do, worked out before the first byte.
#[derive(Default)]
struct Plan {
    dirs: Vec<PathBuf>,
    files: Vec<Pull>,
    /// Items left alone (`on_conflict = "skip"`), and symlinks inside folders.
    skipped: u32,
}

/// Removes a half-written file unless it was finished (also on cancel).
struct PartGuard(Option<PathBuf>);

impl Drop for PartGuard {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

impl Transfers {
    fn link(&self, slot: SlotId) -> Option<Link> {
        self.links.get((slot as usize).wrapping_sub(1)).and_then(|tx| tx.borrow().clone())
    }

    /// Lists `path` on the phone; answers with `UiEvent::RemoteDir`.
    pub fn list_remote(&self, slot: SlotId, path: String) {
        let (ctx, link) = (self.ctx.clone(), self.link(slot));
        tokio::spawn(async move {
            let entries = match link {
                None => Err(NOT_CONNECTED.to_owned()),
                Some(l) if l.direct.is_some() => Err(NO_ADB.to_owned()),
                Some(l) => ctx.list_remote(&l, &path).await,
            };
            let _ = ctx.ui.send(UiEvent::RemoteDir { slot, path, entries }).await;
        });
    }

    /// Copies `items` from the phone in `slot` into `dest`.
    pub fn receive(&self, slot: SlotId, title: String, items: Vec<RemoteItem>, dest: PathBuf) {
        let Some(s) = self.ctx.slot(slot) else { return };
        let link = match self.link(slot) {
            None => return self.ctx.toast(NOT_CONNECTED.into()),
            Some(l) if l.direct.is_some() => return self.ctx.toast(NO_ADB.into()),
            Some(l) => l,
        };
        if items.is_empty() {
            return;
        }
        let mut running = s.receive.lock().unwrap();
        if running.is_some() {
            return self.ctx.toast(format!("Already receiving files from {title}; wait for it to finish or cancel it"));
        }
        let ctx = self.ctx.clone();
        let h = tokio::spawn(async move {
            let result = ctx.receive(slot, &link, &title, &items, &dest).await;
            if let Some(s) = ctx.slot(slot) {
                s.receive.lock().unwrap().take();
            }
            ctx.receive_view(TransferView { slot, active: false, receiving: true, ..idle(slot) });
            match result {
                Ok(msg) => {
                    let _ = ctx.ui.send(UiEvent::Saved { message: msg, folder: dest }).await;
                }
                Err(e) => ctx.toast(e),
            }
        });
        *running = Some(h.abort_handle());
    }

    /// Cancels a running receive; true if there was one.
    pub(super) fn cancel_receive(&self, slot: SlotId) -> bool {
        let Some(s) = self.ctx.slot(slot) else { return false };
        let Some(h) = s.receive.lock().unwrap().take() else { return false };
        h.abort();
        self.ctx.receive_view(TransferView { slot, active: false, receiving: true, ..idle(slot) });
        true
    }
}

fn idle(slot: SlotId) -> TransferView {
    TransferView {
        slot,
        active: false,
        file: String::new(),
        installing: false,
        receiving: true,
        index: 0,
        count: 0,
        sent: 0,
        total: 0,
        rate_bps: 0,
        eta_s: None,
        waiting: false,
    }
}

fn adb_msg(e: ac_adb::AdbError) -> String {
    match e {
        ac_adb::AdbError::Fail(m) => m,
        other => other.to_string(),
    }
}

impl Ctx {
    async fn open_sync(&self, link: &Link) -> Result<SyncSession<TcpStream>, String> {
        self.adb.sync(&link.serial).await.map_err(|e| format!("The phone didn't answer: {}", adb_msg(e)))
    }

    async fn list_remote(&self, link: &Link, path: &str) -> Result<Vec<RemoteEntry>, String> {
        let mut sync = self.open_sync(link).await?;
        let entries = sync.list(&list_path(path)).await.map_err(adb_msg)?;
        if entries.is_empty() {
            // LIST answers a missing or unreadable folder with nothing.
            let st = sync.stat(&list_path(path)).await.map_err(adb_msg)?;
            if !st.exists() {
                let _ = sync.quit().await;
                return Err(format!("There is no folder {} on the phone", trim_dir(path)));
            }
        }
        let _ = sync.quit().await;
        let mut out: Vec<RemoteEntry> = entries.into_iter().map(entry).collect();
        sort_entries(&mut out);
        Ok(out)
    }

    fn receive_view(&self, v: TransferView) {
        let _ = self.ui.try_send(UiEvent::Transfer(v));
    }

    /// Works out every file and folder to create, applying `on_conflict` to
    /// the top-level items.
    async fn plan(&self, sync: &mut SyncSession<TcpStream>, items: &[RemoteItem], dest: &Path) -> Result<Plan, String> {
        let conflict = self.cfg().on_conflict;
        let mut plan = Plan::default();
        for item in items {
            let path = trim_dir(&item.path);
            let Some(name) = path.rsplit('/').next().filter(|n| !n.is_empty() && *n != "." && *n != "..") else { continue };
            let exists = std::fs::symlink_metadata(dest.join(name)).is_ok();
            let local = match (exists, conflict.as_str()) {
                (true, "skip") => {
                    plan.skipped += 1;
                    continue;
                }
                // Folders merge into the existing one, replacing files in it.
                (true, "overwrite") => dest.join(name),
                (true, _) => free_local(dest, name),
                (false, _) => dest.join(name),
            };
            if !item.is_dir {
                let st = sync.stat(path).await.map_err(adb_msg)?;
                plan.files.push(Pull { remote: path.to_owned(), local, size: st.size as u64, mtime: st.mtime });
                continue;
            }
            // Walk the folder; symlinks inside are left out (they may loop).
            let mut queue = vec![(path.to_owned(), local, 0usize)];
            while let Some((dir, local_dir, depth)) = queue.pop() {
                plan.dirs.push(local_dir.clone());
                if depth >= MAX_DEPTH {
                    continue;
                }
                for e in sync.list(&list_path(&dir)).await.map_err(adb_msg)? {
                    let remote = child(&dir, &e.name);
                    let local = local_dir.join(&e.name);
                    if e.is_dir() {
                        queue.push((remote, local, depth + 1));
                    } else if e.is_file() {
                        plan.files.push(Pull { remote, local, size: e.size as u64, mtime: e.mtime });
                    } else {
                        plan.skipped += 1;
                    }
                }
            }
        }
        Ok(plan)
    }

    async fn receive(&self, slot: SlotId, link: &Link, title: &str, items: &[RemoteItem], dest: &Path) -> Result<String, String> {
        tokio::fs::create_dir_all(dest).await.map_err(|e| format!("Can't create {}: {e}", dest.display()))?;
        let mut sync = self.open_sync(link).await?;
        let mut view = TransferView { active: true, file: "Looking at the phone's files…".into(), ..idle(slot) };
        self.receive_view(view.clone());
        let plan = self.plan(&mut sync, items, dest).await?;
        for d in &plan.dirs {
            tokio::fs::create_dir_all(d).await.map_err(|e| format!("Can't create {}: {e}", d.display()))?;
        }
        view.count = plan.files.len() as u32;
        view.total = plan.files.iter().map(|f| f.size).sum();
        let started = Instant::now();
        let mut last_report = Instant::now();
        let (mut done_bytes, mut received, mut failed) = (0u64, 0u32, Vec::<(String, String)>::new());
        for (i, f) in plan.files.iter().enumerate() {
            let name = f.local.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            view.file = name.clone();
            view.index = i as u32 + 1;
            self.receive_view(view.clone());
            match self.pull_one(&mut sync, f, |got| {
                if last_report.elapsed() >= REPORT_EVERY {
                    last_report = Instant::now();
                    let sent = done_bytes + got;
                    let elapsed = started.elapsed();
                    let rate = if elapsed.as_secs_f64() > 0.0 { (sent as f64 / elapsed.as_secs_f64()) as u64 } else { 0 };
                    let total = view.total.max(sent);
                    self.receive_view(TransferView {
                        sent,
                        total,
                        rate_bps: rate,
                        eta_s: (elapsed >= Duration::from_secs(2) && rate > 0).then(|| total.saturating_sub(sent) / rate),
                        ..view.clone()
                    });
                }
            })
            .await
            {
                Ok(n) => {
                    done_bytes += n;
                    received += 1;
                }
                Err((why, broken)) => {
                    failed.push((name, why));
                    done_bytes += f.size;
                    if broken {
                        // adbd ends the session after a failed RECV.
                        sync = match self.open_sync(link).await {
                            Ok(s) => s,
                            Err(e) => {
                                let left = plan.files.len() - i - 1;
                                return Err(format!("Receiving from {title} stopped ({received} received, {left} not): {e}"));
                            }
                        };
                    }
                }
            }
            view.sent = done_bytes;
        }
        let _ = sync.quit().await;
        let plural = |n: u32| if n == 1 { "" } else { "s" };
        let mut parts = vec![format!("{received} file{} received from {title} ({})", plural(received), human_bytes(done_bytes))];
        if plan.files.is_empty() && !plan.dirs.is_empty() {
            parts = vec![format!("Received {} empty folder{} from {title}", plan.dirs.len(), plural(plan.dirs.len() as u32))];
        }
        if plan.skipped > 0 {
            parts.push(format!("{} skipped", plan.skipped));
        }
        match failed.as_slice() {
            [] => {}
            [(name, why)] => parts.push(format!("{name}: {why}")),
            [.., (_, why)] => parts.push(format!("{} failed ({why})", failed.len())),
        }
        if received == 0 && !failed.is_empty() {
            return Err(parts[1..].join(" · "));
        }
        Ok(parts.join(" · "))
    }

    /// Copies one file through a `.part` file. On error, says whether the
    /// sync session is unusable now.
    async fn pull_one(&self, sync: &mut SyncSession<TcpStream>, f: &Pull, progress: impl FnMut(u64)) -> Result<u64, (String, bool)> {
        let part_name = format!(".{}.part", f.local.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        let part = f.local.with_file_name(part_name);
        let mut guard = PartGuard(Some(part.clone()));
        let file = tokio::fs::File::create(&part).await.map_err(|e| (format!("Can't write {}: {e}", part.display()), false))?;
        let mut w = tokio::io::BufWriter::with_capacity(1 << 20, file);
        let n = sync.recv(&f.remote, &mut w, progress).await.map_err(|e| (adb_msg(e), true))?;
        let file = w.into_inner();
        if f.mtime > 0 {
            let t = std::time::UNIX_EPOCH + Duration::from_secs(f.mtime as u64);
            let std_file = file.into_std().await;
            let _ = tokio::task::spawn_blocking(move || std_file.set_modified(t)).await;
        }
        tokio::fs::rename(&part, &f.local).await.map_err(|e| (format!("Can't save {}: {e}", f.local.display()), false))?;
        guard.0 = None;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(list_path("/sdcard"), "/sdcard/");
        assert_eq!(list_path("/sdcard///"), "/sdcard/");
        assert_eq!(list_path("/"), "/");
        assert_eq!(child("/", "sdcard"), "/sdcard");
        assert_eq!(child("/sdcard/", "DCIM"), "/sdcard/DCIM");
    }

    #[test]
    fn folders_sort_first() {
        let e = |name: &str, is_dir| RemoteEntry { name: name.into(), is_dir, is_link: false, size: 0, mtime: 0 };
        let mut v = vec![e("b.txt", false), e("Zed", true), e("a.txt", false), e("alpha", true)];
        sort_entries(&mut v);
        let names: Vec<_> = v.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["alpha", "Zed", "a.txt", "b.txt"]);
    }

    #[test]
    fn free_names() {
        let dir = std::env::temp_dir().join(format!("tb-free-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(free_local(&dir, "a.txt"), dir.join("a.txt"));
        std::fs::write(dir.join("a.txt"), b"x").unwrap();
        assert_eq!(free_local(&dir, "a.txt"), dir.join("a (1).txt"));
        std::fs::create_dir(dir.join("Pics")).unwrap();
        assert_eq!(free_local(&dir, "Pics"), dir.join("Pics (1)"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
