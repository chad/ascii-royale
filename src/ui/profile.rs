//! Persistent player profile (display name + skin color), saved next to the
//! keybindings at ~/.config/ascii-royale/profile.conf. Used by the local
//! binary; web players carry their profile in the browser instead (the VM's
//! home dir is shared across web sessions, so it can't persist per-player).

use std::path::PathBuf;

use crate::net::protocol::parse_hex_color;

#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    pub color: u32,
}

impl Default for Profile {
    fn default() -> Self {
        let name = std::env::var("USER").unwrap_or_else(|_| "player".into());
        Profile { name: sanitize_name(&name), color: 0xff_d7_5f }
    }
}

/// Keep names to the same charset the server enforces, so what you set is
/// what you get.
pub fn sanitize_name(raw: &str) -> String {
    let cleaned: String =
        raw.chars().filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_').take(12).collect();
    if cleaned.is_empty() {
        "anon".to_string()
    } else {
        cleaned
    }
}

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("ascii-royale").join("profile.conf"))
}

impl Profile {
    pub fn load() -> Self {
        let mut p = Profile::default();
        let Some(path) = path() else { return p };
        let Ok(text) = std::fs::read_to_string(path) else { return p };
        for line in text.lines() {
            let line = line.trim();
            let Some((k, v)) = line.split_once('=') else { continue };
            match k.trim() {
                "name" => p.name = sanitize_name(v.trim()),
                "color" => {
                    if let Some(c) = parse_hex_color(v.trim()) {
                        p.color = c;
                    }
                }
                _ => {}
            }
        }
        p
    }

    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = path() else { return Ok(()) };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, format!("name = {}\ncolor = {:06x}\n", self.name, self.color & 0xffffff))
    }

    pub fn hex(&self) -> String {
        format!("{:06x}", self.color & 0xffffff)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_through_disk() {
        let dir = std::env::temp_dir().join("ascii-royale-profile-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        let p = Profile { name: "chad".into(), color: 0xff8800 };
        p.save().unwrap();
        let back = Profile::load();
        assert_eq!(back.name, "chad");
        assert_eq!(back.color, 0xff8800);
        let _ = std::fs::remove_dir_all(&dir);
        std::env::remove_var("XDG_CONFIG_HOME");
    }

    #[test]
    fn name_is_sanitized_and_capped() {
        let n = sanitize_name("a name with spaces!!");
        assert!(n.len() <= 12 && n.chars().all(|c| c.is_alphanumeric()));
        assert_eq!(sanitize_name("   "), "anon");
    }
}
