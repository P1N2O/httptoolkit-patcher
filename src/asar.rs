// ASAR archive reader/writer, format-compatible with @electron/asar 3.x.
//
// Layout: [u32 size-pickle payload (=4)][u32 header len][header pickle][file data]
//   header pickle = [u32 payload size][u32 json len][json, 4-byte aligned]
//   file offsets are relative to 8 + header len, one byte per file, no padding.
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const BLOCK_SIZE: usize = 4 * 1024 * 1024; // integrity block size, matches @electron/asar
const HEX: &[u8; 16] = b"0123456789abcdef";

#[derive(Debug)]
pub enum AsarError {
    Io(std::io::Error),
    Msg(String),
}

impl fmt::Display for AsarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AsarError::Io(e) => write!(f, "{e}"),
            AsarError::Msg(m) => write!(f, "{m}"),
        }
    }
}

impl From<std::io::Error> for AsarError {
    fn from(e: std::io::Error) -> Self {
        AsarError::Io(e)
    }
}

impl From<String> for AsarError {
    fn from(m: String) -> Self {
        AsarError::Msg(m)
    }
}

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

pub fn is_permission(e: &AsarError) -> bool {
    matches!(e, AsarError::Io(io) if io.kind() == std::io::ErrorKind::PermissionDenied)
}

fn err<T>(m: impl Into<String>) -> Result<T, AsarError> {
    Err(AsarError::Msg(m.into()))
}

fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for &x in b {
        s.push(HEX[(x >> 4) as usize] as char);
        s.push(HEX[(x & 0xf) as usize] as char);
    }
    s
}

fn align4(n: usize) -> usize {
    (n + 3) & !3
}

/// Parse the header of an asar archive held fully in memory.
pub fn read_header(data: &[u8]) -> Result<(Value, usize), AsarError> {
    if data.len() < 8 {
        return err("not an asar archive (too small)");
    }
    // Modern archives: u32@0 is the size-pickle payload (=4), u32@4 is the
    // header pickle length. Legacy archives stored the length directly at 0.
    let mut header_len = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
    if header_len == 0 {
        header_len = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
    }
    if 8 + header_len > data.len() {
        return err("truncated asar header");
    }
    let hb = &data[8..8 + header_len];
    if hb.len() < 8 {
        return err("bad asar header pickle");
    }
    let str_len = i32::from_le_bytes(hb[4..8].try_into().unwrap()) as usize;
    if 8 + str_len > hb.len() {
        return err("bad asar header string length");
    }
    let s = std::str::from_utf8(&hb[8..8 + str_len]).map_err(|e| AsarError::Msg(format!("asar header not utf8: {e}")))?;
    let json: Value = serde_json::from_str(s).map_err(|e| AsarError::Msg(format!("bad asar header json: {e}")))?;
    Ok((json, header_len))
}

/// Extract every packed file of `archive` into `dest`. Unpacked entries (which
/// live next to the archive in `<archive>.unpacked`) are skipped.
pub fn extract_all(archive: &Path, dest: &Path) -> Result<(), AsarError> {
    let data = fs::read(archive)?;
    let (json, header_len) = read_header(&data)?;
    let files = json.get("files").ok_or_else(|| AsarError::Msg("asar header has no files".into()))?;
    let data_start = 8 + header_len;
    fs::create_dir_all(dest)?;
    extract_node(files, dest, &data, data_start)
}

fn parse_offset(v: Option<&Value>) -> Result<usize, AsarError> {
    let v = v.ok_or_else(|| AsarError::Msg("file node missing offset".into()))?;
    if let Some(s) = v.as_str() {
        s.parse().map_err(|_| AsarError::Msg("bad file offset".into()))
    } else if let Some(n) = v.as_u64() {
        Ok(n as usize)
    } else {
        err("file node missing offset")
    }
}

fn extract_node(node: &Value, dest: &Path, data: &[u8], data_start: usize) -> Result<(), AsarError> {
    let map = node.as_object().ok_or_else(|| AsarError::Msg("bad asar files node".into()))?;
    for (name, child) in map {
        let child_path = dest.join(name);
        if let Some(files) = child.get("files") {
            fs::create_dir_all(&child_path)?;
            extract_node(files, &child_path, data, data_start)?;
        } else if child.get("unpacked").is_some() {
            continue; // already on disk in <archive>.unpacked
        } else if let Some(link) = child.get("link").and_then(Value::as_str) {            #[cfg(unix)]
            {
                let _ = fs::remove_file(&child_path);
                // link is relative to the archive root; recreate relative to the
                // symlink's parent so the on-disk tree mirrors the archive.
                let target = relative_path(child_path.parent().unwrap_or(dest), &dest.join(link));
                std::os::unix::fs::symlink(&target, &child_path)?;
            }
            #[cfg(windows)]
            {
                // extractAll follows links on Windows: write the target's content.
                let bytes = resolve_linked(node, link, data, data_start)?;
                fs::write(&child_path, bytes)?;
            }
        } else if child.get("size").is_none() && child.get("offset").is_none() {
            // bare {} node = empty directory
            fs::create_dir_all(&child_path)?;
        } else {
            let size = child.get("size").and_then(Value::as_u64).ok_or_else(|| AsarError::Msg(format!("file node {name} missing size")))?
                as usize;
            let start = data_start + parse_offset(child.get("offset"))?;
            let end = start + size;
            if end > data.len() {
                return err(format!("file {name} out of archive range"));
            }
            fs::write(&child_path, &data[start..end])?;
            if child.get("executable").and_then(Value::as_bool) == Some(true) {
                #[cfg(unix)]
                set_executable(&child_path);
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn relative_path(from_dir: &Path, to: &Path) -> PathBuf {
    use std::path::Component;
    let f: Vec<Component> = from_dir.components().collect();
    let t: Vec<Component> = to.components().collect();
    let mut common = 0;
    while common < f.len() && common < t.len() && f[common] == t[common] {
        common += 1;
    }
    let mut out = PathBuf::new();
    for _ in common..f.len() {
        out.push("..");
    }
    for c in &t[common..] {
        out.push(c.as_os_str());
    }
    out
}

#[cfg(windows)]
fn resolve_linked(root: &Value, link: &str, data: &[u8], data_start: usize) -> Result<Vec<u8>, AsarError> {
    let mut node = root;
    for comp in link.split('/') {
        if comp.is_empty() {
            continue;
        }
        node = node
            .get("files")
            .and_then(|f| f.get(comp))
            .ok_or_else(|| AsarError::Msg(format!("link target {link} not found in archive")))?;
    }
    let size = node.get("size").and_then(Value::as_u64).ok_or_else(|| AsarError::Msg("link target missing size".into()))? as usize;
    let start = data_start + parse_offset(node.get("offset"))?;
    Ok(data[start..start + size].to_vec())
}

#[cfg(unix)]
fn set_executable(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o755));
}

/// Pack every file under `src` into a new asar at `dest`. File order and
/// offsets are deterministic (sorted paths); each file gets a SHA-256
/// integrity record like @electron/asar 3.x writes.
pub fn create_package(src: &Path, dest: &Path) -> Result<(), AsarError> {
    let mut paths: Vec<PathBuf> = Vec::new();
    collect(src, src, &mut paths)?;
    paths.sort();

    let mut root: Map<String, Value> = Map::new();
    let mut offset: u64 = 0;
    let mut files: Vec<PathBuf> = Vec::new();

    for rel in &paths {
        let full = src.join(rel);
        let meta = fs::symlink_metadata(&full)?;
        if meta.is_dir() {
            get_dir(&mut root, rel)?; // ensure the tree path exists
        } else {
            // regular file; symlinks are followed (none in the target app)
            let size = meta.len();
            if size > u32::MAX as u64 {
                return err(format!("{}: file size can not be larger than 4.2GB", rel.display()));
            }
            let parent = match rel.parent() {
                Some(p) if !p.as_os_str().is_empty() => get_dir(&mut root, p)?,
                _ => &mut root,
            };
            let name = rel.file_name().unwrap().to_string_lossy().to_string();
            let mut node = Map::new();
            node.insert("size".into(), Value::from(size));
            node.insert("offset".into(), Value::from(offset.to_string()));
            let bytes = fs::read(&full)?;
            node.insert("integrity".into(), integrity_json(&bytes));
            #[cfg(unix)]
            if meta.permissions().mode() & 0o100 != 0 {
                node.insert("executable".into(), Value::from(true));
            }
            parent.insert(name, Value::Object(node));
            offset += size;
            files.push(rel.clone());
        }
    }

    let mut root_node = Map::new();
    root_node.insert("files".into(), Value::Object(root));
    let json = serde_json::to_string(&Value::Object(root_node)).map_err(|e| AsarError::Msg(format!("asar header serialize: {e}")))?;
    let str_len = json.len();
    let payload = 4 + align4(str_len); // [str len][json padded]
    let header_len = 4 + payload; // [payload size][str len][json padded]

    let mut out = File::create(dest)?;
    // size pickle: payload size (=4) + header length
    out.write_all(&4u32.to_le_bytes())?;
    out.write_all(&(header_len as u32).to_le_bytes())?;
    // header pickle: payload size, string length, json, padding
    out.write_all(&(payload as u32).to_le_bytes())?;
    out.write_all(&(str_len as u32).to_le_bytes())?;
    out.write_all(json.as_bytes())?;
    out.write_all(&[0u8; 4][..align4(str_len) - str_len])?;
    // file data in the same order offsets were assigned
    for rel in &files {
        let mut f = File::open(src.join(rel))?;
        let mut buf = [0u8; 1 << 20];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
        }
    }
    Ok(())
}

fn get_dir<'a>(root: &'a mut Map<String, Value>, rel: &Path) -> Result<&'a mut Map<String, Value>, AsarError> {
    let mut cur = root;
    for comp in rel.components() {
        let name = comp.as_os_str().to_string_lossy().to_string();
        let dir_node = cur.entry(name).or_insert_with(|| Value::Object(Map::new()));
        let files = dir_node
            .as_object_mut()
            .ok_or_else(|| AsarError::Msg("path collision in asar tree".into()))?
            .entry("files")
            .or_insert_with(|| Value::Object(Map::new()));
        cur = files.as_object_mut().ok_or_else(|| AsarError::Msg("path collision in asar tree".into()))?;
    }
    Ok(cur)
}

fn collect(base: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), AsarError> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)?.map(|e| e.map(|e| e.path())).collect::<Result<_, _>>()?;
    entries.sort();
    for p in entries {
        let rel = p.strip_prefix(base).map_err(|_| AsarError::Msg("strip_prefix failed".into()))?.to_path_buf();
        out.push(rel.clone());
        let meta = fs::symlink_metadata(&p)?;
        if meta.is_dir() {
            collect(base, &p, out)?;
        }
    }
    Ok(())
}

fn integrity_json(bytes: &[u8]) -> Value {
    let mut h = Sha256::new();
    h.update(bytes);
    let hash = hex(&h.finalize());
    let blocks: Vec<Value> = bytes
        .chunks(BLOCK_SIZE)
        .map(|chunk| {
            let mut bh = Sha256::new();
            bh.update(chunk);
            Value::from(hex(&bh.finalize()))
        })
        .collect();
    serde_json::json!({
        "algorithm": "SHA256",
        "hash": hash,
        "blockSize": BLOCK_SIZE,
        "blocks": blocks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tree(root: &Path) {
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::write(root.join("a/b/empty.js"), b"").unwrap();
        fs::write(root.join("a/b/c.js"), b"console.log(1);").unwrap();
        fs::write(root.join("a/top.css"), b"body{}").unwrap();
        fs::write(root.join("big.bin"), vec![7u8; 4 * 1024 * 1024 + 10]).unwrap();
        fs::write(root.join("root.txt"), b"hello asar").unwrap();
        fs::create_dir_all(root.join("empty-dir")).unwrap();
    }

    #[test]
    fn round_trip() {
        let tmp = std::env::temp_dir().join(format!("asar-rt-{}", std::process::id()));
        let src = tmp.join("src");
        let packed = tmp.join("out.asar");
        let dst = tmp.join("dst");
        let _ = fs::remove_dir_all(&tmp);
        tree(&src);
        create_package(&src, &packed).unwrap();
        extract_all(&packed, &dst).unwrap();
        for rel in ["a/b/empty.js", "a/b/c.js", "a/top.css", "big.bin", "root.txt"] {
            let a = fs::read(src.join(rel)).unwrap();
            let b = fs::read(dst.join(rel)).unwrap();
            assert_eq!(a, b, "content mismatch for {rel}");
        }
        assert!(dst.join("a").is_dir());
        assert!(dst.join("empty-dir").is_dir());
        // offsets must be cumulative file sizes in sorted path order
        let data = fs::read(&packed).unwrap();
        let (json, header_len) = read_header(&data).unwrap();
        let files = json["files"].as_object().unwrap();
        let big = files["big.bin"].as_object().unwrap();
        let root_txt = files["root.txt"].as_object().unwrap();
        let big_off: usize = big["offset"].as_str().unwrap().parse().unwrap();
        let txt_off: usize = root_txt["offset"].as_str().unwrap().parse().unwrap();
        assert_eq!(txt_off, big_off + big["size"].as_u64().unwrap() as usize); // "root.txt" right after "big.bin"
        assert_eq!(big_off, 21); // "a/b/empty.js"(0) + "a/b/c.js"(13) + "a/top.css"(8)
        let _ = fs::remove_dir_all(&tmp);
    }
}


