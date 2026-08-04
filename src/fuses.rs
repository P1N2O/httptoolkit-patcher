// Electron fuse flipping, byte-compatible with @electron/fuses 1.x.
//
// Fuse wire layout (V1): [sentinel 28 bytes]["dL7pKGdnNz796PbbjQWNKmHXBZaB9tsX"
// is the sentinel][u8 version (=1)][u8 fuse count][one byte per fuse].
// Fuse byte values: 48 ('0') = disabled, 49 ('1') = enabled, 144 = inherit.
// V1 fuse order: RunAsNode, EnableCookieEncryption,
// EnableNodeOptionsEnvironmentVariable, EnableNodeCliInspectArguments,
// EnableEmbeddedAsarIntegrityValidation, OnlyLoadAppFromAsar,
// LoadBrowserProcessSpecificV8Snapshot, GrantFileProtocolExtraPrivileges.
use std::fs;
use std::path::{Component, Path, PathBuf};

const SENTINEL: &[u8] = b"dL7pKGdnNz796PbbjQWNKmHXBZaB9tsX";
const DISABLED: u8 = 48; // FuseState.DISABLE
const V1_ASAR_INTEGRITY: usize = 4;
const V1_ONLY_LOAD_APP: usize = 5;

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).rposition(|w| w == needle)
}

/// The binary that holds the fuses. On macOS the fuses live in the Electron
/// Framework binary inside the .app bundle, not in the app executable.
fn fuse_file_path(exe: &Path) -> PathBuf {
    let s = exe.to_string_lossy();
    if s.ends_with(".app") {
        exe.join("Contents").join("Frameworks").join("Electron Framework.framework").join("Electron Framework")
    } else if s.contains(".app") {
        normalize(&exe.join("..").join("..").join("Frameworks").join("Electron Framework.framework").join("Electron Framework"))
    } else {
        exe.to_path_buf()
    }
}

fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Disable EnableEmbeddedAsarIntegrityValidation and OnlyLoadAppFromAsar in
/// the Electron binary. Universal (two-slice) binaries get both sentinels
/// flipped, same as @electron/fuses.
pub fn flip(exe: &Path) -> Result<(), String> {
    let fuse_file = fuse_file_path(exe);
    let mut data = fs::read(&fuse_file).map_err(|e| format!("read {:?}: {e}", fuse_file))?;
    let first = find(&data, SENTINEL).ok_or("could not find fuse sentinel (Electron 12 or higher required)")?;
    let last = rfind(&data, SENTINEL).unwrap();
    let sentinels: Vec<usize> = if first == last { vec![first] } else { vec![first, last] };
    for pos in sentinels {
        let wire = pos + SENTINEL.len();
        if data.get(wire) != Some(&1) {
            return Err(format!("fuse wire version at offset {wire} is not V1"));
        }
        let count = data[wire + 1] as usize;
        if count <= V1_ONLY_LOAD_APP {
            return Err(format!("fuse wire is too short ({count} fuses)"));
        }
        data[wire + 2 + V1_ASAR_INTEGRITY] = DISABLED;
        data[wire + 2 + V1_ONLY_LOAD_APP] = DISABLED;
    }
    fs::write(&fuse_file, data).map_err(|e| format!("write {:?}: {e}", fuse_file))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn flips_both_sentinels() {
        let mut bin = Vec::new();
        // a fake "binary" with two sentinels (universal build) and a V1 wire
        bin.extend_from_slice(&[0u8; 64]);
        bin.extend_from_slice(SENTINEL);
        bin.push(1); // version
        bin.push(8); // 8 fuses
        bin.extend_from_slice(&[49, 49, 49, 49, 49, 49, 49, 49]); // all enabled
        bin.extend_from_slice(&[0u8; 64]);
        bin.extend_from_slice(SENTINEL);
        bin.push(1);
        bin.push(8);
        bin.extend_from_slice(&[49, 49, 49, 49, 49, 49, 49, 49]);
        let tmp = std::env::temp_dir().join(format!("fuse-test-{}", std::process::id()));
        let mut f = fs::File::create(&tmp).unwrap();
        f.write_all(&bin).unwrap();
        drop(f);
        flip(&tmp).unwrap();
        let out = fs::read(&tmp).unwrap();
        let first = find(&out, SENTINEL).unwrap();
        let last = rfind(&out, SENTINEL).unwrap();
        assert_eq!(out[first + SENTINEL.len() + 2 + 4], 48);
        assert_eq!(out[first + SENTINEL.len() + 2 + 5], 48);
        assert_eq!(out[first + SENTINEL.len() + 2 + 0], 49); // untouched
        assert_eq!(out[last + SENTINEL.len() + 2 + 4], 48);
        assert_eq!(out[last + SENTINEL.len() + 2 + 5], 48);
        fs::remove_file(&tmp).unwrap();
    }

    #[test]
    fn missing_sentinel_errors() {
        let tmp = std::env::temp_dir().join(format!("fuse-test-err-{}", std::process::id()));
        fs::write(&tmp, b"not an electron binary").unwrap();
        assert!(flip(&tmp).is_err());
        fs::remove_file(&tmp).unwrap();
    }
}

