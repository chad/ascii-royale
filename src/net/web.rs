//! The arena's public web face: a designed landing page at `/`, the join
//! ticket at `/ticket`, and live state + leaderboard as JSON at `/stats`.
//!
//! Served by a tiny hand-rolled HTTP/1.1 responder (no web framework) over the
//! same tokio runtime as the game. Behind the boxd HTTPS proxy this becomes
//! `https://<vm>.boxd.sh/`. Gameplay never touches it — only the ~64-char
//! ticket and small JSON.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Per-call-sign tallies. Names are unauthenticated (anyone can play as any
/// call sign), so this is a fun board, not a ranked ladder — surfaced as such.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlayerStat {
    pub name: String,
    pub matches: u32,
    pub wins: u32,
    pub kills: u32,
    pub best_placement: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentWin {
    pub winner: String,
    pub kills: u8,
    /// Seconds since arena start when this match ended (for "x ago").
    pub at_secs: u64,
}

/// Persistent tallies, saved to JSON so they survive arena restarts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Stats {
    pub matches: u64,
    pub players: HashMap<String, PlayerStat>,
    pub recent: Vec<RecentWin>,
}

/// Shared between the game loop (writer) and the web server (reader).
pub struct ArenaState {
    /// "boarding" | "countdown" | "live" | "results"
    pub phase: &'static str,
    pub aboard: u8,
    pub alive: u8,
    pub starting_in: Option<u32>,
    pub stats: Stats,
    started: Instant,
    stats_file: Option<PathBuf>,
}

impl ArenaState {
    pub fn new(stats_file: Option<PathBuf>) -> Self {
        let stats = stats_file
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        ArenaState {
            phase: "boarding",
            aboard: 0,
            alive: 0,
            starting_in: None,
            stats,
            started: Instant::now(),
            stats_file,
        }
    }

    pub fn uptime_secs(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    /// Record a finished match: bump totals, per-player tallies, winner feed.
    pub fn record_match(&mut self, results: &[(String, Option<u8>, u8)], winner: Option<String>) {
        self.stats.matches += 1;
        for (name, placement, kills) in results {
            let e = self.stats.players.entry(name.clone()).or_default();
            e.name = name.clone();
            e.matches += 1;
            e.kills += *kills as u32;
            if let Some(p) = placement {
                if e.best_placement == 0 || *p < e.best_placement {
                    e.best_placement = *p;
                }
                if *p == 1 {
                    e.wins += 1;
                }
            }
        }
        if let Some(w) = winner {
            let kills =
                results.iter().find(|(n, _, _)| *n == w).map(|(_, _, k)| *k).unwrap_or(0);
            self.stats.recent.insert(0, RecentWin { winner: w, kills, at_secs: self.uptime_secs() });
            self.stats.recent.truncate(8);
        }
        self.save();
    }

    fn save(&self) {
        if let Some(path) = &self.stats_file {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).ok();
            }
            if let Ok(json) = serde_json::to_string_pretty(&self.stats) {
                std::fs::write(path, json).ok();
            }
        }
    }
}

#[derive(Serialize)]
struct LeaderRow {
    name: String,
    wins: u32,
    kills: u32,
    matches: u32,
    best: u8,
}

#[derive(Serialize)]
struct StatsView {
    phase: &'static str,
    aboard: u8,
    alive: u8,
    starting_in: Option<u32>,
    seats: u8,
    total_matches: u64,
    total_players: usize,
    uptime_secs: u64,
    leaderboard: Vec<LeaderRow>,
    recent: Vec<RecentWin>,
}

fn stats_json(state: &ArenaState, seats: u8) -> String {
    let mut board: Vec<LeaderRow> = state
        .stats
        .players
        .values()
        .map(|p| LeaderRow {
            name: p.name.clone(),
            wins: p.wins,
            kills: p.kills,
            matches: p.matches,
            best: p.best_placement,
        })
        .collect();
    // Most wins, then most kills, then most matches.
    board.sort_by(|a, b| {
        b.wins.cmp(&a.wins).then(b.kills.cmp(&a.kills)).then(b.matches.cmp(&a.matches))
    });
    board.truncate(15);
    let now = state.uptime_secs();
    let recent: Vec<RecentWin> = state
        .stats
        .recent
        .iter()
        .map(|r| RecentWin { winner: r.winner.clone(), kills: r.kills, at_secs: now - r.at_secs })
        .collect();
    let view = StatsView {
        phase: state.phase,
        aboard: state.aboard,
        alive: state.alive,
        starting_in: state.starting_in,
        seats,
        total_matches: state.stats.matches,
        total_players: state.stats.players.len(),
        uptime_secs: now,
        leaderboard: board,
        recent,
    };
    serde_json::to_string(&view).unwrap_or_else(|_| "{}".into())
}

/// Run the web server until the process exits.
pub async fn serve(
    port: u16,
    ticket: String,
    seats: u8,
    state: Arc<Mutex<ArenaState>>,
    browser_play_url: Option<String>,
) {
    let listener = match tokio::net::TcpListener::bind(("0.0.0.0", port)).await {
        Ok(l) => l,
        Err(e) => {
            println!("[web] could not bind :{port}: {e}");
            return;
        }
    };
    println!("[web] landing page + /ticket + /stats on :{port}");
    let page = Arc::new(landing_html(browser_play_url.as_deref()));
    loop {
        let Ok((mut sock, _)) = listener.accept().await else { continue };
        let state = state.clone();
        let ticket = ticket.clone();
        let page = page.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req.split_whitespace().nth(1).unwrap_or("/");
            // The gameplay GIF is binary and immutable — handle it separately
            // so we can send raw bytes with a long cache.
            if path == "/gameplay.gif" {
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: image/gif\r\nContent-Length: {}\r\n\
                     Cache-Control: public, max-age=86400\r\nConnection: close\r\n\r\n",
                    GAMEPLAY_GIF.len()
                );
                let _ = sock.write_all(header.as_bytes()).await;
                let _ = sock.write_all(GAMEPLAY_GIF).await;
                let _ = sock.shutdown().await;
                return;
            }
            let (ctype, body) = match path {
                "/ticket" => ("text/plain; charset=utf-8", format!("{ticket}\n")),
                "/stats" => {
                    let json = {
                        let st = state.lock().unwrap();
                        stats_json(&st, seats)
                    };
                    ("application/json", json)
                }
                "/" | "/index.html" => ("text/html; charset=utf-8", (*page).clone()),
                _ => ("text/plain; charset=utf-8", "not found\n".into()),
            };
            let status = if path == "/" || path == "/index.html" || path == "/ticket" || path == "/stats" {
                "200 OK"
            } else {
                "404 Not Found"
            };
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\
                 Access-Control-Allow-Origin: *\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        });
    }
}

/// Embedded so the arena binary is self-contained (no asset files to ship).
const GAMEPLAY_GIF: &[u8] = include_bytes!("../../assets/gameplay.gif");

fn landing_html(browser_play_url: Option<&str>) -> String {
    let play_button = match browser_play_url {
        Some(url) => format!(
            r#"<a class="btn primary" href="{url}" target="_blank">▶ play in your browser</a>"#
        ),
        None => String::new(),
    };
    HTML.replace("{{PLAY_BUTTON}}", &play_button)
}

const HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>ascii-royale — terminal battle royale</title>
<style>
  :root{
    --bg:#0b0c12; --panel:#12141c; --line:#222637; --fg:#c0caf5;
    --dim:#565f89; --green:#9ece6a; --amber:#e0af68; --red:#f7768e;
    --cyan:#7dcfff; --magenta:#bb9af7;
  }
  *{box-sizing:border-box}
  html,body{margin:0;background:var(--bg);color:var(--fg);
    font-family:ui-monospace,"SF Mono",Menlo,Consolas,monospace;line-height:1.5}
  body{background-image:radial-gradient(circle at 50% -10%,#1a1d2e 0,var(--bg) 60%);
    min-height:100vh;padding:0 16px 64px}
  .wrap{max-width:880px;margin:0 auto}
  header{text-align:center;padding:40px 0 8px}
  pre.logo{color:var(--amber);font-size:clamp(5px,1.35vw,11px);line-height:1.05;
    margin:0;text-shadow:0 0 22px rgba(224,175,104,.35);overflow-x:auto}
  .tag{color:var(--dim);margin:14px 0 0}
  .hero{display:block;margin:22px auto 0;max-width:640px;width:100%;
    border:1px solid var(--line);border-radius:10px;
    box-shadow:0 0 40px rgba(125,207,255,.08);image-rendering:pixelated}
  .live{display:inline-flex;align-items:center;gap:8px;margin:18px 0 0;
    padding:7px 14px;border:1px solid var(--line);border-radius:999px;
    background:var(--panel);font-size:14px}
  .dot{width:9px;height:9px;border-radius:50%;background:var(--dim);
    box-shadow:0 0 8px currentColor}
  .dot.on{background:var(--green);color:var(--green);animation:pulse 1.6s infinite}
  @keyframes pulse{0%,100%{opacity:1}50%{opacity:.35}}
  .cta{display:flex;gap:12px;justify-content:center;flex-wrap:wrap;margin:26px 0 8px}
  .btn{display:inline-block;padding:11px 18px;border-radius:8px;text-decoration:none;
    border:1px solid var(--line);color:var(--fg);background:var(--panel);font-size:15px}
  .btn:hover{border-color:var(--cyan);color:var(--cyan)}
  .btn.primary{border-color:var(--green);color:var(--green)}
  .btn.primary:hover{background:rgba(158,206,106,.1)}
  .cmd{display:flex;align-items:center;gap:10px;justify-content:center;margin:14px 0 0}
  code.run{background:#000;border:1px solid var(--line);border-radius:8px;
    padding:10px 16px;color:var(--green);font-size:16px;cursor:pointer}
  code.run:hover{border-color:var(--green)}
  .copyhint{color:var(--dim);font-size:12px}
  .grid{display:grid;grid-template-columns:1fr 1fr;gap:18px;margin:38px 0}
  @media(max-width:680px){.grid{grid-template-columns:1fr}}
  .card{background:var(--panel);border:1px solid var(--line);border-radius:12px;padding:18px 20px}
  .card h2{margin:0 0 12px;font-size:13px;letter-spacing:.14em;text-transform:uppercase;
    color:var(--dim);font-weight:600}
  table{width:100%;border-collapse:collapse;font-size:14px}
  th,td{text-align:left;padding:5px 6px;border-bottom:1px solid var(--line)}
  th{color:var(--dim);font-weight:600}
  td.n{text-align:right;font-variant-numeric:tabular-nums}
  tr td:first-child{color:var(--cyan)}
  .rank{color:var(--amber);width:26px}
  .stat-row{display:flex;justify-content:space-between;padding:6px 2px;border-bottom:1px solid var(--line)}
  .stat-row b{color:var(--green);font-variant-numeric:tabular-nums}
  .recent li{list-style:none;padding:4px 0;color:var(--fg)}
  .recent .w{color:var(--magenta)}
  .recent .ago{color:var(--dim);font-size:12px}
  ul{margin:0;padding:0}
  .keys{font-size:14px;color:var(--fg)}
  .keys b{color:var(--amber)}
  footer{text-align:center;color:var(--dim);margin-top:40px;font-size:13px}
  a{color:var(--cyan)}
  .muted{color:var(--dim);font-size:12px;margin-top:6px}
</style>
</head>
<body>
<div class="wrap">
  <header>
<pre class="logo"> █████╗ ███████╗ ██████╗██╗██╗      ██████╗  ██████╗ ██╗   ██╗ █████╗ ██╗     ███████╗
██╔══██╗██╔════╝██╔════╝██║██║      ██╔══██╗██╔═══██╗╚██╗ ██╔╝██╔══██╗██║     ██╔════╝
███████║███████╗██║     ██║██║█████╗██████╔╝██║   ██║ ╚████╔╝ ███████║██║     █████╗
██╔══██║╚════██║██║     ██║██║╚════╝██╔══██╗██║   ██║  ╚██╔╝  ██╔══██║██║     ██╔══╝
██║  ██║███████║╚██████╗██║██║      ██║  ██║╚██████╔╝   ██║   ██║  ██║███████╗███████╗
╚═╝  ╚═╝╚══════╝ ╚═════╝╚═╝╚═╝      ╚═╝  ╚═╝ ╚═════╝    ╚═╝   ╚═╝  ╚═╝╚══════╝╚══════╝</pre>
    <p class="tag">a battle royale you play in your terminal · peer-to-peer over iroh · no signup</p>
    <img class="hero" src="/gameplay.gif" alt="ascii-royale gameplay" loading="eager">
    <p class="muted" style="margin-top:-2px">an actual match — drop, scrap, last one standing</p>
    <div class="live"><span id="dot" class="dot"></span><span id="status">connecting…</span></div>
    <div class="cta">
      {{PLAY_BUTTON}}
      <a class="btn" href="https://github.com/chad/ascii-royale" target="_blank">★ source on github</a>
    </div>
    <div class="cmd">
      <code class="run" id="cmd" title="click to copy">ascii-royale play</code>
    </div>
    <div class="copyhint">have the binary? that one command drops you into this arena. <span id="copied"></span></div>
  </header>

  <div class="grid">
    <div class="card">
      <h2>⚑ leaderboard</h2>
      <table><thead><tr><th class="rank">#</th><th>call sign</th>
        <th class="n">wins</th><th class="n">kills</th><th class="n">games</th></tr></thead>
        <tbody id="board"><tr><td colspan="5" class="muted">no matches yet…</td></tr></tbody></table>
      <div class="muted">call signs aren't authenticated — it's a wall of fame, not a ranked ladder.</div>
    </div>
    <div class="card">
      <h2>◷ arena</h2>
      <div class="stat-row"><span>matches played</span><b id="t-matches">—</b></div>
      <div class="stat-row"><span>call signs seen</span><b id="t-players">—</b></div>
      <div class="stat-row"><span>uptime</span><b id="t-uptime">—</b></div>
      <h2 style="margin-top:18px">☠ recent drops</h2>
      <ul class="recent" id="recent"><li class="muted">waiting for a winner…</li></ul>
    </div>
  </div>

  <div class="card">
    <h2>⌨ how to play</h2>
    <p class="keys"><b>install:</b> <code>cargo install --git https://github.com/chad/ascii-royale</code>, then <code>ascii-royale play</code> — or use the in-browser button above.</p>
    <p class="keys"><b>move + aim</b> wasd / arrows · <b>fire</b> f / space (auto-aims at anyone lined up with you) ·
       <b>pick up</b> e · <b>heal</b> h · <b>ready up</b> r · <b>quit</b> q</p>
    <p class="keys">grab a gun from a <b>#</b> building, dodge the <b>%</b> storm, outlast 15 others. the dropship leaves on a timer — more jumpers, sooner drop. empty seats fill with bots.</p>
  </div>

  <footer>peer-to-peer over <a href="https://iroh.computer" target="_blank">iroh</a> ·
    the only server is this scoreboard · <a href="/ticket">/ticket</a> · <a href="/stats">/stats</a></footer>
</div>
<script>
const fmtAgo = s => s<60?`${s|0}s ago`:s<3600?`${s/60|0}m ago`:`${s/3600|0}h ago`;
const fmtUp = s => s<3600?`${s/60|0}m`:s<86400?`${s/3600|0}h ${(s%3600)/60|0}m`:`${s/86400|0}d`;
async function tick(){
  try{
    const r = await fetch('/stats',{cache:'no-store'}); const d = await r.json();
    const dot = document.getElementById('dot'), st = document.getElementById('status');
    let live=false, msg='';
    if(d.phase==='live'){live=true; msg=`a match is live — ${d.alive} still standing`;}
    else if(d.phase==='countdown'){live=true; msg=`dropship boarding — ${d.aboard} aboard, drops in ${d.starting_in??0}s`;}
    else if(d.phase==='boarding' && d.aboard>0){live=true; msg=`${d.aboard} aboard the dropship — join them`;}
    else{msg='arena idle — be the first to drop in';}
    dot.className = 'dot'+(live?' on':''); st.textContent = msg;
    document.getElementById('t-matches').textContent = d.total_matches;
    document.getElementById('t-players').textContent = d.total_players;
    document.getElementById('t-uptime').textContent = fmtUp(d.uptime_secs);
    const tb = document.getElementById('board');
    if(d.leaderboard.length){
      tb.innerHTML = d.leaderboard.map((p,i)=>
        `<tr><td class="rank">${i+1}</td><td>${esc(p.name)}</td>
         <td class="n">${p.wins}</td><td class="n">${p.kills}</td><td class="n">${p.matches}</td></tr>`).join('');
    }
    const rc = document.getElementById('recent');
    if(d.recent.length){
      rc.innerHTML = d.recent.map(r=>
        `<li><span class="w">${esc(r.winner)}</span> took the crown · ${r.kills} kills <span class="ago">${fmtAgo(r.at_secs)}</span></li>`).join('');
    }
  }catch(e){ document.getElementById('status').textContent='arena offline'; document.getElementById('dot').className='dot'; }
}
function esc(s){const d=document.createElement('div');d.textContent=s;return d.innerHTML;}
const cmd=document.getElementById('cmd');
cmd.onclick=()=>{navigator.clipboard?.writeText('ascii-royale play');document.getElementById('copied').textContent='copied!';setTimeout(()=>document.getElementById('copied').textContent='',1500);};
tick(); setInterval(tick,4000);
</script>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_match_tallies_wins_kills_and_recent() {
        let mut s = ArenaState::new(None);
        // chad wins (placement 1, 3 kills), vex 2nd (1 kill).
        s.record_match(
            &[("chad".into(), Some(1), 3), ("vex".into(), Some(2), 1)],
            Some("chad".into()),
        );
        s.record_match(
            &[("chad".into(), Some(2), 2), ("vex".into(), Some(1), 4)],
            Some("vex".into()),
        );
        assert_eq!(s.stats.matches, 2);
        let chad = &s.stats.players["chad"];
        assert_eq!(chad.wins, 1);
        assert_eq!(chad.kills, 5);
        assert_eq!(chad.matches, 2);
        assert_eq!(chad.best_placement, 1);
        assert_eq!(s.stats.recent.len(), 2);
        assert_eq!(s.stats.recent[0].winner, "vex"); // most recent first

        // Leaderboard sorts by wins, then kills.
        let json = stats_json(&s, 16);
        let board_idx = json.find("\"leaderboard\"").unwrap();
        let chad_idx = json[board_idx..].find("chad").unwrap();
        let vex_idx = json[board_idx..].find("vex").unwrap();
        // 1 win each, vex has more kills (5 vs 5? chad 5, vex 5) -> tie, then matches.
        assert!(chad_idx != vex_idx);
    }

    #[test]
    fn stats_persist_to_disk() {
        let dir = std::env::temp_dir().join("ascii-royale-test-stats");
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("stats.json");
        {
            let mut s = ArenaState::new(Some(file.clone()));
            s.record_match(&[("zed".into(), Some(1), 7)], Some("zed".into()));
        }
        // Reload: tallies survive.
        let s2 = ArenaState::new(Some(file.clone()));
        assert_eq!(s2.stats.matches, 1);
        assert_eq!(s2.stats.players["zed"].wins, 1);
        assert_eq!(s2.stats.players["zed"].kills, 7);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn landing_page_has_play_button_only_when_url_given() {
        assert!(landing_html(Some("https://x")).contains("play in your browser"));
        assert!(!landing_html(None).contains("play in your browser"));
        assert!(landing_html(None).contains("ascii-royale play"));
    }
}
