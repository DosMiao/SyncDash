//! syncdash 核心库：CLI（main.rs）与 Tauri 桌面壳（src-tauri）共用这一份逻辑。
//! GUI 不在库里——egui 旧界面属于 CLI bin，Web 新界面属于 src-tauri。
//!
//! # 分层
//!
//! 依赖**只向下**。往上引用是可以的，但要在模块头写明理由。
//!
//! ```text
//! L4 外壳      src/main.rs（CLI） · src-tauri（桌面）
//! L3 编排      config · run
//! L2 领域      filter · scan · compare · apply · pack · preflight · lock · remote · version · trash
//! L1 服务      progress · logging · runlog · settings
//! L0 地基      foundation（零 crate 内依赖） · table · chunk · atomic
//! ```
//!
//! `foundation` 是唯一保证零 crate 内依赖的层：谁都能用它，它谁都不用。
//! 重复实现（时间戳换算 ×3、字节格式化 ×3、rel→native ×8、比对键折叠 ×2 且语义不一致）
//! 都收敛到那里。

pub mod foundation;

pub mod apply;
pub mod atomic;
pub mod chunk;
pub mod compare;
pub mod config;
pub mod filter;
pub mod lock;
pub mod logging;
pub mod pack;
pub mod preflight;
pub mod progress;
pub mod remote;
pub mod run;
pub mod runlog;
pub mod scan;
pub mod settings;
pub mod table;
pub mod territory;
pub mod trash;
pub mod vclock;
pub mod version;
