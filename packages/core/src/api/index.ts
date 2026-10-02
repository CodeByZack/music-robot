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
  ScrapeRequest,
  ScrapeQueryResult,
  SettingsResponse,
  Song,
  SongTags,
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
      update: (id: number, patch: { name?: string; description?: string; is_public?: boolean }) =>
        http.put<{ playlist: Playlist }>(`/api/playlists/${id}`, patch),
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
