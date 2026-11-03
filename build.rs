// Embeds the icon and version info into glance.exe, and puts the FFmpeg DLLs
// next to the built exe so `cargo run` works.
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=assets/glance.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/glance.ico")
            .set("ProductName", "Glance")
            .set("FileDescription", "Glance video player");
        res.compile().expect("failed to embed Windows resources");
    }

    let Ok(ffmpeg) = std::env::var("FFMPEG_DIR") else { return };
    let bin = PathBuf::from(ffmpeg).join("bin");
    // OUT_DIR is target/<profile>/build/glance-<hash>/out.
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let Some(target) = out.ancestors().nth(3) else { return };
    for entry in std::fs::read_dir(&bin).into_iter().flatten().flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        // avdevice isn't linked; the ffmpeg/ffplay tools aren't needed.
        if name.ends_with(".dll") && !name.starts_with("avdevice") {
            let dest = target.join(&name);
            if !dest.exists() {
                let _ = std::fs::copy(&path, dest);
            }
        }
    }
}
