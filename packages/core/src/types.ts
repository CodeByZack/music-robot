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
  /**
   * **专辑名**（不是列，是后端在出口处补的）。
   * 只在列表 / 详情 / 搜索 / 历史的响应里有；写回时后端忽略它。
   */
  album?: string | null;
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

/**
 * 歌手列表的一行。
 *
 * ⚠️ **没有 id** —— 后端把「歌手」当作 `songs.artists` 这个整串，
 * 详情接口的路径参数就是这个**名字**（`/api/artists/{name}`），要 encodeURIComponent。
 * 按曲目数降序。
 */
export interface ArtistRow {
  name: string;
  song_count: number;
  album_count: number;
}

/**
 * 歌手详情的 `artist` 字段是**字符串**（就是名字本身），不是上面那个结构。
 * 别想当然 —— 这里曾经写错过。
 */
export type ArtistName = string;

/** 分页列表的统一外壳（`paginated_json` 产出）。 */
export interface Page<T> {
  items: T[];
  page: number;
  page_size: number;
  total: number;
  total_pages: number;
}

export interface Playlist {
  id: number;
  name: string;
  description: string | null;
  is_public: boolean;
  created_at?: number;
  updated_at?: number;
}

/** `GET /api/playlists/{id}` → 歌单与曲目是**并列**的，不是一个扁平对象。 */
export interface PlaylistDetail {
  playlist: Playlist;
  songs: Song[];
}

export interface HistoryEntry {
  id: number;
  song_id: number;
  played_at: number;
  duration_listened_ms: number | null;
  /** 曲目摘要。**后端已经带上了**，不用前端再拿一次；软删的曲目这里是 null。 */
  song?: Song | null;
}

export interface HistoryPage {
  items: HistoryEntry[];
  limit: number;
  offset: number;
  total: number;
}

/**
 * 设置是**自由形态的字符串键值表**，不是固定结构。
 *
 * 后端 `settings_map` 直接把数据库里的行拼成 JSON 对象，已知的键有
 * `volume`（音量）、`play_mode`（播放模式）、`resume:<song_id>`（断点续播位置）。
 * 值一律是字符串 —— 别按数字处理。
 */
export type SettingsMap = Record<string, string>;

export interface SettingsResponse {
  settings: SettingsMap;
  total: number;
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
  /**
   * `false` = **只入库，绝不碰原文件**。缺省 `true`（与历史行为一致）。
   *
   * 必须是布尔：传 `"false"` 这种字符串后端会 400 —— 一个拼错的取值若被当成 true，
   * 就是「以为没写、其实覆盖了原文件」。
   */
  write_files?: boolean;
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
