//! SyncDash desktop binary entrypoint.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod contracts;
mod features;
mod ipc;
mod secure_random;
mod window;

fn main() {
    app::run();
}
