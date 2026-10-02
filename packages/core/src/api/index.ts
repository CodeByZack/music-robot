import type { Http } from '../http.ts';
import type {
  Album,
  AlbumSummary,
  ArtistName,
  ArtistRow,
  HistoryEntry,
  HistoryPage,
  Job,
  JobAccepted,
  LoginResponse,
  Page,
  Playlist,
  PlaylistDetail,
  RequestStatus,
  ScrapeRequest,
  ScrapeQueryResult,
  SettingsResponse,
  Song,
  SongRequest,
  SongTags,
  SubmitRequestResult,
  TagPatchRequest,
  TagPatchResult,
  User,
} from '../types.ts';

/**
 * REST 端点的薄封装。
 *
 * ⚠️ **每个方法的返回类型都是照 `src/server/routes/*.rs` 里的 `json!` 抄的**，
 * 不是照感觉写的。之前这文件里有好几处猜错的形状（`playlists.list` 写成 `count`、
 * `settings.get` 以为是扁平结构、`add_item` 以为收数组…），已全部对着后端改正。
 * **改这里之前先去看后端对应的 handler。**
 *
 * 只实现**当前页面用得到的**端点。先把 31 条全写完是提前付账。
 */
export function createApi(http: Http) {
  return {
    auth: {
      /** 仅在库为空时可用（初始化引导）；库非空返回 403。**不返回令牌**，之后要再调 login。 */
      register: (username: string, password: string) =>
        http.post<{ user: User }>('/api/auth/register', { username, password }),
      /** 登录。同时下发 `mr_media` cookie（`<audio>`/`<img>` 靠它鉴权）。 */
      login: (username: string, password: string) =>
        http.post<LoginResponse>('/api/auth/login', { username, password }),
      /** 清媒体 cookie。**不要求令牌**（过期后更需要能登出）。 */
      logout: () => http.post<{ ok: boolean }>('/api/auth/logout'),
      me: () => http.get<{ user: User }>('/api/auth/me'),
      adminCreateUser: (username: string, password: string, role?: string) =>
        http.post<{ user: User }>('/api/admin/users', {
          username,
          password,
          ...(role ? { role } : {}),
        }),
    },

    library: {
      /** `sort` 只支持「字段」或「-字段」（减号 = 降序）；`page_size` 硬上限 200。 */
      list: (params: { page?: number; page_size?: number; sort?: string } = {}) =>
        http.get<Page<Song>>(`/api/library${qs(params)}`),
      song: (id: number) => http.get<{ song: Song }>(`/api/songs/${id}`),
      /** 音频流地址。鉴权靠 cookie（`<audio src>` 带不了请求头）。 */
      streamUrl: (id: number) => `/api/stream/${id}`,
      /** 封面地址。同上，靠 cookie。 */
      coverUrl: (id: number) => `/api/songs/${id}/cover`,
    },

    /**
     * S26 标签编辑。
     *
     * ⚠️ **写文件是不可撤销的**（后端 `atomic_replace`：copy → tmp → verify → rename，
     * 没有备份）。所以两件事：
     * 1. `patch` 的 `dry_run` **默认为 true** —— 不传就只算差异，一个字节都不写；
     * 2. 写文件仅管理员可用（后端 `AdminUser`），非管理员调用会 403。
     */
    tags: {
      /** 当前**文件**标签（编辑器初值）。登录即可读。 */
      get: (songId: number) => http.get<SongTags>(`/api/songs/${songId}/tags`),
      /** 预览（默认）或写入。返回改动明细，界面直接展示，不用自己比字段。 */
      patch: (songId: number, body: TagPatchRequest) =>
        http.patch<TagPatchResult>(`/api/songs/${songId}/tags`, body),
      /**
       * **只查不写**地问插件这首歌该是什么标签（编辑页的「刮削」按钮）。
       *
       * 与 `jobs.startScrape` 完全不同：那条会真的覆盖原文件；这条什么都不改，
       * 只是拿回一份提议。仅管理员可用（会发起外部网络请求）。
       */
      queryScrape: (songId: number) =>
        http.post<ScrapeQueryResult>(`/api/songs/${songId}/scrape`, undefined),
    },

    albums: {
      /** 分页列表。每项带现算的 `song_count`，按 `name, album_artist` 排。 */
      list: (params: { page?: number; page_size?: number } = {}) =>
        http.get<Page<AlbumSummary>>(`/api/albums${qs(params)}`),
      get: (id: number) => http.get<{ album: Album; songs: Song[] }>(`/api/albums/${id}`),
    },

    artists: {
      /** 分页列表，按曲目数降序。 */
      list: (params: { page?: number; page_size?: number } = {}) =>
        http.get<Page<ArtistRow>>(`/api/artists${qs(params)}`),
      /**
       * 详情。⚠️ 参数是**歌手名**不是 id（路由是 `/api/artists/{name}`），
       * 且响应里的 `artist` 就是那个**字符串**名字，不是结构体。
       */
      get: (name: string) =>
        http.get<{ artist: ArtistName; songs: Song[]; albums: AlbumSummary[] }>(
          `/api/artists/${encodeURIComponent(name)}`,
        ),
    },

    search: (q: string, params: { page?: number; page_size?: number } = {}) =>
      http.get<Page<Song>>(`/api/search${qs({ q, ...params })}`),

    playlists: {
      list: () => http.get<{ items: Playlist[]; total: number }>('/api/playlists'),
      get: (id: number) => http.get<PlaylistDetail>(`/api/playlists/${id}`),
      create: (name: string, description?: string) =>
        http.post<{ playlist: Playlist }>('/api/playlists', {
          name,
          ...(description ? { description } : {}),
        }),
      /**
       * 部分更新：**字段缺席即不动**。
       *
       * ⚠️ `description: null` 有特殊语义 —— 它是「清空描述」（后端落成 NULL），
       * 与「缺席」（保持原值）**不是一回事**。所以这个字段的类型是 `string | null`
       * 而不是可选 string：省略键 = 别动，显式给 null = 清空。
       */
      update: (
        id: number,
        patch: { name?: string; description?: string | null; is_public?: boolean },
      ) => http.put<{ playlist: Playlist }>(`/api/playlists/${id}`, patch),
      remove: (id: number) => http.del<{ id: number; deleted: boolean }>(`/api/playlists/${id}`),
      /** ⚠️ 一次**只加一首**（`song_id` 是整数不是数组）；`position` 省略表示追加到末尾。 */
      addSong: (id: number, songId: number, position?: number) =>
        http.post<{ added: number; song_id: number; position: number }>(
          `/api/playlists/${id}/items`,
          { song_id: songId, ...(position === undefined ? {} : { position }) },
        ),
      removeSong: (id: number, songId: number) =>
        http.del<{ song_id: number; removed: boolean }>(`/api/playlists/${id}/items/${songId}`),
      reorder: (id: number, songIds: number[]) =>
        http.put<{ song_ids: number[]; count: number }>(`/api/playlists/${id}/items`, {
          song_ids: songIds,
        }),
    },

    favorites: {
      list: () => http.get<{ items: Song[]; total: number }>('/api/favorites'),
      /**
       * 幂等：已收藏再调一次不会重复插。
       * ⚠️ song_id 在**路径**里，不是请求体 —— 路由是 `POST /api/favorites/{song_id}`。
       * （写成 `POST /api/favorites` + body 会得到 405，实测踩过。）
       */
      add: (songId: number) =>
        http.post<{ song_id: number; favorited: boolean; created: boolean }>(
          `/api/favorites/${songId}`,
        ),
      remove: (songId: number) =>
        http.del<{ song_id: number; favorited: boolean; removed: boolean }>(
          `/api/favorites/${songId}`,
        ),
    },

    history: {
      list: (params: { limit?: number; offset?: number } = {}) =>
        http.get<HistoryPage>(`/api/history${qs(params)}`),
      record: (songId: number, durationListenedMs?: number) =>
        http.post<{ history: HistoryEntry }>('/api/history', {
          song_id: songId,
          ...(durationListenedMs === undefined ? {} : { duration_listened_ms: durationListenedMs }),
        }),
    },

    settings: {
      get: () => http.get<SettingsResponse>('/api/settings'),
      /** 批量写。值为 `null` 表示删除该键。 */
      put: (patch: Record<string, string | null>) =>
        http.put<SettingsResponse & { written: number; deleted: number }>('/api/settings', patch),
    },

    jobs: {
      list: () => http.get<{ items: Job[]; count: number }>('/api/jobs'),
      startScan: () => http.post<JobAccepted>('/api/scan'),
      scan: (batchId: string) => http.get<Job>(`/api/scan/${batchId}`),
      /**
       * 触发刮削。**不给 body = pending 队列**，与老客户端行为一致。
       * 传 `song_ids` 可以重刮已经 done 的歌（这是唯一的入口）。
       */
      startScrape: (body?: ScrapeRequest) => http.post<JobAccepted>('/api/scrape', body),
      scrape: (batchId: string) => http.get<Job>(`/api/scrape/${batchId}`),
    },

    /**
     * S23 点歌请求。
     *
     * 权限口径（后端 `routes::requests` 模块头有完整说明）：**普通用户无论带什么
     * query 都只看得到自己提交的**（`user_id` 只来自令牌，绝不从 query / body 读）；
     * `?status=` 是「全量按状态筛」，属于管理端能力 → 普通用户调用会 **403**。
     */
    requests: {
      /** 不带参：普通用户看自己的、admin 看全部（都按票数降序）。 */
      list: (params: { mine?: 0 | 1; status?: RequestStatus } = {}) =>
        http.get<{ items: SongRequest[]; total: number }>(`/api/requests${qs(params)}`),
      /**
       * 提交点歌。归一化（全半角 / 大小写 / 空白折叠）后同键的会**合并到已有请求**
       * 并给当前用户记一票，响应里的 `created` 区分是新建还是合并。
       * 只有 `title` 必填。
       */
      submit: (body: { title: string; artist?: string; album?: string; note?: string }) =>
        http.post<SubmitRequestResult>('/api/requests', body),
      /**
       * 改状态（**仅 admin**）。目标 `rejected` **必须**带非空 `reject_reason`，
       * 否则 400；非法流转（自环 / 从终态出发 / 跳步）也是 400。
       */
      update: (id: number, patch: { status: RequestStatus; reject_reason?: string }) =>
        http.patch<{ request: SongRequest }>(`/api/requests/${id}`, patch),
      /**
       * 关联到已有歌曲（**仅 admin**）——顺便把状态置 `done`，一步到位。
       * 歌曲不存在（含已软删）返 404。
       */
      link: (id: number, songId: number) =>
        http.post<{ request: SongRequest }>(`/api/requests/${id}/link`, { song_id: songId }),
      /**
       * 触发获取（**仅 admin**）—— 让 provider 插件去把音频下下来。
       *
       * ⚠️ 当前**恒 503**：`provider` 插件 kind 尚未实现（注册表只加载 `scraper`），
       * 后端**故意**不如实报错而不是假装成功。**别当 bug 修**。
       */
      fetch: (id: number) => http.post<{ ok: boolean }>(`/api/requests/${id}/fetch`),
    },

    /**
     * 管理端用户管理。
     *
     * 只有这两条：建号（注册关闭后**唯一**入口）与列出用户。**没有**改密码 /
     * 删号接口 —— 删号会牵动一堆 CASCADE，没需求就不做。
     */
    users: {
      /** 列出全部用户（**仅 admin**）。**不含 password_hash**（后端复用 `user_json`）。 */
      list: () => http.get<{ items: User[]; total: number }>('/api/admin/users'),
      /** 建号（**仅 admin**）。`role` 省略即普通用户。用户名重复 → 409。 */
      create: (username: string, password: string, role?: 'user' | 'admin') =>
        http.post<{ user: User }>('/api/admin/users', {
          username,
          password,
          ...(role ? { role } : {}),
        }),
    },
  };
}

export type Api = ReturnType<typeof createApi>;

/** 把 undefined / null / 空串剔掉再拼查询串，避免 `?limit=undefined`。 */
function qs(params: Record<string, unknown>): string {
  const parts = Object.entries(params)
    .filter(([, v]) => v !== undefined && v !== null && v !== '')
    .map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(String(v))}`);
  return parts.length ? `?${parts.join('&')}` : '';
}
