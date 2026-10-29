//! Starting with the computer, so a scheduled or queued download can run without opening the app first: a LaunchAgent
//! on macOS, the Run key of the registry on Windows. The app then starts in the tray only.

use std::path::{Path, PathBuf};

/// Starts the app without its window.
pub const BACKGROUND: &str = "--background";
#[cfg(target_os = "macos")]
const LABEL: &str = "com.eliasbui.lanes";
#[cfg(windows)]
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const NAME: &str = "Lanes";

fn exe() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("Không tìm thấy app: {e}"))
}

/// The LaunchAgent. `open` starts the bundle as Finder does; a binary outside a bundle is started directly.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn plist(label: &str, exe: &Path) -> String {
    let bundle = exe.ancestors().nth(3).filter(|p| p.extension().is_some_and(|e| e == "app"));
    let args: Vec<String> = match bundle {
        Some(app) => vec!["/usr/bin/open".into(), "-a".into(), app.display().to_string(), "--args".into(), BACKGROUND.into()],
        None => vec![exe.display().to_string(), BACKGROUND.into()],
    };
    let xml = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let strings: String = args.iter().map(|a| format!("<string>{}</string>", xml(a))).collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n\
         \x20   <key>Label</key><string>{label}</string>\n\
         \x20   <key>ProgramArguments</key><array>{strings}</array>\n\
         \x20   <key>RunAtLoad</key><true/>\n\
         </dict>\n</plist>\n"
    )
}

#[cfg(target_os = "macos")]
fn file() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join("Library/LaunchAgents").join(format!("{LABEL}.plist"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn file() -> PathBuf {
    dirs::config_dir().unwrap_or_default().join("autostart/lanes.desktop")
}

#[cfg(all(unix, not(target_os = "macos")))]
fn desktop_entry(exe: &Path) -> String {
    format!("[Desktop Entry]\nType=Application\nName=Lanes\nExec=\"{}\" {BACKGROUND}\nX-GNOME-Autostart-enabled=true\n", exe.display())
}

#[cfg(unix)]
pub fn enabled() -> bool {
    file().is_file()
}

#[cfg(unix)]
pub fn set(on: bool) -> Result<(), String> {
    let path = file();
    if !on {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        };
    }
    #[cfg(target_os = "macos")]
    let text = plist(LABEL, &exe()?);
    #[cfg(not(target_os = "macos"))]
    let text = desktop_entry(&exe()?);
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, text).map_err(|e| format!("Không ghi được {}: {e}", path.display()))
}

#[cfg(windows)]
pub fn enabled() -> bool {
    crate::browser::reg_command()
        .args(["query", RUN_KEY, "/v", NAME])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(windows)]
pub fn set(on: bool) -> Result<(), String> {
    let mut cmd = crate::browser::reg_command();
    if on {
        let value = format!("\"{}\" {BACKGROUND}", exe()?.display());
        cmd.args(["add", RUN_KEY, "/v", NAME, "/t", "REG_SZ", "/d", &value, "/f"]);
    } else if enabled() {
        cmd.args(["delete", RUN_KEY, "/v", NAME, "/f"]);
    } else {
        return Ok(());
    }
    let out = cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().map_err(|e| e.to_string())?;
    if out.success() { Ok(()) } else { Err("Windows không cho ghi mục khởi động".into()) }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    #[test]
    fn the_entry_starts_the_bundle_in_the_background() {
        use super::*;
        let app = plist("com.example", Path::new("/Applications/Lanes.app/Contents/MacOS/lanes"));
        assert!(
            app.contains(
                "<string>/usr/bin/open</string><string>-a</string><string>/Applications/Lanes.app</string><string>--args</string><string>--background</string>"
            ),
            "{app}"
        );
        assert!(app.contains("<key>RunAtLoad</key><true/>"));
        let bare = plist("com.example", Path::new("/tmp/R&D/lanes"));
        assert!(bare.contains("<array><string>/tmp/R&amp;D/lanes</string><string>--background</string></array>"), "{bare}");
    }
}
