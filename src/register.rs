//! Registers Glance as a video player for the current user (no admin rights
//! needed). Windows doesn't let apps make themselves the default, so after
//! registering we open Settings > Default apps for the user to pick Glance.

use std::ptr::null_mut;
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::UI::Shell::{ShellExecuteW, SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

pub const EXTENSIONS: &[&str] = &[
    "mp4", "m4v", "mkv", "webm", "avi", "mov", "wmv", "flv", "mpg", "mpeg", "ts", "m2ts", "mts", "3gp", "ogv", "vob",
    "divx", "asf",
];

const PROG_ID: &str = "Glance.Video";
const CAPABILITIES: &str = r"Software\Glance\Capabilities";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Creates `key` under HKCU and sets the value `name` ("" for the default value).
fn set(key: &str, name: &str, value: Option<&str>) -> Result<(), String> {
    unsafe {
        let mut h: HKEY = null_mut();
        let k = wide(key);
        if RegCreateKeyExW(HKEY_CURRENT_USER, k.as_ptr(), 0, null_mut(), 0, KEY_WRITE, null_mut(), &mut h, null_mut())
            != ERROR_SUCCESS
        {
            return Err(format!("can't create HKCU\\{key}"));
        }
        let n = wide(name);
        let r = match value {
            Some(v) => {
                let v = wide(v);
                RegSetValueExW(h, n.as_ptr(), 0, REG_SZ, v.as_ptr().cast(), (v.len() * 2) as u32)
            }
            None => RegSetValueExW(h, n.as_ptr(), 0, REG_NONE, null_mut(), 0),
        };
        RegCloseKey(h);
        if r != ERROR_SUCCESS {
            return Err(format!("can't write HKCU\\{key}\\{name}"));
        }
        Ok(())
    }
}

fn delete_tree(key: &str) {
    let k = wide(key);
    unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, k.as_ptr()) };
    unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, k.as_ptr()) };
}

fn delete_value(key: &str, name: &str) {
    unsafe {
        let mut h: HKEY = null_mut();
        let k = wide(key);
        if RegOpenKeyExW(HKEY_CURRENT_USER, k.as_ptr(), 0, KEY_WRITE, &mut h) == ERROR_SUCCESS {
            let n = wide(name);
            RegDeleteValueW(h, n.as_ptr());
            RegCloseKey(h);
        }
    }
}

fn notify() {
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED as i32, SHCNF_IDLIST, std::ptr::null(), std::ptr::null()) };
}

pub fn register() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.to_string_lossy();
    let open = format!("\"{exe}\" \"%1\"");
    let icon = format!("\"{exe}\",0");
    let classes = r"Software\Classes";

    set(&format!(r"{classes}\{PROG_ID}"), "", Some("Video file"))?;
    set(&format!(r"{classes}\{PROG_ID}\DefaultIcon"), "", Some(&icon))?;
    set(&format!(r"{classes}\{PROG_ID}\shell\open\command"), "", Some(&open))?;

    let app = format!(r"{classes}\Applications\glance.exe");
    set(&app, "FriendlyAppName", Some("Glance"))?;
    set(&format!(r"{app}\shell\open\command"), "", Some(&open))?;

    set(CAPABILITIES, "ApplicationName", Some("Glance"))?;
    set(CAPABILITIES, "ApplicationDescription", Some("Lightweight video player"))?;
    set(CAPABILITIES, "ApplicationIcon", Some(&icon))?;
    for ext in EXTENSIONS {
        set(&format!(r"{classes}\.{ext}\OpenWithProgids"), PROG_ID, None)?;
        set(&format!(r"{app}\SupportedTypes"), &format!(".{ext}"), Some(""))?;
        set(&format!(r"{CAPABILITIES}\FileAssociations"), &format!(".{ext}"), Some(PROG_ID))?;
    }
    set(r"Software\RegisteredApplications", "Glance", Some(CAPABILITIES))?;
    notify();
    Ok(())
}

pub fn unregister() {
    let classes = r"Software\Classes";
    for ext in EXTENSIONS {
        delete_value(&format!(r"{classes}\.{ext}\OpenWithProgids"), PROG_ID);
    }
    delete_tree(&format!(r"{classes}\{PROG_ID}"));
    delete_tree(&format!(r"{classes}\Applications\glance.exe"));
    delete_tree(r"Software\Glance\Capabilities");
    delete_value(r"Software\RegisteredApplications", "Glance");
    notify();
}

/// Opens Settings > Default apps, where the user picks the video player.
pub fn open_default_apps() {
    let verb = wide("open");
    // Windows 11 jumps straight to Glance's page; Windows 10 ignores the parameter.
    let uri = wide("ms-settings:defaultapps?registeredAppUser=Glance");
    unsafe { ShellExecuteW(null_mut(), verb.as_ptr(), uri.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL) };
}

pub fn message(text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK};
    let (t, c) = (wide(text), wide("Glance"));
    unsafe { MessageBoxW(null_mut(), t.as_ptr(), c.as_ptr(), MB_OK | MB_ICONINFORMATION) };
}
