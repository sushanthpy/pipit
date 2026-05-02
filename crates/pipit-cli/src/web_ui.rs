//! Web UI — interactive browser-based dashboard for pipit.
//!
//! Serves a single-page dashboard with real-time status polling.
//! Launch with `pipit web` and open http://127.0.0.1:9090 in a browser.
//!
//! Endpoints:
//!   GET /             — Interactive dashboard SPA
//!   GET /api/health   — Health check (JSON)
//!   GET /api/version  — Version info (JSON)
//!   GET /api/status   — Live session status (JSON)
//!   GET /api/config   — Current configuration (JSON)

use anyhow::Result;
use std::net::SocketAddr;

/// Configuration for the web UI server.
#[derive(Debug, Clone)]
pub struct WebUiConfig {
    /// Address to bind to (default: 127.0.0.1:9090).
    pub bind_addr: SocketAddr,
    /// Enable CORS for development.
    pub cors: bool,
}

impl Default for WebUiConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:9090".parse().unwrap(),
            cors: false,
        }
    }
}

/// Start the web UI HTTP server.
pub async fn start_web_ui(config: WebUiConfig) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind(config.bind_addr).await?;
    eprintln!(
        "\n  🐦 pipit web dashboard\n  → http://{}\n",
        config.bind_addr
    );

    // Try to open browser automatically
    let url = format!("http://{}", config.bind_addr);
    let _ = open_browser(&url);

    loop {
        let (mut stream, _addr) = listener.accept().await?;
        tokio::spawn(async move {
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            if n == 0 {
                return;
            }

            let request = String::from_utf8_lossy(&buf[..n]);
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("/");

            let (status, content_type, body) = route(path);

            let response = format!(
                "HTTP/1.1 {}\r\n\
                 Content-Type: {}; charset=utf-8\r\n\
                 Content-Length: {}\r\n\
                 Cache-Control: no-cache\r\n\
                 Connection: close\r\n\
                 \r\n\
                 {}",
                status,
                content_type,
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });
    }
}

fn route(path: &str) -> (&'static str, &'static str, String) {
    match path {
        "/api/health" => (
            "200 OK",
            "application/json",
            format!(
                r#"{{"status":"ok","version":"{}","uptime_secs":{}}}"#,
                env!("CARGO_PKG_VERSION"),
                0 // placeholder — wire to real uptime later
            ),
        ),
        "/api/version" => (
            "200 OK",
            "application/json",
            format!(
                r#"{{"name":"pipit","version":"{}","target":"{}","profile":"{}"}}"#,
                env!("CARGO_PKG_VERSION"),
                std::env::consts::ARCH,
                if cfg!(debug_assertions) {
                    "debug"
                } else {
                    "release"
                }
            ),
        ),
        "/api/status" => (
            "200 OK",
            "application/json",
            build_status_json(),
        ),
        "/api/config" => (
            "200 OK",
            "application/json",
            build_config_json(),
        ),
        "/favicon.ico" => ("204 No Content", "text/plain", String::new()),
        _ => ("200 OK", "text/html", DASHBOARD_HTML.to_string()),
    }
}

fn build_status_json() -> String {
    // Gather system info available without shared state
    let pid = std::process::id();
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    format!(
        r#"{{"pid":{},"cwd":"{}","version":"{}"}}"#,
        pid,
        cwd.replace('\\', "\\\\").replace('"', "\\\""),
        env!("CARGO_PKG_VERSION")
    )
}

fn build_config_json() -> String {
    // Read pipit config from standard locations
    let home = std::env::var("HOME").unwrap_or_default();
    let config_path = std::path::Path::new(&home).join(".config/pipit/config.toml");
    let config_content = std::fs::read_to_string(&config_path).unwrap_or_default();
    let model = std::env::var("PIPIT_MODEL").unwrap_or_else(|_| "default".to_string());
    let provider = std::env::var("PIPIT_PROVIDER").unwrap_or_else(|_| "auto".to_string());
    format!(
        r#"{{"model":"{}","provider":"{}","config_path":"{}","config_exists":{}}}"#,
        model.replace('"', "\\\""),
        provider.replace('"', "\\\""),
        config_path.display(),
        config_path.exists()
    )
}

fn open_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(url).spawn()?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open").arg(url).spawn()?;
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/c", "start", url])
            .spawn()?;
    }
    Ok(())
}

const DASHBOARD_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>pipit — Dashboard</title>
<style>
  :root {
    --bg: #0d1117; --bg2: #161b22; --bg3: #1c2128; --bg4: #21262d;
    --fg: #c9d1d9; --fg2: #8b949e; --fg3: #484f58;
    --cyan: #58a6ff; --green: #3fb950; --yellow: #d29922;
    --red: #f85149; --magenta: #bc8cff; --orange: #d18616;
    --border: #30363d; --radius: 8px;
  }
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body { font-family: 'SF Mono', 'Cascadia Code', 'Fira Code', monospace;
         background: var(--bg); color: var(--fg); min-height: 100vh; }

  /* ── Top bar ──────────────────────────────────────────── */
  .topbar { display: flex; align-items: center; gap: 12px;
            padding: 12px 20px; background: var(--bg2); border-bottom: 1px solid var(--border); }
  .topbar .logo { font-size: 1.3em; font-weight: 700; color: var(--cyan); }
  .topbar .version { color: var(--fg3); font-size: 0.85em; }
  .topbar .spacer { flex: 1; }
  .topbar .status-dot { width: 8px; height: 8px; border-radius: 50%;
                        background: var(--green); display: inline-block;
                        animation: pulse 2s infinite; }
  .topbar .status-label { color: var(--fg2); font-size: 0.85em; }
  @keyframes pulse { 0%,100% { opacity:1; } 50% { opacity:0.4; } }

  /* ── Layout ───────────────────────────────────────────── */
  .dashboard { display: grid; grid-template-columns: 1fr 1fr; gap: 16px;
               padding: 20px; max-width: 1200px; margin: 0 auto; }
  @media (max-width: 768px) { .dashboard { grid-template-columns: 1fr; } }

  /* ── Cards ────────────────────────────────────────────── */
  .card { background: var(--bg2); border: 1px solid var(--border);
          border-radius: var(--radius); overflow: hidden; }
  .card-header { padding: 12px 16px; border-bottom: 1px solid var(--border);
                 font-weight: 600; font-size: 0.9em; color: var(--cyan);
                 display: flex; align-items: center; gap: 8px; }
  .card-body { padding: 16px; }
  .card.full { grid-column: 1 / -1; }

  /* ── Key-value rows ───────────────────────────────────── */
  .kv { display: flex; justify-content: space-between; padding: 6px 0;
        border-bottom: 1px solid var(--bg3); }
  .kv:last-child { border-bottom: none; }
  .kv .k { color: var(--fg2); }
  .kv .v { color: var(--fg); font-weight: 500; }
  .kv .v.green { color: var(--green); }
  .kv .v.yellow { color: var(--yellow); }
  .kv .v.cyan { color: var(--cyan); }
  .kv .v.magenta { color: var(--magenta); }

  /* ── Hero stats ───────────────────────────────────────── */
  .hero-stats { display: grid; grid-template-columns: repeat(auto-fit, minmax(120px, 1fr));
                gap: 12px; padding: 16px; }
  .stat { text-align: center; padding: 16px 8px; background: var(--bg3);
          border-radius: var(--radius); }
  .stat .value { font-size: 1.8em; font-weight: 700; color: var(--cyan); }
  .stat .label { font-size: 0.75em; color: var(--fg3); text-transform: uppercase;
                 letter-spacing: 1px; margin-top: 4px; }

  /* ── Terminal preview ─────────────────────────────────── */
  .terminal { background: var(--bg); border: 1px solid var(--border);
              border-radius: var(--radius); padding: 16px; font-size: 0.85em;
              line-height: 1.6; min-height: 200px; max-height: 400px;
              overflow-y: auto; }
  .terminal .prompt { color: var(--green); }
  .terminal .cmd { color: var(--fg); }
  .terminal .output { color: var(--fg2); }
  .terminal .spinner { color: var(--cyan); animation: spin 1s linear infinite; display: inline-block; }
  @keyframes spin { 0% { content: '⠋'; } 10% { content: '⠙'; } 20% { content: '⠹'; }
                    30% { content: '⠸'; } 40% { content: '⠼'; } 50% { content: '⠴'; }
                    60% { content: '⠦'; } 70% { content: '⠧'; } 80% { content: '⠇'; }
                    90% { content: '⠏'; } }
  .terminal .cursor { display: inline-block; width: 8px; height: 1.1em;
                      background: var(--cyan); animation: blink 1s step-end infinite;
                      vertical-align: text-bottom; margin-left: 2px; }
  @keyframes blink { 50% { opacity: 0; } }

  /* ── Chat input ───────────────────────────────────────── */
  .chat-input-wrap { display: flex; gap: 8px; padding: 12px 16px;
                     border-top: 1px solid var(--border); background: var(--bg3); }
  .chat-input { flex: 1; background: var(--bg); border: 1px solid var(--border);
                border-radius: 6px; padding: 8px 12px; color: var(--fg);
                font-family: inherit; font-size: 0.9em; outline: none;
                transition: border-color 0.2s; }
  .chat-input:focus { border-color: var(--cyan); }
  .chat-input::placeholder { color: var(--fg3); }
  .chat-send { background: var(--cyan); color: var(--bg); border: none;
               border-radius: 6px; padding: 8px 16px; cursor: pointer;
               font-family: inherit; font-weight: 600; transition: opacity 0.2s; }
  .chat-send:hover { opacity: 0.85; }

  /* ── Activity feed ────────────────────────────────────── */
  .activity { list-style: none; max-height: 300px; overflow-y: auto; }
  .activity li { padding: 6px 0; border-bottom: 1px solid var(--bg3);
                 display: flex; gap: 8px; align-items: flex-start; }
  .activity li:last-child { border-bottom: none; }
  .activity .time { color: var(--fg3); font-size: 0.8em; min-width: 40px; }
  .activity .icon { font-size: 0.9em; }
  .activity .msg { color: var(--fg2); font-size: 0.9em; }

  /* ── Keyboard shortcuts ───────────────────────────────── */
  .shortcuts { display: grid; grid-template-columns: 1fr 1fr; gap: 4px 24px; }
  .shortcut { display: flex; gap: 8px; padding: 4px 0; }
  .shortcut kbd { background: var(--bg3); border: 1px solid var(--border);
                  border-radius: 4px; padding: 2px 6px; font-size: 0.8em;
                  color: var(--yellow); min-width: 24px; text-align: center; }
  .shortcut span { color: var(--fg2); font-size: 0.85em; }

  /* ── Footer ───────────────────────────────────────────── */
  .footer { text-align: center; padding: 16px; color: var(--fg3); font-size: 0.8em; }
  .footer a { color: var(--cyan); text-decoration: none; }
  .footer a:hover { text-decoration: underline; }

  /* ── Shimmer animation ────────────────────────────────── */
  .shimmer { background: linear-gradient(90deg, var(--bg3) 25%, var(--bg4) 50%, var(--bg3) 75%);
             background-size: 200% 100%; animation: shimmer 1.5s infinite;
             border-radius: 4px; height: 1em; }
  @keyframes shimmer { 0% { background-position: 200% 0; } 100% { background-position: -200% 0; } }
</style>
</head>
<body>

<!-- ═══ Top Bar ═══ -->
<div class="topbar">
  <span class="logo">🐦 pipit</span>
  <span class="version" id="version">v—</span>
  <span class="spacer"></span>
  <span class="status-dot" id="statusDot"></span>
  <span class="status-label" id="statusLabel">connecting…</span>
</div>

<!-- ═══ Dashboard Grid ═══ -->
<div class="dashboard">

  <!-- Hero Stats -->
  <div class="card full">
    <div class="hero-stats">
      <div class="stat"><div class="value" id="statVersion">—</div><div class="label">Version</div></div>
      <div class="stat"><div class="value" id="statArch">—</div><div class="label">Architecture</div></div>
      <div class="stat"><div class="value" id="statProfile">—</div><div class="label">Profile</div></div>
      <div class="stat"><div class="value" id="statPid">—</div><div class="label">PID</div></div>
      <div class="stat"><div class="value" id="statUptime">—</div><div class="label">Uptime</div></div>
    </div>
  </div>

  <!-- Configuration -->
  <div class="card">
    <div class="card-header">⚙ Configuration</div>
    <div class="card-body" id="configCard">
      <div class="shimmer" style="width:80%;margin-bottom:8px"></div>
      <div class="shimmer" style="width:60%;margin-bottom:8px"></div>
      <div class="shimmer" style="width:70%"></div>
    </div>
  </div>

  <!-- Quick Actions -->
  <div class="card">
    <div class="card-header">⚡ Quick Actions</div>
    <div class="card-body">
      <div class="shortcuts">
        <div class="shortcut"><kbd>Tab</kbd><span>Switch focus pane</span></div>
        <div class="shortcut"><kbd>S</kbd><span>Settings overlay</span></div>
        <div class="shortcut"><kbd>/</kbd><span>Search in pane</span></div>
        <div class="shortcut"><kbd>?</kbd><span>Help overlay</span></div>
        <div class="shortcut"><kbd>1-4</kbd><span>Switch tabs</span></div>
        <div class="shortcut"><kbd>j/k</kbd><span>Scroll up/down</span></div>
        <div class="shortcut"><kbd>Esc</kbd><span>Close overlay / stop</span></div>
        <div class="shortcut"><kbd>Ctrl-C</kbd><span>Quit pipit</span></div>
      </div>
    </div>
  </div>

  <!-- Interactive Terminal Preview -->
  <div class="card full">
    <div class="card-header">💻 Terminal</div>
    <div class="terminal" id="terminal">
      <div><span class="prompt">pipit❯</span> <span class="cmd">ready</span></div>
      <div class="output">Type a message below to preview the chat flow.</div>
    </div>
    <div class="chat-input-wrap">
      <input type="text" class="chat-input" id="chatInput"
             placeholder="Type a message… (local preview only)" autocomplete="off">
      <button class="chat-send" id="chatSend">Send</button>
    </div>
  </div>

  <!-- Activity Feed -->
  <div class="card">
    <div class="card-header">📋 Activity</div>
    <div class="card-body">
      <ul class="activity" id="activityFeed">
        <li><span class="time">now</span><span class="icon">🟢</span><span class="msg">Dashboard started</span></li>
      </ul>
    </div>
  </div>

  <!-- API Endpoints -->
  <div class="card">
    <div class="card-header">🔗 API Endpoints</div>
    <div class="card-body">
      <div class="kv"><span class="k">Health</span><a class="v cyan" href="/api/health">/api/health</a></div>
      <div class="kv"><span class="k">Version</span><a class="v cyan" href="/api/version">/api/version</a></div>
      <div class="kv"><span class="k">Status</span><a class="v cyan" href="/api/status">/api/status</a></div>
      <div class="kv"><span class="k">Config</span><a class="v cyan" href="/api/config">/api/config</a></div>
    </div>
  </div>

</div>

<div class="footer">
  pipit — AI coding agent · <a href="https://github.com/pipelight/pipit">GitHub</a>
</div>

<script>
// ── State ──────────────────────────────────────────────────────
const startTime = Date.now();
const SPINNERS = ['⠋','⠙','⠹','⠸','⠼','⠴','⠦','⠧','⠇','⠏'];
let spinFrame = 0;

// ── Polling ────────────────────────────────────────────────────
async function fetchJSON(url) {
  try { const r = await fetch(url); return await r.json(); }
  catch { return null; }
}

async function pollStatus() {
  const [ver, status, config] = await Promise.all([
    fetchJSON('/api/version'),
    fetchJSON('/api/status'),
    fetchJSON('/api/config'),
  ]);

  if (ver) {
    document.getElementById('version').textContent = 'v' + ver.version;
    document.getElementById('statVersion').textContent = ver.version;
    document.getElementById('statArch').textContent = ver.target || '—';
    document.getElementById('statProfile').textContent = ver.profile || '—';
    document.getElementById('statusDot').style.background = 'var(--green)';
    document.getElementById('statusLabel').textContent = 'connected';
  } else {
    document.getElementById('statusDot').style.background = 'var(--red)';
    document.getElementById('statusLabel').textContent = 'offline';
  }

  if (status) {
    document.getElementById('statPid').textContent = status.pid || '—';
  }

  // Uptime
  const secs = Math.floor((Date.now() - startTime) / 1000);
  const mins = Math.floor(secs / 60);
  document.getElementById('statUptime').textContent =
    mins > 0 ? mins + 'm' + (secs % 60) + 's' : secs + 's';

  if (config) {
    const cc = document.getElementById('configCard');
    cc.innerHTML = '';
    const fields = [
      ['Model', config.model, 'cyan'],
      ['Provider', config.provider, 'magenta'],
      ['Config Path', config.config_path, ''],
      ['Config Exists', config.config_exists ? 'yes' : 'no',
       config.config_exists ? 'green' : 'yellow'],
    ];
    fields.forEach(([k, v, cls]) => {
      const row = document.createElement('div');
      row.className = 'kv';
      row.innerHTML = `<span class="k">${k}</span><span class="v ${cls}">${v}</span>`;
      cc.appendChild(row);
    });
  }
}

// Poll every 3 seconds
pollStatus();
setInterval(pollStatus, 3000);

// ── Terminal Chat Preview ──────────────────────────────────────
const terminal = document.getElementById('terminal');
const chatInput = document.getElementById('chatInput');
const chatSend = document.getElementById('chatSend');

function addTermLine(html) {
  const div = document.createElement('div');
  div.innerHTML = html;
  terminal.appendChild(div);
  terminal.scrollTop = terminal.scrollHeight;
}

function addActivity(icon, msg) {
  const feed = document.getElementById('activityFeed');
  const secs = Math.floor((Date.now() - startTime) / 1000);
  const time = secs > 60 ? Math.floor(secs/60) + 'm' : secs + 's';
  const li = document.createElement('li');
  li.innerHTML = `<span class="time">${time}</span><span class="icon">${icon}</span><span class="msg">${msg}</span>`;
  feed.prepend(li);
  // Cap at 50 entries
  while (feed.children.length > 50) feed.lastChild.remove();
}

const RESPONSES = [
  "I'll analyze the codebase and implement that for you.",
  "Let me search for the relevant files first…",
  "Found 3 files that need changes. Starting implementation.",
  "I've made the changes. Want me to run the tests?",
  "All tests pass. The implementation looks good.",
  "I see a potential issue — let me fix that edge case.",
  "Done! The changes have been applied successfully.",
];
let respIdx = 0;

async function simulateResponse(userMsg) {
  // Show thinking spinner
  const thinkDiv = document.createElement('div');
  thinkDiv.id = 'thinking';
  thinkDiv.innerHTML = '<span class="spinner">⠋</span> <span style="color:var(--fg3)">thinking…</span>';
  terminal.appendChild(thinkDiv);
  terminal.scrollTop = terminal.scrollHeight;

  // Animate spinner
  let frame = 0;
  const spinInterval = setInterval(() => {
    frame = (frame + 1) % SPINNERS.length;
    const el = thinkDiv.querySelector('.spinner');
    if (el) el.textContent = SPINNERS[frame];
  }, 80);

  // Simulate delay
  await new Promise(r => setTimeout(r, 800 + Math.random() * 1200));

  clearInterval(spinInterval);
  thinkDiv.remove();

  // Type out response
  const resp = RESPONSES[respIdx % RESPONSES.length];
  respIdx++;
  const respDiv = document.createElement('div');
  respDiv.className = 'output';
  terminal.appendChild(respDiv);

  for (let i = 0; i <= resp.length; i++) {
    respDiv.innerHTML = resp.slice(0, i) + '<span class="cursor"></span>';
    terminal.scrollTop = terminal.scrollHeight;
    await new Promise(r => setTimeout(r, 15 + Math.random() * 25));
  }
  respDiv.innerHTML = resp;
  addActivity('✅', resp.length > 50 ? resp.slice(0,50) + '…' : resp);
}

function sendMessage() {
  const msg = chatInput.value.trim();
  if (!msg) return;
  chatInput.value = '';

  // User message
  addTermLine(`<span class="prompt">you❯</span> <span class="cmd">${escapeHtml(msg)}</span>`);
  addActivity('›', msg.length > 50 ? msg.slice(0,50) + '…' : msg);

  simulateResponse(msg);
}

chatSend.addEventListener('click', sendMessage);
chatInput.addEventListener('keydown', e => { if (e.key === 'Enter') sendMessage(); });

function escapeHtml(s) {
  return s.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');
}

// ── Keyboard shortcut ──────────────────────────────────────────
document.addEventListener('keydown', e => {
  if (e.key === '/' && document.activeElement !== chatInput) {
    e.preventDefault();
    chatInput.focus();
  }
});

// Focus input on load
chatInput.focus();
</script>
</body>
</html>"##;
