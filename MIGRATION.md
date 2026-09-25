# music-tag → Rust 迁移记录

> 目标：以 TS 仓库（`/vol1/@appshare/dsh/data/tagwash-test`）为**参照实现**，逐模块移植。
> 铁律：**测试先行；不改期望值让测试通过；不使用第三方标签库（lofty/id3）。**

## 环境（实测，非推断）

| 项 | 值 |
|---|---|
| 工具链 | rustup 1.98.1，装在用户目录，**无需 root**（`RUSTUP_HOME`/`CARGO_HOME` = 本目录下 `../rust-test/{rustup,cargo}`） |
| CPU | ARM Cortex-A55 ×4 @1992MHz，7.4G 内存 + zram |
| 空项目冷编译 | 2.7s；`cargo test` 1.5s |
| 含 48 crate 依赖 | 82s / target 181M |
| ⚠️ 禁止 | 构建产物放 `/tmp`（tmpfs 仅 3.7G）；apt 的 rustc 1.63 低于 lofty MSRV 1.89 |

跑测试前先设环境：
```bash
export RUSTUP_HOME=/vol1/@appshare/dsh/rust-test/rustup \
       CARGO_HOME=/vol1/@appshare/dsh/rust-test/cargo \
       PATH="$CARGO_HOME/bin:$PATH"
cargo test
```

## 依赖策略

- **产品代码零依赖**：标签读写全自研。ffprobe 输出的 JSON 用手写的字节安全标量抽取（`dispatch.rs::json_scalar`），不引 serde。
- **仅 dev-dependency 有 serde_json**：差分测试要解析 TS 侧 `read --json`。曾手搓 60 行解析器，连续踩「对象键前空白未跳过」「嵌套结构」两个 bug 并**导致测试假绿**，故按决策换成 serde_json。
- **encoding_rs（GB18030）**：已接入（原缺口 1 关闭）。TS 靠 Node 内置 `TextDecoder('gb18030')`，属平台能力补位而非标签逻辑外包。实测两帧 COMM 的解码结果与 TS 逐字节一致（含第二帧正确解出「酷我音乐」）。

## 已完成

- [x] `tests/read.rs` —— TS 14 用例逐条移植（先写测试、确认其失败，再实现）
- [x] `src/tag/read/metadata.rs` —— AudioMetadata 契约（Option vs undefined/null 显式区分）
- [x] `src/tag/read/id3v2.rs` —— syncsafe / unsync / UTF-16LE·BE·BOM / APIC / USLT / TXXX / 三版本帧头
- [x] `src/tag/read/id3v1.rs` —— 含 148 项流派表
- [x] `src/tag/read/gbk_sniff.rs` —— 高字节占比 + GB18030 解码双判据（encoding_rs）
- [x] `src/tag/read/probe.rs` —— ID3 链跳跃 + APE hasHeader 位 + 64KB footer 自校验 + MPEG sync
- [x] `src/tag/read/flac.rs` —— 块遍历 / STREAMINFO 位域 / Vorbis Comment / PICTURE
- [x] `src/tag/read/apev2.rs`、`native_probe.rs`（FLAC 精确；MP3 位率表未完成）
- [x] `src/tag/read/dispatch.rs` —— readTags 分派 + ffprobe 双通道
- [x] `tests/differential.rs` —— **跨语言实时对拍**：调 node 读 TS 输出，逐字段比对 6 样本

当前：`cargo test` = 14 read + 1 differential 全绿；差分测试经注入 `+1` 验证确会红。

## 迁移中实际踩到的坑（这部分最值钱）

1. **测试静默 skip 伪装成通过。** `ts_read_json()` 返回 None 时 `return`，cargo 记作 `1 passed`。注入错误断言后仍绿才发现。已改为**参照实现不可用即 panic**——绝不允许静默跳过。（正是 `SCRIPTC-NOTES.md §三` 记的那类假阳性。）
2. **FLAC PICTURE 字段是大端 u32**，且 mime/desc 变长、data 取剩余全部。我最初写成小端 + 固定长度切 slice → 封面数 0。
3. **STREAMINFO 位域丢进位**：`bitsPerSample = (((b12&1)<<4)|(b13>>4))+1`，`b12` 最低位是 sampleRate/channels/bits 三者的共享边界。错读成 `(b12>>1)&7 + 1` → 得 2bit 而非 24bit。
4. **不能按 key 出现顺序切 JSON 区间**：带封面的 MP3 里 `"streams"` 排在 format 的 `duration` **之前**，切片后取不到 duration → 时长 0。改成花括号配平定位对象。
5. **UTF-8 字节边界 panic**：用 char 索引去切含中文的 `&str` → `byte index is not a char boundary`。动态语言转来的人最容易反复撞的一类。

## 已知偏差与缺口（不许悄悄溜过去）

| # | 项 | 状态 |
|---|---|---|
| ~~缺口1~~ | GB18030 解码 | ✅ **已关闭**（encoding_rs）。判据恢复为与 TS 同构的双条件：高字节占比 >0.3 **且** 解码后含 CJK。另多拿到 `had_replacements` 信号（TS 无），将来可用于收紧误报。 |
| 陷阱1 | `id3v2.ts:33-37` unsync 与规范不符（FF+E0..FF 多跳一字节） | Rust 侧**刻意照搬同一行为**并注释标记。修它属破坏性变更，需差分基准背书后再议。 |
| ~~陷阱2~~ | looks_gbk 判据偏松 | ✅ **已随缺口 1 关闭**——两条判据都在，与 TS 完全同构。 |
| ~~缺口2~~ | mp3_native_probe 位率表 | ✅ **已补全**：三张码率表（MPEG1/2 × L1/L2/L3）+ 采样率 + samplesPerFrame + Xing/Info 帧数优先、CBR 字节兜底。实测 `TAGWASH_NO_FFPROBE=1` 下 5 个 MP3 时长误差 **+32~+42ms**（符合设计目标 ±40ms），FLAC 精确。 |
| ~~缺口3~~ | WAV 读侧 | ✅ **已实现**：RIFF chunk 树（含奇数 size 的偶对齐 pad）、fmt / data / LIST INFO / 内嵌 `id3 ` chunk、字段映射、native probe 按 byteRate 精算时长。新增 `tests/wav.rs` 4 用例并做过变异验证。 |
| ~~缺口4~~ | apev2.rs 冗余赋值警告 | ✅ 已清；且借这次重写发现我的移植**原本就走样了**（详见下方「APEv2 重写记录」）。当前 `cargo build` **零警告**。 |

## 下一步（严格保持测试先行）

1. 补 `tests/write.rs`（TS write.test 20 例 + write-cmd 21 例）→ 再实现 `tag/write/*`
2. 决策缺口 1（GB18030），顺带解掉陷阱 2
3. `tests/batch.rs`（scanner/doctor/scan/wash preview）
4. wash --apply + 原子写 + audioHash 不变量

## 附：COMM 帧的坑（本轮实测记录）

盛夏/老男孩等样本有 **2 个 COMM 帧**：第一帧 desc 本身是 UTF-16 乱码（`ÿþ`）、text 也是垃圾；
第二帧才是 `desc="ID3v1 Comment"`、text 为真 GBK（解出「酷我音乐」）。
定位必须按 desc 内容匹配（TS/Rust 同一判据），**不能取第一个 COMM**——否则会拿垃圾当结论。

## WAV fixture 配方（可复现，勿手工摆字节）

fixture 落在 TS 仓库 `tests/fixtures/`，由 ffmpeg 从样本现场切出（体积 16KB~350KB）：

```bash
cd /vol1/@appshare/dsh/data/tagwash-test && mkdir -p tests/fixtures
# 无标签：验证「空」与「解析失败」可区分（占位实现曾对此假成功）
ffmpeg -y -i "samples/华夏传说 - 凤凰传奇.mp3" -t 2 -map_metadata -1 -c:a pcm_s16le -ar 44100 -ac 2 tests/fixtures/plain.wav
# 全字段 LIST INFO：INAM/IART/IPRD/ICRD/IGNR/ICMT 映射
ffmpeg -y -i "samples/华夏传说 - 凤凰传奇.mp3" -t 2 -c:a pcm_s16le -ar 44100 -ac 2 \
  -metadata title="测试标题" -metadata artist="测试歌手" -metadata album="测试专辑" \
  -metadata date="2019" -metadata genre="Rock" -metadata comment="测试备注" tests/fixtures/tagged.wav
# 奇数长度 INFO 子项（单字符标题 + 8kHz 单声道）：专门踩 RIFF 偶对齐 pad
ffmpeg -y -i "samples/华夏传说 - 凤凰传奇.mp3" -t 1 -c:a pcm_s16le -ar 8000 -ac 1 \
  -metadata title="奇" -metadata artist="短名A" tests/fixtures/oddpad.wav
```

Ground truth（ffprobe）：plain/tagged = 2.000s / 44100Hz / 16bit / 2ch；oddpad = 1.000s / 8000Hz / 16bit / 1ch。
TS 参照输出已核对一致；`tsfixtures` 是指向该目录的符号链接。

## 变异测试记录（证明断言真的会咬人）

上一轮差分测试曾因静默 skip 而**假绿**，故本轮对新增用例逐条注入错误验证：

| 注入 | 结果 |
|---|---|
| 去掉 RIFF 偶对齐 pad（`+ (size_us & 1)`） | ❌ 红 —— LIST INFO 字段错位，title None vs 测试标题 |
| read_wav 短路成默认 WavInfo（模拟假成功） | ❌ 红 —— tagged / oddpad 两用例同时失败 |
| 差分测试 rawFrames +1 | ❌ 红 —— Havana 帧数不一致 |

⚠️ 教训：**没有变异验证过的"全绿"不算证据。** 本轮第一次尝试注入 B 时 python 脚本 assert 失败、注入根本没生效，我却看到"4 passed"——如果直接记录下来就是又一次假绿。改成先 assert 再跑才拿到真实结果。

## APEv2 重写记录（清警告时挖出的真问题）

原 `apev2.rs` 是我第一版凭印象写的，与 TS 参照有多处偏差；为消除 `kstart` 未使用告警而重写时逐行对照才发现：

| 偏差 | 后果 |
|---|---|
| **漏了 key 小写化** | TS `parseItems` 有 `.toLowerCase()`（APEv2 键大小写不敏感），下游按 `title`/`artist` 取值。**Rust 侧会全部取不到值** —— 真实功能缺陷 |
| 用 `for _ in 0..count` 驱动遍历 | TS 用 `while p+8<=len`。声明 count 与实际不符的畸形文件会被截断或越界 |
| 缺两处逃逸条件 | ① `key_end >= len` break ② 零长值且 key 贴末尾必须显式跳出，否则 **p 不前进 → 死循环** |
| `valueStart > len` 检查顺序颠倒 | 先算 valueEnd 再查 start，越界 panic 风险 |
| `parse_ape_tag` 里误用 `?` | 第一个候选位置失败即中止整函数，**永远试不到 ID3v1 之前那个位置**（TS 是 continue）。短文件还会因 `checked_sub(160)` 下溢直接返回 None |

### ⚠️ 陷阱 3：APEv2 的 size 字段语义，参照实现与规范不一致

- **规范**：footer 的 tagSize 字段 = items 区 + footer(32B)。
- **你的 TS 实现**（`apev2.ts:29` `buf.slice(pos - info.size, pos)`）把它当 **items-only** 用。
- 实测：同一份合成字节，写 `items+32` 时 TS 返回 null，写 `items` 才解析成功。

这意味着**真实世界的 APEv2 文件（尤其 MusicBrainz Picard 等规范写入器产出的）可能被现有实现判为"无 APE tag"**——静默漏读，不报错。Rust 侧本轮**刻意保持与参照一致**（移植保真优先），并把该疑点登记在此。建议下一步拿一个真实带 APE 的 MP3 验证；若确认是 bug，修在 TS 侧、Rust 跟随。

### fixture 布局备忘（踩过的坑）

APEv2 footer 固定 32B：`APETAGEX(8) + version(4)@8 + items_size(4)@12 + item_count(4)@16 + flags(4)@20 + reserved(8)@24`。
我最初把 reserved 写成 12B（footer 变 36B），导致解析器在错误偏移找 magic → 测试全红。**reserved 只有 8 字节。**

### 变异验证（新增）

| 注入 | 结果 |
|---|---|
| 去掉 `.to_lowercase()` | ❌ 红：`left "Title"` vs `right "title"` |

## MP3 写侧（atomic + id3v2-editor + mp3_writer）

`tests/mp3write.rs` 12 用例 + `tests/diff_write.rs` 6 用例。产品新增依赖 `sha2`（裸音频完整性 hash，
对应 Node 内置 crypto；std 的 DefaultHasher 不保证跨版本稳定，不能承担此职责）。

### ⚠️ 本轮最重要的发现：一条"全绿"的假阳性测试

最初 w02_bare_audio_hash_invariant 报通过，但把 `verify` 改成**恒真**后它仍然通过 ——
因为它比较的是 `audio_hash(原) == audio_hash(新)`，两侧都用同一个被测函数。
若 `mp3_audio_region` 本身算错（例如恒返回同一区间），这条断言永远成立。

修法：**加独立锚点**，不再只依赖被测代码自身：
- `w02b` verify 恒假 → 必须拒绝落盘且原文件逐字节不变（验证闸门真的存在）
- `w02c` 用「原始音频字节区间是否在新文件中逐字节可见」来判定，绕开 hash 实现
  （注入 off-by-one 吞掉 1 字节音频后，此条与另外 4 条同时红 ✓）

教训同前：**跑过 ≠ 验证过**。变异注入必须先用 assert 确认落地，否则连"注入失败"都会被读成"测试通过"。

### 变异验证汇总（写侧）

| 注入 | 结果 |
|---|---|
| C: `raw_start = 0`（不跳 ID3v2） | ❌ 8 条红 |
| E: tag_size off-by-one（吞 1 字节音频） | ❌ 5 条红（含 w02c 独立锚点） |
| F: 多歌手改借位 TPE2（REVIEW §2.5 老 bug） | ❌ diff_write 红：TS=`["甲","乙"]` vs Rust=`["2"]` |
| D: 完全摘掉 verify 传参 | ✅ 仍绿 —— **可接受**：D 单独不损坏数据（写入内容未变，只是少了一道自检），而 E/C 证明真实损坏会被别的测试抓住 |

## FLAC / WAV 写侧

`tests/flacwav.rs` 9 用例。FLAC 键级局部编辑（保留全部非 4/6 元数据块）+ PICTURE 重建 +
STREAMINFO md5 & 裸区 sha256 双校验；WAV INFO 子项级编辑 + 内嵌 id3 chunk + RIFF size 重算 + data chunk hash。

### 本轮抓到的两个问题（都是我自己造的）

**① 双重块头 bug（实现缺陷）**：TS 的 `pieces` 存的是**含头完整块**，末尾用 `b.bytes.subarray(4)` 剥头再重贴；
我改成「Piece 只存 payload」但把 `build_picture_block()`（带头）塞了进去 → PICTURE 头上再套一个头，
读侧解出 `type=0x06000031`、封面数 0。修法：内部统一用不带头的 `picture_payload()`，
公开的 `build_picture_block()` 才加头。**这类"移植时改了数据结构但没改全所有生产者"的错误，
只有靠真实读回测试才能抓到——f02 正是这么抓到的。**

**② 又一个空壳断言（测试缺陷）**：w03 验证「RIFF size 必须重算」，但注入陈旧 size 后仍绿。
诊断发现 `diff=0` —— 我用 `lyrics` 字段写入，而 **WAV 的 LIST INFO 根本没有歌词槽位**，
writer 静默丢弃 → 文件尺寸没变 → 陈旧值恰好等于正确值。修法：改用 `comment`(ICMT) 并加前置断言
`assert_ne!(b.len(), orig.len())`——**先证明"这次写入真的改变了尺寸"，再断言 size 被重算**。

教训与前两条同源：**断言所依赖的前提要显式检查**，否则测试会在"什么都没发生"的状态下报通过。
到目前为止四轮假阳性分别是：静默 skip、自比无锚点、注入未生效、前提未成立。

## 路径沙箱（服务端前置件）

`src/fs.rs` + `tests/sandbox.rs` 10 用例。服务端会拿**用户输入**的路径读写文件，这是越权防线。

### 为什么没有照搬 TS 的 FsLike

TS `FsLike` 的形状是 `Pick<typeof fs, 'readFileSync'|...>`——那是「注入假 fs 做测试」的形状，
接口跟着 mock 走。这里真正的诉求是「限制一个真 fs 能碰哪里」。混在一个 trait 里，mock 会反过来
约束生产接口（TS 就吃了这个亏）。所以拆成：少量能力导向的方法 + **路径策略集中在 resolve()**。

### 两处真实逃逸洞（对抗测试抓出来的）

**① canonicalize 的结果没用上**：第一版把**原始路径**拿去 `starts_with`，
`root/../outside/secret.txt` 的组件序列确实以 `root` 开头 → 检查直接放行。

**② `Path::starts_with` 不折叠 `..`**：纯组件前缀比较，`base/root/../outside` 仍以 `root` 开头。
且修 ① 时改成的「逐层 `file_name()`/`parent()` 向上剥」还有第三处坑——
`file_name()` 对以 `..` 结尾的路径返回 `None`，循环提前终止、留下一段未解析的 `..`。

最终形态是两步：**先按逻辑语义折叠 `.`/`..`**（得到纯下降组件栈，折叠后才可能做归属判断），
**再从顶端找最深存在的真实前缀并 canonicalize**（解析 symlink）。缺任一步都会被测出。

变异验证：
| 注入 | 结果 |
|---|---|
| A: 不做 canonicalize，直接对原始路径 starts_with（**教科书写法**） | ❌ 5 条红（s02/s03/s05/s08/s10） |
| B: 跳过逻辑折叠，保留 canonicalize | ❌ s02 红 |

第 A 条值得单独记：**这是大多数人写路径沙箱的第一反应，它是错的**。

### 已声明的残余缺口（不藏）

- **TOCTOU**：resolve 与真正 read/write 之间非原子，期间 symlink 被换掉仍可逃逸。彻底堵住需要
  openat2(AT_SYMLINK_NOFOLLOW) 或 fd 级传递——那是内核接口选择，本模块不做。
  缓解：服务端把曲库目录设为非用户可写，并只允许服务账号解析 symlink。
- **不是权限模型**：只回答"路径在不在库根下"，不回答"这个用户有没有权限"。
  多租户隔离要靠上层 token/tenant 映射到不同 root。

---

## CLI 外壳（第 5 阶段）

`src/cli/`：零依赖手写参数解析（不引 clap）、read/write/blank/scan/doctor/wash 六个子命令、
`CommandIO` 可注入输出通道、`Logger` 事件流 + NDJSON sink。测试**在内存里直接调函数**，不 spawn 子进程。

### 本阶段抓到的真 bug（不是移植偏差）

**1. ID3v1 genre 恒在 offset 127，旧实现错读 padding 位 → blank 后 genres 残留 "Blues"**

`id3v1.rs` 里 `let genre_byte = if track_present { tag[127] } else { tag[126] }`——非 v1.1 布局时
读的是 126（padding，恒 0x00），于是 genre=0 被当成 ID3v1 的 "Blues"。后果：`blank` 之后
`genres=["Blues"]`，读回复核判「残留」，**wash blank 对每个 MP3 都失败**。

⚠️ **TS 参照实现有同一个 bug**（`id3v1.ts:77` 同样 `trackPresent ? tag[127] : tag[126]`），
已用 TS 端到端复现。Rust 侧按 ID3v1 规格修正，TS 侧待同步修。

变异验证：把 `tag[127]` 改回条件式 → `tests/read.rs::id3v1_genre_always_at_127_not_padding` 红。

**2. FLAC 魔数成立但块序列损坏时静默降级成「空但合法」**

`read_flac` 里 `parse_flac_metadata` 返回 `None` 时 `return Ok(m)`——损坏 FLAC 被判成「无标签的合法文件」。
TS 是抛 "Attempt to access memory outside buffer bounds"。已改成 `Err(Unrecognized)`。

**3. 扫描分级靠错误**文案**匹配，把 broken 误判成 rejected**

`inspect_file` 用 `msg.contains("无法识别")` 兜底判 rejected，但 `ReadError::Unrecognized` 的文案
恰好含「无法识别」四字，于是「fLaC + 损坏体」（TS 判 broken）在 Rust 被判 rejected。
这正是 P2-4 想消灭的脆弱性——**已改成按错误变体映射**：`Id3PrefixedReal/Id3PrefixedUnknown` → rejected，
其余 → broken。变异回退文案匹配 → s04/s06 红。

### 已钉死的「忠实移植」边界（不要"修好"）

| 现象 | 处理 |
|---|---|
| 纯文本冒充 `.mp3` → scanner 判 `ok` / `format=unknown` / `frameCount=0` | 钉死。MP3 是 sync-marker 格式，无帧也合法；TS 同样行为。见 `s08_text_disguised_as_mp3_is_ok_like_ts` |
| `read --json <file>` 的 `--json` 会把 `<file>` 当成自己的值 | 钉死。`parseArgs` 的「下一参数不以 `--` 开头即取值」规则使然；正确写法 `read <file> --json`。见 `e14_boolean_flag_eats_following_positional` |
| wash 对「scanner 判 ok 的文本冒充 mp3」执行 blank → 文件被改写、读回复核失败 | 钉死并如实报 `failed`。这是 scanner「可解析」与 wash「可处理」的语义缝隙，TS 同样存在。见 `e15_wash_on_frameless_mp3_is_a_known_limitation` |

### 顺带修掉的遗留

- `write_tags` 对 FLAC/WAV 直接返回 `Unrecognized`——writer 早已移植完但从未被分派。现已接入。
- `intent::WriteMeta` 与 `Id3EditMeta` 重复定义，`merge_fields` 产出的类型喂不进 `write_tags`。
  已合并为 `pub use Id3EditMeta as WriteMeta`，`AfterView` 补 `disc_total`。

## 测试音频样本（fixtures/ 与 tsfixtures/）

原为软链接指向 TS 项目，现已改为**真拷贝**（不再依赖 TS 目录存在），但**不入库**
（64M 二进制，`.gitignore` 已忽略）。

重建方式：

```bash
cd /vol1/@appshare/dsh/rust-test/music-tag
rm -rf fixtures tsfixtures
cp -a /vol1/@appshare/dsh/data/tagwash-test/samples          fixtures    # 6 个真实样本，64M
cp -a /vol1/@appshare/dsh/data/tagwash-test/tests/fixtures    tsfixtures # 3 个小 WAV，712K
chmod 755 fixtures tsfixtures && chmod 644 fixtures/* tsfixtures/*
```

- `fixtures/`：6 个真实音乐文件（mp3 ×5 + flac ×1），含广告词 / GBK 乱码 / ID3v1 垃圾尾等真实脏数据，
  是 `scan`/`read` 告警检测的主要素材。
- `tsfixtures/`：3 个小 WAV（`tagged` / `plain` / `oddpad`），供 WAV 读写用例使用。

⚠️ 不要在 `fixtures/` 上直接跑 `wash --apply`——那里现在是**本项目自己的副本**，
改坏了直接从 TS 项目重拷一次即可，但会丢失"与 TS 对照基线"的一致性。
