import type { Http } from '../http.ts';
import type {
  Album,
  AlbumSummary,
  ArtistSummary,
  HistoryEntry,
  Job,
  JobAccepted,
  LoginResponse,
  Page,
  Playlist,
  PlaylistDetail,
  ScrapeRequest,
  Song,
  User,
  UserSettings,
} from '../types.ts';

/**
 * REST 端点的薄封装。
 *
 * **放一处的理由**：它们几乎是机械转写（路径 + 类型）。按路由组拆成 16 个文件只会多 16 次跳转，
 * 换不来任何东西。等某一组长出真实逻辑（分页游标、重试、缓存）再拆出去。
 *
 * 只实现**当前页面用得到的**。剩下的等页面来了再加 —— 先把 33 条全写完是提前付账。
 */
export function createApi(http: Http) {
  return {
    auth: {
      /** 仅在库为空时可用（初始化引导）；库非空返回 403。 */
      register: (username: string, password: string) =>
        http.post<{ user: User }>('/api/auth/register', { username, password }),
      login: (username: string, password: string) =>
        http.post<LoginResponse>('/api/auth/login', { username, password }),
      me: () => http.get<User>('/api/auth/me'),
      /** 管理员建号（`role` 可选，默认 user）。 */
      adminCreateUser: (username: string, password: string, role?: string) =>
        http.post<{ user: User }>('/api/admin/users', { username, password, ...(role ? { role } : {}) }),
    },

    library: {
      list: (params: { page?: number; page_size?: number; sort?: string } = {}) =>
        http.get<Page<Song>>(`/api/library${qs(params)}`),
      song: (id: number) => http.get<Song & { has_cover?: boolean }>(`/api/songs/${id}`),
      /** 封面是二进制，直接给 img 用这个 URL（要走令牌时由适配层处理）。 */
      coverUrl: (id: number) => `/api/songs/${id}/cover`,
      /** 音频流地址；支持 Range，播放器直接丢给 `<audio>`。 */
      streamUrl: (id: number) => `/api/stream/${id}`,
    },

    albums: {
      list: (params: { page?: number; page_size?: number } = {}) =>
        http.get<Page<Album>>(`/api/albums${qs(params)}`),
      get: (id: number) => http.get<{ album: Album; songs: Song[] }>(`/api/albums/${id}`),
    },

    artists: {
      list: () => http.get<{ items: ArtistSummary[]; total: number }>('/api/artists'),
      get: (id: number) =>
        http.get<{ artist: ArtistSummary; albums: AlbumSummary[]; songs: Song[] }>(`/api/artists/${id}`),
    },

    search: (q: string, limit?: number) =>
      http.get<{ songs: Song[]; albums: Album[]; artists: ArtistSummary[] }>(
        `/api/search${qs({ q, limit })}`,
      ),

    playlists: {
      list: () => http.get<{ items: Playlist[]; count: number }>('/api/playlists'),
      get: (id: number) => http.get<PlaylistDetail>(`/api/playlists/${id}`),
      create: (name: string, description?: string) =>
        http.post<Playlist>('/api/playlists', { name, ...(description ? { description } : {}) }),
      addSongs: (id: number, songIds: number[]) =>
        http.post<{ added: number }>(`/api/playlists/${id}/items`, { song_ids: songIds }),
      removeSong: (id: number, songId: number) =>
        http.del<{ removed: boolean }>(`/api/playlists/${id}/items/${songId}`),
      remove: (id: number) => http.del<{ deleted: boolean }>(`/api/playlists/${id}`),
    },

    favorites: {
      list: () => http.get<{ items: Song[]; total: number }>('/api/favorites'),
      add: (songId: number) => http.post<{ favorited: boolean }>('/api/favorites', { song_id: songId }),
      remove: (songId: number) => http.del<{ removed: boolean }>(`/api/favorites/${songId}`),
    },

    history: {
      list: (params: { limit?: number; offset?: number } = {}) =>
        http.get<{ items: HistoryEntry[]; total: number }>(`/api/history${qs(params)}`),
      record: (songId: number, durationListenedMs?: number) =>
        http.post<{ written: boolean }>('/api/history', {
          song_id: songId,
          ...(durationListenedMs === undefined ? {} : { duration_listened_ms: durationListenedMs }),
        }),
    },

    settings: {
      get: () => http.get<UserSettings>('/api/settings'),
      put: (patch: Partial<UserSettings>) => http.put<{ ok: boolean }>('/api/settings', patch),
    },

    /** 扫描与刮削共用同一套「后台任务」形状；`job(id)` 两个都能查。 */
    jobs: {
      jobs: () => http.get<{ items: Job[]; count: number }>('/api/jobs'),
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
