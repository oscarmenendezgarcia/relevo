//! Relevo: a small server that keeps and delivers notes between the people of one team, and the
//! clients (`relevo connect`, `relevo mcp`) that agents use to reach it.

pub mod api;
pub mod auth;
pub mod backup;
pub mod client;
pub mod config;
pub mod context;
pub mod db;
pub mod dist;
pub mod error;
pub mod mcp;
pub mod render;
pub mod serve;
pub mod session;
pub mod tls;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::Connection;
use tokio::sync::watch;

/// Waiting `inbox` requests one token may hold at once.
pub const MAX_WAITS_PER_TOKEN: usize = 10;

/// Shared state of the HTTP server.
pub struct App {
    db: Mutex<Connection>,
    pub clock: Clock,
    pub team_questions_per_day: u32,
    /// The clients this server hands out (`crate::dist`), if any.
    pub dist: Option<PathBuf>,
    /// One channel per person, bumped when something may have become new for one of their
    /// sessions, so that their waiting `inbox` requests look again.
    news: Mutex<HashMap<i64, watch::Sender<()>>>,
    /// Waiting `inbox` requests, per token id.
    waits: Mutex<HashMap<String, usize>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl App {
    pub fn new(conn: Connection, clock: Clock, team_questions_per_day: u32) -> Arc<App> {
        App::with_dist(conn, clock, team_questions_per_day, None)
    }

    /// The same, handing out the dist in `dist` (`GET /api/dist/...`).
    pub fn with_dist(
        conn: Connection,
        clock: Clock,
        team_questions_per_day: u32,
        dist: Option<PathBuf>,
    ) -> Arc<App> {
        Arc::new(App {
            db: Mutex::new(conn),
            clock,
            team_questions_per_day,
            dist,
            news: Mutex::new(HashMap::new()),
            waits: Mutex::new(HashMap::new()),
        })
    }

    /// Tells about every bump of `person`'s channel from now on.
    pub fn news(&self, person: i64) -> watch::Receiver<()> {
        lock(&self.news)
            .entry(person)
            .or_insert_with(|| watch::channel(()).0)
            .subscribe()
    }

    /// Wakes the waiting `inbox` requests of these people. A woken request looks again under the
    /// database lock, so calling this before a commit, with the lock held, is as good as after.
    pub fn announce(&self, people: impl IntoIterator<Item = i64>) {
        let news = lock(&self.news);
        for person in people {
            if let Some(tx) = news.get(&person) {
                tx.send_modify(|_| {});
            }
        }
    }

    /// A place for one more waiting `inbox` request of this token, or `None` when it holds
    /// `MAX_WAITS_PER_TOKEN` already. The place is freed when the slot is dropped, which also
    /// happens when the client goes away mid-wait.
    pub fn wait_slot(self: &Arc<Self>, token: &str) -> Option<WaitSlot> {
        let mut waits = lock(&self.waits);
        let held = waits.entry(token.to_string()).or_default();
        if *held >= MAX_WAITS_PER_TOKEN {
            return None;
        }
        *held += 1;
        Some(WaitSlot {
            app: self.clone(),
            token: token.to_string(),
        })
    }

    /// How many `inbox` requests are waiting now, over all tokens.
    pub fn waiting(&self) -> usize {
        lock(&self.waits).values().sum()
    }

    /// The single connection. A panic elsewhere must not lock everyone out, so poison is ignored:
    /// every write runs in a transaction, which a panic rolls back.
    pub fn db(&self) -> MutexGuard<'_, Connection> {
        lock(&self.db)
    }
}

/// One waiting `inbox` request's place in its token's quota.
pub struct WaitSlot {
    app: Arc<App>,
    token: String,
}

impl Drop for WaitSlot {
    fn drop(&mut self) {
        let mut waits = lock(&self.app.waits);
        if let Some(held) = waits.get_mut(&self.token) {
            *held -= 1;
            if *held == 0 {
                waits.remove(&self.token);
            }
        }
    }
}

/// "Now" in unix seconds: the system clock, or a fixed time that tests move by hand.
pub struct Clock(Mutex<Option<i64>>);

impl Clock {
    pub fn system() -> Self {
        Clock(Mutex::new(None))
    }

    pub fn fixed(unix: i64) -> Self {
        Clock(Mutex::new(Some(unix)))
    }

    pub fn now(&self) -> i64 {
        let fixed = *self.0.lock().unwrap_or_else(|e| e.into_inner());
        fixed.unwrap_or_else(db::now_unix)
    }

    /// Moves a fixed clock forward (a system clock becomes fixed at now + `secs`).
    pub fn advance(&self, secs: i64) {
        let mut fixed = self.0.lock().unwrap_or_else(|e| e.into_inner());
        *fixed = Some(fixed.unwrap_or_else(db::now_unix) + secs);
    }
}
