// Build script:
// 1. Embed an application manifest with Common Controls v6 (modern tooltips and menus)
//    using only linker switches.
// 2. Compile an icon + version-info resource with the Windows SDK's rc.exe, so the .exe
//    has the app icon in Explorer and shows as "Claude Usage Widget" in Task Manager.
//    Optional: if rc.exe isn't found the build still succeeds, just without them.
use std::path::{Path, PathBuf};

fn find_rc() -> Option<PathBuf> {
    let root = Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let mut versions: Vec<PathBuf> = std::fs::read_dir(root).ok()?.flatten().map(|e| e.path())
        .filter(|p| p.join("x64").join("rc.exe").exists()).collect();
    versions.sort();
    versions.pop().map(|v| v.join("x64").join("rc.exe"))
}

fn main() {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bins=/MANIFESTDEPENDENCY:type='win32' \
         name='Microsoft.Windows.Common-Controls' version='6.0.0.0' \
         processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
    );

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let icon = manifest_dir.join("..").join("assets").join("app.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    println!("cargo:rerun-if-changed=build.rs");
    let Some(rc) = find_rc() else {
        println!("cargo:warning=rc.exe not found: building without the exe icon/version info");
        return;
    };
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ver = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.1.0".into());
    let nums: Vec<&str> = ver.split('.').collect();
    let commas = format!("{},{},{},0", nums.first().unwrap_or(&"0"), nums.get(1).unwrap_or(&"0"), nums.get(2).unwrap_or(&"0"));
    let rc_text = format!(
        r#"1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {commas}
PRODUCTVERSION {commas}
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "FileDescription", "Claude Usage Widget"
      VALUE "ProductName", "Claude Usage Widget"
      VALUE "FileVersion", "{ver}"
      VALUE "ProductVersion", "{ver}"
      VALUE "OriginalFilename", "ClaudeUsageWidget.exe"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        icon = icon.display().to_string().replace('\\', "\\\\")
    );
    let rc_file = out.join("app.rc");
    let res_file = out.join("app.res");
    std::fs::write(&rc_file, rc_text).unwrap();
    let ok = std::process::Command::new(rc).args(["/nologo", "/fo"]).arg(&res_file).arg(&rc_file)
        .status().map(|s| s.success()).unwrap_or(false);
    if ok {
        println!("cargo:rustc-link-arg-bins={}", res_file.display());
    } else {
        println!("cargo:warning=rc.exe failed: building without the exe icon/version info");
    }
}
