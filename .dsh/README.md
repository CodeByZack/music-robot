# .dsh/ —— 项目级的 agent 配置

## skills/

这里的 skill 由 **DSH 按项目扫描**（`<项目根>/.dsh/skills/<名字>/SKILL.md`，
项目根 = 最近的含 `.git` 的祖先目录），会话启动时进 catalog，正文按需加载。

⚠️ **只扫一层**：`<名字>/SKILL.md` 或顶层 `<名字>.md`。
嵌套的 `**/SKILL.md`（比如 `skills/foo/bar/SKILL.md`）**不会被发现**，
所以别在这里再套一层目录。

另外：DSH **不读** `.claude/skills/`；项目级只有 `.dsh/skills/`（优先）
和 `.agents/skills/` 两个位置。

## 来源

`skills/expo-*` 与 `skills/eas-*` 是 **Expo 官方 skill**，
来自 https://github.com/expo/skills （MIT），安装于 2026-10-01：

    上游 commit: c0dadf355d4caa4e1720de372f0f8766df1a8978 (2026-09-28)
    源路径:      plugins/expo/skills/*  →  .dsh/skills/*

更新方式（会覆盖本地改动，改过就自己 merge）：

    git clone --depth 1 https://github.com/expo/skills.git /tmp/expo-skills
    cp -r /tmp/expo-skills/plugins/expo/skills/* .dsh/skills/
    rm -f .dsh/skills/README.md    # 顶层 .md 会被当成「扁平 skill」解析，要删掉

也装了全部 24 个，没有挑。它们的 `description` 合计约 11.6KB，
会出现在**每个会话**的 skill 目录里（正文不加载，只加载名字+描述）。
如果嫌占上下文，删掉与 Web 无关的即可 —— 对 Expo Web 前端最相关的是：
`expo-overview` / `expo-project-structure` / `expo-router` / `expo-dom` /
`expo-design-system` / `expo-data-fetching` / `expo-animation` / `expo-web-to-native`。
