//! Safe archive handling for mashup downloads.
//!
//! ZIP files are unpacked in-process, so the common mashup format works without extra system
//! packages. `bsdtar` (libarchive) or `7z` is only needed for 7z/RAR and uncommon tar compressions.
//! Before extracting we list the archive and refuse absolute paths, `..`, links, devices and absurd
//! sizes; after extracting we walk the tree again and refuse links. The archive is never extracted
//! into a directory that already contains files, so a malicious entry cannot overwrite anything.

use super::model::{normalize_entry, StagedEntry};
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

const MAX_FILES: usize = 200_000;
const MAX_BYTES: u64 = 40 * 1024 * 1024 * 1024;

fn is_zip(path: &Path) -> bool {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return false,
    };
    let mut magic = [0u8; 4];
    if file.read_exact(&mut magic).is_err() {
        return false;
    }
    magic == *b"PK\x03\x04" || magic == *b"PK\x05\x06" || magic == *b"PK\x07\x08"
}

fn open_zip(archive: &Path) -> Result<zip::ZipArchive<File>, String> {
    let file = File::open(archive).map_err(|e| format!("Could not open the downloaded archive: {e}"))?;
    zip::ZipArchive::new(file).map_err(|e| format!("Could not read the ZIP archive: {e}"))
}

/// Validate one ZIP entry and return its normalized relative path, uncompressed size and kind.
fn zip_entry_info<R: Read>(entry: &zip::read::ZipFile<'_, R>) -> Result<(String, u64, bool), String> {
    if entry.encrypted() {
        return Err("The archive is password protected, which mashups are not allowed to use.".into());
    }
    if entry.is_symlink() {
        return Err("The archive contains a link, which mashups are not allowed to install.".into());
    }
    if let Some(mode) = entry.unix_mode() {
        let kind = mode & 0o170000;
        if kind != 0 && kind != 0o100000 && kind != 0o040000 {
            return Err("The archive contains a special file, which mashups are not allowed to install.".into());
        }
    }
    let raw = entry.name().map_err(|e| format!("Could not read an archive entry name: {e}"))?;
    let clean = normalize_entry(&raw).ok_or_else(|| format!("The archive contains an unsafe path: {raw}"))?;
    Ok((clean, entry.size(), entry.is_dir()))
}

fn copy_entry<R: Read>(mut source: R, target: &mut File, total: &mut u64) -> Result<(), String> {
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = source.read(&mut buffer).map_err(|e| format!("Could not unpack the download: {e}"))?;
        if read == 0 {
            return Ok(());
        }
        *total = total
            .checked_add(read as u64)
            .ok_or_else(|| "The archive unpacks to more than 40 GiB.".to_string())?;
        if *total > MAX_BYTES {
            return Err("The archive unpacks to more than 40 GiB.".into());
        }
        target.write_all(&buffer[..read]).map_err(|e| format!("Could not unpack the download: {e}"))?;
    }
}

pub fn external_tool() -> Option<&'static str> {
    detect_tool()
}

fn detect_tool() -> Option<&'static str> {
    static TOOL: OnceLock<Option<&'static str>> = OnceLock::new();
    *TOOL.get_or_init(|| {
        // Prefer libarchive: one command for 7z/rar and uncommon tar compressions.
        for name in ["bsdtar", "/usr/bin/bsdtar", "/bin/bsdtar"] {
            if Command::new(name).arg("--version").output().is_ok() {
                return Some(name);
            }
        }
        for name in ["7z", "7zz", "7za"] {
            if Command::new(name).arg("i").output().is_ok() {
                return Some(name);
            }
        }
        None
    })
}

fn tool() -> Result<&'static str, String> {
    detect_tool().ok_or_else(|| {
        "This archive format needs `bsdtar` (libarchive) or `7z`. ZIP files do not need an extra package; install `libarchive` (or `p7zip`) only for 7z/RAR downloads.".into()
    })
}

/// Entries of an archive after sanity checks. Directories are included so empty folders can be created.
pub fn list(archive: &Path) -> Result<Vec<StagedEntry>, String> {
    if is_zip(archive) {
        return list_zip(archive);
    }
    let tool = tool()?;
    let (out, err, ok) = if Path::new(tool).file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("bsdtar")) {
        run(Command::new(tool).args(["-tvf"]).arg(archive).env("LC_ALL", "C"))
    } else {
        run(Command::new(tool).args(["l", "-slt"]).arg(archive))
    };
    if !ok {
        return Err(format!("Could not read the downloaded archive: {}", err.trim()));
    }
    let mut entries = Vec::new();
    if Path::new(tool).file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("bsdtar")) {
        for line in out.lines() {
            let kind = line.trim_start().as_bytes().first().copied().unwrap_or(b'-');
            if matches!(kind, b'l' | b'h' | b'b' | b'c' | b'p' | b's') {
                return Err("The archive contains a link or special file, which mashups are not allowed to install.".into());
            }
            let Some(entry) = parse_tar_line(line) else { continue };
            entries.push(entry);
        }
    } else {
        // 7z -slt: blocks separated by blank lines, one `Path = …` and `Size = …` per entry.
        let mut path: Option<String> = None;
        let mut size = 0u64;
        let mut folder = false;
        for line in out.lines().chain(std::iter::once("")) {
            if line.trim().is_empty() {
                if let Some(path) = path.take() {
                    let path = path.replace('\\', "/");
                    if let Some(clean) = normalize_entry(&path) {
                        if !is_archive_header(&clean, archive) {
                            entries.push(StagedEntry { path: clean, size: if folder { 0 } else { size } });
                        }
                    } else if !is_archive_header(&path, archive) {
                        return Err(format!("The archive contains an unsafe path: {path}"));
                    }
                }
                size = 0;
                folder = false;
                continue;
            }
            if let Some(value) = line.strip_prefix("Path = ") {
                path = Some(value.to_string());
            } else if let Some(value) = line.strip_prefix("Size = ") {
                size = value.trim().parse().unwrap_or(0);
            } else if let Some(value) = line.strip_prefix("Folder = ") {
                folder = value.trim() == "+";
            } else if line.starts_with("Attributes = ") && (line.contains(" L") || line.contains(" l")) {
                return Err("The archive contains a link, which mashups are not allowed to install.".into());
            }
        }
    }
    if entries.len() > MAX_FILES {
        return Err("The archive contains too many files.".into());
    }
    let total: u64 = entries.iter().map(|e| e.size).sum();
    if total > MAX_BYTES {
        return Err("The archive unpacks to more than 40 GiB.".into());
    }
    // A second pass with -tf catches absolute paths that some tar listings normalise away.
    if Path::new(tool).file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("bsdtar")) {
        let (names, _, ok) = run(Command::new(tool).args(["-tf"]).arg(archive).env("LC_ALL", "C"));
        if ok {
            for name in names.lines() {
                let clean = name.trim_end_matches('/');
                if clean.is_empty() {
                    continue;
                }
                if name.starts_with('/') || clean.split('/').any(|p| p == "..") || normalize_entry(clean).is_none() {
                    return Err(format!("The archive contains an unsafe path: {name}"));
                }
            }
        }
    }
    Ok(entries)
}

fn list_zip(archive: &Path) -> Result<Vec<StagedEntry>, String> {
    let mut zip = open_zip(archive)?;
    if zip.len() > MAX_FILES {
        return Err("The archive contains too many files.".into());
    }
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    let mut total = 0u64;
    for index in 0..zip.len() {
        let entry = zip.by_index(index).map_err(|e| format!("Could not read the ZIP archive: {e}"))?;
        let (path, size, is_dir) = zip_entry_info(&entry)?;
        if !seen.insert(path.clone()) {
            return Err(format!("The archive contains the same path more than once: {path}"));
        }
        total = total
            .checked_add(size)
            .ok_or_else(|| "The archive unpacks to more than 40 GiB.".to_string())?;
        if total > MAX_BYTES {
            return Err("The archive unpacks to more than 40 GiB.".into());
        }
        entries.push(StagedEntry { path, size: if is_dir { 0 } else { size } });
        if entries.len() > MAX_FILES {
            return Err("The archive contains too many files.".into());
        }
    }
    Ok(entries)
}

fn is_archive_header(path: &str, archive: &Path) -> bool {
    let name = archive.file_name().and_then(|n| n.to_str()).unwrap_or("");
    path == name || path == archive.to_string_lossy()
}

fn parse_tar_line(line: &str) -> Option<StagedEntry> {
    let line = line.trim_end();
    if line.is_empty() {
        return None;
    }
    let mode = line.as_bytes().first().copied()?;
    if !matches!(mode, b'-' | b'd') {
        return None; // links/devices are rejected by the caller because they are not listed
    }
    // -rw-r--r--  0 owner group 123 Oct 22 01:44 path
    let mut rest = line;
    let mut fields = 0;
    while fields < 4 {
        let cut = rest.find(char::is_whitespace)?;
        rest = rest[cut..].trim_start();
        fields += 1;
    }
    // `rest` now starts at the size field.
    let cut = rest.find(char::is_whitespace)?;
    let size = rest[..cut].parse().unwrap_or(0);
    rest = rest[cut..].trim_start();
    // Skip the date (`Oct 22 01:44` with LC_ALL=C, or a numeric date).
    let mut date_parts = 0;
    while date_parts < 3 {
        let cut = rest.find(char::is_whitespace)?;
        rest = rest[cut..].trim_start();
        date_parts += 1;
    }
    let path = rest.trim_end_matches('/').replace('\\', "/");
    if path.is_empty() {
        return None;
    }
    let clean = normalize_entry(&path)?;
    Some(StagedEntry { path: clean, size: if mode == b'd' { 0 } else { size } })
}

/// Extract into an empty directory and return all regular files (relative paths).
pub fn extract(archive: &Path, dest: &Path) -> Result<Vec<StagedEntry>, String> {
    if dest.exists() && std::fs::read_dir(dest).map(|mut d| d.next().is_some()).unwrap_or(true) {
        return Err("Refused to extract into a non-empty folder.".into());
    }
    std::fs::create_dir_all(dest).map_err(|e| format!("Could not create staging folder: {e}"))?;
    if is_zip(archive) {
        return extract_zip(archive, dest);
    }
    let tool = tool()?;
    let result = if Path::new(tool).file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("bsdtar")) {
        run(Command::new(tool).args(["-xf"]).arg(archive).arg("-C").arg(dest).arg("--no-same-owner").env("LC_ALL", "C"))
    } else {
        run(Command::new(tool).arg("x").arg("-y").arg(format!("-o{}", dest.display())).arg(archive))
    };
    if !result.2 {
        return Err(format!("Could not unpack the download: {}", result.1.trim()));
    }
    let mut entries = Vec::new();
    walk(dest, dest, &mut entries, &mut 0)?;
    Ok(entries)
}

fn extract_zip(archive: &Path, dest: &Path) -> Result<Vec<StagedEntry>, String> {
    let mut zip = open_zip(archive)?;
    if zip.len() > MAX_FILES {
        return Err("The archive contains too many files.".into());
    }
    let mut seen = HashSet::new();
    let mut total = 0u64;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(|e| format!("Could not read the ZIP archive: {e}"))?;
        let (path, _size, is_dir) = zip_entry_info(&entry)?;
        if !seen.insert(path.clone()) {
            return Err(format!("The archive contains the same path more than once: {path}"));
        }
        let target = dest.join(&path);
        if is_dir {
            std::fs::create_dir_all(&target).map_err(|e| format!("Could not unpack the download: {e}"))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("Could not unpack the download: {e}"))?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|e| format!("Could not unpack the download: {e}"))?;
        copy_entry(&mut entry, &mut file, &mut total)?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            let mode = mode & 0o777;
            if mode != 0 {
                let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode));
            }
        }
    }
    let mut entries = Vec::new();
    walk(dest, dest, &mut entries, &mut 0)?;
    Ok(entries)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<StagedEntry>, count: &mut usize) -> Result<(), String> {
    for item in std::fs::read_dir(dir).map_err(|e| format!("Could not read {}: {e}", dir.display()))? {
        let item = item.map_err(|e| e.to_string())?;
        let path = item.path();
        let meta = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if meta.file_type().is_symlink() {
            return Err(format!("The archive contains a link ({}), which is not allowed.", path.display()));
        }
        if meta.is_dir() {
            walk(root, &path, out, count)?;
        } else if meta.is_file() {
            *count += 1;
            if *count > MAX_FILES {
                return Err("The archive contains too many files.".into());
            }
            let rel = path.strip_prefix(root).map_err(|_| "Archive path escaped its folder.".to_string())?;
            let rel = rel.to_string_lossy().replace('\\', "/");
            let rel = normalize_entry(&rel).ok_or_else(|| format!("Archive produced an unsafe path: {rel}"))?;
            out.push(StagedEntry { path: rel, size: meta.len() });
        } else {
            return Err("The archive contains a special file, which is not allowed.".into());
        }
    }
    Ok(())
}

fn run(cmd: &mut Command) -> (String, String, bool) {
    match cmd.output() {
        Ok(out) => (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.success(),
        ),
        Err(e) => (String::new(), e.to_string(), false),
    }
}

/// Copy a non-archive component into its own staging folder under its base name.
pub fn stage_single(file: &Path, dest: &Path, file_name: &str) -> Result<Vec<StagedEntry>, String> {
    let name = file_name
        .rsplit(['/', '\\'])
        .next()
        .and_then(normalize_entry)
        .ok_or_else(|| format!("Refused an unsafe file name: {file_name}"))?;
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let target = dest.join(&name);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::copy(file, &target).map_err(|e| format!("Could not place {name}: {e}"))?;
    Ok(vec![StagedEntry { path: name, size: std::fs::metadata(&target).map(|m| m.len()).unwrap_or(0) }])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;
    use zip::write::SimpleFileOptions;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "melty-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn zip_roundtrip_without_external_tools() {
        let dir = temp_dir("zip");
        let archive = dir.join("a.zip");
        {
            let file = File::create(&archive).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            zip.start_file("Mod Folder/hello.dll", options).unwrap();
            zip.write_all(b"hello zip").unwrap();
            zip.start_file("run.sh", options.unix_permissions(0o755)).unwrap();
            zip.write_all(b"#!/bin/sh\n").unwrap();
            zip.add_directory("empty", options).unwrap();
            zip.finish().unwrap();
        }
        let entries = list(&archive).unwrap();
        assert!(entries.iter().any(|e| e.path == "Mod Folder/hello.dll" && e.size == 9));
        assert!(entries.iter().any(|e| e.path == "run.sh" && e.size == 10));
        assert!(entries.iter().any(|e| e.path == "empty" && e.size == 0));

        let out = dir.join("out");
        let files = extract(&archive, &out).unwrap();
        assert_eq!(std::fs::read_to_string(out.join("Mod Folder/hello.dll")).unwrap(), "hello zip");
        assert!(files.iter().any(|e| e.path == "Mod Folder/hello.dll" && e.size == 9));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(out.join("run.sh")).unwrap().permissions().mode() & 0o777, 0o755);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn zip_traversal_is_rejected() {
        let dir = temp_dir("zip-evil");
        let archive = dir.join("evil.zip");
        {
            let file = File::create(&archive).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            zip.start_file("../escape.txt", SimpleFileOptions::default()).unwrap();
            zip.write_all(b"no").unwrap();
            zip.finish().unwrap();
        }
        assert!(list(&archive).is_err());
        let out = dir.join("out");
        assert!(extract(&archive, &out).is_err());
        assert!(!dir.join("escape.txt").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tar_lines_parse_paths_with_spaces() {
        let e = parse_tar_line("-rw-r--r--  0 mecke mecke 12 Oct 22 01:44 Mod Folder/a.dll").unwrap();
        assert_eq!(e.path, "Mod Folder/a.dll");
        assert_eq!(e.size, 12);
        assert!(parse_tar_line("lrwxrwxrwx 0 mecke mecke 0 Oct 22 01:44 link -> /etc/passwd").is_none());
    }

    #[test]
    fn archive_listing_rejects_traversal() {
        // Build a tiny tar with an absolute member using bsdtar, then verify that listing refuses it.
        let dir = std::env::temp_dir().join(format!("melty-tar-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("x");
        std::fs::write(&file, b"x").unwrap();
        let archive = dir.join("a.tar");
        let ok = Command::new("bsdtar").args(["-cf"]).arg(&archive).arg("-C").arg(&dir).arg("x").status().map(|s| s.success()).unwrap_or(false);
        if ok {
            let list = list(&archive).unwrap();
            assert!(list.iter().any(|e| e.path == "x"));
        }
    }
}
