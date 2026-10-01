# 设计参考：从「飞牛音乐」提取的设计系统

> **性质**：这不是我们的设计，是**从用户自建的飞牛音乐（fnOS，`http://192.168.6.194:5666/music/`）里逆向提取**的。
> 提取方式：下载其生产 CSS 后**按变量名与取值统计**得出，不是目测。核实日期 2026-10-01。
> **用途**：music-robot Web 端的视觉参考。用户明确说喜欢这套观感。
> ⚠️ 提取的是**方法与比例**，不是照抄它的资产。真要贴近，改动下面标了「我们的取舍」的地方即可。

---

## 0. 一句话总结它为什么好看

**近黑渐变底 + 层次全靠白色透明度 + 彩色只用一个（并且只用 5 次）。**

它的 CSS 约 900KB，品牌红 `#f62c55` 只出现 **5 次**。这是它不花的原因，也是最该学的一条。

---

## 1. 颜色

### 1.1 页面底色 —— 是**渐变**，不是纯色

```css
--ds-bg-gradient-primary: linear-gradient(to bottom, #2d293a 0%, #14121b 100%);
```

顶部带一点冷紫、向下沉到近黑。纯黑会显得"死"，这个渐变给了纵深。
另有三个配套渐变：

| token | 值 | 用途 |
|---|---|---|
| `--ds-bg-gradient-overlay` | `linear-gradient(to bottom, #14121b66 0%, #14121b 70%)` | 封面大图上的压暗蒙层，保证文字可读 |
| `--ds-bg-gradient-settings` | `linear-gradient(180deg, #14121b 0%, #2e2b39 100%)` | 设置页反向渐变 |
| `--semi-color-bg-0` | `#16161a` | 兜底纯色底 |

### 1.2 表面层级 —— 全部是「白色 N%」

**这是整套系统的核心。** 没有一堆灰阶色值，只有白/黑的不同透明度：

| token | 值 | 用途 |
|---|---|---|
| `--ds-bg-card` | `#ffffff14` (8%) | 卡片 |
| `--ds-bg-card-hover` | `#ffffff1f` (12%) | 卡片 hover |
| `--ds-bg-button-primary` | `#ffffff14` | 次要按钮 |
| `--ds-bg-button-primary-hover` | `#ffffff1f` | |
| `--ds-bg-button-secondary` | `#ffffff1a` (10%) | |
| `--ds-bg-button-secondary-hover` | `#fff3` (20%) | |
| `--ds-bg-input` | `#ffffff14` | 输入框（**填充块，不是描边框**） |
| `--ds-bg-avatar` | `#ffffff1a` | 头像底 |
| **`--ds-bg-floating-pill`** | **`#ffffff12` (7%)** | **悬浮播放条** |
| `--ds-bg-dropdown` | `#0a0a0eb8` (72%) | 浮层（半透明 + 模糊） |

> **配方**：8% 做静默面、12% 做 hover、20% 做按下。**就这三档。**

### 1.3 文字层级 —— 一个基色 + 透明度

基色 **`#f2f3f4`**，全部通过 alpha 分级：

| 层级 | 值 | 用途 |
|---|---|---|
| 主文字 | `#f2f3f4` | 标题、曲名 |
| 次文字 | `#f2f3f4cc` (80%) | 图标 mid |
| 弱文字 | `#f2f3f499` (60%) | 歌手、专辑 |
| 更弱 | `#f2f3f48c` (55%) | 说明文字 |
| 最弱 | `#f2f3f480` (50%) / `#f2f3f473` (45%) | 占位符、禁用 |

**不要引入第二种灰**。需要更弱就调 alpha。

### 1.4 边框

| token | 值 |
|---|---|
| `--ds-border-default` | `#ffffff1a` (10%) |
| `--ds-border-control-inactive` | `#ffffff14` (8%) |
| `--ds-border-dropdown` | `#ffffff30` (19%) |
| 行分隔 | `#ffffff09` 左右（比 default 更淡） |

### 1.5 强调色 —— 单一 + `color-mix` 派生

它有一组固定色，但**同时只有一个生效**（`--ds-accent-current`）：

```
red #f62c55   orange #fc5e25   yellow #f8bf28   green #6bab45
blue #1b73fb  purple #c934e1   pink  #f05672
```

所有派生样式都从 `--ds-accent-current` 用 `color-mix` 算出来，**不手写第二遍**：

```css
--ds-action-primary-bg:        var(--ds-accent-current);
--ds-action-primary-bg-hover:  color-mix(in srgb, var(--ds-accent-current) 86%, #000);
--ds-action-primary-soft:      color-mix(in srgb, var(--ds-accent-current) 12%, transparent);
--ds-action-primary-border:    color-mix(in srgb, var(--ds-accent-current) 42%, transparent);
--ds-action-primary-ring:      color-mix(in srgb, var(--ds-accent-current) 34%, transparent);
--ds-action-primary-text:      #fff;   /* 或 #111，看底色亮度 */
```

**用法纪律**：强调色只出现在 **① logo ② 主操作按钮/胶囊 ③ 进度条填充** 三处。别扩散。

> **我们的取舍**：换成 music-robot 自己的主色。原型里先用 `#ef6b3c`（暖橙）占位，定色时只改这一个变量。

---

## 2. 字体与字阶

### 2.1 字体栈

```css
--ds-font-family-base: Montserrat, -apple-system, BlinkMacSystemFont,
                       "Segoe UI", Roboto, "PingFang SC", "Microsoft YaHei", sans-serif;
```

拉丁用 **Montserrat**（几何无衬线，字面干净），中文回落到 PingFang SC / 雅黑。
另有一处用 **Space Grotesk**——就是那些宽字距的全大写小标签（见 §7.1）。

> Montserrat / Space Grotesk 都要联网取。**Web 端可以内嵌**；不想引外部字体就退回系统栈，
> 损失的是"那点味道"，结构不受影响。

### 2.2 字阶（从生产 CSS 统计得出，14px 是绝对主力）

| 字号 | 行高 | 用途 |
|---|---|---|
| 12px | 16px | 歌手名、副信息、表头 |
| 13px | 20px | 导航项、按钮 |
| **14px** | **20px** | **正文 / 列表主文字（用得最多）** |
| 16px | 24px | 小标题 |
| 18–20px | 28px | 分区标题 |
| 24px | 32px | 页面主标题 |
| 32px | — | 登录页品牌字 |

**只有这 7 档，别发明第 8 档。**

### 2.3 字重

只用 `400` / `500`（次级强调，如曲名）/ `600`（标题、logo）。**不用 700 以上**——
深色底上粗体很容易糊。

---

## 3. 间距

**4px 基准**，实测高频值就这几个：

```
4 · 8 · 12 · 16 · 20
```

| 场景 | 值 |
|---|---|
| 图标与文字 | `8px`（`column-gap:8px` 出现 34 次，最高频） |
| 紧密并排（如序号与内容） | `4px` |
| 卡片内边距 | `12px` / `12px 16px` |
| 按钮内边距 | `8px 12px`（最高频）/ `8px 16px` |
| 列表行左右内边距 | `12px` 起 |
| 区块之间 | `20px` / `24px` |

**不确定就用 8 或 12。** 它没有 6/10/14 这种值。

---

## 4. 圆角

| 值 | 用途 |
|---|---|
| `3px` | 极小元素、标签 |
| `4px` / `6px` | 小控件、封面缩略图 |
| `8px` | 按钮、图标按钮 |
| `10px` | 卡片 |
| `12px` | 面板 |
| `16px` | 悬浮条、大卡片 |
| `999px` | 胶囊按钮、搜索框、头像 |
| `50%` | 圆形头像 |

Semi 的 token 里另有 `extra-small:3 / small:10 / medium:24 / large:32`，
但**实际界面用得最多的还是 4–16**，24/32 基本只在模态上。

---

## 5. 阴影

```css
--ds-shadow-small:    0 1px 4px  #0006
--ds-shadow-medium:   0 4px 16px #00000014
--ds-shadow-large:    0 8px 32px #00000047
--ds-shadow-dropdown: 0 8px 32px #00000080
```

深色底上阴影**要重**才看得出来。悬浮播放条用的是 `large` 档 + 模糊背景。

---

## 6. 动效

实测时长：**`.2s` 为主**，其次 `.15s`、`.18s`，`@` 大过渡 `.3s`。

- 默认 `transition: .2s`（hover、颜色）
- `.15s` 用于小控件
- `.3s` 用于面板展开

缓动没特殊声明，用默认 `ease` 即可。**不要加弹跳、不要加延迟**。

---

## 7. 值得直接抄的 5 个手法（跟配色无关，纯粹是技巧）

### 7.1 宽字距的全大写小标签

```css
text-transform: uppercase;
letter-spacing: .24em;   /* 实测还有 .28em / .12em */
font-size: 11px;
color: var(--text-muted);
```

登录页角落的 `PERSONAL MUSIC ARCHIVE`、`TRACKS // ALBUMS // ARTISTS` 就是这个。
**成本几乎为零，但立刻显得"有人设计过"。** 适合用在：分组标题、空状态、页脚。

### 7.2 列表行三合一

`[封面缩略图] 歌名 / 歌手` 堆叠在一列里 —— 省掉两个独立列，还让列表一眼是"音乐"不是"数据表"。

### 7.3 极淡斑马纹

```css
tbody tr:nth-child(even) { background: rgba(255,255,255,.017); }
```

深色下几乎看不见，但眼睛能跟着走。**超过 3% 就俗了。**

### 7.4 悬浮毛玻璃播放条

```css
position: absolute; bottom: 18px; left: 50%; transform: translateX(-50%);
background: #ffffff12;
backdrop-filter: blur(18px) saturate(1.4);
border: 1px solid #ffffff1a;
border-radius: 16px;
box-shadow: 0 8px 32px #00000047;
```

浮在内容上方、不贴底。**代价：会盖住最后两行** → 内容区底部要留 ~120px padding。

### 7.5 安静的侧边栏

它确实有侧边栏，但"不像后台"：

- 宽 **158px**（窄）
- 项高 ~34px，图标 15px + 13.5px 文字
- **未选中 = 弱文字色，选中 = 只是加一层 8% 白底的圆角块**
- **不用粗体、不用左侧色条、不用大字号**
- 底部一个「收起」

后台感来自「粗体 + 高饱和 + 左侧色条」，这些它一个都没用。

---

## 8. 别抄的部分

| | 为什么 |
|---|---|
| `大小` / `格式` 两列 | 那是"资源管理"心态。我们要的是音乐播放器，不是文件管理器。要留就做成可关的列。 |
| 顶栏那排图标按钮（3 个） | 图标一多就退回"后台管理系统"。我们只留搜索 + 头像菜单。 |
| 侧边栏 8 项全平铺 | 我们的功能更少（曲库/专辑/歌手/歌单/收藏 + 任务），别为了凑数塞满。 |
| 它的品牌红 `#f62c55` | 那是飞牛的标识色，别用。 |

---

## 9. 落到 CSS 变量（可直接抄进项目）

```css
:root{
  /* 底 */
  --bg-grad: linear-gradient(to bottom, #2d293a 0%, #14121b 100%);
  --bg-solid:#16161a;

  /* 面（白透明度三档） */
  --surface:       rgba(255,255,255,.08);
  --surface-hover: rgba(255,255,255,.12);
  --surface-press: rgba(255,255,255,.20);
  --glass:         rgba(255,255,255,.07);

  /* 字（一个基色 + alpha） */
  --fg:#f2f3f4;
  --fg-2:rgba(242,243,244,.80);
  --fg-3:rgba(242,243,244,.60);
  --fg-4:rgba(242,243,244,.45);

  /* 线 */
  --line:rgba(255,255,255,.10);
  --line-weak:rgba(255,255,255,.04);

  /* 强调（只有一个，改这里就换肤） */
  --accent:#ef6b3c;
  --accent-hover:color-mix(in srgb, var(--accent) 86%, #000);
  --accent-soft:color-mix(in srgb, var(--accent) 12%, transparent);
  --accent-ring:color-mix(in srgb, var(--accent) 34%, transparent);

  /* 间距 / 圆角 / 阴影 / 动效 */
  --s1:4px; --s2:8px; --s3:12px; --s4:16px; --s5:20px; --s6:24px;
  --r-sm:6px; --r-md:8px; --r-lg:10px; --r-xl:16px; --r-pill:999px;
  --sh-1:0 1px 4px #0006;
  --sh-2:0 4px 16px rgba(0,0,0,.08);
  --sh-3:0 8px 32px rgba(0,0,0,.28);
  --t:.2s;
}
```

---

## 10. 一句话规则（写代码时贴屏幕上）

> **面用白色 8/12/20%，字用 `#f2f3f4` 的 100/80/60/45%，间距只用 4 的倍数，
> 圆角 6/8/10/16/999，彩色只出现在 logo 和主操作上。**
