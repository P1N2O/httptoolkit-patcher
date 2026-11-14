mod asar;
mod fuses;

use std::env;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process;

const MARKER: &str = "Injected by HTTP Toolkit Patcher";
const PATCH_START: &str = "// ------- Injected by HTTP Toolkit Patcher -------";
const PATCH_END: &str = "// ------- End patched content -------";
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
  --path <dir>     Path to the HTTP Toolkit installation (the install dir or its
                   resources folder)
  --email <addr>   Email for the pro plan (skips the interactive prompt)
  -h, --help       Show this help"
    );
}

fn parse_args() -> (Option<String>, Option<String>, Option<String>) {
    let mut cmd = None;
    let mut path_opt = None;
    let mut email_opt = None;
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
        } else if a == "--email" {
            if i + 1 < args.len() {
                email_opt = Some(args[i + 1].clone());
                i += 1;
            }
        } else if let Some(v) = a.strip_prefix("--email=") {
            email_opt = Some(v.to_string());
        } else if a == "--help" || a == "-h" {
            cmd = Some("help".into());
        } else if !a.starts_with('-') && cmd.is_none() {
            cmd = Some(a.clone());
        }
        i += 1;
    }
    (cmd, path_opt, email_opt)
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

// Strip what an earlier run injected, so re-patching replaces it rather than
// stacking a second copy (its duplicate `const`s would crash the app)
fn strip_previous_patch(source: &str) -> Result<String, String> {
    let Some(start) = source.find(PATCH_START) else {
        return Ok(source.to_string());
    };
    let Some(end_rel) = source[start..].find(PATCH_END) else {
        return Err("Couldn't find the end of the previous patch".into());
    };
    let after = start + end_rel + PATCH_END.len();
    let mut tail = &source[after..];
    if let Some(rest) = tail.strip_prefix("\r\n") {
        tail = rest;
    } else if let Some(rest) = tail.strip_prefix('\n') {
        tail = rest;
    }
    Ok(format!("{}{}", &source[..start], tail))
}

// The APP_URL declaration keyword changed across HTTP Toolkit builds:
// `const` (≤1.27.3), `var` (1.27.4+ ESM builds). Pick whichever is present.
fn app_url_needle(data: &str) -> Option<&'static str> {
    ["const APP_URL =", "var APP_URL =", "let APP_URL ="]
        .into_iter()
        .find(|n| data.contains(n))
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

// All server bundles that back MCP / remote control. HTTP Toolkit may keep the
// server next to the asar in the install dir, and/or user-level client bundles:
// Windows: %LOCALAPPDATA%\httptoolkit-server\client\<version>\bundle\index.js
// Linux:   ~/.local/share/httptoolkit-server/client/<version>/bundle/index.js
// macOS:   ~/Library/Application Support/httptoolkit-server/client/<version>/bundle/index.js
fn get_server_bundle_paths(app_path: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let server_bundle = app_path.join("httptoolkit-server").join("bundle").join("index.js");
    if server_bundle.exists() {
        paths.push(server_bundle);
    }

    let mut client_dirs: Vec<PathBuf> = Vec::new();
    #[cfg(windows)]
    if let Some(la) = env::var_os("LOCALAPPDATA") {
        client_dirs.push(PathBuf::from(la).join("httptoolkit-server").join("client"));
    }
    #[cfg(unix)]
    if let Some(home) = env::var_os("HOME") {
        let home = PathBuf::from(home);
        if cfg!(target_os = "macos") {
            client_dirs.push(home.join("Library").join("Application Support").join("httptoolkit-server").join("client"));
        } else {
            client_dirs.push(home.join(".local").join("share").join("httptoolkit-server").join("client"));
        }
    }
    for dir in client_dirs {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let p = entry.path().join("bundle").join("index.js");
            if p.exists() && !paths.contains(&p) {
                paths.push(p);
            }
        }
    }
    paths
}

// Patch all discovered server bundles so the MCP / remote-control bridge accepts
// our patched UI. Edits:
//   1. Add our proxy origin to the server's ALLOWED_ORIGINS allow-list (widens a
//      deliberate security control - it normally only trusts app.httptoolkit.tech).
//   2. Make the bridge's WebSocket auth succeed for any JWT - we hand the UI a
//      dummy token (see payload.js) since the faked Pro user has no real account JWT.
//      Two shapes exist: v1.27.4+ and the legacy minified `if(!1===A.jwt)` form.
//   3. Force the bridge's isPaidUser() true so Pro-only MCP operations are allowed.
//   4. Suppress the auto-updater ("update-requested") so the server doesn't exit
//      and tear down Electron while patched.
// These strings are minified and version-specific: non-matching patterns are
// skipped (the Pro unlock still works, only MCP remote control is affected).
fn patch_server_bundles(app_path: &Path, port: &str) {
    let bundle_paths = get_server_bundle_paths(app_path);
    if bundle_paths.is_empty() {
        log!("33", "Server bundle not found next to the app, skipping MCP bridge patch");
        return;
    }

    let edits: [(&str, &str, String); 6] = [
        (
            "allow proxy origin",
            // The prod branch of `IS_PROD_BUILD ? [...] : [...]`, whatever the minifier names it
            r"?[/^https:\/\/app\.httptoolkit\.tech$/]:",
            format!(r"?[/^https:\/\/app\.httptoolkit\.tech$/,/^http:\/\/localhost:{}$/]:", port),
        ),
        (
            "accept bridge auth (v1.27.4+)",
            r#"e.user=void 0,A.sendAuthResult(e,{success:!1,error:"JWT validation failed: ".concat(t.message)}),e.ws.close(1007,"JWT validation failed: ".concat(t.message))"#,
            r#"e.user={isPaidUser:function(){return!0}},r&&A.completeInitialAuth(e),A.sendAuthResult(e,{success:!0})"#.to_string(),
        ),
        (
            "accept bridge auth (legacy)",
            r"if(!1===A.jwt){e.user=void 0,r&&this.completeInitialAuth(e)",
            r"if(!0){e.user=void 0,r&&this.completeInitialAuth(e)".to_string(),
        ),
        (
            "force bridge isPaidUser",
            r#"key:"isPaidUser",value:function(){"#,
            r#"key:"isPaidUser",value:function(){return!0;"#.to_string(),
        ),
        (
            "suppress trigger update",
            r#"onTriggerUpdate:function(){return g.emit("update-requested")}"#,
            r#"onTriggerUpdate:function(){return}"#.to_string(),
        ),
        (
            "suppress update-requested listener",
            r#"s.on("update-requested",function(){c.runExclusive(function(){return H().run(["stable"])"#,
            r#"s.on("update-requested",function(){return;c.runExclusive(function(){return H().run(["stable"])"#.to_string(),
        ),
    ];

    for path in &bundle_paths {
        let Ok(mut out) = fs::read_to_string(path) else {
            log!("33", "Couldn't read server bundle: {}", path.display());
            continue;
        };
        if edits.iter().all(|(_, _, to)| out.contains(to.as_str())) {
            log!("32", "Server bundle already patched for MCP: {}", path.display());
        } else {
            let bak = PathBuf::from(format!("{}.bak", path.display()));
            if !bak.exists() {
                let _ = fs::copy(path, &bak);
            }
            let mut applied = 0;
            for (name, from, to) in &edits {
                if out.contains(to.as_str()) {
                    applied += 1;
                    continue;
                }
                let count = out.matches(from).count();
                if count != 1 {
                    log!("33", "Couldn't apply MCP patch '{name}' in {} (pattern changed?) - skipping", path.display());
                    continue;
                }
                out = out.replacen(from, to, 1);
                applied += 1;
            }
            if let Err(e) = fs::write(path, &out) {
                log!("31", "Couldn't write server bundle {}: {e}", path.display());
                continue;
            }
            log!("32", "Patched server bundle for MCP ({} edits) at {}", applied, path.display());
        }

        // The CLI's lib/index.js also listens for update-requested; silence it the
        // same way so the server process doesn't exit on a patched install.
        let lib_index = path.parent().and_then(Path::parent).unwrap_or(path).join("lib").join("index.js");
        if lib_index.exists() && patch_update_listener(&lib_index) {
            log!("32", "Patched update handler in {}", lib_index.display());
        }
    }
}

// Replace the auto-update listener in the CLI's lib/index.js with a no-op.
// The listener statement is `apiServer.on('update-requested', () => {...});`
// (minified or compiled TS) - locate its arrow-function body by brace balance
// and splice a no-op in its place.
fn patch_update_listener(path: &Path) -> bool {
    let Ok(mut data) = fs::read_to_string(path) else { return false };
    const START: &str = "apiServer.on('update-requested',";
    let Some(start) = data.find(START) else { return false };
    let rest = &data[start..];
    // First `{` opens the arrow-function body
    let Some(body_open) = rest.find('{') else { return false };
    let mut depth = 0usize;
    let mut body_close = None;
    for (off, ch) in rest[body_open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    body_close = Some(body_open + off);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(body_close) = body_close else { return false };
    // The statement ends with `});` right after the body close
    let Some(end_rel) = rest[body_close..].find("});") else { return false };
    let end = start + body_close + end_rel + 3;
    if data[start..end].contains("() => {});") {
        return true; // already patched
    }
    let bak = PathBuf::from(format!("{}.bak", path.display()));
    if !bak.exists() {
        let _ = fs::copy(path, &bak);
    }
    data.replace_range(start..end, "apiServer.on('update-requested', () => {});");
    fs::write(path, data).is_ok()
}

fn restore_server_bundle(app_path: &Path) {
    for path in get_server_bundle_paths(app_path) {
        let bak = PathBuf::from(format!("{}.bak", path.display()));
        if bak.exists() {
            let _ = fs::copy(&bak, &path);
            let _ = fs::remove_file(&bak);
            log!("32", "Restored original server bundle at {}", path.display());
        }
        let lib_index = path.parent().and_then(Path::parent).unwrap_or(&path).join("lib").join("index.js");
        let lib_bak = PathBuf::from(format!("{}.bak", lib_index.display()));
        if lib_bak.exists() {
            let _ = fs::copy(&lib_bak, &lib_index);
            let _ = fs::remove_file(&lib_bak);
            log!("32", "Restored original lib/index.js at {}", lib_index.display());
        }
    }
}

fn cmd_patch(custom: Option<String>, email_opt: Option<String>) {
    let app_path = get_app_path(custom.as_deref());
    let file_path = app_path.join("app.asar");
    let temp_path = app_path.join("app");
    let exe = exe_path(&app_path);
    let port = env::var("PATCHER_PORT").unwrap_or_else(|_| "5067".into());

    if !file_path.exists() {
        log!("31", "Couldn't find HTTP Toolkit! Make sure it's installed.");
        process::exit(1);
    }
    log!("34", "Found HTTP Toolkit at {}", bold(&app_path.parent().unwrap_or(&app_path).display().to_string()));

    // An already-patched app gets re-patched in place, so a patcher update only
    // needs another `patch` run. Its .bak then still holds the original app.
    let was_patched = file_contains(&file_path, MARKER.as_bytes());

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

    let email = match email_opt {
        Some(e) if e.contains('@') => e,
        Some(_) => {
            log!("31", "Invalid email provided via --email");
            cleanup_exit(&app_path);
        }
        None => match prompt_email() {
            Some(e) => e,
            None => {
                log!("31", "No email provided");
                cleanup_exit(&app_path);
            }
        },
    };

    let mut data = match fs::read_to_string(&index_path) {
        Ok(d) => d,
        Err(e) => {
            log!("31", "Error reading index.js: {e}");
            cleanup_exit(&app_path);
        }
    };

    // Re-patching: strip the block an earlier run injected so we don't stack a
    // second copy (duplicate `const`s would crash the app).
    match strip_previous_patch(&data) {
        Ok(stripped) => data = stripped,
        Err(e) => {
            log!("31", "{e} - run restore first");
            cleanup_exit(&app_path);
        }
    }

    // Some builds ship a CommonJS entry point, where the payload's top-level
    // `await import()` is a syntax error and the app won't even start. Detect
    // via package.json `type`, falling back to sniffing the entry point.
    let is_esm = match fs::read_to_string(temp_path.join("package.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(|t| t == "module"))
    {
        Some(esm) => esm,
        None => data.lines().any(|l| {
            let l = l.trim_start();
            l.starts_with("import ") && l.contains(" from ")
        }),
    };
    log!("33", "Entry point looks like {}", if is_esm { "ESM" } else { "CommonJS" });

    let mut payload = include_str!("../payload.js").to_string();
    if !is_esm {
        payload = payload.replace("await import('node:", "require('node:");
    }

    // v1.27.4+ ESM builds declare `var APP_URL = process.env.APP_URL || ...`,
    // older builds used `const`. Match whichever keyword the build uses and
    // inject the patch right before the declaration.
    let needle = match app_url_needle(&data) {
        Some(n) => n,
        None => {
            log!("31", "Patch failed - couldn't find injection point");
            cleanup_exit(&app_path);
        }
    };
    let prefix = format!(
        "{PATCH_START}\nconst email = `{}`\nconst globalProxy = process.env.PROXY ?? `{}`\n{payload}\n{PATCH_END}\n{needle}",
        email.replace('`', "\\`"),
        global_proxy.clone().unwrap_or_default().replace('`', "\\`"),
    );
    let patched = data.replacen(needle, &prefix, 1);
    if patched == data {
        log!("31", "Patch failed - couldn't find injection point");
        cleanup_exit(&app_path);
    }
    if let Err(e) = fs::write(&index_path, patched) {
        log!("31", "Error writing index.js: {e}");
        cleanup_exit(&app_path);
    }
    log!("32", "Patched index.js successfully");

    // When re-patching, app.asar is our own output: backing it up would replace
    // the original and leave `restore` nothing to restore.
    let bak = app_path.join("app.asar.bak");
    if !was_patched {
        if let Err(e) = fs::copy(&file_path, &bak) {
            log!("31", "Error creating backup: {e}{}", permission_hint(e.kind() == std::io::ErrorKind::PermissionDenied));
            cleanup_exit(&app_path);
        }
        log!("32", "Created backup at {}", bold(&bak.display().to_string()));
    } else if bak.exists() {
        log!("32", "Keeping the backup of the original app at {}", bold(&bak.display().to_string()));
    } else {
        log!("33", "No backup of the original app found, so restore won't work - reinstall HTTP Toolkit to undo the patch");
    }

    log!("33", "Repacking the app...");
    if let Err(e) = asar::create_package(&temp_path, &file_path) {
        let perm = asar::is_permission(&e);
        log!("31", "Error repacking app: {e}{}", permission_hint(perm));
        cleanup_exit(&app_path);
    }
    rm_path(&temp_path);
    log!("32", "App patched!");

    // Patch the node server(s) so the MCP / remote-control bridge works with our UI
    patch_server_bundles(&app_path, &port);

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

    // Patching changes code inside the signed bundle, so macOS kills the app at
    // launch: CODESIGNING, Invalid Page. Re-sign the whole bundle ad-hoc.
    resign_mac_app(&app_path);
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
    // The fuses stay flipped and the files just changed again
    resign_mac_app(&app_path);
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
            s.contains(r"?[/^https:\/\/app\.httptoolkit\.tech$/,/^http:\/\/localhost:")
                || s.contains(r#"key:"isPaidUser",value:function(){return!0;"#)
                || s.contains(r#"e.user={isPaidUser:function(){return!0}}"#)
                || s.contains(r#"onTriggerUpdate:function(){return}"#)
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

// Patching changes code inside the signed bundle (the fuses live in Electron
// Framework), so macOS kills the app at launch: CODESIGNING, Invalid Page.
// Re-sign the whole bundle ad-hoc. Not @electron/fuses' resetAdHocDarwinSignature:
// it keeps the hardened runtime, whose library validation then refuses to load
// the ad-hoc signed framework.
#[cfg(target_os = "macos")]
fn resign_mac_app(app_path: &Path) {
    use std::process::Command;
    // app_path = <app>.app/Contents/Resources -> the .app bundle is two levels up
    let app_bundle = app_path.parent().and_then(Path::parent).unwrap_or(app_path);
    log!("33", "Re-signing {}...", bold(&app_bundle.display().to_string()));
    // Best effort: quarantine makes Gatekeeper reject the new signature, and
    // other extended attributes make codesign refuse to sign at all
    let _ = Command::new("xattr").arg("-cr").arg(app_bundle).status();
    match Command::new("codesign").args(["--force", "--deep", "--sign", "-"]).arg(app_bundle).status() {
        Ok(st) if st.success() => log!("32", "App re-signed successfully"),
        _ => log!("31", "Re-signing failed, so the app will crash on launch - run codesign --force --deep --sign - \"{}\"", app_bundle.display()),
    }
}

#[cfg(not(target_os = "macos"))]
fn resign_mac_app(_app_path: &Path) {}

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
    let (cmd, path_opt, email_opt) = parse_args();
    match cmd.as_deref() {
        None | Some("patch") => cmd_patch(path_opt, email_opt),
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
