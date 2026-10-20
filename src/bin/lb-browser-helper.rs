//! CEF helper process for the browser source: Chromium's renderer, GPU and utility processes run
//! this executable (`browser_subprocess_path`), so they don't load the studio itself.

#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
    let args = cef::args::Args::new();
    let code = cef::execute_process(Some(args.as_main_args()), None, std::ptr::null_mut());
    std::process::exit(code.max(0));
}
