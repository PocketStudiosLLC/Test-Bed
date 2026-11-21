//! Remote access from the phone app: pairing with PCs, on the LAN (PIN) or
//! over the internet (invite code), and viewing one. Java's `Remote` class
//! calls in here. Slow calls block, so Java makes them off the main thread.
//!
//! A session's events come out of [`next`] as byte arrays:
//! `0` + JSON (status), `1` + keyframe flag + H.264 access unit, `2` + Opus packet.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use ac_remote::connection::{self, NetEvent, NetHandle, Outgoing};
use ac_remote::proto::{ClientMsg, Motion, StreamSettings};
use ac_remote::trust::{KnownHost, KnownHosts};
use anyhow::{anyhow, Result};
use jni::objects::{JClass, JString};
use jni::sys::{jboolean, jbyteArray, jfloat, jint, jlong, jstring, JNI_TRUE};
use jni::JNIEnv;
use serde_json::{json, Value};

pub const EV_STATUS: u8 = 0;
pub const EV_VIDEO: u8 = 1;
pub const EV_AUDIO: u8 = 2;

/// Events waiting for Java; video beyond this is dropped (and a keyframe asked for).
const QUEUE: usize = 90;

fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("tokio runtime"))
}

/// Points Test Bed's files (identity, paired PCs) at the app's own folder.
pub fn init(files_dir: &str) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = std::path::Path::new(files_dir);
        std::env::set_var("HOME", dir);
        std::env::set_var("XDG_CONFIG_HOME", dir.join("config"));
        std::env::set_var("XDG_DATA_HOME", dir.join("data"));
        std::env::set_var("XDG_RUNTIME_DIR", dir.join("run"));
        log::init();
    });
}

fn host_json(h: &KnownHost) -> Value {
    json!({ "name": h.name, "fingerprint": h.fingerprint, "address": h.address, "internet": h.internet, "linked": h.link.is_some() })
}

fn all_hosts() -> Result<Vec<KnownHost>> {
    let mut v = KnownHosts::load()?.hosts;
    v.extend(KnownHosts::load_internet()?.hosts.into_iter().map(|h| KnownHost { internet: true, ..h }));
    Ok(v)
}

fn find(fingerprint: &str) -> Result<KnownHost> {
    all_hosts()?.into_iter().find(|h| h.fingerprint == fingerprint).ok_or_else(|| anyhow!("that PC isn't paired with this phone any more"))
}

/// Paired PCs: `[{name, fingerprint, address, internet}]`.
pub fn hosts() -> Result<Value> {
    Ok(Value::Array(all_hosts()?.iter().map(host_json).collect()))
}

/// Which paired PCs answer: `{fingerprint: rtt_ms or null}`, and under
/// `"away"` the LAN PCs that only answered over the internet.
pub fn probe() -> Result<Value> {
    let hosts = all_hosts()?;
    let (lan, net): (Vec<_>, Vec<_>) = hosts.into_iter().partition(|h| !h.internet);
    let t = Duration::from_millis(1500);
    let (a, b) = runtime().block_on(async {
        tokio::join!(ac_remote::client::probe(&lan, t), ac_remote::internet::probe(&net, "", Duration::from_secs(6)))
    });
    let mut out = serde_json::Map::new();
    for (h, r) in lan.iter().zip(&a).chain(net.iter().zip(&b)) {
        out.insert(h.fingerprint.clone(), r.rtt_ms.map(Value::from).unwrap_or(Value::Null));
    }
    // LAN PCs out of reach here but linked: maybe away from home.
    let (lan_fps, twins): (Vec<String>, Vec<KnownHost>) =
        lan.iter().zip(&a).filter(|(_, r)| r.rtt_ms.is_none()).filter_map(|(h, _)| Some((h.fingerprint.clone(), h.over_internet()?))).unzip();
    let mut away = Vec::new();
    if !twins.is_empty() {
        let c = runtime().block_on(ac_remote::internet::probe(&twins, "", Duration::from_secs(6)));
        for (fp, r) in lan_fps.into_iter().zip(c) {
            if let Some(ms) = r.rtt_ms {
                out.insert(fp.clone(), Value::from(ms));
                away.push(Value::from(fp));
            }
        }
    }
    out.insert("away".into(), Value::Array(away));
    Ok(Value::Object(out))
}

/// PCs sharing on this network: `[{name, address, fingerprint, pairing}]`.
pub fn discover() -> Result<Value> {
    let found = ac_remote::discovery::discover_blocking(Duration::from_millis(1500))?;
    Ok(serde_json::to_value(found)?)
}

pub fn pair_lan(address: &str, pin: &str, name: &str) -> Result<Value> {
    Ok(host_json(&runtime().block_on(ac_remote::client::pair(address, pin, name))?))
}

pub fn pair_internet(invite: &str, name: &str) -> Result<Value> {
    let h = runtime().block_on(ac_remote::internet::pair(invite.trim(), name, ""))?;
    Ok(host_json(&KnownHost { internet: true, ..h }))
}

pub fn forget(fingerprint: &str) -> Result<()> {
    for internet in [false, true] {
        let mut kh = if internet { KnownHosts::load_internet()? } else { KnownHosts::load()? };
        if kh.remove(fingerprint) {
            kh.save()?;
        }
    }
    Ok(())
}

pub struct Session {
    net: NetHandle,
    /// What the stream was asked for, so a monitor switch keeps the rest.
    settings: Mutex<StreamSettings>,
    events: Mutex<Receiver<Vec<u8>>>,
    seq: AtomicU32,
}

fn status(v: Value) -> Vec<u8> {
    let mut b = vec![EV_STATUS];
    b.extend_from_slice(v.to_string().as_bytes());
    b
}

fn encode(ev: NetEvent) -> Option<Vec<u8>> {
    Some(match ev {
        NetEvent::Frame { meta, data } => {
            let mut b = Vec::with_capacity(data.len() + 2);
            b.push(EV_VIDEO);
            b.push(meta.keyframe as u8);
            b.extend_from_slice(&data);
            b
        }
        NetEvent::Audio { data, .. } => {
            let mut b = Vec::with_capacity(data.len() + 1);
            b.push(EV_AUDIO);
            b.extend_from_slice(&data);
            b
        }
        NetEvent::Connecting { attempt } => status(json!({ "t": "connecting", "attempt": attempt })),
        NetEvent::Connected { host_name, mut monitors, host_os } => {
            // Numbered in reading order, as on the PC viewer.
            monitors.sort_by_key(|m| (m.y, m.x));
            let monitors: Vec<Value> =
                monitors.iter().map(|m| json!({ "name": m.name, "width": m.width, "height": m.height })).collect();
            status(json!({ "t": "connected", "host": host_name, "monitors": monitors, "os": host_os }))
        }
        NetEvent::Stream { monitor, width, height, .. } => {
            status(json!({ "t": "stream", "monitor": monitor, "width": width, "height": height }))
        }
        NetEvent::Notice(text) => status(json!({ "t": "notice", "text": text })),
        NetEvent::Disconnected { reason } => status(json!({ "t": "disconnected", "text": reason })),
        NetEvent::Reconnecting { retry_in, reason, .. } => {
            status(json!({ "t": "reconnecting", "text": reason, "retry_ms": retry_in.as_millis() as u64 }))
        }
        NetEvent::Fatal(text) => status(json!({ "t": "fatal", "text": text })),
        NetEvent::Clipboard(text) => status(json!({ "t": "clipboard", "text": text })),
        NetEvent::InternetLinked(Ok(new)) => status(json!({ "t": "linked", "new": new })),
        NetEvent::InternetLinked(Err(text)) => status(json!({ "t": "link_failed", "text": text })),
        NetEvent::Route { internet } => status(json!({ "t": "route", "internet": internet })),
        NetEvent::SwitchRefused(text) => status(json!({ "t": "notice", "text": text })),
        NetEvent::Clock { rtt_us, .. } => status(json!({ "t": "clock", "rtt_ms": rtt_us as f64 / 1000.0 })),
        NetEvent::FileProgress { name, sent, total } => {
            status(json!({ "t": "file_progress", "name": name, "done": sent, "total": total }))
        }
        NetEvent::FileSent { name, path } => status(json!({ "t": "file_sent", "name": name, "path": path })),
        NetEvent::FileFailed { name, error } => status(json!({ "t": "file_failed", "name": name, "text": error })),
        NetEvent::Listing { id, result } => match result {
            Ok(l) => status(json!({ "t": "listing", "id": id, "listing": l })),
            Err(e) => status(json!({ "t": "listing", "id": id, "error": e })),
        },
        NetEvent::TakeProgress { name, got, total } => {
            status(json!({ "t": "take_progress", "name": name, "done": got, "total": total }))
        }
        NetEvent::Taken { name, path } => status(json!({ "t": "taken", "name": name, "path": path.to_string_lossy() })),
        NetEvent::TakeFailed { name, error } => status(json!({ "t": "take_failed", "name": name, "text": error })),
        _ => return None,
    })
}

/// Starts viewing the paired PC `fingerprint`; it reconnects by itself until closed.
pub fn connect(fingerprint: &str, client_name: &str, max_height: u32) -> Result<Box<Session>> {
    let host = find(fingerprint)?;
    let settings = StreamSettings {
        max_height: (max_height > 0).then_some(max_height),
        // Over the internet, a little less than the LAN's default.
        bitrate_kbps: host.internet.then_some(8_000),
        ..Default::default()
    };
    let internet = host.internet;
    let (tx, rx) = sync_channel::<Vec<u8>>(QUEUE);
    let (req_tx, req_rx) = sync_channel::<()>(1);
    let lagging = Arc::new(AtomicBool::new(false));
    let net = connection::spawn(host, settings.clone(), client_name.to_string(), move |ev| {
        let video = matches!(ev, NetEvent::Frame { .. });
        // Like frames, these are soon superseded: dropped rather than waited for.
        let droppable = matches!(ev, NetEvent::Clock { .. } | NetEvent::FileProgress { .. } | NetEvent::TakeProgress { .. });
        if matches!(&ev, NetEvent::Frame { meta, .. } if meta.keyframe) {
            lagging.store(false, Ordering::Relaxed);
        }
        let Some(b) = encode(ev) else { return };
        match tx.try_send(b) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) if droppable => {}
            Err(TrySendError::Full(b)) if !video => {
                // Status matters more than a frame: wait for room.
                let _ = tx.send(b);
            }
            Err(TrySendError::Full(_)) => {
                // Frames after a dropped one are broken until the next keyframe.
                if !lagging.swap(true, Ordering::Relaxed) {
                    let _ = req_tx.try_send(());
                }
            }
            Err(TrySendError::Disconnected(_)) => {}
        }
    })?;
    if !internet {
        // From home, so it can be reached away from it later.
        let _ = net.tx.send(Outgoing::LinkInternet);
    }
    let keyframes = net.tx.clone();
    std::thread::Builder::new().name("remote-keyframe".into()).spawn(move || {
        while req_rx.recv().is_ok() {
            if keyframes.send(Outgoing::Msg(ClientMsg::RequestKeyframe)).is_err() {
                break;
            }
        }
    })?;
    Ok(Box::new(Session { net, settings: Mutex::new(settings), events: Mutex::new(rx), seq: AtomicU32::new(0) }))
}

impl Session {
    /// The next event, or `None` after `timeout`.
    pub fn next(&self, timeout: Duration) -> Option<Vec<u8>> {
        match self.events.lock().unwrap().recv_timeout(timeout) {
            Ok(b) => Some(b),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => Some(status(json!({ "t": "closed" }))),
        }
    }

    pub fn msg(&self, m: ClientMsg) {
        let _ = self.net.tx.send(Outgoing::Msg(m));
    }

    pub fn motion(&self, x: f32, y: f32) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        let _ = self.net.tx.send(Outgoing::Motion(Motion::Abs { seq, x, y }));
    }

    /// Streams the PC's monitor `name` instead.
    pub fn monitor(&self, name: String) {
        let s = {
            let mut s = self.settings.lock().unwrap();
            s.monitor = Some(name);
            s.clone()
        };
        self.msg(ClientMsg::UpdateSettings(s));
    }

    /// Drops the connection and dials again now: the PC lets go of every
    /// key and button and starts input afresh.
    pub fn reconnect(&self) {
        let _ = self.net.tx.send(Outgoing::Reconnect(std::time::Instant::now()));
    }

    /// Sends the file at `path` (on the phone) to the PC's Downloads.
    pub fn send_file(&self, path: String) {
        let _ = self.net.tx.send(Outgoing::SendFile { path: path.into(), drop_at: None });
    }

    /// Lists the PC's folder `dir` (home if empty), or its subfolder `name`:
    /// answered by a `listing` event with the same `id`.
    pub fn list_files(&self, id: u64, dir: String, name: Option<String>) {
        let _ = self.net.tx.send(Outgoing::ListFiles { id, dir, name });
    }

    /// Copies the PC's file `name` in `dir` into the phone's folder `dest`.
    pub fn take_file(&self, dir: String, name: String, dest: String) {
        let _ = self.net.tx.send(Outgoing::TakeFile { dir, name, dest: dest.into() });
    }

    /// Puts `text` on the PC's clipboard.
    pub fn clipboard(&self, text: String) {
        let _ = self.net.tx.send(Outgoing::Clipboard(text));
    }

    /// From now on the PC's clipboard comes as `clipboard` events.
    pub fn watch_clipboard(&self) {
        let _ = self.net.tx.send(Outgoing::WatchClipboard);
    }

    pub fn close(&self) {
        self.net.quit.store(true, Ordering::Relaxed);
        self.msg(ClientMsg::Bye);
        let _ = self.net.tx.send(Outgoing::Quit);
    }
}

// ---- JNI ----------------------------------------------------------------------------------

fn jstr(env: &mut JNIEnv, s: &JString) -> String {
    env.get_string(s).map(Into::into).unwrap_or_default()
}

fn new_jstring(env: &mut JNIEnv, s: &str) -> jstring {
    env.new_string(s).map(|s| s.into_raw()).unwrap_or(std::ptr::null_mut())
}

/// `{"ok": value}` or `{"error": text}`, as a string for Java's JSONObject.
fn reply(env: &mut JNIEnv, r: Result<Value>) -> jstring {
    let v = match r {
        Ok(v) => json!({ "ok": v }),
        Err(e) => json!({ "error": format!("{e:#}") }),
    };
    new_jstring(env, &v.to_string())
}

fn session<'a>(h: jlong) -> Option<&'a Session> {
    // SAFETY: `h` came from `connect` and Java stops using it at `close`.
    (h != 0).then(|| unsafe { &*(h as *const Session) })
}

#[no_mangle]
pub extern "system" fn Java_org_androidcontroller_keeper_Remote_init(mut env: JNIEnv, _: JClass, dir: JString) {
    let dir = jstr(&mut env, &dir);
    init(&dir);
}

#[no_mangle]
pub extern "system" fn Java_org_androidcontroller_keeper_Remote_hosts(mut env: JNIEnv, _: JClass) -> jstring {
    let r = hosts();
    reply(&mut env, r)
}

#[no_mangle]
pub extern "system" fn Java_org_androidcontroller_keeper_Remote_probe(mut env: JNIEnv, _: JClass) -> jstring {
    let r = probe();
    reply(&mut env, r)
}

#[no_mangle]
pub extern "system" fn Java_org_androidcontroller_keeper_Remote_discover(mut env: JNIEnv, _: JClass) -> jstring {
    let r = discover();
    reply(&mut env, r)
}

#[no_mangle]
pub extern "system" fn Java_org_androidcontroller_keeper_Remote_pairLan(
    mut env: JNIEnv,
    _: JClass,
    address: JString,
    pin: JString,
    name: JString,
) -> jstring {
    let (a, p, n) = (jstr(&mut env, &address), jstr(&mut env, &pin), jstr(&mut env, &name));
    let r = pair_lan(&a, &p, &n);
    reply(&mut env, r)
}

#[no_mangle]
pub extern "system" fn Java_org_androidcontroller_keeper_Remote_pairInternet(mut env: JNIEnv, _: JClass, invite: JString, name: JString) -> jstring {
    let (i, n) = (jstr(&mut env, &invite), jstr(&mut env, &name));
    let r = pair_internet(&i, &n);
    reply(&mut env, r)
}

#[no_mangle]
pub extern "system" fn Java_org_androidcontroller_keeper_Remote_forget(mut env: JNIEnv, _: JClass, fingerprint: JString) -> jstring {
    let f = jstr(&mut env, &fingerprint);
    let r = forget(&f).map(|()| Value::Null);
    reply(&mut env, r)
}

/// Why the last `connect` returned 0.
static LAST_ERROR: Mutex<String> = Mutex::new(String::new());

#[no_mangle]
pub extern "system" fn Java_org_androidcontroller_keeper_Remote_connect(
    mut env: JNIEnv,
    _: JClass,
    fingerprint: JString,
    name: JString,
    max_height: jint,
) -> jlong {
    let (f, n) = (jstr(&mut env, &fingerprint), jstr(&mut env, &name));
    match connect(&f, &n, max_height.max(0) as u32) {
        Ok(s) => Box::into_raw(s) as jlong,
        Err(e) => {
            *LAST_ERROR.lock().unwrap() = format!("{e:#}");
            0
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_org_androidcontroller_keeper_Remote_lastError(mut env: JNIEnv, _: JClass) -> jstring {
    let e = LAST_ERROR.lock().unwrap().clone();
    new_jstring(&mut env, &e)
}

#[no_mangle]
pub extern "system" fn Java_org_androidcontroller_keeper_Remote_next(env: JNIEnv, _: JClass, h: jlong, timeout_ms: jint) -> jbyteArray {
    let Some(s) = session(h) else { return std::ptr::null_mut() };
    match s.next(Duration::from_millis(timeout_ms.max(0) as u64)) {
        Some(b) => env.byte_array_from_slice(&b).map(|a| a.into_raw()).unwrap_or(std::ptr::null_mut()),
        None => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "system" fn Java_org_androidcontroller_keeper_Remote_motion(_: JNIEnv, _: JClass, h: jlong, x: jfloat, y: jfloat) {
    if let Some(s) = session(h) {
        s.motion(x, y);
    }
}

#[no_mangle]
pub extern "system" fn Java_org_androidcontroller_keeper_Remote_button(_: JNIEnv, _: JClass, h: jlong, code: jint, pressed: jboolean) {
    if let Some(s) = session(h) {
        s.msg(ClientMsg::Button { code: code as u16, pressed: pressed == JNI_TRUE });
    }
}

#[no_mangle]
pub extern "system" fn Java_org_androidcontroller_keeper_Remote_key(_: JNIEnv, _: JClass, h: jlong, code: jint, pressed: jboolean) {
    if let Some(s) = session(h) {
        s.msg(ClientMsg::Key { code: code as u16, pressed: pressed == JNI_TRUE });
    }
}

#[no_mangle]
pub extern "system" fn Java_org_androidcontroller_keeper_Remote_scroll(_: JNIEnv, _: JClass, h: jlong, x: jint, y: jint) {
    if let Some(s) = session(h) {
        s.msg(ClientMsg::Scroll { v120_x: x, v120_y: y });
    }
}

#[no_mangle]
pub extern "system" fn Java_org_androidcontroller_keeper_Remote_releaseAll(_: JNIEnv, _: JClass, h: jlong) {
    if let Some(s) = session(h) {
        s.msg(ClientMsg::ReleaseAll);
    }
}

#[no_mangle]
pub extern "system" fn Java_org_androidcontroller_keeper_Remote_keyframe(_: JNIEnv, _: JClass, h: jlong) {
    if let Some(s) = session(h) {
        s.msg(ClientMsg::RequestKeyframe);
    }
}

#[no_mangle]
pub extern "system" fn Java_org_androidcontroller_keeper_Remote_setClipboard(mut env: JNIEnv, _: JClass, h: jlong, text: JString) {
    let text = jstr(&mut env, &text);
    if let Some(s) = session(h) {
        s.clipboard(text);
    }
}

#[no_mangle]
pub extern "system" fn Java_org_androidcontroller_keeper_Remote_monitor(mut env: JNIEnv, _: JClass, h: jlong, name: JString) {
    let name = jstr(&mut env, &name);
    if let Some(s) = session(h) {
        s.monitor(name);
    }
}

#[no_mangle]
pub extern "system" fn Java_org_androidcontroller_keeper_Remote_reconnect(_: JNIEnv, _: JClass, h: jlong) {
    if let Some(s) = session(h) {
        s.reconnect();
    }
}

#[no_mangle]
pub extern "system" fn Java_org_androidcontroller_keeper_Remote_sendFile(mut env: JNIEnv, _: JClass, h: jlong, path: JString) {
    let path = jstr(&mut env, &path);
    if let Some(s) = session(h) {
        s.send_file(path);
    }
}

/// `name` may be null: list `dir` itself.
#[no_mangle]
pub extern "system" fn Java_org_androidcontroller_keeper_Remote_listFiles(
    mut env: JNIEnv,
    _: JClass,
    h: jlong,
    id: jlong,
    dir: JString,
    name: JString,
) {
    let dir = jstr(&mut env, &dir);
    let name = (!name.is_null()).then(|| jstr(&mut env, &name));
    if let Some(s) = session(h) {
        s.list_files(id as u64, dir, name);
    }
}

#[no_mangle]
pub extern "system" fn Java_org_androidcontroller_keeper_Remote_takeFile(
    mut env: JNIEnv,
    _: JClass,
    h: jlong,
    dir: JString,
    name: JString,
    dest: JString,
) {
    let (d, n, to) = (jstr(&mut env, &dir), jstr(&mut env, &name), jstr(&mut env, &dest));
    if let Some(s) = session(h) {
        s.take_file(d, n, to);
    }
}

#[no_mangle]
pub extern "system" fn Java_org_androidcontroller_keeper_Remote_watchClipboard(_: JNIEnv, _: JClass, h: jlong) {
    if let Some(s) = session(h) {
        s.watch_clipboard();
    }
}

/// Ends the session and frees it: `h` must not be used again.
#[no_mangle]
pub extern "system" fn Java_org_androidcontroller_keeper_Remote_close(_: JNIEnv, _: JClass, h: jlong) {
    if h != 0 {
        // SAFETY: see `session`; this is the one place it is freed.
        let s = unsafe { Box::from_raw(h as *mut Session) };
        s.close();
    }
}

mod log {
    //! `tracing` to logcat (tag "TestBed"), or stderr off Android.

    pub fn init() {
        let _ = tracing_subscriber::fmt().with_ansi(false).without_time().with_writer(Logcat).with_env_filter("info").try_init();
    }

    #[derive(Clone, Copy)]
    struct Logcat;

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Logcat {
        type Writer = Logcat;
        fn make_writer(&'a self) -> Self::Writer {
            Logcat
        }
    }

    impl std::io::Write for Logcat {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            write_line(&String::from_utf8_lossy(buf));
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[cfg(target_os = "android")]
    fn write_line(s: &str) {
        #[link(name = "log")]
        extern "C" {
            fn __android_log_write(prio: i32, tag: *const std::ffi::c_char, text: *const std::ffi::c_char) -> i32;
        }
        let text = std::ffi::CString::new(s.trim_end().replace('\0', " ")).unwrap_or_default();
        // 4 = ANDROID_LOG_INFO
        unsafe { __android_log_write(4, c"TestBed".as_ptr(), text.as_ptr()) };
    }

    #[cfg(not(target_os = "android"))]
    fn write_line(s: &str) {
        eprint!("{s}");
    }
}
