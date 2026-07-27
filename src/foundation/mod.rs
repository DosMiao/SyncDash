//! L0 地基层：零 crate 内依赖，只用 std 与第三方 crate。
//!
//! 契约：谁都能依赖它，它谁都不依赖。任何 `use crate::…` 都破坏分层，评审直接打回。
//! 不做 re-export 桶——调用方写 `foundation::fmt::human_bytes`，桶会抹掉依赖关系。

pub mod fmt;
pub mod names;
pub mod path;
pub mod text;
pub mod time;
