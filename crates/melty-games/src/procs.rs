//! Prozessliste aus /proc, inklusive Windows-Prozessen unter Wine/Proton.
//! Ersetzt `processes`/`commands` des windows-inspector. Die Felder heißen wie die PowerShell-Felder des Originals
//! (`Name`, `ProcessId`, …), damit der übrige Code sie unverändert weiterverwenden kann.

use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub struct Proc {
    pub name: String,
    pub process_id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executable_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_line: Option<String>,
}

fn is_drive_path(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
}

/// Erster `…\datei.exe`-Pfad einer Windows-Kommandozeile (mit oder ohne Anführungszeichen).
pub fn first_windows_exe(cmdline: &str) -> Option<String> {
    let s = cmdline.trim_start().trim_start_matches('"');
    if !is_drive_path(s) {
        return None;
    }
    let lower = s.to_ascii_lowercase();
    let end = lower.find(".exe")? + 4;
    Some(s[..end].to_string())
}

/// `Z:\home\u\…\x.exe` → `/home/u/…/x.exe`. Andere Laufwerke liegen im Prefix und sind ohne ihn nicht abbildbar.
pub fn unix_path_of_windows(path: &str) -> Option<PathBuf> {
    let b = path.as_bytes();
    if is_drive_path(path) && b[0].eq_ignore_ascii_case(&b'z') {
        return Some(PathBuf::from(path[2..].replace('\\', "/")));
    }
    None
}

fn base_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

pub fn scan(proc_root: &Path) -> Vec<Proc> {
    let Ok(entries) = fs::read_dir(proc_root) else { return Vec::new() };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
        let dir = entry.path();
        let raw = match fs::read(dir.join("cmdline")) {
            Ok(r) if !r.is_empty() => r,
            _ => continue, // Kernel-Threads, Zombies, bereits beendet
        };
        let args: Vec<String> = raw
            .split(|&b| b == 0)
            .filter(|a| !a.is_empty())
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect();
        let command_line = args.join(" ");
        let comm = fs::read_to_string(dir.join("comm")).map(|s| s.trim().to_string()).unwrap_or_default();
        let exe = fs::read_link(dir.join("exe")).ok().map(|p| p.to_string_lossy().into_owned());
        // Wine/Proton: der echte Name steht in der Windows-Kommandozeile (comm ist auf 15 Zeichen gekürzt)
        let windows_exe = first_windows_exe(&command_line);
        let name = match (&windows_exe, comm.is_empty()) {
            (Some(w), _) => base_name(w).to_string(),
            (None, false) => comm,
            (None, true) => exe.as_deref().map(|e| base_name(e).to_string()).unwrap_or_default(),
        };
        if name.is_empty() {
            continue;
        }
        out.push(Proc { name, process_id: pid, executable_path: windows_exe.or(exe), command_line: Some(command_line) });
    }
    out.sort_by_key(|p| p.process_id);
    out
}

/// Antwort der Inspector-Methode `commands`: `[{ProcessId, CommandLine|null}]`, höchstens 200 eindeutige PIDs (wie im Original).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub struct CommandEntry {
    pub process_id: u32,
    pub command_line: Option<String>,
}

pub fn commands(proc_root: &Path, pids: &[u32]) -> Vec<CommandEntry> {
    let mut wanted: Vec<u32> = pids.iter().copied().filter(|p| *p > 0).collect();
    wanted.sort_unstable();
    wanted.dedup();
    wanted.truncate(200);
    wanted
        .into_iter()
        .filter_map(|pid| {
            let raw = fs::read(proc_root.join(pid.to_string()).join("cmdline")).ok()?; // Prozess weg → kein Eintrag
            let args: Vec<String> = raw.split(|&b| b == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
            Some(CommandEntry { process_id: pid, command_line: (!args.is_empty()).then(|| args.join(" ")) })
        })
        .collect()
}

/// Windows-Pfad in einem Wine-/Proton-Prefix auflösen. `Z:` ist das Host-Dateisystem,
/// `C:` liegt unter `<prefix>/drive_c`, andere Laufwerke über `<prefix>/dosdevices/<buchstabe>:`.
pub fn unix_path_in_prefix(path: &str, prefix: &Path) -> Option<PathBuf> {
    if let Some(host) = unix_path_of_windows(path) {
        return Some(host);
    }
    if !is_drive_path(path) {
        return None;
    }
    let letter = path.as_bytes()[0].to_ascii_lowercase() as char;
    let rest = path[2..].trim_start_matches(['\\', '/']).replace('\\', "/");
    if letter == 'c' {
        return Some(prefix.join("drive_c").join(rest));
    }
    let link = prefix.join("dosdevices").join(format!("{letter}:"));
    let base = fs::read_link(&link).ok().map(|target| prefix.join(target)).unwrap_or_else(|| prefix.join("drive_c"));
    Some(base.join(rest))
}

impl Proc {
    /// Pfad der Programmdatei auf dem Host, soweit ableitbar.
    pub fn host_path(&self) -> Option<PathBuf> {
        let p = self.executable_path.as_deref()?;
        unix_path_of_windows(p).or_else(|| p.starts_with('/').then(|| PathBuf::from(p)))
    }

    /// Wie `host_path`, aber Windows-Pfade werden gegen einen Wine-Prefix aufgelöst.
    pub fn host_path_with_prefix(&self, prefix: &Path) -> Option<PathBuf> {
        let p = self.executable_path.as_deref()?;
        unix_path_in_prefix(p, prefix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::steam::testutil::*;

    fn fake_proc(root: &Path, pid: u32, comm: &str, args: &[&str]) {
        let d = root.join(pid.to_string());
        write(&d.join("comm"), &format!("{comm}\n"));
        let mut raw = Vec::new();
        for a in args {
            raw.extend_from_slice(a.as_bytes());
            raw.push(0);
        }
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("cmdline"), raw).unwrap();
    }

    #[test]
    fn windows_exe_parsing() {
        assert_eq!(first_windows_exe("\"Z:\\home\\u\\Game\\x.exe\" -a b").as_deref(), Some("Z:\\home\\u\\Game\\x.exe"));
        assert_eq!(first_windows_exe("C:\\windows\\system32\\Services.EXE").as_deref(), Some("C:\\windows\\system32\\Services.EXE"));
        assert_eq!(first_windows_exe("/usr/bin/java -jar x.jar"), None);
        assert_eq!(unix_path_of_windows("Z:\\home\\u\\a b\\x.exe"), Some(PathBuf::from("/home/u/a b/x.exe")));
        assert_eq!(unix_path_of_windows("C:\\windows\\x.exe"), None);
    }

    #[test]
    fn prefix_paths_are_resolved_for_wine_games() {
        let prefix = Path::new("/home/u/Games/elden");
        assert_eq!(
            unix_path_in_prefix("C:\\Games\\Elden\\eldenring.exe", prefix),
            Some(PathBuf::from("/home/u/Games/elden/drive_c/Games/Elden/eldenring.exe"))
        );
        assert_eq!(
            unix_path_in_prefix("Z:\\home\\u\\Game\\x.exe", prefix),
            Some(PathBuf::from("/home/u/Game/x.exe"))
        );
        assert_eq!(unix_path_in_prefix("/usr/bin/game", prefix), None);
    }

    #[test]
    fn commands_matches_inspector_shape() {
        let t = Tmp::new("cmds");
        fake_proc(t.path(), 5, "java", &["/usr/bin/java", "-jar", "mc.jar"]);
        let out = commands(t.path(), &[5, 5, 0, 99]);
        assert_eq!(out, vec![CommandEntry { process_id: 5, command_line: Some("/usr/bin/java -jar mc.jar".into()) }]);
        let json = serde_json::to_value(&out[0]).unwrap();
        assert!(json.get("ProcessId").is_some() && json.get("CommandLine").is_some());
    }

    #[test]
    fn scans_native_wine_and_skips_kernel_threads() {
        let t = Tmp::new("proc");
        fake_proc(t.path(), 100, "fish", &["/usr/bin/fish"]);
        fake_proc(t.path(), 200, "GrandTheftAuto", &["Z:\\home\\u\\.local\\share\\Steam\\steamapps\\common\\Grand Theft Auto V\\GTA5.exe"]);
        fake_proc(t.path(), 300, "kworker", &[]); // leere cmdline
        std::fs::create_dir_all(t.path().join("self")).unwrap(); // nicht numerisch
        let procs = scan(t.path());
        assert_eq!(procs.len(), 2);
        assert_eq!(procs[0].name, "fish");
        assert_eq!(procs[1].name, "GTA5.exe", "Name kommt aus der Windows-Kommandozeile, nicht aus dem gekürzten comm");
        assert_eq!(
            procs[1].host_path().unwrap(),
            PathBuf::from("/home/u/.local/share/Steam/steamapps/common/Grand Theft Auto V/GTA5.exe")
        );
        let json = serde_json::to_value(&procs[1]).unwrap();
        assert!(json.get("Name").is_some() && json.get("ProcessId").is_some());
    }
}
