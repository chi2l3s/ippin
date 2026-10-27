use crate::config::{Store, atomic_json};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{fs, io};
use winreg::{RegKey, enums::*};

const SETTINGS: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";
const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APP: &str = "IPPinGatekeeper";

#[derive(Serialize, Deserialize)]
struct Snapshot {
    url: Option<String>,
    enabled: Option<u32>,
    autodetect: Option<u32>,
    applied: String,
    #[serde(default)]
    previous: Vec<String>,
}

impl Snapshot {
    fn owns(&self, current: Option<&str>) -> bool {
        current.is_some_and(|url| url == self.applied || self.previous.iter().any(|old| old == url))
    }

    fn apply_url(&mut self, url: String) {
        if self.applied != url {
            let old = std::mem::replace(&mut self.applied, url);
            if !self.previous.contains(&old) {
                self.previous.push(old);
            }
        }
    }
}

fn optional<T: winreg::types::FromRegValue>(key: &RegKey, name: &str) -> Result<Option<T>> {
    match key.get_value(name) {
        Ok(value) => Ok(Some(value)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read Windows setting {name}")),
    }
}
fn remove_value(key: &RegKey, name: &str) -> Result<()> {
    match key.delete_value(name) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).context("remove Windows registry value"),
    }
}
fn restore<T: winreg::types::ToRegValue>(
    key: &RegKey,
    name: &str,
    value: &Option<T>,
) -> Result<()> {
    match value {
        Some(v) => key
            .set_value(name, v)
            .context("restore Windows registry value"),
        None => remove_value(key, name),
    }
}
fn notify() {
    use windows_sys::Win32::Networking::WinInet::{
        INTERNET_OPTION_REFRESH, INTERNET_OPTION_SETTINGS_CHANGED, InternetSetOptionW,
    };
    // SAFETY: both documented broadcast options take null handle/buffer and zero length.
    unsafe {
        InternetSetOptionW(
            std::ptr::null_mut(),
            INTERNET_OPTION_SETTINGS_CHANGED,
            std::ptr::null(),
            0,
        );
        InternetSetOptionW(
            std::ptr::null_mut(),
            INTERNET_OPTION_REFRESH,
            std::ptr::null(),
            0,
        );
    }
}

pub fn enable_pac(store: &Store, port: u16) -> Result<()> {
    let _lock = store.lock("pac.lock", false)?;
    let key =
        RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(SETTINGS, KEY_READ | KEY_WRITE)?;
    let url = format!("http://127.0.0.1:{port}/proxy.pac");
    let backup = store.path("proxy-backup.json");
    let mut saved = if backup.exists() {
        let saved: Snapshot = serde_json::from_slice(&fs::read(&backup)?)?;
        ensure!(
            saved.owns(optional::<String>(&key, "AutoConfigURL")?.as_deref()),
            "Windows proxy settings changed externally; run 'ippin win disable' before enabling again"
        );
        saved
    } else {
        Snapshot {
            url: optional(&key, "AutoConfigURL")?,
            enabled: optional(&key, "ProxyEnable")?,
            autodetect: optional(&key, "AutoDetect")?,
            applied: url.clone(),
            previous: Vec::new(),
        }
    };
    // Keep both owned URLs in the durable backup before changing the registry.
    // Recovery remains possible if the process crashes between these writes.
    saved.apply_url(url.clone());
    atomic_json(&backup, &saved)?;
    let result = (|| -> Result<()> {
        key.set_value("AutoConfigURL", &url)?;
        key.set_value("ProxyEnable", &0u32)?;
        key.set_value("AutoDetect", &0u32)?;
        Ok(())
    })();
    if result.is_err() {
        // Best-effort rollback of a partial registry update; keep backup on error.
        restore(&key, "AutoConfigURL", &saved.url)?;
        restore(&key, "ProxyEnable", &saved.enabled)?;
        restore(&key, "AutoDetect", &saved.autodetect)?;
        fs::remove_file(backup)?;
    }
    notify();
    result
}

pub fn disable_pac(store: &Store) -> Result<()> {
    let _lock = store.lock("pac.lock", false)?;
    let backup = store.path("proxy-backup.json");
    let saved: Snapshot = match fs::read(&backup) {
        Ok(data) => serde_json::from_slice(&data).context("invalid proxy settings backup")?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let key =
        RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(SETTINGS, KEY_READ | KEY_WRITE)?;
    if saved.owns(optional::<String>(&key, "AutoConfigURL")?.as_deref()) {
        restore(&key, "ProxyEnable", &saved.enabled)?;
        restore(&key, "AutoDetect", &saved.autodetect)?;
        restore(&key, "AutoConfigURL", &saved.url)?;
        notify();
    } else {
        tracing::warn!("Windows proxy changed externally; preserving the new settings");
    }
    fs::remove_file(backup)?;
    Ok(())
}

pub fn install_autostart(store: &Store) -> Result<()> {
    let exe = std::env::current_exe()?;
    let args = [
        exe.as_os_str(),
        std::ffi::OsStr::new("--home"),
        store.dir.as_os_str(),
        std::ffi::OsStr::new("start"),
        std::ffi::OsStr::new("--headless"),
        std::ffi::OsStr::new("-w"),
    ];
    let command = args
        .iter()
        .map(|arg| String::from_utf16(&quote_arg(arg)))
        .collect::<std::result::Result<Vec<_>, _>>()?
        .join(" ");
    let script = format!(
        "CreateObject(\"WScript.Shell\").Run \"{}\", 0, False\r\n",
        command.replace('"', "\"\"")
    );
    let path = store.path("autostart.vbs");
    fs::write(&path, script)?;
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(RUN)?;
    key.set_value(APP, &format!("wscript.exe \"{}\"", path.display()))
        .context("install Windows autostart")
}

pub fn uninstall_autostart(store: &Store) -> Result<()> {
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(RUN)?;
    remove_value(&key, APP)?;
    match fs::remove_file(store.path("autostart.vbs")) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Quote one argv element according to CommandLineToArgvW's backslash rules.
fn quote_arg(arg: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    let mut out = vec![b'"' as u16];
    let mut slashes = 0;
    for ch in arg.encode_wide() {
        if ch == b'\\' as u16 {
            slashes += 1;
            continue;
        }
        if ch == b'"' as u16 {
            out.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2 + 1));
        } else {
            out.extend(std::iter::repeat_n(b'\\' as u16, slashes));
        }
        slashes = 0;
        out.push(ch);
    }
    out.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    out.push(b'"' as u16);
    out
}

pub struct BackgroundChild(windows_sys::Win32::Foundation::HANDLE);
impl BackgroundChild {
    pub fn try_wait(&mut self) -> Result<Option<u32>> {
        use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
        // SAFETY: self owns a live process handle for the entire call.
        let wait = unsafe { WaitForSingleObject(self.0, 0) };
        if wait == windows_sys::Win32::Foundation::WAIT_TIMEOUT {
            return Ok(None);
        }
        ensure!(
            wait == windows_sys::Win32::Foundation::WAIT_OBJECT_0,
            "wait for background process failed"
        );
        let mut code = 0;
        // SAFETY: code points to writable storage; the process handle is valid.
        ensure!(
            unsafe { GetExitCodeProcess(self.0, &mut code) } != 0,
            "get background process exit code failed"
        );
        Ok(Some(code))
    }
    pub fn kill(&mut self) -> Result<()> {
        // SAFETY: the handle belongs only to the child created by this process.
        ensure!(
            unsafe { windows_sys::Win32::System::Threading::TerminateProcess(self.0, 1) } != 0,
            "stop background child failed"
        );
        Ok(())
    }
    pub fn wait(&mut self) -> Result<()> {
        // SAFETY: self owns a valid process handle.
        let result =
            unsafe { windows_sys::Win32::System::Threading::WaitForSingleObject(self.0, 5000) };
        ensure!(
            result == windows_sys::Win32::Foundation::WAIT_OBJECT_0,
            "background child did not exit"
        );
        Ok(())
    }
}
impl Drop for BackgroundChild {
    fn drop(&mut self) {
        // SAFETY: closes the one owned handle exactly once; does not kill the child.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

pub fn spawn_background(command: std::process::Command) -> Result<BackgroundChild> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::Threading::{
        CREATE_NO_WINDOW, CreateProcessW, PROCESS_INFORMATION, STARTUPINFOW,
    };
    let exe: Vec<u16> = command.get_program().encode_wide().chain(Some(0)).collect();
    let mut args = quote_arg(command.get_program());
    for arg in command.get_args() {
        args.push(b' ' as u16);
        args.extend(quote_arg(arg));
    }
    ensure!(!args.contains(&0), "NUL in process arguments");
    args.push(0);
    // SAFETY: STARTUPINFOW and PROCESS_INFORMATION support all-zero initialization.
    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    // SAFETY: the output structure supports zero initialization.
    let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: exe/args are NUL-terminated buffers alive across this call. No
    // handles are inherited. Environment/cwd are inherited via documented nulls.
    let created = unsafe {
        CreateProcessW(
            exe.as_ptr(),
            args.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            CREATE_NO_WINDOW,
            std::ptr::null(),
            std::ptr::null(),
            &startup,
            &mut info,
        )
    };
    if created == 0 {
        return Err(std::io::Error::last_os_error()).context("create hidden background process");
    }
    // SAFETY: CreateProcessW returned an owned thread handle we do not need.
    unsafe {
        windows_sys::Win32::Foundation::CloseHandle(info.hThread);
    }
    Ok(BackgroundChild(info.hProcess))
}

#[cfg(test)]
mod process_tests {
    use super::*;
    #[test]
    fn windows_argv_escaping_handles_spaces_quotes_and_trailing_slashes() {
        fn quoted(s: &str) -> String {
            String::from_utf16(&quote_arg(std::ffi::OsStr::new(s))).unwrap()
        }
        assert_eq!(quoted("a b"), "\"a b\"");
        assert_eq!(quoted("a\"b"), "\"a\\\"b\"");
        assert_eq!(quoted("a\\"), "\"a\\\\\"");
    }

    #[test]
    fn pac_recovery_owns_old_and_new_ports_but_preserves_external_settings() {
        let mut snapshot = Snapshot {
            url: Some("https://original.example/proxy.pac".into()),
            enabled: Some(1),
            autodetect: None,
            applied: "http://127.0.0.1:8888/proxy.pac".into(),
            previous: Vec::new(),
        };
        snapshot.apply_url("http://127.0.0.1:9999/proxy.pac".into());
        let persisted = serde_json::to_vec(&snapshot).unwrap();
        let restored: Snapshot = serde_json::from_slice(&persisted).unwrap();
        assert!(restored.owns(Some("http://127.0.0.1:8888/proxy.pac")));
        assert!(restored.owns(Some("http://127.0.0.1:9999/proxy.pac")));
        assert!(!restored.owns(Some("https://other.example/proxy.pac")));
        assert!(!restored.owns(None));
        assert_eq!(
            restored.url.as_deref(),
            Some("https://original.example/proxy.pac")
        );
        assert_eq!(restored.enabled, Some(1));
        assert_eq!(restored.autodetect, None);
    }
}
