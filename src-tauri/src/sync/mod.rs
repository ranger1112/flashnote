//! 云同步：条目级三方合并（协议见 `docs/sync-protocol.md`）。
//!
//! 这里是同步能力的地基，只有纯逻辑、不依赖网络；状态与引擎在后续变更中加入。

// 合并逻辑在引擎接入前只有测试会调用它。
#![allow(dead_code)]

pub(crate) mod merge;
