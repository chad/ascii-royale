use std::collections::VecDeque;
use std::time::Duration;

use anyhow::Result;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::game::items::ItemKind;
use crate::game::map::{Map, Tile};
use crate::game::state::{InputCmd, Snapshot};
use crate::game::Dir;
use crate::net::protocol::{Aboard, ClientMsg, ServerHandle, ServerMsg, Standing};
use crate::ui::keys::{Action, Keybinds};
use crate::ui::sound::Sounds;

const FEED_LINES: usize = 64;
/// Seconds the dropship bar treats as "empty"; matches the server's 1-human
/// base countdown so the bar reads full right as the drop fires.
const LOBBY_BAR_MAX: u32 = 30;

enum Screen {
    Connecting,
    Lobby,
    Game,
    Results(Vec<Standing>),
    /// Joined mid-match on an arena server; the lobby reopens afterwards.
    Waiting { alive: u8 },
    Fatal(String),
}

pub struct App {
    handle: ServerHandle,
    screen: Screen,
    ticket: Option<String>,
    is_host: bool,
    map: Option<Map>,
    my_id: u8,
    snap: Option<Snapshot>,
    aboard: Vec<Aboard>,
    seats: u8,
    starting_in: Option<u32>,
    /// Whether we've toggled ourselves ready in the dropship lobby.
    ready: bool,
    feed: VecDeque<String>,
    link_lost: bool,
    sounds: Sounds,
    binds: Keybinds,
    keys_ui: Option<KeysUi>,
}

/// State of the key-rebinding overlay (opened with `k`).
struct KeysUi {
    selected: usize,
    awaiting: bool,
}

/// Run the whole client UI on the calling thread; network tasks keep
/// running on the tokio runtime in the background.
pub fn run(handle: ServerHandle, ticket: Option<String>, is_host: bool) -> Result<()> {
    let mut app = App {
        handle,
        screen: Screen::Connecting,
        ticket,
        is_host,
        map: None,
        my_id: 0,
        snap: None,
        aboard: Vec::new(),
        seats: 16,
        starting_in: None,
        ready: false,
        feed: VecDeque::new(),
        link_lost: false,
        sounds: Sounds::new(),
        binds: Keybinds::load(),
        keys_ui: None,
    };
    let mut terminal = ratatui::init();
    let result = app.main_loop(&mut terminal);
    ratatui::restore();
    result
}

/// Live lobby browser: shows hosts discovered over gossip and returns the
/// chosen game ticket (or None if the user backs out). `a` auto-joins the
/// best open game.
pub fn browse(listings: crate::net::lobby::Listings) -> Result<Option<String>> {
    use crate::net::lobby;
    let mut terminal = ratatui::init();
    let mut selected = 0usize;
    let chosen = loop {
        let rows = lobby::snapshot(&listings);
        selected = selected.min(rows.len().saturating_sub(1));
        terminal.draw(|f| draw_browse(f, &rows, selected))?;
        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break None,
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        break None
                    }
                    KeyCode::Up | KeyCode::Char('w') | KeyCode::Char('k') => {
                        selected = selected.saturating_sub(1)
                    }
                    KeyCode::Down | KeyCode::Char('s') | KeyCode::Char('j') => {
                        if !rows.is_empty() {
                            selected = (selected + 1).min(rows.len() - 1);
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(l) = rows.get(selected) {
                            break Some(l.beacon.ticket.clone());
                        }
                    }
                    KeyCode::Char('a') => {
                        // Auto-join: snapshot is sorted best-first, so take the
                        // first joinable game.
                        if let Some(l) = rows.iter().find(|l| l.joinable()).or(rows.first()) {
                            break Some(l.beacon.ticket.clone());
                        }
                    }
                    _ => {}
                }
            }
        }
    };
    ratatui::restore();
    Ok(chosen)
}

fn draw_browse(f: &mut Frame, rows: &[crate::net::lobby::Listing], selected: usize) {
    let mut lines: Vec<Line> = Vec::new();
    for row in TITLE {
        lines.push(Line::from((*row).yellow().bold()));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from("open games on the lobby".bold()));
    lines.push(Line::raw(""));
    if rows.is_empty() {
        lines.push(Line::from("  searching the gossip network…".dark_gray()));
        lines.push(Line::from("  (hosts appear here within a few seconds)".dark_gray()));
    }
    for (i, l) in rows.iter().enumerate() {
        let b = &l.beacon;
        let marker = if i == selected { "> " } else { "  " };
        let status = match b.phase.as_str() {
            "boarding" => "boarding".green(),
            "countdown" => format!("drops in {}s", b.starting_in.unwrap_or(0)).green(),
            "live" => "in progress".red(),
            _ => b.phase.clone().dark_gray(),
        };
        let head = format!("{marker}{:<14} {:>2}/{:<2} ", b.name, b.aboard, b.seats);
        let head = if i == selected { head.yellow().bold() } else { head.into() };
        lines.push(Line::from(vec![head, status]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(
        "↑↓ select · enter join · a auto-join best · q quit".dark_gray(),
    ));
    let h = (lines.len() as u16 + 4).min(f.area().height);
    let area = centered(f.area(), 64, h);
    f.render_widget(Clear, area);
    let p = Paragraph::new(lines)
        .centered()
        .block(Block::bordered().title(" ascii-royale · lobby browser "));
    f.render_widget(p, area);
}

impl App {
    fn main_loop(&mut self, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
        loop {
            // Drain everything the server sent since last frame.
            loop {
                match self.handle.rx.try_recv() {
                    Ok(msg) => self.on_server_msg(msg),
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                        if !matches!(self.screen, Screen::Results(_) | Screen::Fatal(_)) {
                            self.link_lost = true;
                        }
                        break;
                    }
                }
            }

            terminal.draw(|f| self.draw(f))?;

            if event::poll(Duration::from_millis(33))? {
                if let Event::Key(key) = event::read()? {
                    if (key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat)
                        && self.on_key(key.code, key.modifiers) {
                            return Ok(());
                        }
                }
            }
        }
    }

    fn on_server_msg(&mut self, msg: ServerMsg) {
        match msg {
            ServerMsg::Welcome { id, map, .. } => {
                // A Welcome mid-session means a fresh match: reset everything.
                self.my_id = id;
                self.map = Some(map);
                self.snap = None;
                self.feed.clear();
                self.starting_in = None;
                self.ready = false;
                self.screen = Screen::Lobby;
            }
            ServerMsg::Roster { aboard, seats, starting_in } => {
                // Trust the server's view of our own ready state.
                self.ready = aboard.iter().any(|a| a.is_you && a.ready);
                self.aboard = aboard;
                self.seats = seats;
                self.starting_in = starting_in;
            }
            ServerMsg::Waiting { alive } => {
                if !matches!(self.screen, Screen::Game) {
                    self.screen = Screen::Waiting { alive };
                }
            }
            ServerMsg::Snapshot(snap) => {
                for line in &snap.feed {
                    self.feed.push_front(line.clone());
                }
                self.feed.truncate(FEED_LINES);
                self.sounds.on_snapshot(self.snap.as_ref(), &snap);
                self.snap = Some(*snap);
                if matches!(self.screen, Screen::Lobby | Screen::Connecting) {
                    self.screen = Screen::Game;
                }
            }
            ServerMsg::End { standings } => self.screen = Screen::Results(standings),
            ServerMsg::Rejected { reason } => self.screen = Screen::Fatal(reason),
        }
    }

    /// Returns true when the app should exit.
    fn on_key(&mut self, code: KeyCode, mods: KeyModifiers) -> bool {
        if code == KeyCode::Char('c') && mods.contains(KeyModifiers::CONTROL) {
            return true;
        }
        // The rebinding overlay swallows all input while open.
        if self.keys_ui.is_some() {
            self.on_keys_ui_key(code);
            return false;
        }
        if code == KeyCode::Char('k')
            && matches!(self.screen, Screen::Lobby | Screen::Results(_))
        {
            self.keys_ui = Some(KeysUi { selected: 0, awaiting: false });
            return false;
        }
        if self.binds.action_for(code) == Some(Action::Mute) {
            self.sounds.toggle_mute();
            return false;
        }
        if self.link_lost {
            return matches!(code, KeyCode::Char('q') | KeyCode::Esc | KeyCode::Enter);
        }
        match &self.screen {
            Screen::Fatal(_) => {
                matches!(code, KeyCode::Char('q') | KeyCode::Esc | KeyCode::Enter)
            }
            Screen::Results(_) => match code {
                KeyCode::Char('q') | KeyCode::Esc => true,
                KeyCode::Enter if self.is_host => {
                    // Boss restarts: a fresh Welcome brings everyone back.
                    let _ = self.handle.tx.try_send(ClientMsg::Start);
                    false
                }
                _ => false,
            },
            Screen::Connecting | Screen::Waiting { .. } => {
                matches!(code, KeyCode::Char('q') | KeyCode::Esc)
            }
            Screen::Lobby => match code {
                KeyCode::Char('q') | KeyCode::Esc => true,
                KeyCode::Enter if self.is_host => {
                    let _ = self.handle.tx.try_send(ClientMsg::Start);
                    false
                }
                KeyCode::Char('r') => {
                    self.ready = !self.ready;
                    let _ = self.handle.tx.try_send(ClientMsg::Ready(self.ready));
                    false
                }
                _ => false,
            },
            Screen::Game => {
                if matches!(code, KeyCode::Char('q') | KeyCode::Esc) {
                    return true;
                }
                let cmd = match self.binds.action_for(code) {
                    Some(Action::Up) => Some(InputCmd::Move(Dir::North)),
                    Some(Action::Down) => Some(InputCmd::Move(Dir::South)),
                    Some(Action::Left) => Some(InputCmd::Move(Dir::West)),
                    Some(Action::Right) => Some(InputCmd::Move(Dir::East)),
                    Some(Action::Fire) => Some(InputCmd::Fire),
                    Some(Action::Pickup) => Some(InputCmd::Pickup),
                    Some(Action::Heal) => Some(InputCmd::Heal),
                    Some(Action::Mute) | None => None,
                };
                if let Some(cmd) = cmd {
                    let _ = self.handle.tx.try_send(ClientMsg::Input(cmd));
                }
                false
            }
        }
    }

    fn on_keys_ui_key(&mut self, code: KeyCode) {
        let Some(ui) = &mut self.keys_ui else { return };
        if ui.awaiting {
            match code {
                KeyCode::Esc => ui.awaiting = false,
                _ => {
                    if self.binds.bind(Action::ALL[ui.selected], code) {
                        ui.awaiting = false;
                        let _ = self.binds.save();
                    }
                }
            }
            return;
        }
        match code {
            KeyCode::Esc | KeyCode::Char('k') | KeyCode::Char('q') => self.keys_ui = None,
            KeyCode::Up => ui.selected = ui.selected.saturating_sub(1),
            KeyCode::Down => ui.selected = (ui.selected + 1).min(Action::ALL.len() - 1),
            KeyCode::Enter => ui.awaiting = true,
            KeyCode::Char('r') => {
                self.binds.reset();
                let _ = self.binds.save();
            }
            _ => {}
        }
    }

    fn draw(&self, f: &mut Frame) {
        match &self.screen {
            Screen::Connecting => self.draw_center_box(f, "connecting", connecting_lines()),
            Screen::Lobby => self.draw_lobby(f),
            Screen::Game => self.draw_game(f),
            Screen::Results(standings) => self.draw_results(f, standings),
            Screen::Waiting { alive } => self.draw_center_box(
                f,
                "waiting",
                vec![
                    Line::raw(""),
                    Line::from("a match is in progress".yellow().bold()),
                    Line::from(format!("{alive} still standing")),
                    Line::raw(""),
                    Line::from("you're in line for the next island".dark_gray()),
                    Line::from("q to leave".dark_gray()),
                ],
            ),
            Screen::Fatal(reason) => self.draw_center_box(
                f,
                "no dice",
                vec![
                    Line::raw(""),
                    Line::from(reason.clone().red()),
                    Line::raw(""),
                    Line::from("press q to exit".dark_gray()),
                ],
            ),
        }
        if let Some(ui) = &self.keys_ui {
            self.draw_keys_ui(f, ui);
        }
        if self.link_lost {
            let area = centered(f.area(), 40, 5);
            f.render_widget(Clear, area);
            let p = Paragraph::new(vec![
                Line::raw(""),
                Line::from("connection to host lost".red().bold()),
                Line::from("press q to exit".dark_gray()),
            ])
            .centered()
            .block(Block::bordered().border_style(Style::new().red()));
            f.render_widget(p, area);
        }
    }

    fn draw_keys_ui(&self, f: &mut Frame, ui: &KeysUi) {
        let h = Action::ALL.len() as u16 + 6;
        let area = centered(f.area(), 44, h);
        f.render_widget(Clear, area);
        let mut lines: Vec<Line> = vec![Line::raw("")];
        for (i, action) in Action::ALL.iter().enumerate() {
            let marker = if i == ui.selected { "> " } else { "  " };
            let label = format!("{marker}{:<10}", action.name());
            let keys = if i == ui.selected && ui.awaiting {
                "press a key...".to_string()
            } else {
                self.binds.keys_label(*action)
            };
            let row = format!("{label} {keys}");
            lines.push(if i == ui.selected {
                Line::from(row.yellow().bold())
            } else {
                Line::from(row)
            });
        }
        lines.push(Line::raw(""));
        lines.push(Line::from(
            if ui.awaiting {
                "esc to cancel"
            } else {
                "up/down select · enter rebind · r reset · k close"
            }
            .dark_gray(),
        ));
        let p = Paragraph::new(lines)
            .block(Block::bordered().title(" key bindings (arrows always move) "));
        f.render_widget(p, area);
    }

    fn draw_center_box(&self, f: &mut Frame, title: &str, lines: Vec<Line>) {
        let area = centered(f.area(), 60, lines.len() as u16 + 4);
        let p = Paragraph::new(lines)
            .centered()
            .wrap(Wrap { trim: false })
            .block(Block::bordered().title(format!(" ascii-royale · {title} ")));
        f.render_widget(p, area);
    }

    fn draw_lobby(&self, f: &mut Frame) {
        let mut lines: Vec<Line> = Vec::new();
        for row in TITLE {
            lines.push(Line::from((*row).yellow().bold()));
        }
        lines.push(Line::raw(""));
        if let Some(t) = &self.ticket {
            lines.push(Line::from(vec![
                "ticket  ".dark_gray(),
                t.clone().cyan().bold(),
            ]));
            lines.push(Line::from(
                "friends join with: ascii-royale join <ticket>".dark_gray(),
            ));
        } else if !self.is_host {
            lines.push(Line::from("connected to host".green()));
        }
        lines.push(Line::raw(""));

        // Dropship countdown bar (arena): fills as the drop approaches.
        if let Some(secs) = self.starting_in {
            let mins = secs / 60;
            let clock = format!("{mins}:{:02}", secs % 60);
            lines.push(Line::from(format!("NEXT DROP IN  {clock}").yellow().bold()));
            let width = 22usize;
            let filled = (((LOBBY_BAR_MAX.saturating_sub(secs)) as usize * width)
                / LOBBY_BAR_MAX.max(1) as usize)
                .min(width);
            let bar: String = "▓".repeat(filled) + &"░".repeat(width - filled);
            lines.push(Line::from(vec![bar.cyan(), "  more = sooner".dark_gray()]));
            lines.push(Line::raw(""));
        }

        lines.push(Line::from(vec![
            format!("aboard ({})", self.aboard.len()).bold(),
            format!("        the drop fills to {}", self.seats).dark_gray(),
        ]));
        for a in &self.aboard {
            let tag = if a.is_you { " (you)" } else { "" };
            let label = format!("  @ {}{tag}", a.name);
            let label = format!("{label:<20}");
            let name_span = if a.is_you { label.yellow().bold() } else { label.into() };
            let state = if a.ready { "ready".green() } else { "......".dark_gray() };
            lines.push(Line::from(vec![name_span, state]));
        }
        lines.push(Line::raw(""));

        // Action hint depends on mode.
        let ready_hint = if self.ready { "[r] unready" } else { "[r] ready up" };
        if self.starting_in.is_some() {
            lines.push(Line::from(format!("{ready_hint} · [k] keys · [q] quit").dark_gray()));
        } else if self.is_host {
            lines.push(Line::from(
                format!("[enter] drop now · {ready_hint} · [k] keys · [q] quit").dark_gray(),
            ));
        } else {
            lines.push(Line::from(
                format!("{ready_hint} · waiting for host · [k] keys · [q] quit").dark_gray(),
            ));
        }
        let h = (lines.len() as u16 + 4).min(f.area().height);
        let area = centered(f.area(), 72, h);
        let title = if self.starting_in.is_some() { " ascii-royale · dropship " } else { " ascii-royale · lobby " };
        let p = Paragraph::new(lines)
            .centered()
            .wrap(Wrap { trim: false })
            .block(Block::bordered().title(title));
        f.render_widget(p, area);
    }

    fn draw_results(&self, f: &mut Frame, standings: &[Standing]) {
        let mut lines: Vec<Line> = Vec::new();
        for row in TITLE {
            lines.push(Line::from((*row).yellow().bold()));
        }
        lines.push(Line::raw(""));
        let winner = standings.iter().find(|s| s.placement == Some(1));
        if let Some(w) = winner {
            let txt = if w.is_you {
                "*** VICTORY ROYALE — you win ***".to_string()
            } else {
                format!("{} takes the crown", w.name)
            };
            lines.push(Line::from(txt.green().bold()));
            lines.push(Line::raw(""));
        }
        for s in standings.iter().take(16) {
            let place = s.placement.map(|p| format!("#{p:<2}")).unwrap_or("-- ".into());
            let row = format!("{place} {:<14} {} kills", s.name, s.kills);
            lines.push(if s.is_you {
                Line::from(row.yellow().bold())
            } else {
                Line::from(row)
            });
        }
        lines.push(Line::raw(""));
        lines.push(Line::from(
            if self.is_host {
                "[enter] play again · [q] quit"
            } else {
                "next match starts shortly · [q] quit"
            }
            .dark_gray(),
        ));
        let h = (lines.len() as u16 + 4).min(f.area().height);
        let area = centered(f.area(), 60, h);
        let p = Paragraph::new(lines)
            .centered()
            .block(Block::bordered().title(" ascii-royale · results "));
        f.render_widget(p, area);
    }

    fn draw_game(&self, f: &mut Frame) {
        let Some(snap) = &self.snap else { return };
        let Some(map) = &self.map else { return };
        let b = &self.binds;
        let sound = if !self.sounds.available() {
            String::new()
        } else if self.sounds.muted {
            format!(" · {} unmute", b.keys_label(Action::Mute))
        } else {
            format!(" · {} mute", b.keys_label(Action::Mute))
        };
        let controls = format!(
            "{}{}{}{}/arrows move+aim · {} fire · {} pickup · {} heal{sound} · q quit",
            b.keys_label(Action::Up),
            b.keys_label(Action::Left),
            b.keys_label(Action::Down),
            b.keys_label(Action::Right),
            b.keys_label(Action::Fire),
            b.keys_label(Action::Pickup),
            b.keys_label(Action::Heal),
        );
        GameView { map, snap, feed: &self.feed, controls }.draw(f);
    }
}

/// The in-match screen, decoupled from App so headless tools (the GIF
/// capture example) can render gameplay without a terminal or network.
pub struct GameView<'a> {
    pub map: &'a Map,
    pub snap: &'a Snapshot,
    pub feed: &'a VecDeque<String>,
    pub controls: String,
}

impl GameView<'_> {
    pub fn draw(&self, f: &mut Frame) {
        let (map, snap) = (self.map, self.snap);
        let [main, status] =
            Layout::vertical([Constraint::Min(5), Constraint::Length(1)]).areas(f.area());
        let [map_area, side] =
            Layout::horizontal([Constraint::Min(20), Constraint::Length(26)]).areas(main);

        let map_block = Block::bordered().title(" the island ");
        let inner = map_block.inner(map_area);
        f.render_widget(map_block, map_area);
        render_map(map, snap, inner, f.buffer_mut());

        self.draw_sidebar(f, side, snap);

        f.render_widget(Paragraph::new(self.controls.clone().dark_gray()).centered(), status);

        if let Some(n) = snap.countdown {
            let area = centered(f.area(), 30, 5);
            f.render_widget(Clear, area);
            let p = Paragraph::new(vec![
                Line::raw(""),
                Line::from(format!("dropping in {n}...").yellow().bold()),
                Line::raw(""),
            ])
            .centered()
            .block(Block::bordered());
            f.render_widget(p, area);
        } else if !snap.you.alive && !snap.over {
            let place = snap
                .you
                .placement
                .map(|p| format!("#{p}"))
                .unwrap_or_default();
            let area = Rect { x: inner.x, y: inner.y, width: inner.width, height: 1 };
            let p = Paragraph::new(
                format!(" ELIMINATED {place} — spectating your corpse · q to leave ")
                    .red()
                    .bold(),
            )
            .centered();
            f.render_widget(p, area);
        }
    }

    fn draw_sidebar(&self, f: &mut Frame, area: Rect, snap: &Snapshot) {
        // (method of GameView; feed comes from the view, not the App)
        let you = &snap.you;
        let mut lines: Vec<Line> = Vec::new();

        lines.push(Line::from(vec![
            "alive ".dark_gray(),
            format!("{:<3}", snap.alive).white().bold(),
            " kills ".dark_gray(),
            format!("{}", you.kills).white().bold(),
        ]));
        lines.push(Line::raw(""));
        lines.push(bar_line("HP ", you.hp, 100, hp_color(you.hp)));
        lines.push(bar_line("ARM", you.armor, 100, Color::Cyan));
        lines.push(Line::raw(""));

        let stats = you.weapon.stats();
        let dry = stats.ammo_cost > 0 && you.ammo < stats.ammo_cost;
        let ammo = if stats.ammo_cost == 0 {
            "--".to_string()
        } else {
            format!("{}", you.ammo)
        };
        let ammo_span = if dry { format!("  ammo {ammo}").red().bold() } else { format!("  ammo {ammo}").into() };
        lines.push(Line::from(vec![
            stats.name.to_string().magenta().bold(),
            ammo_span,
        ]));
        if dry {
            lines.push(Line::from("NO AMMO - grab = packs".on_red().white().bold()));
        }
        // Unarmed or dry: point at the nearest fix on screen.
        let needs_gun = stats.ammo_cost == 0 && stats.speed == 0;
        if needs_gun || dry {
            let fix = snap
                .loot
                .iter()
                .filter(|(_, item)| match item {
                    ItemKind::Weapon(_) => needs_gun,
                    ItemKind::Ammo(_) => dry,
                    _ => false,
                })
                .min_by_key(|(p, _)| (p.0 - you.pos.0).abs() + (p.1 - you.pos.1).abs());
            if let Some((p, item)) = fix {
                let label = match item {
                    ItemKind::Weapon(_) => "gun",
                    _ => "ammo",
                };
                lines.push(Line::from(vec![
                    format!("{label} nearby: ").dark_gray(),
                    compass(you.pos, *p).cyan().bold(),
                ]));
            }
        }
        let aim = match you.dir {
            Dir::North => "^",
            Dir::South => "v",
            Dir::East => ">",
            Dir::West => "<",
        };
        let ready = if you.fire_cd == 0 { "ready".green() } else { "....".dark_gray() };
        lines.push(Line::from(vec![
            "trigger ".dark_gray(),
            ready,
            "  aim ".dark_gray(),
            aim.yellow(),
        ]));
        lines.push(Line::from(vec![
            format!("medkits {}", you.medkits).into(),
            if you.heal_cd > 0 { "  (cooling)".dark_gray() } else { "  [h]".dark_gray() },
        ]));
        lines.push(Line::raw(""));

        // Storm status.
        let z = &snap.zone;
        let inside = {
            let dx = you.pos.0 as f32 + 0.5 - z.center.0;
            let dy = you.pos.1 as f32 + 0.5 - z.center.1;
            dx * dx + dy * dy <= z.radius * z.radius
        };
        if z.shrinking {
            lines.push(Line::from(
                format!("STORM CLOSING {}s", z.seconds_left).red().bold(),
            ));
        } else {
            lines.push(Line::from(vec![
                "storm holds ".dark_gray(),
                format!("{}s", z.seconds_left).white(),
            ]));
        }
        if !inside {
            lines.push(Line::from(
                format!("OUTSIDE ZONE -{}hp", z.damage).on_red().white().bold(),
            ));
            let dx = z.center.0 - you.pos.0 as f32;
            let dy = z.center.1 - you.pos.1 as f32;
            let arrow = if dx.abs() > dy.abs() {
                if dx > 0.0 { "east ->" } else { "<- west" }
            } else if dy > 0.0 {
                "south v"
            } else {
                "north ^"
            };
            lines.push(Line::from(format!("safety: {arrow}").yellow()));
        }
        lines.push(Line::raw(""));

        // Loot underfoot.
        if let Some((_, item)) = snap.loot.iter().find(|(p, _)| *p == you.pos) {
            lines.push(Line::from(vec![
                "here: ".dark_gray(),
                item.label().cyan().bold(),
                " [e]".dark_gray(),
            ]));
            lines.push(Line::raw(""));
        }

        lines.push(Line::from("-- feed --".dark_gray()));
        let used = lines.len();
        let room = (area.height as usize).saturating_sub(used + 2);
        for line in self.feed.iter().take(room) {
            lines.push(Line::from(line.clone().dark_gray()));
        }

        let p = Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .block(Block::bordered().title(" status "));
        f.render_widget(p, area);
    }
}

fn connecting_lines() -> Vec<Line<'static>> {
    vec![
        Line::raw(""),
        Line::from("reaching the host over iroh...".cyan()),
        Line::from("(NAT holepunching can take a few seconds)".dark_gray()),
        Line::raw(""),
        Line::from("press q to give up".dark_gray()),
    ]
}

const TITLE: &[&str] = &[
    r"                _ _                        _      ",
    r"  __ _ ___  ___(_|_)  _ __ ___  _   _  __ _| | ___ ",
    r" / _` / __|/ __| | | | '__/ _ \| | | |/ _` | |/ _ \",
    r"| (_| \__ \ (__| | | | | | (_) | |_| | (_| | |  __/",
    r" \__,_|___/\___|_|_| |_|  \___/ \__, |\__,_|_|\___|",
    r"                                |___/              ",
];

/// Coarse compass hint ("12 NE") from one cell to another.
fn compass(from: (i32, i32), to: (i32, i32)) -> String {
    let dx = to.0 - from.0;
    let dy = to.1 - from.1;
    let ns = if dy <= -2 { "N" } else if dy >= 2 { "S" } else { "" };
    let ew = if dx <= -2 { "W" } else if dx >= 2 { "E" } else { "" };
    let dir = format!("{ns}{ew}");
    let dist = dx.abs().max(dy.abs());
    if dir.is_empty() {
        "right here (e)".to_string()
    } else {
        format!("{dist} {dir}")
    }
}

fn hp_color(hp: i32) -> Color {
    match hp {
        0..=25 => Color::Red,
        26..=55 => Color::Yellow,
        _ => Color::Green,
    }
}

fn bar_line(label: &str, val: i32, max: i32, color: Color) -> Line<'static> {
    let width = 14usize;
    let filled = ((val.max(0) * width as i32) / max) as usize;
    let bar: String = "#".repeat(filled.min(width)) + &"-".repeat(width - filled.min(width));
    Line::from(vec![
        format!("{label} ").dark_gray(),
        Span::styled(bar, Style::new().fg(color)),
        format!(" {val:>3}").into(),
    ])
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

fn tile_cell(tile: Tile) -> (char, Color) {
    match tile {
        Tile::Grass => ('.', Color::Green),
        Tile::Tree => ('T', Color::LightGreen),
        Tile::Water => ('~', Color::Blue),
        Tile::Wall => ('#', Color::Gray),
        Tile::Floor => (',', Color::DarkGray),
        Tile::Road => (':', Color::DarkGray),
    }
}

fn item_cell(item: ItemKind) -> (char, Color) {
    match item {
        ItemKind::Weapon(_) => (')', Color::Magenta),
        ItemKind::Ammo(_) => ('=', Color::Yellow),
        ItemKind::Medkit => ('+', Color::Red),
        ItemKind::Vest => (']', Color::Cyan),
    }
}

fn render_map(map: &Map, snap: &Snapshot, area: Rect, buf: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let (vw, vh) = (area.width as i32, area.height as i32);
    let me = snap.you.pos;
    let cam_x = (me.0 - vw / 2).clamp(0, (map.w - vw).max(0));
    let cam_y = (me.1 - vh / 2).clamp(0, (map.h - vh).max(0));
    let z = &snap.zone;

    for sy in 0..vh {
        for sx in 0..vw {
            let (wx, wy) = (cam_x + sx, cam_y + sy);
            let Some(cell) = buf.cell_mut((area.x + sx as u16, area.y + sy as u16)) else {
                continue;
            };
            if wx >= map.w || wy >= map.h {
                cell.set_char(' ');
                continue;
            }
            let (mut ch, mut color) = tile_cell(map.get((wx, wy)));
            let mut modifier = Modifier::DIM;

            let dx = wx as f32 + 0.5 - z.center.0;
            let dy = wy as f32 + 0.5 - z.center.1;
            let outside = dx * dx + dy * dy > z.radius * z.radius;
            if outside {
                // The storm: everything washes blue.
                color = Color::LightBlue;
                ch = '%';
            } else {
                // Hint where the storm settles next.
                let tdx = wx as f32 + 0.5 - z.target_center.0;
                let tdy = wy as f32 + 0.5 - z.target_center.1;
                let td = (tdx * tdx + tdy * tdy).sqrt();
                if (td - z.target_radius).abs() < 0.6 {
                    ch = 'o';
                    color = Color::LightCyan;
                }
            }

            buf_set(cell, ch, color, modifier);

            // Entities draw over terrain (aim mark < loot < bullets < players < you).
            modifier = Modifier::BOLD;
            let pos = (wx, wy);
            if snap.you.alive && pos == snap.you.dir.step(me) {
                // Crosshair: the cell your next shot leaves through.
                let ch = match snap.you.dir {
                    Dir::North => '^',
                    Dir::South => 'v',
                    Dir::East => '>',
                    Dir::West => '<',
                };
                buf_set(cell, ch, Color::Yellow, Modifier::DIM);
            }
            if let Some((_, item)) = snap.loot.iter().find(|(p, _)| *p == pos) {
                let (ch, color) = item_cell(*item);
                buf_set(cell, ch, color, modifier);
            }
            if let Some((_, dir, impact)) = snap
                .bullets
                .iter()
                .filter(|(p, _, _)| *p == pos)
                .max_by_key(|(_, _, impact)| *impact)
            {
                let ch = if *impact {
                    '*'
                } else {
                    match dir {
                        Dir::North | Dir::South => '|',
                        Dir::East | Dir::West => '-',
                    }
                };
                buf_set(cell, ch, Color::Yellow, modifier);
            }
            if snap.players.iter().any(|p| p.pos == pos) {
                buf_set(cell, '@', Color::Red, modifier);
            }
            if pos == me && snap.you.alive {
                buf_set(cell, '@', Color::Yellow, modifier);
            } else if pos == me {
                buf_set(cell, 'x', Color::DarkGray, modifier);
            }
        }
    }
}

fn buf_set(cell: &mut ratatui::buffer::Cell, ch: char, fg: Color, modifier: Modifier) {
    cell.set_char(ch);
    cell.set_style(Style::new().fg(fg).add_modifier(modifier));
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::game::state::{MatchPhase, World};
    use crate::game::GameConfig;
    use ratatui::{backend::TestBackend, Terminal};

    fn test_app() -> App {
        let (_tx, rx) = tokio::sync::mpsc::channel(8);
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        App {
            handle: ServerHandle { rx, tx },
            screen: Screen::Connecting,
            ticket: Some("abc123ticket".into()),
            is_host: true,
            map: None,
            my_id: 0,
            snap: None,
            aboard: Vec::new(),
            seats: 16,
            starting_in: None,
            ready: false,
            feed: VecDeque::new(),
            link_lost: false,
            sounds: Sounds::disabled(),
            binds: Keybinds::default(),
            keys_ui: None,
        }
    }

    fn frame_text(app: &App) -> String {
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn aboard(names: &[&str]) -> Vec<Aboard> {
        names
            .iter()
            .enumerate()
            .map(|(i, n)| Aboard { name: (*n).into(), ready: false, is_you: i == 0 })
            .collect()
    }

    #[test]
    fn lobby_screen_shows_ticket_and_roster() {
        let mut app = test_app();
        let world = World::new(11, GameConfig::default());
        app.on_server_msg(ServerMsg::Welcome { id: 0, map: world.map, config: world.config });
        app.on_server_msg(ServerMsg::Roster {
            aboard: aboard(&["chad", "wanderer"]),
            seats: 16,
            starting_in: None,
        });
        let text = frame_text(&app);
        assert!(text.contains("abc123ticket"), "lobby should show the ticket");
        assert!(text.contains("@ chad") && text.contains("@ wanderer"));
        assert!(text.contains("[enter] drop now"));
    }

    #[test]
    fn dropship_shows_countdown_and_ready() {
        let mut app = test_app();
        app.is_host = false;
        let world = World::new(11, GameConfig::default());
        app.on_server_msg(ServerMsg::Welcome { id: 0, map: world.map, config: world.config });
        let mut roster = aboard(&["chad", "vex"]);
        roster[1].ready = true;
        app.on_server_msg(ServerMsg::Roster { aboard: roster, seats: 16, starting_in: Some(12) });
        let text = frame_text(&app);
        assert!(text.contains("NEXT DROP IN"), "countdown header should show");
        assert!(text.contains("0:12"), "clock should render mm:ss");
        assert!(text.contains("fills to 16"), "seat fill framing");
        assert!(text.contains("ready up"), "ready hint should show");
        assert!(text.contains("ready"), "vex's ready state should render");
    }

    #[test]
    fn game_screen_renders_player_hud_and_feed() {
        let mut app = test_app();
        let mut world = World::new(11, GameConfig::default());
        world.add_player("chad".into(), false);
        for i in 0..5 {
            world.add_player(format!("bot{i}"), true);
        }
        app.on_server_msg(ServerMsg::Welcome {
            id: 0,
            map: world.map.clone(),
            config: world.config.clone(),
        });
        world.start_match();
        while world.phase != MatchPhase::Active {
            world.step();
        }
        for _ in 0..10 {
            world.step();
        }
        let snap = world.snapshot_for(0, &["bot1 eliminated bot2 (SMG)".to_string()]);
        app.on_server_msg(ServerMsg::Snapshot(Box::new(snap)));

        let text = frame_text(&app);
        assert!(text.contains('@'), "the player glyph should be on screen");
        assert!(text.contains("HP"), "HUD should show HP bar");
        assert!(text.contains("alive"), "HUD should show alive count");
        assert!(text.contains("eliminated bot2"), "feed line should render");
        assert!(text.contains("move"), "controls hint should render");
    }

    pub(super) fn print_midgame_frame() {
        use crate::game::items::WeaponKind;
        use crate::game::state::InputCmd;
        let mut app = test_app();
        let mut world = World::new(7, GameConfig::default());
        world.add_player("chad".into(), false);
        for i in 0..7 {
            world.add_player(format!("bot{i}"), true);
        }
        app.on_server_msg(ServerMsg::Welcome {
            id: 0,
            map: world.map.clone(),
            config: world.config.clone(),
        });
        world.start_match();
        while world.phase != MatchPhase::Active {
            world.step();
        }
        for _ in 0..150 {
            world.step();
        }
        // Stage a firefight near a building for the screenshot.
        let poi = world.map.pois[3];
        world.players[0].pos = (poi.0 - 6, poi.1 + 2);
        world.players[0].weapon = WeaponKind::Rifle;
        world.players[0].ammo = 24;
        world.players[0].armor = 31;
        world.players[0].hp = 64;
        world.players[0].kills = 2;
        world.players[1].pos = (poi.0 + 6, poi.1 + 2);
        world.players[1].hp = 100;
        world.queue_input(0, InputCmd::Fire);
        world.step();
        let snap = world.snapshot_for(
            0,
            &["bot3 eliminated bot5 (Shotgun)".to_string(), "Zone closing: 20s".to_string()],
        );
        app.on_server_msg(ServerMsg::Snapshot(Box::new(snap)));
        println!("{}", frame_text(&app));
    }

    pub(super) fn print_lobby_frame() {
        let mut app = test_app();
        app.is_host = false;
        let world = World::new(11, GameConfig::default());
        app.on_server_msg(ServerMsg::Welcome { id: 0, map: world.map, config: world.config });
        let mut roster = aboard(&["chad", "vex", "kestrel"]);
        roster[1].ready = true;
        app.on_server_msg(ServerMsg::Roster { aboard: roster, seats: 16, starting_in: Some(12) });
        println!("{}", frame_text(&app));
    }

    pub(super) fn print_keys_frame() {
        let mut app = test_app();
        let world = World::new(11, GameConfig::default());
        app.on_server_msg(ServerMsg::Welcome { id: 0, map: world.map, config: world.config });
        app.on_server_msg(ServerMsg::Roster {
            aboard: aboard(&["chad"]),
            seats: 16,
            starting_in: None,
        });
        app.on_key(KeyCode::Char('k'), KeyModifiers::NONE);
        app.on_key(KeyCode::Down, KeyModifiers::NONE);
        println!("{}", frame_text(&app));
    }

    #[test]
    fn results_screen_crowns_the_winner() {
        let mut app = test_app();
        app.on_server_msg(ServerMsg::End {
            standings: vec![
                Standing { name: "chad".into(), placement: Some(1), kills: 4, is_you: true },
                Standing { name: "bot1".into(), placement: Some(2), kills: 2, is_you: false },
            ],
        });
        let text = frame_text(&app);
        assert!(text.contains("VICTORY ROYALE"));
        assert!(text.contains("#1"));
    }

    #[test]
    fn browse_screen_lists_games_with_status() {
        use crate::net::lobby::{Beacon, Listing};
        use std::time::Instant;
        let rows = vec![
            Listing {
                beacon: Beacon {
                    ticket: "t1".into(),
                    name: "arena".into(),
                    aboard: 4,
                    seats: 16,
                    phase: "countdown".into(),
                    starting_in: Some(8),
                },
                last_seen: Instant::now(),
            },
            Listing {
                beacon: Beacon {
                    ticket: "t2".into(),
                    name: "chads-game".into(),
                    aboard: 9,
                    seats: 16,
                    phase: "live".into(),
                    starting_in: None,
                },
                last_seen: Instant::now(),
            },
        ];
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw_browse(f, &rows, 0)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let mut text = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                text.push_str(buf[(x, y)].symbol());
            }
        }
        assert!(text.contains("arena"));
        assert!(text.contains("chads-game"));
        assert!(text.contains("drops in 8s"));
        assert!(text.contains("in progress"));
        assert!(text.contains("auto-join"));
    }
}

#[cfg(test)]
mod preview {
    //! Not an assertion — prints a mid-match frame for eyeballing the layout.
    //! cargo test --lib preview -- --ignored --nocapture
    use super::tests as helpers;

    #[test]
    #[ignore = "visual aid, run with --nocapture to see a frame"]
    fn print_game_frame() {
        helpers::print_midgame_frame();
    }

    #[test]
    #[ignore = "visual aid, run with --nocapture to see a frame"]
    fn print_lobby_frame() {
        helpers::print_lobby_frame();
    }

    #[test]
    #[ignore = "visual aid, run with --nocapture to see a frame"]
    fn print_keys_frame() {
        helpers::print_keys_frame();
    }
}
