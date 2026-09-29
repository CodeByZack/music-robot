//! S18 / S19 / S20 · 音频子系统。
//!
//! 画布把「音频」相关的实现集中放在 `src/audio/` 下，这里按步骤分子模块：
//!
//! * [`stream`]（S18 · Range 流式）—— 流式播放需要的 HTTP Range 语义：
//!   * [`stream::parse_range`] —— 纯函数，把请求头 `Range` 解析成闭区间
//!     [`stream::ByteRange`]，便于单测穷举；
//!   * [`stream::content_type_for`] —— 按文件扩展名给出音频 MIME。
//! * [`cover`]（S20 · 封面）—— 封面挑选 / MIME 判定 / 上限检查的纯逻辑。
//! * [`transcode`]（S19 · 转码缓存）—— 缓存键 / ffmpeg 参数这些纯逻辑，加上
//!   「调 ffmpeg 子进程 + 写缓存 + 原子落盘」的 IO：转码本身就是子进程 + 文件系统
//!   两件事，不像前两个模块那样能只留纯函数。它的取舍（缓存键、失效方式、
//!   并发、超时、stderr 不外泄）都写在模块文档里。
//!
//! 真正的响应组装、存储读取与状态码映射在 `crate::server::routes::stream` 与
//! `crate::server::routes::cover`：那是 HTTP 层的事（鉴权提取器、ApiError 统一形状、
//! axum 响应头），不该塞进音频模块。

pub mod stream;

pub mod cover;

pub mod transcode;
