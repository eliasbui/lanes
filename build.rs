use std::{env, fs, path::Path};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=ui/parts");
    parts();
    // The ffmpeg scripts/build-ffmpeg.sh made, carried inside the app when the build names it (src/ytdlp.rs).
    println!("cargo:rustc-check-cfg=cfg(ffmpeg)");
    println!("cargo:rerun-if-env-changed=LANES_FFMPEG");
    if let Some(path) = env::var_os("LANES_FFMPEG") {
        let path = fs::canonicalize(&path).expect("LANES_FFMPEG names the ffmpeg to carry");
        println!("cargo:rustc-cfg=ffmpeg");
        println!("cargo:rustc-env=LANES_FFMPEG={}", path.display());
    }
    // The icon of the .exe on Windows; the other platforms take theirs from the bundle or the window. The target is
    // read from the environment, since cfg(windows) in a build script is the machine that builds, not the one built for.
    if env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows") && Path::new("assets/icon.ico").exists() {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.compile().expect("Windows resources");
    }
}

/// Each feature keeps its script and style in ui/parts; they go after the page's own, in the order of their names,
/// each script in a tag of its own so one that fails leaves the others running.
fn parts() {
    let mut files: Vec<_> = fs::read_dir("ui/parts").map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    files.sort();
    let mut html = String::new();
    for path in &files {
        let text = fs::read_to_string(path).expect("a part of the page");
        assert!(!text.to_lowercase().contains("</script"), "{} must not contain </script>", path.display());
        match path.extension().and_then(|e| e.to_str()) {
            Some("css") => html += &format!("<style>\n{text}</style>\n"),
            Some("js") => html += &format!("<script>\n{text}</script>\n"),
            _ => {}
        }
    }
    fs::write(Path::new(&env::var("OUT_DIR").unwrap()).join("parts.html"), html).expect("parts.html");
}
