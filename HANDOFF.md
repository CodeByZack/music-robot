# HANDOFF.md — 接手这个项目的指南

面向**新的 agent 会话 / 新接手的人**。读完这一份就能动手，不需要重读全部历史。

---

## 0. 一句话现状

TS 音乐标签清洗工具（`/vol1/@appshare/dsh/data/tagwash-test`）的 Rust 重写。**CLI 部分已完成**
（6 个子命令、120 用例全绿、0 警告），下一步是**服务端**（axum + tokio）。
TS 已被决定抛弃，不再需要同步修改。

---

## 1. 环境（先读这个，否则会卡在第一步）

**Rust 不是系统安装的**，装在 `/vol1/@appshare/dsh/rust-test/` 下，需要 3 个环境变量。
`/usr/bin/rustc` 是 apt 装的 1.63，太旧用不了。

```bash
cd /vol1/@appshare/dsh/data/music-tag
source env.sh          # 已提供，含 RUSTUP_HOME / CARGO_HOME / PATH
cargo --version        # 应输出 cargo 1.98.1
```

或者直接跑编译好的二进制（**不需要 cargo**）：

```bash
B=./target/release/music-tag
$B -h
```

### 构建/测试耗时（ARM NAS，性能有限）

| 操作 | 耗时 |
|---|---|
| 冷编译 release | ~75 秒 |
| 全量 `cargo test` | ~90 秒 |
| 单个测试文件 | 1–15 秒 |

⚠️ **本机约定（AGENTS.md）**：NAS 资源有限，**不要随意全量 build / 全量 test**。
优先 `cargo test --test <文件名>` 只跑改动的部分，验证完再全量跑一次。

#### ⚠️ 移动项目目录后必须强制重建

测试里大量使用 `env!("CARGO_MANIFEST_DIR")`，它是**编译期常量**。
`mv` 会**保留文件 mtime**，cargo 的指纹因此不变 → 复用旧二进制 →
测试拿着旧路径去找 fixture，报 `No such file or directory`。

```bash
touch tests/*.rs src/lib.rs   # 骗过 cargo 指纹，触发重建
cargo test
```

（2026-09 从 `rust-test/` 搬到 `data/` 时踩的坑，17 个用例假红。）

---

## 2. 目录结构

```
music-tag/
├── Cargo.toml          # 依赖只有 3 个平台库：encoding_rs / sha2 / serde_json
├── env.sh              # source 一下就有 cargo
├── src/
│   ├── main.rs         # 唯一可执行入口，只做 stdout/stderr 桥接
│   ├── lib.rs          # 库根：tag / cli / fs / scanner / logger
│   ├── tag/
│   │   ├── read/       # probe 探测 → id3v2 / id3v1 / apev2 / flac / wav
│   │   │               #        + gbk_sniff 乱码检测 + warnings 告警
│   │   │               #        + metadata 统一结构 + native_probe(ffmpeg)
│   │   └── write/      # intent 意图层 → mp3/flac/wav writer + atomic 原子替换
│   ├── cli/            # read/write/blank/scan/doctor/wash + args/io
│   ├── fs.rs           # PathSandbox 路径沙箱（Rust 独有，服务端必备）
│   ├── scanner.rs      # 递归枚举 + ok/warn/rejected/broken 四分级
│   └── logger.rs       # 事件流 + NDJSON sink
├── tests/              # 12 个文件，120 用例
├── fixtures/           # 9 个样本：6 音乐 + 3 小 WAV，65M（不入库，见 MIGRATION.md）
├── MIGRATION.md        # 移植记录 + 已抓到的 bug + 忠实移植边界
└── HANDOFF.md          # ← 本文件
```

### 核心设计：库层是纯函数

```rust
pub fn run(argv: &[String], io: &dyn CommandIO) -> i32   // 返回退出码，不 exit()
pub trait CommandIO { fn log(&self, m: &str); fn error(&self, m: &str); }
```

**所有命令都是「返回退出码的纯函数」**，不 spawn 子进程、不写死 stdout。
好处：① 测试直接调函数断言退出码（比 spawn 快得多）② 服务端能直接 import 复用。
`main.rs` 只做桥接 + `std::process::exit`。

退出码约定：`0` 成功 / `1` 运行期失败 / `2` 用法错误。
`wash`：`0` 全部 applied / `1` 有 failed。

---

## 3. 已完成 vs 未完成

### ✅ 已完成

| 模块 | 说明 |
|---|---|
| CLI 6 个子命令 | read / write / blank / scan / doctor / wash |
| 读侧 | id3v2(含 USLT/APEv2) / id3v1 / flac(vorbis comment) / wav(INFO) / probe |
| 写侧 | mp3(局部编辑保留未知帧) / flac / wav / 原子替换 + 音频 hash 校验 |
| 意图层 | `merge_fields` / `diff_fields` / `preview_view` / `sniff_image_mime` |
| 告警检测 | 广告词 / GBK 乱码 / ID3v1 垃圾尾 / 非白名单帧 |
| 路径沙箱 | `PathSandbox`，10 对抗用例（**Rust 独有，TS 没做**） |
| 事件流 | `WashEvent` + `console_sink` / `ndjson_sink` |
| 参数解析 | 零依赖手写（**没有引 clap**） |

**技术约束（用户明确要求）**：
- **不用 lofty / id3 等第三方库做标签读写**，全部自己实现
- 平台能力库可用：`encoding_rs`（GB18030 编码）、`sha2`（完整性 hash）、`serde_json`（JSON 输出）
- 不引 clap（参数解析自己写，见 `src/cli/args.rs`）

### ❌ 未完成

| 项目 | 状态 |
|---|---|
| **服务端（axum + tokio）** | 完全没开始，这是主要下一步 |
| `config.ts` 移植 | **用户明确说暂时不做** |
| TS 侧同步修 bug | **TS 要抛弃，不用管了** |

---

## 4. 与 TS 的解耦状态

**已解耦完毕**，TS 可以随时删除，本仓库不受影响：

- ✅ `tests/differential.rs` / `tests/diff_write.rs`（7 个会 shell 出去调 TS 的差分测试）已删除。
  它们的使命已完成——验证 Rust 移植与参照实现一致，历史结论见 `MIGRATION.md`。
- ✅ `tsfixtures/` 目录已删除，3 个小 WAV 已并入 `fixtures/`。
  （WAV 本身是测试素材不是 TS 代码，只是名字当初跟着软链叫的，改名而非删除。）
- ✅ `fixtures/` 是真拷贝，不依赖 TS 目录存在。
- ✅ 全部源码/测试已无 `tagwash-test` 路径引用。

**唯一残留**：`MIGRATION.md` 底部的**样本重建脚本**还写着 TS 路径。
如果 TS 已删除，重建只能从备份恢复——**删 TS 前请先把 `fixtures/` 备份到别处**（65M）。

---

## 5. 后续计划（按优先级）

### P0 — 服务端

用户已定：**服务端也用 Rust，axum + tokio**。

**可直接复用的基础**：库层是纯函数，直接 `use music_tag::...` 就能调。
`read_tags` / `write_tags` / `scan_dir` / `inspect_file` / `PathSandbox` 都是现成的。

**必须解决的 3 个前置问题**：

1. **同步阻塞** —— `std::fs` 全是同步的，axum handler 里必须 `tokio::task::spawn_blocking` 包一层。
   注意：`scan_dir` 递归扫目录 + 逐文件解析，大库会阻塞很久，要考虑分片/进度上报（`logger` 事件流已经为此留好了口子）。
2. **取消粒度** —— 目前没有任何取消机制。大目录扫描中途想停，只能杀进程。
3. **fsync 缺失** —— `atomic.rs` 做了临时文件 + rename 的原子替换，但**没有 fsync**。
   **这个用户已经明确接受**，不要"顺手修掉"，除非用户改了主意。

**多租户隔离**：`PathSandbox` 只回答"路径在不在库根下"，**不是权限模型**。
多租户要靠上层 token/tenant 映射到不同 root。这是架构决策，别在沙箱层做。

**写入权限归属问题**：TS 那边的工具链写出来的文件 vs Rust 服务端写出来的文件，
归属/权限一致性要在服务端设计阶段定下来（用户提过这个点）。

### P1 — 服务端配套

- 服务端拿用户路径读写文件 → **必须走 `PathSandbox`**，不要直接 `std::fs`
- 事件上报：`logger::ndjson_sink` 已就绪，服务端可直接复用做后台任务日志
- 并发：`config.ts` 里的 `concurrency: { ioWorkers, netWorkers }` 原本是为服务端设计的
  （但 config 移植被搁置了，需要时再定）

### P2 — 待清理的技术债

- `MIGRATION.md` 里记录的**残余缺口**：`PathSandbox` 有 TOCTOU（需要 openat2 才能彻底堵住）
- `fs.rs` 的 TOCTOU 已文档化，服务端上线前需要重新评估

---

## 6. 已知 bug 与遗留疑点

### 移植期抓到的 3 个真 bug（已修）

1. **ID3v1 genre 恒在 offset 127**，旧实现非 v1.1 时错读 126（padding 位）→
   `genre=0` 被当成 "Blues"，**blank 后 genres 残留，wash 对每个 MP3 都失败**。
   已修（`src/tag/read/id3v1.rs`）。TS 侧同一个 bug，但 TS 要抛弃，不用管。
2. **FLAC 魔数成立但块序列损坏时静默降级**成「空但合法」。已改成 `Err(Unrecognized)`。
3. **扫描分级靠错误文案匹配**（`msg.contains("无法识别")`）→ 损坏 FLAC 被误判 rejected。
   已改成**按错误变体映射**。

### 已钉死的「忠实移植」边界（**不要"修好"**）

这些是 TS 的既有行为，Rust 故意保持一致。**改之前必须先改 TS 并同步**：

| 现象 | 为什么不改 |
|---|---|
| 纯文本冒充 `.mp3` → scanner 判 `ok` | MP3 是 sync-marker 格式，无帧也合法。见 `s08_text_disguised_as_mp3_is_ok_like_ts` |
| `read --json <file>` 中 `--json` 会吃掉后面的文件 | `parseArgs` 规则使然；正确写法 `read <file> --json`。见 `e14_boolean_flag_eats_following_positional` |
| wash 对「文本冒充 mp3」执行 blank 会改写文件 | scanner「可解析」与 wash「可处理」的语义缝隙。见 `e15_wash_on_frameless_mp3_is_a_known_limitation` |

改之前先跑对应测试确认，别只改代码不看用例注释。

### 待定的疑点（记下来了，用户说后面再议）

1. **ID3v2 unsync 处理** —— `id3v2.ts:33-37`，早期记录的一个不确定点，没深究
2. **APEv2 的 size 字段语义** —— 读出来的 size 是含头还是不含头，需要确认
3. 上面两条在多个地方重复记录过，**优先级低**，不影响当前功能

---

## 7. 设计约定（必须遵守）

### 7.1 测试哲学：没有变异验证过的「全绿」不算证据 ⭐

**这是本项目最重要的约定，用户明确背书。**

写任何断言之前，先回答：**「改坏什么会让这个用例变红？」**
然后真的去改坏一次，确认它红了，再改回来。

历史上抓到过 5 类「空壳断言」，都会漏测：

1. **静默跳过** —— 参照实现不可用时直接跳过，等于没测
2. **自指比较** —— `audio_hash(orig) == audio_hash(new)` 两边用同一段代码，
   删掉 verify 也不会红
3. **变异没真的应用** —— 改了源码但没生效，以为验证过了
4. **前置条件没满足** —— 比如断言文件长度变了，但写入其实静默丢弃了字段，
   长度恰好没变，断言碰巧通过
5. **教科书实现就是错的** —— 路径沙箱写 `path.starts_with(root)` 是绝大多数人的第一反应，
   它**不折叠 `..` 也不解析 symlink**，是错的。必须「先逻辑折叠再 canonicalize」。

**禁止为了让测试变绿而放宽预期**。要改断言，必须先证明断言错了，而不是代码错了。

### 7.2 提交规范

```bash
git -c user.name=migration -c user.email=none@local commit -q -m "..."
```

提交信息写中文，一句话概括改动 + 用例数 + 抓到的 bug。例：

```
CLI 外壳：read/write/blank/scan/doctor/wash + 事件流，120 用例；修 ID3v1 genre 错位/FLAC 静默降级/分级文案匹配三个真 bug
```

### 7.3 依赖纪律

- **标签读写自己实现**，不用 lofty / id3
- 平台能力库 OK：`encoding_rs` / `sha2` / `serde_json`
- 不引 clap（参数解析已手写完成，见 `src/cli/args.rs`）
- 引新依赖前先在 `Cargo.toml` 注释里说明理由

### 7.4 输出与事件

- **不写死 stdout**，一律走 `CommandIO` trait
- 事件流走 `logger::EventSink`，不写死 sink 类型
- CLI 用 `ConsoleIO`，测试用 `CollectingIO`，服务端可以注入自己的

### 7.5 错误类型分离

- **IO 错误 ≠ 路径越权**。`FsError::Escape` 和 `FsError::Io` 必须分开，可观测。
  服务端要区分"用户传了坏路径"和"磁盘出问题了"。

---

## 8. 测试约定

### 8.1 怎么跑

```bash
cargo test                          # 全套 ~90 秒（NAS 上别乱跑）
cargo test --test cli_scan          # 只跑一个文件，13 秒
cargo test --test read id3v1_genre  # 只跑一个用例
```

### 8.2 测试文件对照

| 文件 | 用例 | 覆盖 |
|---|---|---|
| `tests/read.rs` | 15 | 读侧 + ID3v1 genre 回归 |
| `tests/cli_args.rs` | 14 | 参数解析 |
| `tests/cli_read.rs` | 21 | read/write/blank 命令 |
| `tests/cli_scan.rs` | 23 | scan/doctor/wash |
| `tests/mp3write.rs` | 12 | MP3 写侧 |
| `tests/intent.rs` | 7 | 意图层 |
| `tests/sandbox.rs` | 10 | 路径沙箱（**10 个对抗用例**） |
| `tests/flacwav.rs` | 9 | FLAC/WAV 写侧 |
| `tests/apev2.rs` / `tests/wav.rs` | 5 / 4 | APEv2 / WAV 读 |

### 8.3 测试数据

`fixtures/`（65M，9 个文件）**不入库**。重建方式在 `MIGRATION.md` 底部。
删 TS 之前务必先备份这个目录（重建脚本依赖 TS 路径）。

**不要在 `fixtures/` 上跑 `wash --apply`** —— 那是你的对照基线。测试代码都自己 `cp` 到
`target/` 下再折腾。

---

## 9. 手动试用（最短路径）

```bash
source env.sh
B=./target/release/music-tag

rm -rf /tmp/try && mkdir /tmp/try && cp fixtures/* /tmp/try/

$B -h
$B read /tmp/try/盛夏-毛不易.mp3
$B read "/tmp/try/牵丝戏 - 白兀.flac" --json
$B scan /tmp/try                 # 四级分级 + 告警码
$B doctor /tmp/try               # 环境体检
$B write /tmp/try/华夏传说*.mp3 --title 测试 --preview   # 不落盘
$B wash /tmp/try --blank                          # 默认 preview
$B wash /tmp/try --blank --bak --apply           # 真写
```

**验证安全的两个点**：

```bash
md5sum /tmp/try/*.mp3 > /tmp/h1
$B wash /tmp/try --blank          # preview
md5sum /tmp/try/*.mp3 > /tmp/h2 && diff /tmp/h1 /tmp/h2 && echo "✓ preview 零写入"

$B wash /tmp/try --unset title --bak --apply
md5sum /tmp/try/*.bak > /tmp/b1
$B wash /tmp/try --unset title --bak --apply
md5sum /tmp/try/*.bak > /tmp/b2 && diff /tmp/b1 /tmp/b2 && echo "✓ 首备份保留"
```

注意：文件名带空格要加引号（`牵丝戏 - 白兀.flac`）。

---

## 10. 快速参考

- `MIGRATION.md` —— 移植全过程记录、已抓 bug 的详细分析、`PathSandbox` 的对抗测试矩阵
- `src/cli/mod.rs` —— 顶层分发，看命令怎么注册
- `src/scanner.rs` —— 四分级逻辑，**分级按错误变体不是文案**
- `src/tag/read/id3v1.rs` —— 本轮修 bug 的地方
- `src/fs.rs` —— 服务端隔离的唯一入口
- `AGENTS.md`（`$DSH_HOME/AGENTS.md`）—— 本机工作约定，含 NAS 资源保护、docs 目录权限等
