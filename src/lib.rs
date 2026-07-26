//! syncdash 核心库：CLI（main.rs）与 Tauri 桌面壳（src-tauri）共用这一份逻辑。
//! GUI 不在库里——egui 旧界面属于 CLI bin，Web 新界面属于 src-tauri。

pub mod apply;
pub mod atomic;
pub mod chunk;
pub mod compare;
pub mod config;
pub mod filter;
pub mod lock;
pub mod pack;
pub mod preflight;
pub mod progress;
pub mod remote;
pub mod run;
pub mod scan;
pub mod table;
pub mod territory;
pub mod trash;
pub mod vclock;
pub mod version;
