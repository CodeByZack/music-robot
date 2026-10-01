/**
 * 后端 REST 契约的类型。
 *
 * ⚠️ **字段名是从 `src/server/routes/*.rs` 的 `json!` 里逐个抄出来的**，不是猜的。
 * 新增字段时请对着后端改，别照感觉补 —— 类型撒谎比没有类型更糟。
 *
 * 时序提示：`duration_ms` / `file_size` / `added_at` / `updated_at` 都是**毫秒**。
 */

/** 刮削状态。`pending` 只在新入库时出现；`processing` 期间重复触发会被跳过。 */
export type ScrapeStatus = 'pending' | 'processing' | 'done' | 'failed';

export type Role = 'admin' | 'user';

export interface User {
  id: number;
  username: string;
  role: Role;
  created_at?: number | null;
  last_login?: number | null;
}

export interface LoginResponse {
  token: string;
  token_type: string;
  /** 有效期秒数。**目前后端没有 refresh 接口**，过期只能重新登录。 */
  expires_in: number;
  user: User;
}

/** 曲目。列表和详情共用（详情多 `has_cover` 之类时按需扩展）。 */
export interface Song {
  id: number;
  album_id: number | null;
  title: string | null;
  artists: string | null;
  album_artist: string | null;
  year: number | null;
  genres: string | null;
  track: number | null;
  disc: number | null;
  duration_ms: number | null;
  bitrate_bps: number | null;
  format: string | null;
  file_size: number | null;
  scrape_status: ScrapeStatus;
  added_at?: number;
  updated_at?: number;
}

export interface Album {
  id: number;
  name: string;
  album_artist: string | null;
  year: number | null;
  has_cover?: boolean;
  updated_at?: number;
}

/** 歌手页用的专辑摘要（带现算的曲目数）。 */
export interface AlbumSummary {
  id: number;
  name: string;
  album_artist: string | null;
  year: number | null;
  song_count: number;
  updated_at?: number;
}

export interface ArtistSummary {
  id: number;
  name: string;
  song_count: number;
  album_count: number;
}

/** 分页列表的统一外壳。 */
export interface Page<T> {
  items: T[];
  page: number;
  page_size: number;
  total: number;
}

export interface Playlist {
  id: number;
  name: string;
  description: string | null;
  is_public: boolean;
  created_at?: number;
  updated_at?: number;
}

export interface PlaylistDetail extends Playlist {
  songs: Song[];
  total: number;
}

export interface HistoryEntry {
  id: number;
  song_id: number;
  played_at: number;
  duration_listened_ms: number | null;
}

export interface UserSettings {
  play_mode: string | null;
  volume: number | null;
}

/** 后台任务（扫描 / 刮削共用同一种形状）。 */
export type JobKind = 'scan' | 'scrape';
export type JobStatus = 'running' | 'done' | 'failed';

export interface Job {
  batch_id: string;
  kind: JobKind;
  status: JobStatus;
  started_at: number;
  finished_at: number | null;
  total: number;
  done: number;
  failed: number;
  skipped: number;
  /** 中文摘要，失败时是原因。 */
  message: string | null;
}

/**
 * `POST /api/scrape` 的请求体（**整个可选**）。
 *
 * - 不给 / `{}` → pending 队列（新入库的歌）
 * - `{mode:'failed'}` → 重刮失败项
 * - `{song_ids:[1,2]}` → 只听点名的，**可含已 done 的**（唯一的「重新刮削」入口）
 */
export interface ScrapeRequest {
  mode?: 'pending' | 'failed';
  song_ids?: number[];
}

export interface JobAccepted {
  batch_id: string;
  kind: JobKind;
  status: JobStatus;
}

/** 后端统一错误形状：`{ error: { code, message } }`。 */
export interface ApiErrorBody {
  error: { code: string; message: string; details?: unknown };
}
