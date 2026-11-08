//! Relevo: a small server that keeps and delivers notes between the people of one team.

pub mod api;
pub mod auth;
pub mod db;
pub mod error;

use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::Connection;

/// Shared state of the HTTP server.
pub struct App {
    db: Mutex<Connection>,
    pub clock: Clock,
    pub team_questions_per_day: u32,
}

impl App {
    pub fn new(conn: Connection, clock: Clock, team_questions_per_day: u32) -> Arc<App> {
        Arc::new(App {
            db: Mutex::new(conn),
            clock,
            team_questions_per_day,
        })
    }

    /// The single connection. A panic elsewhere must not lock everyone out, so poison is ignored:
    /// every write runs in a transaction, which a panic rolls back.
    pub fn db(&self) -> MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|e| e.into_inner())
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
