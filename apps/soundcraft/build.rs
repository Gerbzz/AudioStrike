//! Windows only: embed version info (VERSIONINFO) into `audiostrike.exe`, so it
//! shows in Explorer, the taskbar, the Start menu and Alt-Tab.
//!
//! On every other target this does nothing. A missing resource compiler is a warning, so a
//! cross-compile from macOS or Linux still links, unless `AUDIOSTRIKE_REQUIRE_WINRES=1` turns it
//! into an error (for release builds).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/branding/AudioStrike.ico");
    println!("cargo:rerun-if-env-changed=AUDIOSTRIKE_REQUIRE_WINRES");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/branding/AudioStrike.ico")
        .set("ProductName", "AudioStrike")
        .set("FileDescription", "AudioStrike digital audio workstation")
        .set("LegalCopyright", "SoundCraft contributors and AudioStrike contributors. MIT OR Apache-2.0.")
        .set("OriginalFilename", "audiostrike.exe")
        .set("InternalName", "audiostrike");
    if let Err(e) = res.compile() {
        if std::env::var_os("AUDIOSTRIKE_REQUIRE_WINRES").is_some() {
            eprintln!("embedding Windows resources failed: {e}");
            std::process::exit(1);
        }
        println!("cargo:warning=audiostrike.exe built without version resources: {e}");
    }
}
