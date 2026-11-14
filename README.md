# HTTP Toolkit Patcher

A simple, lightweight patcher to enable Pro features in HTTP Toolkit.

Based on the JS version by [ayyo42069](https://github.com/ayyo42069/HTTPToolkit-Patcher-2026)
\*\*Original by [@XielQ](https://github.com/XielQs)

## Usage

```bash
# Patch (no arguments = patch, same as the explicit command)
httptoolkit-patcher
httptoolkit-patcher patch

# Show whether the app is patched
httptoolkit-patcher status

# Restore original (if needed)
httptoolkit-patcher restore

# Start HTTP Toolkit
httptoolkit-patcher start

# Specify a custom install path (the install dir or its resources folder)
httptoolkit-patcher patch --path "C:\Program Files\HTTP Toolkit"
httptoolkit-patcher status --path "/opt/HTTP Toolkit/resources"

# Skip the interactive email prompt
httptoolkit-patcher patch --email pro@example.com

# Linux / macOS may need elevated permissions to write into the install dir
sudo ./httptoolkit-patcher patch
```

Works on Windows, macOS, and Linux x64/arm64. Install locations are
auto-detected on each platform; use `--path` to point at a non-default
location. The binary decides at runtime which platform it is on and applies
the matching patch (executable name, install paths, fuse location).

## Patching on each OS

Download the binary for your OS/arch from [releases](https://github.com/p1n2o/httptoolkit-patcher/releases).

| OS      | Architecture  | Binary                                   | Command                                    |
| ------- | ------------- | ---------------------------------------- | ------------------------------------------ |
| Windows | x64           | `httptoolkit-patcher-windows-x86_64.exe` | `httptoolkit-patcher-windows-x86_64.exe`   |
| macOS   | Intel         | `httptoolkit-patcher-mac-x86_64`         | `sudo ./httptoolkit-patcher-mac-x86_64`    |
| macOS   | Apple Silicon | `httptoolkit-patcher-mac-aarch64`        | `sudo ./httptoolkit-patcher-mac-aarch64`   |
| Linux   | x64           | `httptoolkit-patcher-linux-x86_64`       | `sudo ./httptoolkit-patcher-linux-x86_64`  |
| Linux   | ARM64         | `httptoolkit-patcher-linux-aarch64`      | `sudo ./httptoolkit-patcher-linux-aarch64` |

Windows (cmd or PowerShell):

```bat
httptoolkit-patcher-windows-x86_64.exe
```

macOS / Linux need `sudo` because the install dir (`/Applications`,
`/opt/...`) is root-owned. First run `chmod +x` on the binary:

```bash
chmod +x httptoolkit-patcher-mac-aarch64
sudo ./httptoolkit-patcher-mac-aarch64
```

macOS Gatekeeper: a binary downloaded from the browser may be blocked; allow
it with `xattr -d com.apple.quarantine httptoolkit-patcher-*` first.

Same commands work for `status`, `restore`, and `start`:

```bash
sudo ./httptoolkit-patcher-mac-aarch64 status
sudo ./httptoolkit-patcher-mac-aarch64 restore
```

## How it works

1. Extracts `app.asar` from HTTP Toolkit
2. Injects a local proxy server (Node builtins only - no npm packages to
   install) that intercepts UI requests
3. Patches the subscription data on-the-fly to enable Pro features
4. Flips Electron fuses to bypass ASAR integrity checks
5. Repacks the app

The injected server is plain JavaScript (it must run inside the app's own
Node runtime) but it only uses `node:http`/`node:https`/`node:net`/`node:tls`/
`node:fs`/`node:os`/`node:path` builtins, so the patcher itself never needs to
install anything.

## MCP / remote control

The patch keeps HTTP Toolkit's MCP (and CLI remote-control) bridge working. To
do this it also patches the bundled node server (`httptoolkit-server/bundle/index.js`,
restored by `httptoolkit-patcher restore`) to:

- allow the patched UI's local origin through the server's `ALLOWED_ORIGINS` list,
- accept the bridge WebSocket auth (the faked Pro account has no real JWT), and
- treat the session as Pro so Pro-only operations are available over MCP.

## Notes

- Creates a backup at `app.asar.bak` (and `httptoolkit-server/bundle/index.js.bak`) before patching
- `status` shows PATCHED/NOT PATCHED plus backup and MCP-bridge state
- You can set a custom proxy with the `PROXY` environment variable (used by
  the injected UI proxy at app runtime)
- Use `--path` to point at a non-default install location; auto-detection covers
  `Program Files` and the newer `Programs\httptoolkit\HTTP Toolkit` layout on
  Windows, `/Applications/HTTP Toolkit.app` on macOS, and `/opt`, `/usr/lib`,
  `/usr/share` locations on Linux
- Works offline after first run (caches UI files locally)
- Patching needs no network and no Node.js; the binary is fully standalone

## Building

```bash
# native build
./build.sh

# all OS/arch binaries (needs cargo-zigbuild for cross-linking)
rustup target add aarch64-unknown-linux-gnu x86_64-pc-windows-gnu \
  x86_64-apple-darwin aarch64-apple-darwin
cargo install cargo-zigbuild
./build.sh all
```

Outputs land in `dist/` as `httptoolkit-patcher-<os>-<arch>[.exe]`

## License

MIT

## Disclaimer

Please buy the original license from [HTTP Toolkit](https://httptoolkit.com/pricing/) after trying the program.
