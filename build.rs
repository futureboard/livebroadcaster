//! Windows: embed the application manifest (OS compatibility, per-monitor DPI, common controls
//! v6) in the executables, and delay-load libcef.dll so the studio starts even without the
//! Chromium runtime (only the browser source then reports it missing).

fn main() {
    println!("cargo::rerun-if-changed=res/app.manifest");
    let target = std::env::var("TARGET").unwrap_or_default();
    if !target.contains("windows-msvc") {
        return;
    }
    let manifest = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("res").join("app.manifest");
    println!("cargo::rustc-link-arg-bins=/MANIFEST:EMBED");
    println!("cargo::rustc-link-arg-bins=/MANIFESTINPUT:{}", manifest.display());
    if std::env::var_os("CARGO_FEATURE_BROWSER").is_some() {
        println!("cargo::rustc-link-arg-bin=live-broadcaster=/DELAYLOAD:libcef.dll");
        println!("cargo::rustc-link-arg-bin=live-broadcaster=delayimp.lib");
    }
}
