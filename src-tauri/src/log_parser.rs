use regex::Regex;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct ParsedReward {
    pub item_name: String,
    pub quantity: i64,
    pub raw_line: String,
}

pub struct LogParser {
    patterns: Vec<Regex>,
}

impl LogParser {
    pub fn new() -> Self {
        let pattern_strings = vec![
            // "received ItemName x2"
            r"(?i)\breceived\s+([A-Za-z][A-Za-z0-9\s'\-]+?)\s+[xX]\s*(\d+)",
            // "reward: ItemName x2"
            r"(?i)\brewards?\s*[:\s]+([A-Za-z][A-Za-z0-9\s'\-]+?)\s+[xX]\s*(\d+)",
            // "Adding item: /path/ItemName x1"
            r"(?i)adding item.*?/([A-Za-z][A-Za-z0-9\s'\-]+?)\s+[xX]\s*(\d+)",
            // "ItemName x2" after mission/fissure keyword
            r"(?i)(?:mission|fissure|syndicate|foundry)[^\n]*?([A-Za-z][A-Za-z0-9\s'\-]{3,40}?)\s+[xX]\s*(\d+)",
            // "You received: ItemName x1"
            r"(?i)you received[:\s]+([A-Za-z][A-Za-z0-9\s'\-]+?)\s+[xX]\s*(\d+)",
        ];

        let patterns = pattern_strings
            .iter()
            .filter_map(|p| Regex::new(p).ok())
            .collect();

        Self { patterns }
    }

    pub fn parse_line(&self, line: &str) -> Option<ParsedReward> {
        for pattern in &self.patterns {
            if let Some(caps) = pattern.captures(line) {
                if let (Some(name), Some(qty_str)) = (caps.get(1), caps.get(2)) {
                    let item_name = name.as_str().trim().to_string();
                    let quantity: i64 = qty_str.as_str().parse().unwrap_or(1);

                    if item_name.len() < 3 || item_name.len() > 80 {
                        continue;
                    }

                    return Some(ParsedReward {
                        item_name,
                        quantity,
                        raw_line: line.to_string(),
                    });
                }
            }
        }
        None
    }

    pub fn parse_file_from_offset(&self, path: &Path, offset: u64) -> (Vec<ParsedReward>, u64) {
        let file = match File::open(path) {
            Ok(f) => f,
            Err(_) => return (vec![], offset),
        };

        let file_size = file.metadata().map(|m| m.len()).unwrap_or(0);

        // File was rotated (Warframe restarted) — start from beginning
        let actual_offset = if offset > file_size { 0 } else { offset };

        let mut reader = BufReader::new(file);
        if reader.seek(SeekFrom::Start(actual_offset)).is_err() {
            return (vec![], actual_offset);
        }

        let mut rewards = vec![];
        let mut new_offset = actual_offset;

        for line in reader.lines() {
            match line {
                Ok(l) => {
                    new_offset += l.len() as u64 + 1;
                    if let Some(reward) = self.parse_line(&l) {
                        rewards.push(reward);
                    }
                }
                Err(_) => break,
            }
        }

        (rewards, new_offset)
    }
}

const APPDATA_TAIL: &str = "compatdata/230410/pfx/drive_c/users/steamuser/AppData/Local/Warframe/EE.log";

/// Build a Proton prefix EE.log path using a specific Windows username.
fn proton_ee_log_path(steamapps: &std::path::Path, win_user: &str) -> std::path::PathBuf {
    steamapps.join("compatdata/230410/pfx/drive_c/users")
        .join(win_user)
        .join("AppData/Local/Warframe/EE.log")
}

/// Extract the second quoted string from a VDF line (e.g. the path value).
fn extract_vdf_quoted_value(line: &str) -> Option<String> {
    let mut in_quote = false;
    let mut values = Vec::new();
    let mut current = String::new();
    for c in line.chars() {
        match c {
            '"' => {
                if in_quote {
                    if !current.is_empty() {
                        values.push(current.clone());
                        current.clear();
                    }
                    in_quote = false;
                } else {
                    in_quote = true;
                }
            }
            _ if in_quote => current.push(c),
            _ => {}
        }
    }
    // First quoted string is the key, second is the value
    values.get(1).cloned()
}

/// Append a line to the main debug log in /tmp (useful when stdout is swallowed).
pub fn debug_log(msg: &str) {
    let path = std::env::temp_dir().join("frameforge_main_debug.log");
    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    let _ = std::fs::OpenOptions::new()
        .create(true).append(true).open(&path)
        .and_then(|mut f| writeln!(f, "[{}] {}", ts, msg));
}

fn add_steam_candidates(candidates: &mut Vec<std::path::PathBuf>, steamapps: &std::path::Path, local_user: &str) {
    candidates.push(steamapps.join(APPDATA_TAIL));
    if !local_user.is_empty() && local_user != "steamuser" {
        candidates.push(proton_ee_log_path(steamapps, local_user));
    }
}

/// Returns every known EE.log candidate path on this platform.
/// Only checks direct paths — no recursive filesystem scanning.
pub fn get_ee_log_candidates() -> Vec<std::path::PathBuf> {
    #[cfg(target_os = "windows")]
    {
        if let Some(d) = dirs::data_local_dir() {
            return vec![d.join("Warframe").join("EE.log")];
        }
        return vec![];
    }

    #[cfg(not(target_os = "windows"))]
    {
        let mut candidates: Vec<std::path::PathBuf> = Vec::new();
        let local_user = std::env::var("USER")
            .or_else(|_| std::env::var("LOGNAME"))
            .unwrap_or_default();

        // Local Steam library
        if let Some(home) = dirs::home_dir() {
            let steamapps = home.join(".local/share/Steam/steamapps");
            add_steam_candidates(&mut candidates, &steamapps, &local_user);

            // Alternative Steam installation path
            let alt = home.join(".steam/steam/steamapps");
            add_steam_candidates(&mut candidates, &alt, &local_user);
        }

        // Parse libraryfolders.vdf for additional Steam libraries (fast, single file read)
        if let Some(home) = dirs::home_dir() {
            let vdf_path = home.join(".local/share/Steam/steamapps/libraryfolders.vdf");
            if let Ok(content) = std::fs::read_to_string(&vdf_path) {
                for line in content.lines() {
                    let trimmed = line.trim();
                    if trimmed.contains("\"path\"") || trimmed.contains("'path'") {
                        if let Some(path_str) = extract_vdf_quoted_value(trimmed) {
                            let lib_path = std::path::PathBuf::from(path_str);
                            let steamapps = lib_path.join("steamapps");
                            add_steam_candidates(&mut candidates, &steamapps, &local_user);
                        }
                    }
                }
            }
        }

        // Flatpak Steam
        if let Some(home) = dirs::home_dir() {
            let steamapps = home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam/steamapps");
            add_steam_candidates(&mut candidates, &steamapps, &local_user);
        }

        // Generic Wine fallback
        if let Some(home) = dirs::home_dir() {
            let wine = home.join(".wine/drive_c/users")
                .join(&local_user)
                .join("AppData/Local/Warframe/EE.log");
            candidates.push(wine);
        }

        candidates
    }
}

pub fn get_default_log_path() -> Option<String> {
    let candidates = get_ee_log_candidates();
    debug_log(&format!("EE.log candidates ({} paths):", candidates.len()));
    for p in &candidates {
        debug_log(&format!("  candidate: {}  exists={}", p.display(), p.exists()));
    }
    for path in candidates {
        if path.exists() {
            let s = path.to_string_lossy().to_string();
            debug_log(&format!("EE.log selected: {}", s));
            return Some(s);
        }
    }
    debug_log("EE.log: no existing candidate found");
    None
}
