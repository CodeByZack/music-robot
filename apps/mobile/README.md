# apps/mobile —— 手机端（**尚未开始**）

这里现在是**空壳**，故意不装任何东西。

## 真开工那天做什么

```sh
npx create-expo-app@latest apps/mobile   # 装之前先问用户（NAS 资源有限）
```

然后：

- `app/` 用 Expo Router 文件路由
- `src/adapters/` 加 `token-store.native.ts` / `audio.native.ts` —— **这是唯一需要新写的接缝**
- 消费 `@music-robot/core`，**一行逻辑都不用改**
- 样式层届时时点再评估（NativeWind 5.x 届时可能已 stable；组件库别现在定 —— gluestack 半年内就从 universal 转向 native-only 了）

## 页面砍一半

手机是**消费端**，不是管理端。只做：曲库 / 搜索 / 专辑 / 歌手 / 播放 / 歌单 / 收藏 / 历史 / 点歌 / 断点续播。

**不做**：`scan` / `scrape` / `jobs` / `admin` —— 扫描刮削建号留在 PC。

## 开工前要先补的后端项

`token_expiry_hours` 默认 **24 小时**且**没有 refresh 接口**（只有 login/register/me）。
Web 上天天重登还能忍，**手机上天天输密码不能忍**。要么调大有效期，要么加 refresh token。
