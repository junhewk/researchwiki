pub mod app;
pub mod config;
pub mod db;
pub mod error;
pub mod models;
pub mod runtime;
pub mod services;
pub mod state;
pub mod ui;

use std::sync::Once;

use tracing_subscriber::{EnvFilter, fmt, prelude::*};

static SQLITE_VEC_INIT: Once = Once::new();

/// Register the sqlite-vec extension as a SQLite auto-extension.
///
/// Must be called before any rusqlite Connection is opened — anywhere in the
/// process. Safe to call multiple times; only the first call takes effect.
pub fn register_sqlite_vec() {
    SQLITE_VEC_INIT.call_once(|| {
        // Safety: sqlite3_auto_extension takes an extension entry point with
        // the documented SQLite ABI. sqlite_vec::sqlite3_vec_init matches
        // that ABI but its Rust type doesn't, so we transmute through a
        // *const () to dodge the signature mismatch. Sets process-global
        // state and must run before any DB connection is opened in any
        // thread, including tokio workers.
        unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(
                sqlite_vec::sqlite3_vec_init as *const (),
            )));
        }
    });
}

/// Install a tracing subscriber appropriate for the desktop app.
///
/// Honors `RUST_LOG` if set; otherwise defaults to `researchwiki=info`.
pub fn init_tracing() {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("researchwiki=info")))
        .with(fmt::layer())
        .init();
}
