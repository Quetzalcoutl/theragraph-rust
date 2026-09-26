//! Transport layer for NebulaGraph queries.
//!
//! A-01: extracted from graph_client.rs to separate transport concerns (connection
//! lifecycle, subprocess management, pool management) from graph semantics
//! (circuit-breaker, edge writes, FoF traversals).
//!
//! `GraphTransport` is the seam — swap in a mock to test `GraphClient` logic
//! without spawning real nebula-console processes.

use anyhow::{Context, Result};
use std::process::Stdio;
use std::sync::LazyLock;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::{Mutex, Semaphore};
use tracing::{debug, info, warn};

// Cap concurrent nebula-console processes — prevents fork storms under load.
// 8 is conservative for a single-machine PoC deployment.
pub(super) static NEBULA_SEMAPHORE: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(8));

// ── NebulaConfig ──────────────────────────────────────────────────────────────

/// Connection parameters shared by both Nebula transport implementations.
///
/// Single parse point — eliminates the 8 duplicate `std::env::var` reads
/// that previously appeared in `NebulaConsoleTransport::from_env` and
/// `NebulaPoolTransport::from_env`.
#[derive(Clone, Debug)]
pub(crate) struct NebulaConfig {
    pub host:     String,
    pub port:     String,
    pub user:     String,
    pub password: String,
}

impl NebulaConfig {
    pub fn from_env() -> Self {
        Self {
            host:     std::env::var("NEBULA_HOST").unwrap_or_else(|_| "graphd".to_owned()),
            port:     std::env::var("NEBULA_PORT").unwrap_or_else(|_| "9669".to_owned()),
            user:     std::env::var("NEBULA_USER").unwrap_or_else(|_| "root".to_owned()),
            password: std::env::var("NEBULA_PASSWORD").unwrap_or_else(|_| "nebula".to_owned()),
        }
    }
}

// ── GraphTransport trait ──────────────────────────────────────────────────────

/// Execute a raw nGQL string and return stdout.
///
/// `NebulaConsoleTransport` is the production impl (spawns a subprocess).
/// A second adapter can be a mock or a future HTTP client — that makes this
/// a real seam rather than a hypothetical one.
///
/// RS-11: `GraphTransport` is only used as a generic bound — never as
/// `dyn GraphTransport` — so it does not need object safety. We use the
/// RPITIT form with an explicit `+ Send` bound (without lifetime parameters)
/// which is cleaner than the original `fn execute<'a>(&'a self, query: &'a str)`
/// while still satisfying `async_trait`'s `Send` requirement on the
/// `GraphTraversal` impl block.
pub trait GraphTransport: Send + Sync {
    fn execute(&self, query: &str) -> impl std::future::Future<Output = Result<String>> + Send;
}

// ── NebulaConsoleTransport ────────────────────────────────────────────────────

/// Production transport: spawns `nebula-console` via stdin piping.
///
/// Bounded by `NEBULA_SEMAPHORE` to prevent fork storms.
pub struct NebulaConsoleTransport {
    config: NebulaConfig,
}

impl NebulaConsoleTransport {
    pub fn from_env() -> Self {
        Self { config: NebulaConfig::from_env() }
    }
}

impl GraphTransport for NebulaConsoleTransport {
    // RS-11: RPITIT without lifetime parameters — shorter than the original
    // `fn execute<'a>(&'a self, query: &'a str) -> impl Future... + Send + 'a`.
    fn execute(&self, query: &str) -> impl std::future::Future<Output = Result<String>> + Send {
        let config = self.config.clone();
        let query  = query.to_owned();
        async move {
            // TAG-S28-05: unbounded acquire() can queue tasks indefinitely when all
            // 8 nebula-console slots are busy. 5s timeout converts queue-pile-up into
            // fast transient errors so callers can degrade gracefully and retry later.
            let _permit = tokio::time::timeout(
                Duration::from_secs(5),
                NEBULA_SEMAPHORE.acquire(),
            )
            .await
            .context("Nebula semaphore acquire timed out after 5s")?
            .context("Nebula semaphore closed")?;

            let mut child = Command::new("nebula-console")
                .args(["-addr", &config.host, "-port", &config.port,
                       "-u",    &config.user, "-p",    &config.password])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)   // SIGKILL on drop — prevents zombie accumulation on timeout
                .spawn()
                .context("Failed to spawn nebula-console — is it on PATH?")?;

            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(query.as_bytes()).await.context("Failed to write to nebula-console stdin")?;
                stdin.shutdown().await.context("Failed to close stdin")?;
            }

            // Guard against a hung Nebula process exhausting the semaphore pool.
            const NEBULA_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
            let output = tokio::time::timeout(NEBULA_TIMEOUT, child.wait_with_output())
                .await
                .context("nebula-console query timed out (30s)")?
                .context("nebula-console did not exit cleanly")?;

            if !output.status.success() {
                anyhow::bail!(
                    "nebula-console exited {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }

            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        }
    }
}

// ── NebulaPoolTransport ───────────────────────────────────────────────────────

/// A single persistent nebula-console process kept alive in interactive mode.
///
/// Serialises query execution — only one query can run at a time per session.
/// The pool maintains N sessions so N queries can run concurrently.
struct NebulaSession {
    stdin:  tokio::process::ChildStdin,
    reader: tokio::io::BufReader<tokio::process::ChildStdout>,
    _child: tokio::process::Child,
}

/// Read one line from `reader`, OR — if nebula-console's interactive prompt
/// appears before a newline does — return the prompt text itself.
///
/// nebula-console's prompt (`"(root@nebula) [space]> "`) is written to stdout
/// WITHOUT a trailing newline: it's designed for a real TTY, where the '\n'
/// would come from the terminal echoing the user's keystroke, not from the
/// program. When piped (no TTY), that newline never arrives — the process is
/// now blocked waiting on stdin. A plain `AsyncBufReadExt::read_line` here
/// blocks forever waiting for a '\n' the prompt will never emit, which is
/// why every NebulaPoolTransport connection attempt hung for the full 30s
/// timeout and the pool never actually connected (always silently falling
/// back to the per-query subprocess transport). Reading byte-by-byte and
/// checking for the prompt suffix after each byte sidesteps this.
async fn read_line_or_prompt(
    reader: &mut tokio::io::BufReader<tokio::process::ChildStdout>,
) -> Result<String> {
    use tokio::io::AsyncReadExt as _;

    let mut acc = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    loop {
        let n = reader.read(&mut byte).await.context("nebula-console stdout read failed")?;
        if n == 0 {
            anyhow::bail!("nebula-console stdout closed (EOF)");
        }
        acc.push(byte[0]);
        if byte[0] == b'\n' {
            break;
        }
        if acc.ends_with(b"> ") {
            let s = String::from_utf8_lossy(&acc);
            if s.trim_start().starts_with("(root@nebula)") {
                break;
            }
        }
    }
    Ok(String::from_utf8_lossy(&acc).into_owned())
}

impl NebulaSession {
    /// Spawn a nebula-console in interactive mode, wait for the initial prompt,
    /// then switch to the theragraph space so all subsequent queries land there.
    async fn connect(host: &str, port: &str, user: &str, password: &str) -> Result<Self> {
        let mut child = Command::new("nebula-console")
            .args(["-addr", host, "-port", port, "-u", user, "-p", password])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("Failed to spawn nebula-console for pool session")?;

        let stdin  = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let mut session = NebulaSession {
            stdin,
            reader: tokio::io::BufReader::new(stdout),
            _child: child,
        };

        // Drain the startup banner + initial prompt line.
        session.drain_until_prompt().await
            .context("nebula-console startup did not emit a prompt")?;
        Ok(session)
    }

    /// Read lines from stdout until we see a line beginning with `(root@nebula)`
    /// (the interactive prompt). Returns the accumulated output *before* the prompt.
    async fn drain_until_prompt(&mut self) -> Result<String> {
        let mut output = String::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                anyhow::bail!("nebula-console prompt wait exceeded 30s total");
            }
            let line = tokio::time::timeout(remaining, read_line_or_prompt(&mut self.reader))
                .await
                .context("timeout waiting for nebula-console prompt")??;
            if line.trim_start().starts_with("(root@nebula)") {
                break;
            }
            output.push_str(&line);
        }
        Ok(output)
    }

    /// Execute an nGQL string (may contain multiple `;`-separated statements).
    ///
    /// Protocol:
    ///  1. Write the query to stdin (ensure trailing newline).
    ///  2. Count expected "time spent" completions = number of `;` in the query.
    ///  3. Read lines until that many completions seen, then drain to the prompt.
    ///  4. Return accumulated output minus prompt lines.
    ///
    /// Error detection: if any line contains `[ERROR` or `SyntaxError`, we surface
    /// it as an `Err` after draining to the next prompt so the session stays usable.
    async fn execute(&mut self, query: &str) -> Result<String> {
        use tokio::io::AsyncWriteExt as _;

        let stmt_count = query.chars().filter(|&c| c == ';').count().max(1);

        self.stdin.write_all(query.as_bytes()).await.context("stdin write")?;
        if !query.ends_with('\n') {
            self.stdin.write_all(b"\n").await.context("stdin newline")?;
        }
        self.stdin.flush().await.context("stdin flush")?;

        let mut output       = String::new();
        let mut completions  = 0usize;
        let mut error_line: Option<String> = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                anyhow::bail!("Nebula pool query exceeded 30s total");
            }
            let line = tokio::time::timeout(remaining, read_line_or_prompt(&mut self.reader))
                .await
                .context("Nebula pool query timed out")??;

            // Prompt line — response is complete.
            if line.trim_start().starts_with("(root@nebula)") {
                break;
            }

            // Collect error lines so we can surface them after draining.
            if error_line.is_none()
                && (line.contains("[ERROR") || line.contains("SyntaxError"))
            {
                error_line = Some(line.trim().to_owned());
            }

            output.push_str(&line);

            if line.contains("(time spent") {
                completions += 1;
                if completions >= stmt_count {
                    // All statements done; drain remaining output until the prompt.
                    loop {
                        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                        if remaining.is_zero() {
                            anyhow::bail!("Nebula pool prompt drain exceeded 30s total");
                        }
                        let rest = tokio::time::timeout(remaining, read_line_or_prompt(&mut self.reader))
                            .await
                            .context("Nebula pool prompt drain timed out")??;
                        if rest.trim_start().starts_with("(root@nebula)") {
                            break;
                        }
                        output.push_str(&rest);
                    }
                    break;
                }
            }
        }

        if let Some(e) = error_line {
            anyhow::bail!("Nebula query error: {}", e);
        }
        Ok(output)
    }
}

/// Pool of persistent nebula-console sessions.
///
/// Replaces the per-query `NEBULA_SEMAPHORE` + subprocess spawn. Each session
/// stays alive and handles many queries over its lifetime: fork cost (20-80 ms)
/// is paid once at pool warm-up, not per query.
///
/// Design: `Semaphore(0)` tracks available sessions; `Mutex<VecDeque>` holds the
/// actual sessions. The separation is the key:
///
///   - `available.acquire()` is the ONLY blocking point — many callers wait here
///     concurrently without serializing. Tokio wakes the next waiter the moment
///     a permit is released.
///   - The Mutex is held only for the microsecond push/pop on `idle` — never
///     across a blocking wait. This is strictly faster than the previous
///     `Mutex<Receiver>` design where the mutex was held for the entire
///     `recv()` duration (up to 5 s when all sessions were busy), forcing all
///     other callers to queue behind one lock.
///
/// Invariant: #permits == #sessions in `idle` at all times.
///
/// `NEBULA_POOL_SIZE` env var controls the number of persistent sessions
/// (default 8). Increase in production.
pub struct NebulaPoolTransport {
    /// Idle sessions. Mutex held only for push/pop — microseconds.
    idle:      Arc<Mutex<std::collections::VecDeque<NebulaSession>>>,
    /// One permit per idle session. Multiple callers wait here concurrently.
    available: Arc<Semaphore>,
    config:    NebulaConfig,
    #[allow(dead_code)]
    pool_size: usize,
}

impl NebulaPoolTransport {
    /// Build from environment, pre-warming all pool slots.
    ///
    /// Gracefully degrades — if a slot fails to connect, a warning is emitted
    /// and the pool starts with fewer sessions (down to 1).
    pub async fn from_env() -> Result<Self> {
        let config = NebulaConfig::from_env();
        let pool_size: usize = std::env::var("NEBULA_POOL_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8);

        let idle      = Arc::new(Mutex::new(std::collections::VecDeque::with_capacity(pool_size)));
        let available = Arc::new(Semaphore::new(0));

        let mut connected = 0usize;
        for i in 0..pool_size {
            match NebulaSession::connect(&config.host, &config.port, &config.user, &config.password).await {
                Ok(s)  => {
                    idle.lock().await.push_back(s);
                    connected += 1;
                }
                Err(e) => warn!("NebulaPoolTransport: slot {}/{} failed: {}", i + 1, pool_size, e),
            }
        }

        if connected == 0 {
            anyhow::bail!(
                "NebulaPoolTransport: could not open any session to {}:{}", config.host, config.port
            );
        }

        // Release one permit per successfully connected session.
        available.add_permits(connected);
        info!("NebulaPoolTransport: {} of {} pool slots ready", connected, pool_size);

        Ok(Self { idle, available, config, pool_size })
    }
}

impl GraphTransport for NebulaPoolTransport {
    fn execute(&self, query: &str) -> impl std::future::Future<Output = Result<String>> + Send {
        let idle      = Arc::clone(&self.idle);
        let available = Arc::clone(&self.available);
        let config    = self.config.clone();
        let query     = query.to_owned();
        async move {
            // Wait for a free session. Many callers can await here simultaneously —
            // no mutex held during the wait. Tokio wakes the next waiter as soon as
            // `add_permits(1)` is called on session return.
            let permit = tokio::time::timeout(
                Duration::from_secs(5),
                available.acquire(),
            )
            .await
            .context("NebulaPoolTransport: checkout timed out after 5s")?
            .context("NebulaPoolTransport: semaphore closed")?;
            // Forget the permit — we manage the count manually so it only goes back
            // up when we explicitly call add_permits(1) on return.
            permit.forget();

            // Pop the session. Mutex held for microseconds — always non-empty here
            // because we hold a consumed permit (one permit per idle session invariant).
            let mut session = idle.lock().await
                .pop_front()
                .context("NebulaPoolTransport: idle queue empty despite permit (invariant violated)")?;

            let result = session.execute(&query).await;

            match result {
                Ok(output) => {
                    idle.lock().await.push_back(session);
                    available.add_permits(1);
                    Ok(output)
                }
                Err(e) => {
                    warn!("NebulaPoolTransport: session discarded after error: {}", e);
                    // Spawn replacement to keep pool at capacity. add_permits(1) only
                    // after the replacement is ready — callers stay blocked until
                    // a real session is available, preventing spurious checkout attempts.
                    let (idle2, avail2) = (Arc::clone(&idle), Arc::clone(&available));
                    let cfg2 = config.clone();
                    tokio::spawn(async move {
                        match NebulaSession::connect(&cfg2.host, &cfg2.port, &cfg2.user, &cfg2.password).await {
                            Ok(fresh) => {
                                idle2.lock().await.push_back(fresh);
                                avail2.add_permits(1);
                            }
                            Err(e) => {
                                warn!("NebulaPoolTransport: replacement failed: {e}");
                                // Restore the permit so the pool stays at capacity.
                                // Without this, each replacement failure permanently
                                // shrinks the pool by one slot — a death spiral under
                                // sustained Nebula unavailability.
                                avail2.add_permits(1);
                            }
                        }
                    });
                    Err(e)
                }
            }
        }
    }
}

// ── Output parser ─────────────────────────────────────────────────────────────

/// Parse a NebulaGraph console table, extracting `(key, score)` pairs.
///
/// Column indices are 1-based within the pipe-delimited rows (column 0 is the
/// leading empty cell after the opening `|`).
///
/// Handles `__NULL__` scores produced by aggregate functions (e.g.
/// `max(l.liked_at)` on a node with no likes) — these emit score `0.0` so
/// callers can apply their own re-ranking instead of silently dropping rows.

// ── Output parsing ────────────────────────────────────────────────────────────

pub(crate) fn parse_nebula_table(output: &str, col_a: usize, col_b: usize) -> Vec<(String, f64)> {
    // Each data row uses ~80 bytes in Nebula's ASCII table format. Pre-size to avoid
    // the 4–5 doubling reallocations that occur on the default empty-Vec growth path.
    let mut results = Vec::with_capacity((output.len() / 80).max(8));
    for line in output.lines() {
        let line = line.trim();
        if line.starts_with('+') || line.is_empty() {
            continue;
        }
        // Avoid materializing a Vec per row — use nth() to index the split iterator directly.
        let mut parts = line.split('|').map(str::trim);
        // Skip the leading empty cell (opening '|') by consuming from index 0.
        // col_a and col_b are 1-based positions in the pipe-split parts.
        let key = match parts.nth(col_a) {
            Some(k) => k.trim_matches('"'),
            None => continue,
        };
        // Skip header rows and border artefacts. Header cells contain spaces
        // (e.g. "post_id") while VID keys never do.
        if key.is_empty() || key.contains(' ') {
            continue;
        }
        // col_b is relative to col_a; after nth(col_a) consumed col_a+1 items,
        // we need col_b - col_a - 1 more skips to reach col_b.
        let raw_score = if col_b > col_a {
            match parts.nth(col_b - col_a - 1) {
                Some(s) => s,
                None => continue,
            }
        } else {
            continue
        };
        if raw_score == "__NULL__" || raw_score.eq_ignore_ascii_case("null") {
            results.push((key.to_owned(), 0.0));
        } else if let Ok(score) = raw_score.parse::<f64>() {
            results.push((key.to_owned(), score));
        } else {
            debug!("parse_nebula_table: skipping unparseable score {:?} for key {:?}", raw_score, key);
        }
    }
    results
}

#[cfg(test)]
mod tests;
