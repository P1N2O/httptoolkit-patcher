mod asar;
mod fuses;

use std::env;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process;

const MARKER: &str = "Injected by HTTP Toolkit Patcher";
const CACHE_DIR: &str = "httptoolkit-patch";

const PRISMA_PKG: &str = r#"{"name":"@prisma/instrumentation","version":"0.0.0-stub","main":"index.js","type":"module"}"#;
const PRISMA_STUB: &str = r#"export class PrismaInstrumentation {
  constructor(config) { this._config = config; }
  instrumentationName = "@prisma/instrumentation";
  instrumentationVersion = "0.0.0";
  enable() {}
  disable() {}
  setTracerProvider() {}
  setMeterProvider() {}
  getConfig() { return this._config || {}; }
}
export default PrismaInstrumentation;
"#;

// ANSI colors on unix, plain on windows (cmd.exe lacks VT by default)
#[cfg(unix)]
fn paint(code: &str, s: &str) -> String {
    format!("\x1b[{code}m{s}\x1b[0m")
}
#[cfg(not(unix))]
fn paint(_code: &str, s: &str) -> String {
    s.to_string()
}

macro_rules! log {
    ($code:expr, $($arg:tt)*) => {{
        let msg = format!($($arg)*);
        println!("{}", $crate::paint($code, &msg));
    }};
}

fn bold(s: &str) -> String {
    paint("1", s)
}

fn usage() {
    println!(
        "Usage: httptoolkit-patcher [command] [options]

Commands:
  (default)  Patch HTTP Toolkit (same as 'patch')
  patch      Patch HTTP Toolkit using the specified script
  restore    Restore HTTP Toolkit files to their original state
  status     Show whether HTTP Toolkit is patched
  start      Start HTTP Toolkit

Options:
  --path <dir>  Path to the HTTP Toolkit installation (the install dir or its
                resources folder)
  -h, --help    Show this help"
    );
}

fn parse_args() -> (Option<String>, Option<String>) {
    let mut cmd = None;
    let mut path_opt = None;
    let mut i = 0;
    let args: Vec<String> = env::args().skip(1).collect();
    while i < args.len() {
        let a = &args[i];
        if a == "--path" {
            if i + 1 < args.len() {
                path_opt = Some(args[i + 1].clone());
                i += 1;
            }
        } else if let Some(v) = a.strip_prefix("--path=") {
            path_opt = Some(v.to_string());
        } else if a == "--help" || a == "-h" {
            cmd = Some("help".into());
        } else if !a.starts_with('-') && cmd.is_none() {
            cmd = Some(a.clone());
        }
        i += 1;
    }
    (cmd, path_opt)
}

fn tmpdir() -> PathBuf {
    env::temp_dir()
}

// Try to find where HTTP Toolkit is installed
fn get_app_path(custom: Option<&str>) -> PathBuf {
    let is_mac = cfg!(target_os = "macos");
    if let Some(p) = custom {
        return if p.to_ascii_lowercase().ends_with("resources") {
            PathBuf::from(p)
        } else {
            let base = PathBuf::from(p);
            base.join(if is_mac { "Contents/Resources" } else { "resources" })
        };
    }
    let candidates: Vec<PathBuf> = if cfg!(windows) {
        let la = env::var("LOCALAPPDATA").unwrap_or_default();
        let pf = env::var("PROGRAMFILES").unwrap_or_default();
        let pfx = env::var("PROGRAMFILES(X86)").unwrap_or_default();
        vec![
            PathBuf::from(&la).join("Programs").join("HTTP Toolkit").join("resources"),
            PathBuf::from(&la).join("Programs").join("httptoolkit").join("HTTP Toolkit").join("resources"),
            PathBuf::from(&la).join("Programs").join("httptoolkit").join("resources"),
            PathBuf::from(&pf).join("HTTP Toolkit").join("resources"),
            PathBuf::from(&pfx).join("HTTP Toolkit").join("resources"),
        ]
    } else if is_mac {
        vec![PathBuf::from("/Applications/HTTP Toolkit.app/Contents/Resources")]
    } else {
        vec![
            PathBuf::from("/opt/HTTP Toolkit/resources"),
            PathBuf::from("/opt/httptoolkit/resources"),
            PathBuf::from("/usr/lib/httptoolkit"),
            PathBuf::from("/usr/share/httptoolkit/resources"),
        ]
    };
    for p in &candidates {
        if !p.as_os_str().is_empty() && p.join("app.asar").exists() {
            return p.clone();
        }
    }
    candidates[0].clone()
}

fn exe_path(app_path: &Path) -> PathBuf {
    let dir = app_path.parent().unwrap_or(app_path);
    if cfg!(windows) {
        dir.join("HTTP Toolkit.exe")
    } else if cfg!(target_os = "macos") {
        dir.join("MacOS").join("HTTP Toolkit")
    } else {
        dir.join("httptoolkit")
    }
}

#[cfg(unix)]
fn is_sudo() -> bool {
    (unsafe { libc::getuid() == 0 }) || env::var_os("SUDO_UID").is_some()
}

#[cfg(not(unix))]
fn is_sudo() -> bool {
    false
}

fn rm_path(p: &Path) {
    let meta = match fs::symlink_metadata(p) {
        Ok(m) => m,
        Err(_) => return,
    };
    if meta.is_dir() {
        let _ = fs::remove_dir_all(p);
    } else {
        let _ = fs::remove_file(p);
    }
}

fn file_contains(path: &Path, needle: &[u8]) -> bool {
    let mut f = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut buf = vec![0u8; 1 << 20];
    let mut tail: Vec<u8> = Vec::new();
    loop {
        let n = match f.read(&mut buf) {
            Ok(n) => n,
            Err(_) => return false,
        };
        if n == 0 {
            return false;
        }
        let mut chunk = Vec::with_capacity(tail.len() + n);
        chunk.extend_from_slice(&tail);
        chunk.extend_from_slice(&buf[..n]);
        if chunk.windows(needle.len()).any(|w| w == needle) {
            return true;
        }
        let keep = needle.len().saturating_sub(1).min(chunk.len());
        tail = chunk[chunk.len() - keep..].to_vec();
    }
}

fn permission_hint(is_perm: bool) -> &'static str {
    if !is_perm {
        return "";
    }
    if cfg!(windows) {
        ", try running as administrator"
    } else {
        ", try running with sudo"
    }
}

fn cleanup_exit(app_path: &Path) -> ! {
    rm_path(&app_path.join("app"));
    rm_path(&tmpdir().join(CACHE_DIR));
    process::exit(1);
}

fn prompt_email() -> Option<String> {
    loop {
        print!("Enter an email for the pro plan: ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) => return None,
            Ok(_) => {
                let e = line.trim().to_string();
                if e.contains('@') {
                    return Some(e);
                }
                println!("Invalid email");
            }
            Err(_) => return None,
        }
    }
}

fn patch_server_bundle(path: &Path, port: &str) {
    if !path.exists() {
        log!("33", "Server bundle not found at {}, skipping MCP bridge patch", path.display());
        return;
    }
    let edits: [(&str, &str, String); 3] = [
        (
            "allow proxy origin",
            r"ei?[/^https:\/\/app\.httptoolkit\.tech$/]:",
            format!(r"ei?[/^https:\/\/app\.httptoolkit\.tech$/,/^http:\/\/localhost:{}$/]:", port),
        ),
        (
            "accept bridge auth",
            r"if(!1===A.jwt){e.user=void 0,r&&this.completeInitialAuth(e)",
            r"if(!0){e.user=void 0,r&&this.completeInitialAuth(e)".to_string(),
        ),
        (
            "force bridge isPaidUser",
            r#"key:"isPaidUser",value:function(){"#,
            r#"key:"isPaidUser",value:function(){return!0;"#.to_string(),
        ),
    ];
    let data = match fs::read_to_string(path) {
        Ok(d) => d,
        Err(e) => {
            log!("33", "Couldn't read server bundle: {e}");
            return;
        }
    };
    if edits.iter().all(|(_, _, to)| data.contains(to.as_str())) {
        log!("32", "Server bundle already patched for MCP");
        return;
    }
    let bak = PathBuf::from(format!("{}.bak", path.display()));
    if !bak.exists() {
        let _ = fs::copy(path, &bak);
    }
    let mut out = data;
    let mut applied = 0;
    for (name, from, to) in &edits {
        if out.contains(to.as_str()) {
            applied += 1;
            continue;
        }
        let count = out.matches(from).count();
        if count != 1 {
            log!("33", "Couldn't apply MCP patch '{name}' (pattern changed?) - skipping");
            continue;
        }
        out = out.replacen(from, to, 1);
        applied += 1;
    }
    if applied == 0 {
        log!("33", "No MCP bridge patches applied - remote control may be unavailable");
        return;
    }
    if let Err(e) = fs::write(path, &out) {
        log!("31", "Couldn't write server bundle: {e}");
        return;
    }
    log!("32", "Patched server bundle for MCP ({applied}/{})", edits.len());
}

fn restore_server_bundle(app_path: &Path) {
    let bundle = app_path.join("httptoolkit-server").join("bundle").join("index.js");
    let bak = PathBuf::from(format!("{}.bak", bundle.display()));
    if bak.exists() {
        let _ = fs::copy(&bak, &bundle);
        let _ = fs::remove_file(&bak);
        log!("32", "Restored original server bundle");
    }
}

fn cmd_patch(custom: Option<String>) {
    let app_path = get_app_path(custom.as_deref());
    let file_path = app_path.join("app.asar");
    let temp_path = app_path.join("app");
    let server_bundle = app_path.join("httptoolkit-server").join("bundle").join("index.js");
    let exe = exe_path(&app_path);
    let port = env::var("PATCHER_PORT").unwrap_or_else(|_| "5067".into());

    if !file_path.exists() {
        log!("31", "Couldn't find HTTP Toolkit! Make sure it's installed.");
        process::exit(1);
    }
    log!("34", "Found HTTP Toolkit at {}", bold(&app_path.parent().unwrap_or(&app_path).display().to_string()));

    if file_contains(&file_path, MARKER.as_bytes()) {
        log!("32", "App is already patched!");
        return;
    }

    let global_proxy = env::var("PROXY").ok().filter(|p| !p.is_empty());
    log!("34", "Starting the patch process...");
    if let Some(p) = &global_proxy {
        if !(p.starts_with("http://") || p.starts_with("https://")) {
            log!("31", "Global proxy must start with http:// or https://");
            process::exit(1);
        }
        log!("33", "Using custom proxy: {}", bold(p));
    }

    log!("33", "Extracting app files...");
    rm_path(&temp_path);
    if let Err(e) = asar::extract_all(&file_path, &temp_path) {
        let perm = asar::is_permission(&e);
        log!("31", "Error extracting app: {e}{}", permission_hint(perm));
        cleanup_exit(&app_path);
    }

    // Replace @prisma/instrumentation with a stub - it has a broken nested
    // node_modules reference to @opentelemetry/instrumentation that causes a
    // crash after repacking. @sentry/node's prisma integration imports from it,
    // so we can't just delete it - we need a stub that exports the expected
    // symbols as noops.
    let prisma = temp_path.join("node_modules").join("@prisma").join("instrumentation");
    if prisma.exists() {
        rm_path(&prisma);
        let _ = fs::create_dir_all(&prisma);
        let _ = fs::write(prisma.join("package.json"), PRISMA_PKG);
        let _ = fs::write(prisma.join("index.js"), PRISMA_STUB);
        log!("33", "Replaced broken @prisma/instrumentation with stub");
    }

    let index_path = temp_path.join("build").join("index.js");
    if !index_path.exists() {
        log!("31", "Couldn't find index.js file");
        cleanup_exit(&app_path);
    }

    let email = match prompt_email() {
        Some(e) => e,
        None => {
            log!("31", "No email provided");
            cleanup_exit(&app_path);
        }
    };

    let data = match fs::read_to_string(&index_path) {
        Ok(d) => d,
        Err(e) => {
            log!("31", "Error reading index.js: {e}");
            cleanup_exit(&app_path);
        }
    };

    let payload = include_str!("../payload.js");
    let prefix = format!(
        "// ------- Injected by HTTP Toolkit Patcher -------\nconst email = `{}`\nconst globalProxy = process.env.PROXY ?? `{}`\n{}\n// ------- End patched content -------\nconst APP_URL =",
        email.replace('`', "\\`"),
        global_proxy.clone().unwrap_or_default().replace('`', "\\`"),
        payload
    );
    let patched = data.replacen("const APP_URL =", &prefix, 1);
    if patched == data {
        log!("31", "Patch failed - couldn't find injection point");
        cleanup_exit(&app_path);
    }
    if let Err(e) = fs::write(&index_path, patched) {
        log!("31", "Error writing index.js: {e}");
        cleanup_exit(&app_path);
    }
    log!("32", "Patched index.js successfully");

    let bak = app_path.join("app.asar.bak");
    if let Err(e) = fs::copy(&file_path, &bak) {
        log!("31", "Error creating backup: {e}{}", permission_hint(e.kind() == std::io::ErrorKind::PermissionDenied));
        cleanup_exit(&app_path);
    }
    log!("32", "Created backup at {}", bold(&bak.display().to_string()));

    log!("33", "Repacking the app...");
    if let Err(e) = asar::create_package(&temp_path, &file_path) {
        let perm = asar::is_permission(&e);
        log!("31", "Error repacking app: {e}{}", permission_hint(perm));
        cleanup_exit(&app_path);
    }
    rm_path(&temp_path);
    log!("32", "App patched!");

    // Patch the node server so the MCP / remote-control bridge works with our UI
    patch_server_bundle(&server_bundle, &port);

    // Drop any cached UI so main.js is re-fetched and re-patched (picks up the
    // MCP userJwt fix in payload.js on next launch).
    rm_path(&tmpdir().join(CACHE_DIR));

    // Disable ASAR integrity checks so Electron doesn't complain
    log!("33", "Disabling ASAR integrity checks...");
    if !exe.exists() {
        log!("33", "Executable not found at {}, skipping fuse flipping", exe.display());
    } else {
        match fuses::flip(&exe) {
            Ok(()) => log!("32", "Integrity checks disabled successfully"),
            Err(e) => log!("33", "Failed to flip fuses (might already be done or not supported): {e}"),
        }
    }
}

fn cmd_restore(custom: Option<String>) {
    let app_path = get_app_path(custom.as_deref());
    let file_path = app_path.join("app.asar");
    log!("34", "Restoring app...");
    let bak = app_path.join("app.asar.bak");
    if !bak.exists() {
        log!("31", "Backup file not found - maybe the app wasn't patched?");
    } else {
        if let Err(e) = fs::copy(&bak, &file_path) {
            log!("31", "Error restoring app: {e}{}", permission_hint(e.kind() == std::io::ErrorKind::PermissionDenied));
            process::exit(1);
        }
        log!("32", "App restored successfully");
    }
    restore_server_bundle(&app_path);
    rm_path(&tmpdir().join(CACHE_DIR));
}

fn cmd_status(custom: Option<String>) {
    let app_path = get_app_path(custom.as_deref());
    let file_path = app_path.join("app.asar");
    if !file_path.exists() {
        log!("31", "Couldn't find HTTP Toolkit! Make sure it's installed.");
        process::exit(1);
    }
    let patched = file_contains(&file_path, MARKER.as_bytes());
    let backup = app_path.join("app.asar.bak").exists();
    let bundle = app_path.join("httptoolkit-server").join("bundle").join("index.js");
    let bundle_patched = match fs::read_to_string(&bundle) {
        Ok(s) => {
            s.contains(r"ei?[/^https:\/\/app\.httptoolkit\.tech$/,/^http:\/\/localhost:")
                || s.contains(r#"key:"isPaidUser",value:function(){return!0;"#)
        }
        Err(_) => false,
    };

    log!("34", "HTTP Toolkit at {}", bold(&app_path.parent().unwrap_or(&app_path).display().to_string()));
    if patched {
        log!("32", "Status: PATCHED");
    } else {
        log!("31", "Status: NOT PATCHED");
    }
    log!("37", "Backup (app.asar.bak): {}", if backup { "exists" } else { "missing" });
    match (bundle.exists(), bundle_patched) {
        (true, true) => log!("32", "MCP server bundle: patched"),
        (true, false) => log!("31", "MCP server bundle: original"),
        (false, _) => log!("37", "MCP server bundle: not found"),
    }
}

fn cmd_start(custom: Option<String>) {
    let app_path = get_app_path(custom.as_deref());
    log!("34", "Starting HTTP Toolkit...");
    let result = if cfg!(windows) {
        process::Command::new(exe_path(&app_path)).status()
    } else if cfg!(target_os = "macos") {
        process::Command::new("open").arg("-a").arg("HTTP Toolkit").status()
    } else {
        process::Command::new("httptoolkit").status()
    };
    match result {
        Ok(st) => process::exit(st.code().unwrap_or(0)),
        Err(e) => {
            log!("31", "Error starting app: {e}");
            if is_sudo() {
                log!("31", "Try running without sudo");
            }
            process::exit(1);
        }
    }
}

fn main() {
    let (cmd, path_opt) = parse_args();
    match cmd.as_deref() {
        None | Some("patch") => cmd_patch(path_opt),
        Some("restore") => cmd_restore(path_opt),
        Some("status") => cmd_status(path_opt),
        Some("start") => cmd_start(path_opt),
        Some("help") => usage(),
        Some(other) => {
            log!("31", "Unknown command: {other}");
            usage();
            process::exit(1);
        }
    }
    if cmd.as_deref() != Some("help") {
        log!("32", "All done!");
    }
}
