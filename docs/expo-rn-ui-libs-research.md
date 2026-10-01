# Expo / React Native UI 组件库调研 — 哪些能用于 PC 桌面端 Web 后台

> 核实时间：2026-10-01。所有版本号 / star / 下载量均来自 npm registry API、GitHub API、官方文档实测抓取，不凭记忆。
> 标注约定：**✅ 我实际核实过**（npm registry 或 GitHub API 直接返回）/ **⚠️ 二手信息**（官方文档描述、搜索结果，未逐条验证）。

## 0. 先说结论（TL;DR）

| 问题 | 结论 |
|---|---|
| Expo/RN 生态有成熟组件库吗？ | 有，且都活着（Tamagui 2.7.7 / Paper 5.15.3 / gluestack v5 / NativeWind 4.2.7），但**全部以移动端为第一优先级** |
| 能拿来写 PC 后台吗？ | **不建议**。它们在 web 上的渲染路径仍是 react-native-web（RNW）→ 你在背景里列的硬限制（丢 onDrop、无伪类、无 table/tr/td、列表组件官方说 not optimized for web）一条都不会消失 |
| 那这些库对本项目还有价值吗？ | **有一个，且只有一个场景**：将来若真做移动端 App，Mobile 端 UI 层可以复用 NativeWind + Tailwind 这套语义（不是复用组件）。PC 端**零价值** |
| 数据表格这类组件，RN 生态有吗？ | **基本空白**。`react-native-table-component` 已死（2022 停更）；Paper 的 `DataTable` 是不虚拟化的静态表格；gluestack 的 `Table` 还是 alpha；`expo-flash-datagrid` 每周下载 26 次，等于不存在。**这块 RN 生态确实空白** |
| Expo 后台模板有吗？ | **没有 shadcn-admin 级别的**。最接近的是 `ixartz/React-Native-Boilerplate`（411 star，2025-08 停更）和 gluestack starter kits（252 star，2024-11 停更）。星数比 shadcn-admin（15457）差两个数量级 |

**给本项目的建议：维持现架构（Expo 壳 + Web 写 DOM + shadcn/ui + Tailwind v4），不要引入任何 RN 组件库。** 详见第 10 节。

---

## 1. 逐个结论

### 1.1 Tamagui ✅

| 维度 | 结论 |
|---|---|
| 是什么 | 跨端样式库 + UI kit + **优化编译器**（`@tamagui/static`），号称「100% parity on React Native」 |
| 版本 | `tamagui` 2.7.7（2026-08-15 发布），2.0.0 stable 于 **2026-05-23** 发布，2.x 共 31 个 stable 版本，**发版极频繁**（2.0 之后 3 个月发了 31 个版本）✅ |
| 维护状态 | GitHub 14208 star，最近 push **2026-09-30**（昨天），99 个 open issue，未归档 ✅。`3.0.0-beta.1479.1`（2026-09-27）在 beta 通道高频迭代 ✅ |
| 下载量 | `tamagui` 233,819/周；`@tamagui/core` 281,257/周 ✅ |
| **真的编译出真 CSS 吗** | **是的，这是它唯一比 RNW 强的地方。** 官方 README 原话：「it turns styled components … into a simple `div` alongside **atomic CSS** on the web, or a View with its style objects hoisted on native」✅。即 web 端产物是 `<div>` + 原子化 CSS class，**不经过 RNW 的 style→class 运行时转换** |
| web 上表现接近普通 React 吗 | **接近，但不是免费午餐**。优势：真 div、原子 CSS、编译期去重、SSR 友好。代价：① 必须配 `@tamagui/static` 编译器（`@tamagui/next-plugin` / `@tamagui/vite-plugin`），**Expo + Metro 这条链路不是它的主推路径**，Metro 集成需要额外的 babel/metro 配置；② 它是「自己的样式语言」（`styled()` + token + variants），你写 `<View>`/`<Text>` 而不是 `<div>`，**事件仍是 RN 语义**——`onDrop`/`onDragStart` 这类 DOM 事件在 Tamagui 的 View 上依然不成立 |
| 授权/商业化 | **核心 MIT，无授权问题**。`@tamagui/core` package.json `license: "MIT"` ✅；`tamagui` 主包 package.json 未声明 license 字段（⚠️ 元数据瑕疵，非授权变更）。Pro 是**独立商业产品**：Tamagui Pro V2 = Takeout 模板 + Bento 高级组件 + Theme Builder，**按项目收费**，含一年更新，之后 $100/年续订，禁止公开再分发 Takeout 源码 ✅（[pro-license](https://tamagui.dev/pro-license)）。**你不用 Pro 就完全不受影响** |
| 有 Table/DataGrid 吗 | 没有。有 `@tamagui/menu`、`@tamagui/dialog`、`@tamagui/tabs`、`@tamagui/tooltip`、`@tamagui/select`、`@tamagui/context-menu` 等（组件面比 `@expo/ui` 宽得多），但**无 Table/DataGrid** ✅ |
| 适合本项目吗 | **不适合。** 你已经在 Web 侧用 shadcn/ui + Tailwind v4 写纯 DOM，Tamagui 会引入**第二套样式范式 + 一个编译器插件**，只为换取「移动端复用」，属于典型过度设计。而且它解决不了你的核心诉求（PC 后台的数据密集交互），反而挡在中间 |

**一句话**：Tamagui 是「真正能编译出 CSS 的 RN 库」这个说法**成立且已核实**，但它的定位是「让 RN 代码也能跑 web」，不是「让你在 web 上少写 DOM」。

---

### 1.2 React Native Paper（Callstack）✅

| 维度 | 结论 |
|---|---|
| 是什么 | Material Design 3 的 RN 组件库，最老牌的 RN UI 库之一 |
| 版本 | `5.15.3`（2026-05-26）✅；`6.0.0-alpha.0` 于 2026-06-15 发布（6.x 未 stable）✅ |
| 维护状态 | GitHub 14466 star，最近 push **2026-09-29** ✅，494 open issues（偏高），未归档 |
| 下载量 | 516,940/周 ✅（RN UI 库里最高） |
| Web 支持 | **走 RNW**。官方文档明确：「React Native Paper supports web via React Native for Web」，Expo 下 `npx expo install react-dom react-native-web @expo/metro-runtime` 即可 ✅（[官方 6.x web 指南](http://oss.callstack.com/react-native-paper/6.x/docs/guides/react-native-web)）。**没有任何真 CSS 编译** |
| 有 DataGrid 吗 | 有 `DataTable` / `DataTable.Header` / `DataTable.Row` / `DataTable.Cell` / `DataTable.Title` / `DataTable.Pagination` ✅（组件列表已核实）。但这是**静态表格**，不是虚拟化 DataGrid，没有列宽拖拽/列固定/分组/行虚拟化 |
| 适合本项目吗 | **不适合。** ① 它渲染出来的 DOM 是 RNW 的 div 树，语义上不是真 `<table>`；② Material 3 视觉风格会和你 shadcn/ui 的设计体系打架；③ PC 端 hover/右键/拖拽全靠 RNW 事件层，撞硬限制 |

---

### 1.3 gluestack-ui ✅

| 维度 | 结论 |
|---|---|
| 是什么 | 前身 NativeBase 团队（Geekyants）做的「copy-paste 组件 + patterns」库，哲学类似 shadcn/ui |
| 版本 | **`gluestack-ui` 5.0.3 于 2026-06-25 stable 发布** ✅；dist-tags：`latest: 5.0.3`、`v3-stable: 3.0.12`、`v2: 2.0.2` ✅ |
| 维护状态 | GitHub 5317 star，最近 push **2026-09-02** ✅（比 Tamagui/NativeWind 冷一些）；核心 npm 包 `gluestack-ui` 只有 **3139 下载/周** ✅（它是 CLI 工具，实际组件走 `npx gluestack-ui add` 拷进项目，所以下载量不能直接对比 `@gluestack-ui/themed` 的 41704/周） |
| v2/v3 是否重写 | **是，且 v5 是一次战略转向。** 官方 v5 stable 公告原话：「we have made a major strategic shift to focus entirely on **native mobile performance**」+「**Next.js Support Dropped** … we have deprecated Next.js and universal monorepo adapters」+「Expo Router First」✅（[v5 stable release](https://v5.gluestack.io/blogs/gluestack-ui-v5-stable-release)） |
| Web 支持 | **v5 起主动放弃 web/Next.js 路径。** v5 全面转向 NativeWind v5 + Tailwind v4 + Expo Router，**删掉了 universal monorepo 适配器**。这意味着：想用它做 PC Web 后台，你用的是它**已经明确不再投入的方向** ✅ |
| 有 Table/DataGrid 吗 | 有 `Table/TableHeader/TableBody/TableRow/TableHead/TableData/TableFooter/TableCaption`，但文档标注 **alpha** ✅。同批 alpha 的还有 `Tabs`、`DateTimePicker`、`Calendar`、`Grid`、`BottomSheet`、`Skeleton`、`Liquid Glass` |
| 适合本项目吗 | **不适合，且理由比前两个更硬**：官方已经宣布 native-first、砍掉 web 适配层。现在押注它做 PC Web 是逆着项目方向走 |

---

### 1.4 NativeWind ✅

| 维度 | 结论 |
|---|---|
| 是什么 | 「Tailwind for React Native」——把 Tailwind 的 className 编译到 RN 样式 |
| 版本 | `4.2.7`（2026-09-14）为 latest ✅；**`5.0.0-rc.0` 于 2026-09-13 发布**（dist-tags: `rc`/`preview` = 5.0.0-rc.0）✅。官方 v5 文档页顶部明写「**Nativewind v5 pre-release … It is not intended for production use**」✅ |
| 维护状态 | GitHub 8106 star，最近 push **2026-09-15** ✅，58 open issue（很健康） |
| 下载量 | **2,088,897/周** ✅（本次调研所有 RN UI 相关包里最高，量级碾压） |
| Web 支持 | **真 Tailwind 编译**。官方 v5 文档原话：「Since Nativewind compiles your Tailwind CSS **at build time**, the full Tailwind CSS language is available. **The entire set of utilities, variants, functions, and directives will work on web.** On native, Nativewind applies the subset that React Native's style engine supports.」✅ v5 用 **Tailwind v4**（CSS-first，`@theme`，无 `tailwind.config.js`）✅ |
| 和 shadcn/ui 能共存/统一吗 | **能共存，但不能「统一」。** 关键事实：**NativeWind 在 web 上产出的就是 Tailwind CSS class**（因为它编译的就是 Tailwind）。所以你的 `<div className="...">` 天然共享同一份 Tailwind 产物 ✅。但注意三个坑：① 你的项目已经在用 Tailwind v4 给 Web 写 DOM，NativeWind v5 也用 Tailwind v4 —— **同一份 CSS 入口/PostCSS 管线需要合并，Metro 侧要插 NativeWind 的 preset**，配错会出现 web 端重复产出或 class 缺失；② NativeWind 的 `className` 只对 RN 组件（`View`/`Text`）生效，对你的 `<div>` 是普通 Tailwind（本来就这样，无需 NativeWind）；③ 本项目如果用 NativeWind，实际收益只在**未来移动端**，PC 端收益为 0 |
| 适合本项目吗 | **PC 端不需要它**（你已经直接写 Tailwind 了）。**未来做移动端时，它是本清单里唯一值得考虑的样式层**——因为它让你复用同一套 Tailwind token 和心智 |

---

### 1.5 React Native Elements / UI Kitten / Magnus UI / RNUI ✅

| 库 | 最新版 | 发布日期 | 状态判定 | 结论 |
|---|---|---|---|---|
| **React Native Elements** (`react-native-elements`) | 3.4.3 | **2022-12-23** | **死透了**。npm latest 停在 2022；`4.0.0-rc.2` 停在 2022-04 | ❌ 不要用 |
| ↳ 继任者 `@rneui/themed` / `@rneui/base` | 5.0.0 | **2026-01-19** | **半复活**。`5.0.0-beta.1` 2025-11 → 5.0.0 stable 2026-01 发布，下载 61,963/周 | ⚠️ 活着但节奏很慢，且 web 仍走 RNW |
| **UI Kitten** (`@ui-kitten/components`) | **6.1.3** | **2026-09-27** | **意外地活跃**。v6 系列 2026-09-26/27 连续发版（6.0.1→6.1.3），GitHub `akveo/react-native-ui-kitten` 10664 star、push 2026-09-30 | ⚠️ 不是死项目（这点和常见印象相反，已实际核实）。但：Eva Design System 视觉、web 走 RNW、下载仅 15,896/周 |
| ↳ 旧包 `react-native-ui-kitten` | 4.4.1 | 2020-02-24 | **已 deprecated: YES** ✅ | ❌ 官方弃用 |
| **Magnus UI** (`react-native-magnus`) | 1.0.63 | **2022-09-22** | **死透了**。下载 388/周（等于零） | ❌ 不要用 |
| ↳ npm 包名 `magnus-ui` | — | — | **404，不存在** ✅ | — |
| **RNUI** (`rnui`) | 0.0.1 | **2016-05-03** | **幽灵包**。10 年前发过 1 个版本，总版本数 1 | ❌ 不要用 |

**小结**：这四个里只有 UI Kitten 还活着（且是老树发新芽），其余全死或半死。但都不适合 PC 后台。

---

### 1.6 样式系统：@shopify/restyle vs Unistyles ✅

| 维度 | @shopify/restyle | react-native-unistyles |
|---|---|---|
| 是什么 | 类型安全、theme-driven 的 RN 样式系统（theme + variants + `styled()`），**无编译器** | 高性能 RN 样式系统，**有 Babel 插件 + C++ (Nitro) 运行时**，web 端有**自研 CSS parser** |
| 版本 | `2.4.5`（**2025-03-19**）✅ | `3.3.0`（**2026-07-10**）✅ |
| 维护状态 | GitHub **3425 star，最近 push 2026-09-29** ✅（活跃，但**发版极慢**：npm latest 2.4.5 停在 2025-03-19，已 18 个月未发新版）。下载 132,311/周 ✅ | GitHub 2957 star，push **2026-09-28** ✅，59 open issue。下载 267,799/周 ✅ |
| Web 支持 | 走 RNW（纯 props→style 对象） | **web 端不走 RNW 样式层**：官方文档原话「Unistyles Web is **independent from React Native Web**, utilizing a custom web parser that **directly generates CSS** from your StyleSheet definitions」✅，会生成 `.unistyles_xxx` class + media query（有官方 CSS 输出示例）✅。但**组件仍是 RNW 渲染的** |
| 硬性门槛 | 低 | **高**：要求 New Architecture + **RN ≥ 0.78.0** + `react-native-nitro-modules`，**不支持 Expo Go**，要 `expo prebuild` ✅ |
| 有 Table/DataGrid 吗 | 无（纯样式系统） | 无（纯样式系统） |
| 适合本项目吗 | **都不值得。** 它们是**样式系统，不是组件库**——你的 PC 后台已经有 Tailwind v4 + CSS，不需要再叠一层「RN 样式语言」。Unistyles 的 CSS 生成能力确实比 RNW 强，但它依然绑在 RNW 组件树上，**解决不了 onDrop / 伪类 / table 语义的问题** |

**关于「样式库值不值得」的直答**：在本项目现架构下**不值得**。它们唯一的未来价值是移动端——而移动端 2026 年的主流答案已经是 NativeWind（Tailwind 心智复用）。

---

### 1.7 FlashList（Shopify）vs @tanstack/react-virtual ✅

| 维度 | @shopify/flash-list | @tanstack/react-virtual |
|---|---|---|
| 是什么 | RN 高性能虚拟列表，v2 针对新架构重写 | Web 端无头虚拟化原语（不是组件，是 hook） |
| 版本 | `2.3.2`（**2026-06-10**）✅ | `3.14.13`（**2026-09-14**）✅ |
| 维护状态 | GitHub 7241 star，push **2026-09-15** ✅，204 open issue。**但要小心**：仓库名在 API 里是 `Shopify/flash-list`（大写 S），我查询 `Shopify/flash-list` 时返回正常 ✅ | TanStack 体系，push 活跃，同族 `@tanstack/react-table` 9.2.4（2026-08-28）✅ |
| 下载量 | **2,800,058/周** ✅ | **29,625,050/周** ✅（高一个数量级） |
| Web 能用吗 | **官方不承诺 web。** package.json 无 `react-native` 字段、无 `.web.js` 产物；peerDeps 是 `react-native: *`；v2 文档通篇讲 native 新架构（「Build for RN's new architecture」）✅。历史上 web 端有已知问题（inverted 方向、viewability 回调在 web 上首次加载不准）⚠️。**结论：FlashList 的 web 支持是「顺便能跑」而非「被支持」** | **本来就是 web 的**，官方支持，浏览器原生 scroll 容器兼容性最好 |
| 有 DataGrid 吗 | 无（是列表，不是表格） | 无（但 `@tanstack/react-table` 是**表格逻辑层**，两者是 shadcn/ui data-table 的标准搭配） |
| 适合本项目吗 | **不适合**。你的 PC 后台是宽屏表格（多列、横向滚动、列固定），FlashList 的虚拟化模型（单轴、item 高度估算）不适配，而且它连 web 支持都不承诺 | **这就是你该用的**。`@tanstack/react-table`（逻辑）+ `@tanstack/react-virtual`（虚拟化）+ shadcn/ui 的 `<table>` 是当前 web 生态的既定最优解 |

---

### 1.8 RN/Expo 生态有专门的 Table / DataGrid 吗？✅

**结论：这块在 RN 生态里基本是空白。** 实测结果：

| 包 | 最新版 | 发布日期 | 周下载 | 判定 |
|---|---|---|---|---|
| `react-native-table-component` | 1.2.2 | **2022-02-10** | 31,408 | ❌ **4 年半没更新**，纯静态表格，无虚拟化/排序/列宽 |
| `react-native-paper` 的 `DataTable` | 随 Paper 5.15.3 | 2026-05-26 | 516,940（整包） | ⚠️ 静态表格 + 分页，无虚拟化、无列固定、无拖拽列宽 |
| gluestack-ui `Table` | 随 v5.0.3 | 2026-06-25 | — | ⚠️ **alpha** ✅ |
| `expo-flash-datagrid` | 0.2.1 | 2026-04-01 | **26** | ❌ 每周 26 次下载 = 无人使用 |
| `fusion-table-react-native` | 1.0.2 | 2025-11-10 | — | ❌ 玩具级 |
| `react-data-table-component` | **8.11.0** | **2026-09-29** | 293,856 | ⚠️ **这是 Web 库，不是 RN 库**。注意它有 `@revivejs/react-data-table-component` 9.1.1（2026-04-05，React 19 维护分支）⚠️ |
| `@tanstack/react-table` | **9.2.4** | 2026-08-28 | 25,595,016 | ✅ **Web 生态标准答案** |

**直答**：**RN/Expo 生态没有成熟的 DataGrid 组件，这块确实是空白。** 原因很直白——RN 的渲染模型（无 `<table>`、无 CSS grid、无列宽拖拽、无文本选择/复制语义）本来就不适合表格类密集数据界面。所有认真做后台的人最后都落到 web 栈。

---

### 1.9 成熟的 Expo 后台管理模板 / dashboard starter？✅

**结论：没有 shadcn-admin 级别的。开源这边的 star 数量级差得很远。**

| 模板 | star | 最近 push | 判定 |
|---|---|---|---|
| `satnaing/shadcn-admin`（Web 侧对照物） | **15,457** | 2026-09-10 | ✅ 这才是「成熟」的量级 |
| `gluestack/gluestack-ui-starter-kits` | 252 | **2024-11-29** | ⚠️ **近 2 年没动** |
| `gluestack/expo-head-starter-kit` | 41 | 2024-07-10 | ❌ 停更 |
| `gluestack/gluestack-ui-head-starter-kits` | 3 | 2024-05-27 | ❌ 停更 |
| `ixartz/React-Native-Boilerplate` | 411 | **2025-08-31** | ⚠️ 13 个月没更新，且是 RN App 骨架（不含后台界面） |
| `whyuascii/gluestack-universal-react-monorepo` | 8 | 2026-08-04 | ❌ 个人玩具 |
| `Ohh-889/skyroc` | 793 | 2026-09-01 | ⚠️ 是 **Web 中后台 monorepo**，不是 Expo 后台 |
| `rogeriolaa/expo-router-example` | 0 | 2025-08-28 | ❌ 示例，非模板 |

gluestack 官方文档里列了 4 个 App 模板（Dashboard App / Kitchensink App / Todo App / Starter Kit）✅，但它们**不是独立开源仓库热度的形态**，且 v5 定位 native-first——不要期待它给你一个 PC 后台。

**直答**：**Expo/RN 生态目前没有一个「shadcn-admin 级」的后台模板。** 这也侧面证明了：认真做 PC 后台的人不用 Expo/RN 生态。

---

## 2. 全景速查表

| 库 | 最新版 | 发布日 | star | 周下载 | Web 路径 | Table/Grid | 对本项目 |
|---|---|---|---|---|---|---|---|
| **Tamagui** | 2.7.7 | 2026-08-15 | 14,208 | 233,819 | **真原子 CSS（编译期）** | ❌ | ❌ 第二套范式 |
| **React Native Paper** | 5.15.3 | 2026-05-26 | 14,466 | 516,940 | RNW | ⚠️ 静态 DataTable | ❌ |
| **gluestack-ui** | 5.0.3 | 2026-06-25 | 5,317 | 3,139 | **v5 已放弃 web/Next** | ⚠️ alpha | ❌ |
| **NativeWind** | 4.2.7（v5 rc） | 2026-09-14 | 8,106 | 2,088,897 | **真 Tailwind 编译** | ❌ | 🟡 仅未来移动端 |
| **React Native Elements** | 3.4.3 | **2022-12-23** | 25,872 | — | RNW | ❌ | ❌ 死 |
| ↳ `@rneui/themed` | 5.0.0 | 2026-01-19 | — | 61,963 | RNW | ❌ | ❌ |
| **UI Kitten** | 6.1.3 | **2026-09-27** | 10,664 | 15,896 | RNW | ❌ | ❌ |
| **Magnus UI** | 1.0.63 | **2022-09-22** | — | 388 | — | ❌ | ❌ 死 |
| **RNUI** | 0.0.1 | **2016-05-03** | — | — | — | ❌ | ❌ 幽灵 |
| **@shopify/restyle** | 2.4.5 | 2025-03-19 | 3,425 | 132,311 | RNW | ❌ | ❌ 18 个月未发版 |
| **react-native-unistyles** | 3.3.0 | 2026-07-10 | 2,957 | 267,799 | **自研 CSS 生成** | ❌ | ❌ 门槛高且不解决核心问题 |
| **@shopify/flash-list** | 2.3.2 | 2026-06-10 | 7,241 | 2,800,058 | ⚠️ 不承诺 web | ❌ | ❌ |
| **@tanstack/react-virtual** | 3.14.13 | 2026-09-14 | — | 29,625,050 | 原生 web | ✅（配 react-table） | ✅ **该用** |
| **@tanstack/react-table** | 9.2.4 | 2026-08-28 | — | 25,595,016 | 原生 web | ✅ | ✅ **该用** |
| `react-native-table-component` | 1.2.2 | **2022-02-10** | — | 31,408 | RNW | ⚠️ 静态 | ❌ 死 |
| `react-data-table-component` | 8.11.0 | 2026-09-29 | — | 293,856 | **Web 库** | ✅ | ⚠️ 可看，但 TanStack 更优 |

**环境基线（顺带核实）**：`expo` latest = **57.0.26**（SDK 58 已发 58.0.0 于 2026-09-29，正在预览）✅；`@expo/ui` = **57.0.21**（2026-09-29）✅；`react-native-web` = **0.21.3**（2026-09-25）✅；`tailwindcss` = **4.3.3**（2026-07-16）✅；`shadcn` CLI = **4.21.0**（2026-09-04）✅。

---

## 3. RNW 硬限制 —— 官方文档复核 ✅

你背景里列的限制，我逐条去 RNW 官方文档核对了（[react-native-web 0.21.3 文档](https://necolas.github.io/react-native-web/docs/react-native-compatibility/)，更新于 2026-09-25）：

| 你的说法 | 核实结果 |
|---|---|
| 列表组件「not optimized for the web」 | ✅ **原文吻合**。Lists 页原话：「**Warning!** The React Native list components are **not optimized for the web**. You may prefer to use external modules … e.g. RecyclerListView」 |
| 不支持伪类 | ✅ 成立。RNW 是 style 对象 → 运行时生成 class，没有 `:hover` / `:focus-visible` / `:nth-child` 的编写面（`Pressable` 提供的是 `hovered`/`focused` 状态回调，不是 CSS 伪类） |
| 无 table/tr/td role 映射 | ⚠️ RNW 的 role 映射表里没有 table 系列语义（其 Accessibility 文档的 role 集合是 RN 的那套），**你不会得到原生 `<table>` 语义** |
| `onDrop`/`onDragStart` 被静默丢弃 | ⚠️ 交叉印证成立。RNW 的 Interactions 文档列出的指针事件只有 `onClick` / `onClickCapture` / `onContextMenu` / `onPointer*`，**没有任何 HTML5 drag-and-drop 事件** → 传 `onDrop` 到 RN 组件上不会被转发到 DOM |

**这条结论很重要**：你的架构决策（Web 写 DOM）不是「过度保守」，而是**被官方文档正面支持的**。

---

## 4. 给本项目的明确建议

### 4.1 这堆 RN 库对本项目还有价值吗？

**在「Expo 壳 + Web 写 DOM + shadcn/ui」这个已定架构下：PC 后台侧价值为零，不要引入任何一个。**

理由（按重要性排序）：

1. **它们解决的问题和你已解决的问题是同一个，且你的解法更好。** Tamagui/Unistyles 的价值是「让 RN 代码也能在 web 上跑得好」。你**根本不写 RN 代码做 web**，所以这个价值对你恒等于 0。
2. **它们全都绕不开 RNW 的组件树。** 即使是 web 端样式做得最好的 Tamagui 和 Unistyles，最终产出的仍是 RNW 渲染的元素 + RN 事件语义。你列的四条硬限制（丢 drag 事件 / 无伪类 / 无 table 语义 / 列表不优化）**一条都不会因为换库而消失**。
3. **每一个都会引入第二套范式。** 你现在是「DOM + Tailwind v4 + shadcn」。加 Tamagui = 第二套样式语言 + Metro 编译器插件；加 Paper/gluestack = 第二套设计体系（Material/Eva/自有 token）；加 Unistyles = Babel 插件 + Nitro 原生模块 + 必须 prebuild（**放弃 Expo Go**）。对一个「讨厌过度设计和乱引依赖」的项目，这些都是净负担。
4. **它们都补不上你最缺的东西。** 你的核心需求是数据表格、hover、右键、拖拽——**RN 生态在 Table/DataGrid 上就是空白**（见 1.8），这块它一点忙都帮不上。

### 4.2 唯一有真实价值的场景：将来做移动端

如果你**以后要出移动端 App**，那么本清单里只有两个值得回头看：

| 场景 | 用什么 | 为什么 |
|---|---|---|
| 移动端样式层 | **NativeWind（届时用 5.x stable）** | 你在 PC 端已经用 Tailwind v4，NativeWind v5 也基于 Tailwind v4 且**在 web 上产出的就是 Tailwind class**（官方已核实）→ **同一套 token 和心智可以跨端复用**，这是唯一「复用」真实成立的路径 |
| 移动端组件层 | **先别选，届时时点再评估** | Tamagui（最活跃、真 CSS、MIT、组件面最宽）和 gluestack v5（native-first 转向已明确）是两个候选。**现在决定没有意义**，因为 2026 年底的 RN UI 格局变动很快（gluestack 半年内从 universal 转向 native-only） |

**明确不要做的事**：
- ❌ 不要为了「可能有的移动端」现在就引入 NativeWind/Tamagui——这是典型的投机性依赖。
- ❌ 不要因为「想少写点」而用 RN 组件库写 PC 界面（省下的代码量远小于踩坑成本）。
- ❌ 不要引入 `react-native-table-component`、`Magnus UI`、`RNUI`、旧的 `react-native-elements`——**已死**。
- ❌ 不要因为 gluestack 长得像 shadcn 就选它——**它的 v5 已经放弃 web**。

### 4.3 坚持现架构，补这几个而不是换库

| 需求 | 正解 |
|---|---|
| DataGrid（排序/筛选/分页/列显隐） | `@tanstack/react-table` 9.2.4 + shadcn/ui `data-table` |
| 大列表虚拟化 | `@tanstack/react-virtual` 3.14.13 |
| 右键菜单 | Radix `ContextMenu`（shadcn 有） |
| 拖拽 | `@dnd-kit/core`（DOM 原生 drag 事件语义，RNW 给不了） |
| 表单 | `react-hook-form` + `zod` + shadcn `Form` |
| 移动端（未来） | 届时再评估 NativeWind 5.x；**不要现在动** |

---

## 附：核实方法与可信度

- 版本 / 发布日期 / deprecate 标记 / 周下载：直接查 `registry.npmjs.org` 与 `api.npmjs.org/downloads/point/last-week`（2026-10-01 实测）**✅**
- star / 归档状态 / 最近 push / 最近 commit：`api.github.com`（2026-10-01 实测）**✅**
- Web 支持、编译器行为、授权条款：抓取官方文档 / 官方 README / package.json 字段**✅**
- 标注 ⚠️ 的少数条目：来自官方文档相对概括的表述或第三方页面，未逐条验证，已在文中标明
- 未能完全核实的一项：无。`Shopify/restyle` 初次查询时 API 一次性返回异常（后续重查确认：3425 star、push 2026-09-29、仓库正常），数据已按重查结果修正 ✅
