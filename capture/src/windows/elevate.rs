//! Administrator rights: recording needs them, because Windows only lets administrators
//! trace frames and network traffic.

use super::util::{wide, Owned};
use std::time::Duration;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_CANCELLED, HANDLE};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, WaitForSingleObject, PROCESS_SYNCHRONIZE,
};
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

pub fn elevated() -> bool {
    let mut token = HANDLE::default();
    // SAFETY: the pseudo handle needs no closing; the token handle is closed by `Owned`.
    unsafe {
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let token = Owned(token);
        let mut info = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        GetTokenInformation(
            token.0,
            TokenElevation,
            Some(&mut info as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
        .is_ok()
            && info.TokenIsElevated != 0
    }
}

/// Quotes one command line argument the way Windows programs split them.
fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut slashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
                out.push('"');
                slashes = 0;
                continue;
            }
            _ => {}
        }
        if c != '\\' {
            out.extend(std::iter::repeat_n('\\', slashes));
            slashes = 0;
            out.push(c);
        }
    }
    out.extend(std::iter::repeat_n('\\', slashes * 2));
    out.push('"');
    out
}

pub fn relaunch_elevated(args: &[&str]) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("Couldn't find Culprit's own program file: {e}"))?;
    let file = wide(&exe.to_string_lossy());
    let params = wide(&args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" "));
    let verb = wide("runas");
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: every string outlives the call.
    match unsafe { ShellExecuteExW(&mut info) } {
        Ok(()) => Ok(()),
        Err(e) if e.code() == ERROR_CANCELLED.to_hresult() => Err(
            "Recording needs administrator rights, because Windows only lets administrators watch frames and network traffic."
                .into(),
        ),
        Err(e) => Err(format!("Couldn't restart Culprit as administrator: {}", e.message())),
    }
}

pub fn wait_for_exit(pid: u32, timeout: Duration) {
    // SAFETY: the process handle is closed by `Owned`.
    unsafe {
        if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            let h = Owned(h);
            WaitForSingleObject(h.0, timeout.as_millis().min(u32::MAX as u128) as u32);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::quote;

    #[test]
    fn quotes_arguments() {
        assert_eq!(quote("--record"), "--record");
        assert_eq!(quote("a b"), "\"a b\"");
        assert_eq!(quote(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote(r"C:\dir with space\"), r#""C:\dir with space\\""#);
        assert_eq!(quote(""), "\"\"");
    }
}
