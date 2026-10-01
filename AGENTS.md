# AGENTS.md

**接手这个仓库先读 [`AGENT-HANDOFF.md`](AGENT-HANDOFF.md)** —— 怎么跑起来、现状实测数字、
还剩哪些活、我踩过的坑，都在那一份里。需要「为什么这么设计」时再翻 [`HANDOFF.md`](HANDOFF.md)；
架构与 28 步计划在画布 `music-server-architecture.excalidraw`。

## 四条不许破的

1. **生产路径（`src/`）不写 `unwrap` / `expect` / `panic!`**（测试里随便用）。
2. **`cargo` 一律套 `timeout`**（`timeout 600 cargo build`）。这是 ARM NAS，卡住很难看。
3. **加依赖前先问用户**。Cargo.toml 里每条依赖都写了引入理由，别绕过。
4. **中文**：注释、日志、界面文案、提交信息。

## 两条口径

- **证据 ≠ 声称**：没有变异验证过的「全绿」不算证据；手写 API 转写层必须对着**真服务**验
  （这个项目上已经错过 6 次）；文档里的数字要重测再写。
- **精简**：能删就删、少加抽象、少加依赖 —— 但不许为了短而简化掉安全、明确的需求、或理解本身。

细节（包括为什么这些规矩是这么来的）全在 `AGENT-HANDOFF.md`。
