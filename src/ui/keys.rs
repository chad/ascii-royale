//! Rebindable key configuration, persisted as a tiny human-editable file
//! at ~/.config/ascii-royale/keys.conf (one `action = key key...` per line).
//!
//! Arrows (movement), Enter (start), q/Esc (quit) and `k` (this screen)
//! are hardwired so a wild config can never lock you out.

use std::path::PathBuf;

use ratatui::crossterm::event::KeyCode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Up,
    Down,
    Left,
    Right,
    Fire,
    Pickup,
    Heal,
    Mute,
}

impl Action {
    pub const ALL: [Action; 8] = [
        Action::Up,
        Action::Down,
        Action::Left,
        Action::Right,
        Action::Fire,
        Action::Pickup,
        Action::Heal,
        Action::Mute,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Action::Up => "move up",
            Action::Down => "move down",
            Action::Left => "move left",
            Action::Right => "move right",
            Action::Fire => "fire",
            Action::Pickup => "pick up",
            Action::Heal => "heal",
            Action::Mute => "mute",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Action::Up => "up",
            Action::Down => "down",
            Action::Left => "left",
            Action::Right => "right",
            Action::Fire => "fire",
            Action::Pickup => "pickup",
            Action::Heal => "heal",
            Action::Mute => "mute",
        }
    }

    fn from_id(s: &str) -> Option<Action> {
        Action::ALL.iter().copied().find(|a| a.id() == s)
    }
}

/// Keys that can never be rebound (they have fixed meanings everywhere).
pub const RESERVED: &[&str] = &["q", "k"];

#[derive(Debug, Clone)]
pub struct Keybinds {
    /// Key names per action, indexed in Action::ALL order.
    keys: [Vec<String>; 8],
}

impl Default for Keybinds {
    fn default() -> Self {
        Keybinds {
            keys: [
                vec!["w".into()],
                vec!["s".into()],
                vec!["a".into()],
                vec!["d".into()],
                vec!["f".into(), "space".into()],
                vec!["e".into(), "g".into()],
                vec!["h".into(), "m".into()],
                vec!["M".into()],
            ],
        }
    }
}

/// Single canonical text name for a bindable key, if it is bindable.
pub fn key_name(code: KeyCode) -> Option<String> {
    match code {
        KeyCode::Char(' ') => Some("space".into()),
        KeyCode::Char(c) if !c.is_control() => Some(c.to_string()),
        KeyCode::Tab => Some("tab".into()),
        _ => None,
    }
}

fn idx(action: Action) -> usize {
    Action::ALL.iter().position(|a| *a == action).expect("action in ALL")
}

impl Keybinds {
    pub fn config_path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("ascii-royale").join("keys.conf"))
    }

    /// Load from disk, falling back to defaults for anything missing/broken.
    pub fn load() -> Self {
        let mut binds = Keybinds::default();
        let Some(path) = Self::config_path() else { return binds };
        let Ok(text) = std::fs::read_to_string(path) else { return binds };
        binds.apply_config(&text);
        binds
    }

    fn apply_config(&mut self, text: &str) {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((action, keys)) = line.split_once('=') else { continue };
            let Some(action) = Action::from_id(action.trim()) else { continue };
            let keys: Vec<String> = keys
                .split_whitespace()
                .map(str::to_string)
                .filter(|k| !RESERVED.contains(&k.as_str()))
                .collect();
            self.keys[idx(action)] = keys;
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = Self::config_path() else { return Ok(()) };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut out = String::from("# ascii-royale key bindings (edit freely)\n");
        for action in Action::ALL {
            out.push_str(&format!("{} = {}\n", action.id(), self.keys[idx(action)].join(" ")));
        }
        std::fs::write(path, out)
    }

    pub fn keys_label(&self, action: Action) -> String {
        let keys = &self.keys[idx(action)];
        if keys.is_empty() {
            "(unbound)".to_string()
        } else {
            keys.join(" / ")
        }
    }

    /// What does this key do? Reserved/special keys are handled by callers.
    pub fn action_for(&self, code: KeyCode) -> Option<Action> {
        // Arrows are permanent movement fallbacks.
        match code {
            KeyCode::Up => return Some(Action::Up),
            KeyCode::Down => return Some(Action::Down),
            KeyCode::Left => return Some(Action::Left),
            KeyCode::Right => return Some(Action::Right),
            _ => {}
        }
        let name = key_name(code)?;
        Action::ALL
            .iter()
            .copied()
            .find(|a| self.keys[idx(*a)].contains(&name))
    }

    /// Bind a key to an action: the key is stolen from any other action,
    /// and becomes the action's only binding. Returns false for keys that
    /// can't be bound (reserved or non-character).
    pub fn bind(&mut self, action: Action, code: KeyCode) -> bool {
        let Some(name) = key_name(code) else { return false };
        if RESERVED.contains(&name.as_str()) {
            return false;
        }
        for keys in &mut self.keys {
            keys.retain(|k| *k != name);
        }
        self.keys[idx(action)] = vec![name];
        true
    }

    pub fn reset(&mut self) {
        *self = Keybinds::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_map_wasd_and_alternates() {
        let b = Keybinds::default();
        assert_eq!(b.action_for(KeyCode::Char('w')), Some(Action::Up));
        assert_eq!(b.action_for(KeyCode::Char(' ')), Some(Action::Fire));
        assert_eq!(b.action_for(KeyCode::Char('g')), Some(Action::Pickup));
        assert_eq!(b.action_for(KeyCode::Up), Some(Action::Up));
        assert_eq!(b.action_for(KeyCode::Char('z')), None);
    }

    #[test]
    fn rebinding_steals_conflicting_keys() {
        // The user's actual request: sdfc movement.
        let mut b = Keybinds::default();
        assert!(b.bind(Action::Up, KeyCode::Char('e')));
        assert!(b.bind(Action::Left, KeyCode::Char('s')));
        assert!(b.bind(Action::Down, KeyCode::Char('d')));
        assert!(b.bind(Action::Right, KeyCode::Char('f')));
        assert_eq!(b.action_for(KeyCode::Char('s')), Some(Action::Left));
        assert_eq!(b.action_for(KeyCode::Char('d')), Some(Action::Down));
        assert_eq!(b.action_for(KeyCode::Char('f')), Some(Action::Right));
        // 'f' no longer fires; space (untouched alternate) still does.
        assert_eq!(b.action_for(KeyCode::Char(' ')), Some(Action::Fire));
        // 'w' lost its meaning, arrows still work.
        assert_eq!(b.action_for(KeyCode::Char('w')), None);
        assert_eq!(b.action_for(KeyCode::Up), Some(Action::Up));
    }

    #[test]
    fn reserved_keys_refuse_to_bind() {
        let mut b = Keybinds::default();
        assert!(!b.bind(Action::Fire, KeyCode::Char('q')));
        assert!(!b.bind(Action::Fire, KeyCode::Char('k')));
        assert_eq!(b.action_for(KeyCode::Char('f')), Some(Action::Fire));
    }

    #[test]
    fn config_roundtrips_through_text() {
        let mut b = Keybinds::default();
        b.bind(Action::Up, KeyCode::Char('e'));
        b.bind(Action::Fire, KeyCode::Char('j'));
        let mut out = String::new();
        for action in Action::ALL {
            out.push_str(&format!("{} = {}\n", action.id(), b.keys[idx(action)].join(" ")));
        }
        let mut b2 = Keybinds::default();
        b2.apply_config(&out);
        assert_eq!(b2.action_for(KeyCode::Char('e')), Some(Action::Up));
        assert_eq!(b2.action_for(KeyCode::Char('j')), Some(Action::Fire));
        assert_eq!(b2.action_for(KeyCode::Char('w')), None);
    }
}
