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

/// Must run before any rusqlite Connection is opened, in any thread.
pub fn register_sqlite_vec() {
    use rusqlite::ffi::{sqlite3, sqlite3_api_routines};
    type ExtInit = unsafe extern "C" fn(*mut sqlite3, *mut *mut i8, *const sqlite3_api_routines) -> i32;

    SQLITE_VEC_INIT.call_once(|| {
        // Safety: sqlite_vec::sqlite3_vec_init has the SQLite extension ABI but
        // not the Rust type rusqlite expects — transmute through *const () to
        // bridge that.
        unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<*const (), ExtInit>(
                sqlite_vec::sqlite3_vec_init as *const (),
            )));
        }
    });
}

pub fn init_tracing() {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("researchwiki=info")))
        .with(fmt::layer())
        .init();
}
