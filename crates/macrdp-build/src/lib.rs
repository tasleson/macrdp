use std::env;
use std::path::PathBuf;
use std::process::Command;

/// Candidate paths for the Swift Concurrency runtime library, ordered by priority.
pub fn swift_lib_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    // User-specified path (highest priority)
    if let Ok(path) = env::var("SWIFT_LIB_PATH") {
        candidates.push(PathBuf::from(path));
    }

    // System Swift runtime (usually sufficient on macOS 12.3+)
    candidates.push(PathBuf::from("/usr/lib/swift"));

    // Detect via xcode-select
    if let Ok(output) = Command::new("xcode-select").arg("-p").output() {
        if output.status.success() {
            let dev_path = String::from_utf8_lossy(&output.stdout).trim().to_string();

            if dev_path.contains("CommandLineTools") {
                candidates.push(PathBuf::from(format!("{dev_path}/usr/lib/swift/macosx")));
                candidates.push(PathBuf::from(format!(
                    "{dev_path}/usr/lib/swift-5.5/macosx"
                )));
            } else {
                // Full Xcode installation
                candidates.push(PathBuf::from(format!(
                    "{dev_path}/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift/macosx"
                )));
                candidates.push(PathBuf::from(format!(
                    "{dev_path}/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift-5.5/macosx"
                )));
            }
        }
    }

    // Common fallback paths
    candidates.push(PathBuf::from(
        "/Library/Developer/CommandLineTools/usr/lib/swift/macosx",
    ));
    candidates.push(PathBuf::from(
        "/Library/Developer/CommandLineTools/usr/lib/swift-5.5/macosx",
    ));

    candidates
}

/// Returns true on macOS 12.3+, where the system ships Swift Concurrency in /usr/lib/swift.
pub fn has_modern_system_swift_runtime() -> bool {
    if !cfg!(target_os = "macos") {
        return false;
    }

    let Ok(output) = Command::new("sw_vers").arg("-productVersion").output() else {
        return false;
    };
    if !output.status.success() {
        return false;
    }

    let version = String::from_utf8_lossy(&output.stdout);
    let mut parts = version.trim().split('.');
    let major = parts.next().and_then(|part| part.parse::<u32>().ok());
    let minor = parts.next().and_then(|part| part.parse::<u32>().ok());

    matches!((major, minor), (Some(major), _) if major > 12)
        || matches!((major, minor), (Some(12), Some(minor)) if minor >= 3)
}

/// Locate `libswift_Concurrency.dylib` and emit the `cargo:rustc-link-arg` rpath directive.
///
/// Search order: `SWIFT_LIB_PATH` env var, modern system runtime shortcut (macOS 12.3+),
/// then the full candidate list from `swift_lib_candidates()`.
pub fn link_swift_concurrency() {
    let target_lib = "libswift_Concurrency.dylib";

    if let Ok(path) = env::var("SWIFT_LIB_PATH") {
        let p = PathBuf::from(&path);
        if p.join(target_lib).exists() {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{}", p.display());
            return;
        }
    }

    if has_modern_system_swift_runtime() {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
        return;
    }

    for candidate in swift_lib_candidates() {
        if candidate.join(target_lib).exists() {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{}", candidate.display());
            return;
        }
    }

    println!("cargo:warning=Could not find {target_lib}. Set SWIFT_LIB_PATH env var to the directory containing it.");
}
