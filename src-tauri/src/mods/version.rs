//! Liest die Windows-Dateiversion aus einer PE-Datei und prüft die Versionsbereiche aus Rezepten.
//!
//! Rezepte können für ein Spiel z. B. `>=2.7.0.0 <=2.7.1.1` verlangen. Unter Linux lässt sich die
//! Version ohne Windows-API aus den Versionsressourcen (`VS_FIXEDFILEINFO`) der `.exe` lesen. So kann
//! der Client vor dem Start erkennen, dass eine Spielaktualisierung die Bridge eines Mashups
//! unbrauchbar gemacht hat, statt beide Spiele ohne Verbindung zu starten.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Eine vierteilige Windows-Dateiversion, z. B. `2.7.2.0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct GameVersion(pub [u16; 4]);

impl std::fmt::Display for GameVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}.{}", self.0[0], self.0[1], self.0[2], self.0[3])
    }
}

/// Parst `2.7.2.0`, `2.7` oder `v2.7.2.0`. Fehlende Teile werden mit 0 aufgefüllt.
pub fn parse_version(text: &str) -> Option<GameVersion> {
    let text = text.trim().trim_start_matches(|c| c == 'v' || c == 'V');
    let text = text.split(|c| c == '-' || c == '+').next().unwrap_or(text);
    if text.is_empty() {
        return None;
    }
    let mut parts = [0u16; 4];
    let mut count = 0;
    for part in text.split('.') {
        if count == parts.len() || part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        parts[count] = part.parse().ok()?;
        count += 1;
    }
    (count > 0).then_some(GameVersion(parts))
}

/// Formatiert den häufigen Bereich `>=A <=B` für eine Fehlermeldung als `A bis B`.
pub fn requirement_label(requirement: &str) -> String {
    let words: Vec<&str> = requirement.split_whitespace().collect();
    if let [start, end] = words.as_slice() {
        if let (Some(start), Some(end)) = (start.strip_prefix(">="), end.strip_prefix("<=")) {
            return format!("{start} bis {end}");
        }
    }
    requirement.trim().to_string()
}

/// Prüft eine Rezeptangabe wie `>=2.7.0.0 <=2.7.1.1`, `2.7.1.1` oder `2.7.0.0 - 2.7.1.1`.
///
/// `None` bedeutet: Die Schreibweise wird nicht verstanden. Der Aufrufer sollte dann lieber starten
/// als ein Mashup wegen einer unbekannten, aber vielleicht gültigen Angabe zu blockieren.
pub fn matches_requirement(requirement: &str, version: GameVersion) -> Option<bool> {
    let requirement = requirement.trim();
    if requirement.is_empty() {
        return Some(true);
    }
    let mut unknown = false;
    for alternative in requirement.split("||") {
        match matches_all(alternative.trim(), version) {
            Some(true) => return Some(true),
            Some(false) => {}
            None => unknown = true,
        }
    }
    if unknown {
        None
    } else {
        Some(false)
    }
}

fn matches_all(text: &str, version: GameVersion) -> Option<bool> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if let Some((start, end)) = text.split_once(" - ") {
        let start = parse_version(start)?;
        let end = parse_version(end)?;
        return Some(version >= start && version <= end);
    }

    let mut any = false;
    let mut words = text
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|word| !word.is_empty());
    while let Some(word) = words.next() {
        any = true;
        let (operator, raw) = if let Some(rest) = word.strip_prefix(">=") {
            (">=", rest)
        } else if let Some(rest) = word.strip_prefix("<=") {
            ("<=", rest)
        } else if let Some(rest) = word.strip_prefix("==") {
            ("==", rest)
        } else if let Some(rest) = word.strip_prefix('>') {
            (">", rest)
        } else if let Some(rest) = word.strip_prefix('<') {
            ("<", rest)
        } else if let Some(rest) = word.strip_prefix('=') {
            ("=", rest)
        } else {
            ("=", word)
        };
        // Erlaubt auch `>= 2.7.0.0` mit Leerzeichen zwischen Operator und Version.
        let raw = if raw.is_empty() { words.next()? } else { raw };
        let wanted = parse_version(raw)?;
        let matches = match operator {
            ">=" => version >= wanted,
            "<=" => version <= wanted,
            ">" => version > wanted,
            "<" => version < wanted,
            "=" | "==" => version == wanted,
            _ => return None,
        };
        if !matches {
            return Some(false);
        }
    }
    any.then_some(true)
}

/// Liest `VS_FIXEDFILEINFO` aus der `.rsrc`-Sektion. Gibt `None` zurück, wenn die Datei keine
/// PE-Datei ist, keine Versionsressource hat oder die Ressource zu groß/ungültig ist.
pub fn read_file_version(path: &Path) -> Option<GameVersion> {
    const MAX_RESOURCE_BYTES: u64 = 64 * 1024 * 1024;
    const SIGNATURE: [u8; 4] = [0xbd, 0x04, 0xef, 0xfe]; // VS_FIXEDFILEINFO, little endian

    let mut file = File::open(path).ok()?;
    let mut dos = [0u8; 0x40];
    file.read_exact(&mut dos).ok()?;
    if &dos[0..2] != b"MZ" {
        return None;
    }
    let pe_offset = u32::from_le_bytes(dos[0x3c..0x40].try_into().ok()?) as u64;
    file.seek(SeekFrom::Start(pe_offset)).ok()?;
    let mut pe = [0u8; 24];
    file.read_exact(&mut pe).ok()?;
    if &pe[0..4] != b"PE\0\0" {
        return None;
    }
    let sections = u16::from_le_bytes(pe[6..8].try_into().ok()?) as usize;
    let optional_size = u16::from_le_bytes(pe[20..22].try_into().ok()?) as usize;
    if sections == 0 || sections > 96 || optional_size < 2 {
        return None;
    }

    let optional_offset = pe_offset + 24;
    file.seek(SeekFrom::Start(optional_offset)).ok()?;
    let mut optional = vec![0u8; optional_size];
    file.read_exact(&mut optional).ok()?;
    let magic = u16::from_le_bytes(optional[0..2].try_into().ok()?);
    let data_dir_offset = match magic {
        0x10b => 96,  // PE32
        0x20b => 112, // PE32+
        _ => return None,
    };
    if optional.len() < data_dir_offset + 24 {
        return None;
    }

    // Der dritte Eintrag der DataDirectory-Tabelle ist die Ressourcen-Sektion.
    let resource_entry = data_dir_offset + 2 * 8;
    let resource_rva = u32::from_le_bytes(optional[resource_entry..resource_entry + 4].try_into().ok()?);
    let resource_size = u32::from_le_bytes(optional[resource_entry + 4..resource_entry + 8].try_into().ok()?);
    if resource_rva == 0 || resource_size == 0 {
        return None;
    }

    let section_offset = optional_offset + optional_size as u64;
    file.seek(SeekFrom::Start(section_offset)).ok()?;
    let mut table = vec![0u8; sections * 40];
    file.read_exact(&mut table).ok()?;
    let mut resource_raw = None;
    for index in 0..sections {
        let offset = index * 40;
        let name = &table[offset..offset + 8];
        let virtual_size = u32::from_le_bytes(table[offset + 8..offset + 12].try_into().ok()?);
        let virtual_address = u32::from_le_bytes(table[offset + 12..offset + 16].try_into().ok()?);
        let raw_size = u32::from_le_bytes(table[offset + 16..offset + 20].try_into().ok()?);
        let raw_offset = u32::from_le_bytes(table[offset + 20..offset + 24].try_into().ok()?);
        let span = virtual_size.max(raw_size);
        let contains_resource = resource_rva >= virtual_address
            && resource_rva < virtual_address.saturating_add(span);
        if name.starts_with(b".rsrc") || contains_resource {
            if raw_size > 0 && raw_offset > 0 {
                resource_raw = Some((raw_offset as u64, raw_size as u64));
            }
            break;
        }
    }

    let (raw_offset, raw_size) = resource_raw?;
    if raw_size > MAX_RESOURCE_BYTES {
        return None;
    }
    file.seek(SeekFrom::Start(raw_offset)).ok()?;
    let mut resource = vec![0u8; raw_size as usize];
    file.read_exact(&mut resource).ok()?;

    for index in 0..resource.len().saturating_sub(16) {
        if !resource[index..].starts_with(&SIGNATURE) {
            continue;
        }
        let structure_version = u32::from_le_bytes(resource[index + 4..index + 8].try_into().ok()?);
        if structure_version != 0x0001_0000 {
            continue;
        }
        let file_version_ms = u32::from_le_bytes(resource[index + 8..index + 12].try_into().ok()?);
        let file_version_ls = u32::from_le_bytes(resource[index + 12..index + 16].try_into().ok()?);
        return Some(GameVersion([
            (file_version_ms >> 16) as u16,
            (file_version_ms & 0xffff) as u16,
            (file_version_ls >> 16) as u16,
            (file_version_ls & 0xffff) as u16,
        ]));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(value: &str) -> GameVersion {
        parse_version(value).unwrap()
    }

    #[test]
    fn parses_four_part_versions_and_short_forms() {
        assert_eq!(version("2.7.2.0"), GameVersion([2, 7, 2, 0]));
        assert_eq!(version("v2.7"), GameVersion([2, 7, 0, 0]));
        assert_eq!(version("2.7.1.1-beta"), GameVersion([2, 7, 1, 1]));
        assert!(parse_version("").is_none());
        assert!(parse_version("2.7.1.1.1").is_none());
        assert!(parse_version("2.x.1.1").is_none());
    }

    #[test]
    fn checks_recipe_version_ranges() {
        let requirement = ">=2.7.0.0 <=2.7.1.1";
        assert_eq!(matches_requirement(requirement, version("2.7.0.0")), Some(true));
        assert_eq!(matches_requirement(requirement, version("2.7.1.1")), Some(true));
        assert_eq!(matches_requirement(requirement, version("2.7.2.0")), Some(false));
        assert_eq!(matches_requirement(requirement, version("2.6.9.9")), Some(false));
        assert_eq!(matches_requirement("2.7.1.1", version("2.7.1.1")), Some(true));
        assert_eq!(matches_requirement("2.7.1.1", version("2.7.1.2")), Some(false));
        assert_eq!(matches_requirement("2.7.0.0 - 2.7.1.1", version("2.7.1.0")), Some(true));
        assert_eq!(matches_requirement("^2.7.0", version("2.7.1.0")), None);
        assert_eq!(requirement_label(">=2.7.0.0 <=2.7.1.1"), "2.7.0.0 bis 2.7.1.1");
        assert_eq!(requirement_label("2.7.1.1"), "2.7.1.1");
    }

    fn synthetic_pe(version: GameVersion) -> Vec<u8> {
        let mut file = vec![0u8; 0x400];
        file[0..2].copy_from_slice(b"MZ");
        file[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        let pe = 0x40usize;
        file[pe..pe + 4].copy_from_slice(b"PE\0\0");
        file[pe + 6..pe + 8].copy_from_slice(&1u16.to_le_bytes());
        file[pe + 20..pe + 22].copy_from_slice(&0xf0u16.to_le_bytes());
        let optional = pe + 24;
        file[optional..optional + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        let resource = optional + 112 + 16;
        file[resource..resource + 4].copy_from_slice(&0x1000u32.to_le_bytes());
        file[resource + 4..resource + 8].copy_from_slice(&0x200u32.to_le_bytes());
        let section = optional + 0xf0;
        file[section..section + 5].copy_from_slice(b".rsrc");
        file[section + 8..section + 12].copy_from_slice(&0x200u32.to_le_bytes());
        file[section + 12..section + 16].copy_from_slice(&0x1000u32.to_le_bytes());
        file[section + 16..section + 20].copy_from_slice(&0x200u32.to_le_bytes());
        file[section + 20..section + 24].copy_from_slice(&0x200u32.to_le_bytes());
        let raw = 0x200usize;
        file[raw..raw + 4].copy_from_slice(&[0xbd, 0x04, 0xef, 0xfe]);
        file[raw + 4..raw + 8].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        let ms = ((version.0[0] as u32) << 16) | version.0[1] as u32;
        let ls = ((version.0[2] as u32) << 16) | version.0[3] as u32;
        file[raw + 8..raw + 12].copy_from_slice(&ms.to_le_bytes());
        file[raw + 12..raw + 16].copy_from_slice(&ls.to_le_bytes());
        file
    }

    #[test]
    fn reads_the_version_from_pe_resources() {
        let dir = std::env::temp_dir().join(format!(
            "melty-pe-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("game.exe");
        std::fs::write(&path, synthetic_pe(GameVersion([2, 7, 2, 0]))).unwrap();
        assert_eq!(read_file_version(&path), Some(GameVersion([2, 7, 2, 0])));
        let _ = std::fs::remove_dir_all(dir);
    }
}
