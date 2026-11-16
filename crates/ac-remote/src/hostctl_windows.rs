//! Starting the share service on Windows, at sign-in and in the signed-in
//! session (it needs the desktop to capture it). Two ways:
//!
//! - A `Run` entry under the user's registry key: no administrator rights,
//!   but Windows then blocks its input to administrator windows (UIPI), so
//!   a paired computer can't click or type in those.
//! - "Control administrator windows": a scheduled task that runs it with the
//!   user's highest rights. Setting it up (and removing it) asks once
//!   through UAC; starting it doesn't. UAC prompts themselves stay out of
//!   reach: Windows shows them on a desktop of their own.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{bail, Context, Result};

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "Test Bed Share";
/// The scheduled task for running with administrator rights.
const TASK: &str = "Test Bed Share";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const DETACHED_PROCESS: u32 = 0x0000_0008;

/// Runs a console tool without flashing a window.
fn quiet(program: &str, args: &[&str]) -> Result<Output> {
    Command::new(program).args(args).creation_flags(CREATE_NO_WINDOW).output().with_context(|| format!("running {program}"))
}

fn run_key_set() -> bool {
    quiet("reg", &["query", RUN_KEY, "/v", RUN_VALUE]).map(|o| o.status.success()).unwrap_or(false)
}

fn task_exists() -> bool {
    quiet("schtasks", &["/Query", "/TN", TASK]).map(|o| o.status.success()).unwrap_or(false)
}

/// Whether sharing starts at sign-in.
pub fn service_enabled() -> bool {
    run_key_set() || task_exists()
}

/// Whether sharing runs with administrator rights, so paired computers can
/// use administrator windows too.
pub fn admin_enabled() -> bool {
    task_exists()
}

/// The binary the Run entry starts.
fn run_key_binary() -> Option<PathBuf> {
    let o = quiet("reg", &["query", RUN_KEY, "/v", RUN_VALUE]).ok()?;
    let out = String::from_utf8_lossy(&o.stdout);
    let line = out.lines().find(|l| l.contains("REG_SZ"))?;
    let value = line.split("REG_SZ").nth(1)?.trim();
    value.strip_prefix('"')?.split('"').next().map(PathBuf::from)
}

/// Console output that may be UTF-16 (schtasks /XML) or the code page.
fn console_text(bytes: &[u8]) -> String {
    let utf16 = bytes.len() >= 2 && bytes.len().is_multiple_of(2) && bytes.iter().skip(1).step_by(2).filter(|&&b| b == 0).count() * 2 > bytes.len() / 2;
    if utf16 {
        let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        String::from_utf16_lossy(&units).trim_start_matches('\u{feff}').to_string()
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// The binary the scheduled task starts.
fn task_binary() -> Option<PathBuf> {
    let o = quiet("schtasks", &["/Query", "/TN", TASK, "/XML"]).ok().filter(|o| o.status.success())?;
    let xml = console_text(&o.stdout);
    let cmd = xml.split("<Command>").nth(1)?.split("</Command>").next()?;
    Some(PathBuf::from(xml_unescape(cmd).trim().trim_matches('"')))
}

/// The binary that starts at sign-in.
pub fn service_binary() -> Option<PathBuf> {
    run_key_binary().or_else(task_binary)
}

fn add_run_key(bin: &Path) -> Result<()> {
    let command = format!("\"{}\" run", bin.display());
    let o = quiet("reg", &["add", RUN_KEY, "/v", RUN_VALUE, "/t", "REG_SZ", "/d", &command, "/f"])?;
    if !o.status.success() {
        bail!("couldn't add the sign-in entry: {}", String::from_utf8_lossy(&o.stderr).trim());
    }
    Ok(())
}

fn remove_run_key() {
    let _ = quiet("reg", &["delete", RUN_KEY, "/v", RUN_VALUE, "/f"]);
}

/// Start `bin` now and at every sign-in.
pub fn enable_service(bin: &Path) -> Result<()> {
    if task_exists() {
        // Test Bed moved: point the task at this copy (asks through UAC).
        let same = |p: &Path| p.to_string_lossy().eq_ignore_ascii_case(&bin.to_string_lossy());
        if !task_binary().is_some_and(|p| same(&p)) {
            register_task(bin)?;
        }
        remove_run_key();
        stop();
        return run_task();
    }
    add_run_key(bin)?;
    stop();
    start(bin)
}

pub fn restart_service() -> Result<()> {
    stop();
    if task_exists() {
        return run_task();
    }
    let bin = service_binary().or_else(super::host_binary).context("test-bed-share.exe not found")?;
    start(&bin)
}

/// Stop sharing and keep it from starting at sign-in. Removing the
/// administrator task asks through UAC.
pub fn disable_service() -> Result<()> {
    if task_exists() {
        elevated("schtasks.exe", &format!("/Delete /TN \"{TASK}\" /F"))?;
    }
    remove_run_key();
    stop();
    Ok(())
}

/// Run sharing with administrator rights (`on`) or without, and restart it
/// that way. Either change asks once through UAC.
pub fn set_admin(bin: &Path, on: bool) -> Result<()> {
    if on {
        register_task(bin)?;
        remove_run_key();
        stop();
        run_task()
    } else {
        if task_exists() {
            elevated("schtasks.exe", &format!("/Delete /TN \"{TASK}\" /F"))?;
        }
        stop();
        add_run_key(bin)?;
        start(bin)
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// The task: at this user's sign-in, in their session, with their highest
/// rights; no time limit, on battery too, at normal priority (a task's
/// default is below normal).
pub(super) fn task_xml(user: &str, bin: &Path) -> String {
    let dir = bin.parent().map(|d| d.display().to_string()).unwrap_or_default();
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Test Bed: shares this PC with paired computers, with administrator rights so they can use administrator windows too.</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>"{bin}"</Command>
      <Arguments>run</Arguments>
      <WorkingDirectory>{dir}</WorkingDirectory>
    </Exec>
  </Actions>
</Task>
"#,
        user = xml_escape(user),
        bin = xml_escape(&bin.display().to_string()),
        dir = xml_escape(&dir),
    )
}

/// Creates (or replaces) the task for `bin`. Creating a task that runs with
/// administrator rights needs them: this asks through UAC.
fn register_task(bin: &Path) -> Result<()> {
    let user = match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
        (Ok(d), Ok(u)) if !d.is_empty() => format!("{d}\\{u}"),
        (_, Ok(u)) => u,
        _ => bail!("can't tell which user this is"),
    };
    let xml = task_xml(&user, bin);
    let path = std::env::temp_dir().join(format!("test-bed-share-task-{}.xml", std::process::id()));
    // schtasks reads the encoding the declaration names: UTF-16, with a BOM.
    let bytes: Vec<u8> = std::iter::once(0xfeffu16).chain(xml.encode_utf16()).flat_map(u16::to_le_bytes).collect();
    std::fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;
    let r = elevated("schtasks.exe", &format!("/Create /TN \"{TASK}\" /XML \"{}\" /F", path.display()));
    let _ = std::fs::remove_file(&path);
    r.context("setting up sharing with administrator rights")
}

/// Starts the task now. No UAC prompt: the user may run their own task.
fn run_task() -> Result<()> {
    let o = quiet("schtasks", &["/Run", "/TN", TASK])?;
    if !o.status.success() {
        bail!("couldn't start sharing: {}", console_text(&o.stderr).trim());
    }
    Ok(())
}

/// Runs `program` with administrator rights: Windows asks through UAC.
fn elevated(program: &str, params: &str) -> Result<()> {
    use windows::core::{w, HSTRING};
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

    let (file, args) = (HSTRING::from(program), HSTRING::from(params));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: w!("runas"),
        lpFile: windows::core::PCWSTR(file.as_ptr()),
        lpParameters: windows::core::PCWSTR(args.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    if let Err(e) = unsafe { ShellExecuteExW(&mut info) } {
        // ERROR_CANCELLED: "No" on the UAC prompt.
        if e.code() == windows::core::HRESULT::from_win32(1223) {
            bail!("Windows' administrator prompt was declined");
        }
        bail!("couldn't run {program} as administrator: {e}");
    }
    if info.hProcess.is_invalid() {
        return Ok(());
    }
    let mut code = 0u32;
    unsafe {
        let waited = WaitForSingleObject(info.hProcess, INFINITE);
        let got = GetExitCodeProcess(info.hProcess, &mut code);
        let _ = CloseHandle(info.hProcess);
        if waited != WAIT_OBJECT_0 || got.is_err() {
            bail!("lost track of {program}");
        }
    }
    if code != 0 {
        bail!("{program} {params} failed (exit code {code})");
    }
    Ok(())
}

fn start(bin: &Path) -> Result<()> {
    Command::new(bin)
        .arg("run")
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("starting {}", bin.display()))?;
    Ok(())
}

/// Asks the running service to stop, over its control pipe. The only way to
/// stop one running as administrator from here: taskkill isn't allowed to.
fn ask_to_quit() -> bool {
    use std::io::{BufRead, BufReader, Write};
    let Ok(mut pipe) = std::fs::OpenOptions::new().read(true).write(true).open(crate::paths::host_control_socket()) else {
        return false;
    };
    if pipe.write_all(b"{\"cmd\":\"quit\"}\n").is_err() {
        return false;
    }
    let mut line = String::new();
    BufReader::new(pipe).read_line(&mut line).is_ok() && line.contains("quitting")
}

/// Ends this user's share service, and waits until its pipe is free.
fn stop() {
    let pipe_gone = || !Path::new(&crate::paths::host_control_socket()).exists();
    if ask_to_quit() {
        for _ in 0..30 {
            if pipe_gone() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    // An older service without "quit", or one that's stuck.
    if task_exists() {
        let _ = quiet("schtasks", &["/End", "/TN", TASK]);
    }
    let user = std::env::var("USERNAME").unwrap_or_default();
    let _ = quiet("taskkill", &["/F", "/IM", "test-bed-share.exe", "/FI", &format!("USERNAME eq {user}")]);
    for _ in 0..20 {
        if pipe_gone() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// RemoteLinux never ran on Windows.
pub fn remotelinux_enabled() -> bool {
    false
}

pub fn retire_remotelinux() -> Result<()> {
    Ok(())
}

/// The last lines of the service's log file.
pub fn service_log(lines: usize) -> String {
    let text = std::fs::read_to_string(crate::paths::share_log()).unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

// Linux's one-time setup has no Windows counterpart: SendInput needs no
// permission, and Windows asks about the firewall itself the first time the
// service listens.

pub fn uinput_writable() -> bool {
    true
}

pub fn input_readable() -> bool {
    true
}

pub fn ufw_active() -> bool {
    false
}

pub fn firewall_opened() -> bool {
    true
}

pub fn open_firewall() -> Result<()> {
    Ok(())
}

pub fn fix_uinput() -> Result<()> {
    Ok(())
}
