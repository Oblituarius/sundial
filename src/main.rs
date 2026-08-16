#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod catalog;
mod class_items;
mod dummy_items;
mod export;
mod game_settings;
mod storage;

fn main() -> eframe::Result<()> {
    app::run()
}
