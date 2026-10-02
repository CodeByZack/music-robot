import { useCallback, useEffect, useMemo, useState } from 'react';
import { Link, useNavigate, useParams } from 'react-router';
import type { Playlist, PlaylistDetail, Song } from '@music-robot/core';
import { Dialog } from '@/components/dialog.tsx';
import SongTable from '@/components/song-table.tsx';
import { api } from '@/lib/client.ts';
import { messageOf } from '@/lib/session.tsx';
import { ErrorNote, LoadingNote, useAsync } from '@/lib/use-async.tsx';

/**
 * 歌单详情：看曲目 + 属主能改（改名 / 描述 / 可见性 / 排序 / 移除曲目 / 删歌单）。
 *
 * # 为什么所有编辑入口都挂在 `is_owner` 上
 *
 * 后端权限是「看得见就能读，只有 owner 能改」（见 `routes::playlists.rs` 头注释），
 * 而**别人的公开歌单你也能打开**。所以这一页对非属主必须是**只读**的：
 * 不是「点了才 403」，而是**根本不画那些按钮** —— 后端返回的 `is_owner`
 * 就是为这件事准备的（它刻意不回 `user_id`，只回答「是不是你的」）。
 *
 * # 排序：为什么既有拖拽又有「上移 / 下移」
 *
 * 画布写的是「拖动排序」，所以拖拽是主路径。但 **HTML5 拖拽在触屏上完全不工作**，
 * 键盘用户也拖不了，所以 ⋯ 菜单里另给了上下移动。两者调同一个 `move()`。
 *
 * 排序是**乐观更新**：浏览器里先把顺序换过来（拖动必须有即时反馈，等一个来回
 * 才动会让人以为没生效），再调接口，最后以服务端返回的顺序为准。
 * 失败就丢掉本地顺序并报错 —— 不能让人对着一个假顺序继续操作。
 */
export default function PlaylistPage() {
  const { id } = useParams<{ id: string }>();
  const pid = Number(id);
  const navigate = useNavigate();
  const load = useCallback(() => api.playlists.get(pid), [pid]);
  const { data, error, loading, reload } = useAsync<PlaylistDetail>(load);

  // ⚠️ 后端返回的是 { playlist, songs } —— 歌单与曲目**并列**，不是一个扁平对象
  const playlist = data?.playlist;
  const songs = data?.songs ?? [];
  const isOwner = playlist?.is_owner ?? false;

  const [editing, setEditing] = useState(false);
  const [confirmingDelete, setConfirmingDelete] = useState(false);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  /**
   * 本地顺序（song id 列表）。`null` = 用服务端给的顺序。
   *
   * 只在拖动的那一瞬存在：拿到新数据（或失败回滚）就清掉，避免和 `songs` 打架。
   */
  const [order, setOrder] = useState<number[] | null>(null);
  // 服务端数据一变就交还控制权 —— reload() 之后界面必须是服务端的真相。
  useEffect(() => {
    setOrder(null);
  }, [data]);

  const rows = useMemo(() => {
    if (!order) return songs;
    const byId = new Map(songs.map((s) => [s.id, s]));
    // 查不到的 filter 掉：`order` 是乐观值，可能和 songs 短暂不一致
    return order.map((n) => byId.get(n)).filter((s): s is Song => s !== undefined);
  }, [order, songs]);

  /** 统一收口：报错就地显示，返回是否成功（调用方据此决定回滚 / 跳转）。 */
  async function run(fn: () => Promise<unknown>): Promise<boolean> {
    setBusy(true);
    setErr(null);
    try {
      await fn();
      return true;
    } catch (e) {
      setErr(messageOf(e));
      return false;
    } finally {
      setBusy(false);
    }
  }

  /** 把第 from 首挪到第 to 位。拖拽与「上移 / 下移」共用这一份。 */
  function move(from: number, to: number) {
    if (to < 0 || to >= rows.length || from === to) return;
    const ids = rows.map((s) => s.id);
    const moved = ids[from];
    if (moved === undefined) return;
    ids.splice(from, 1);
    ids.splice(to, 0, moved);
    setOrder(ids);
    void run(async () => {
      await api.playlists.reorder(pid, ids);
      reload();
    }).then((ok) => {
      // 失败时把乐观顺序撤掉，否则界面停在一个服务端并没有保存的顺序上
      if (!ok) setOrder(null);
    });
  }

  async function removeSong(song: Song) {
    await run(async () => {
      await api.playlists.removeSong(pid, song.id);
      reload();
    });
  }

  async function removePlaylist() {
    const ok = await run(() => api.playlists.remove(pid));
    // 删成功就回列表页 —— 留在一个已经 404 的详情页上没有意义
    if (ok) navigate('/playlists');
  }

  return (
    <div className="flex-1 overflow-auto px-5 pt-2 pb-32 max-[1024px]:px-4 max-[640px]:px-3">
      <div className="pt-2.5 pb-5">
        <Link to="/playlists" className="text-nav text-ink-3 hover:text-ink">
          ← 歌单
        </Link>
        <div className="mt-2 flex flex-wrap items-end gap-4">
          <div className="min-w-0">
            <h1 className="text-2xl leading-8 font-semibold tracking-[-.2px] max-[640px]:text-xl">
              {playlist?.name ?? (loading ? '读取中…' : '歌单')}
            </h1>
            <div className="mt-1.5 flex flex-wrap items-center gap-2 text-nav text-ink-3">
              {/* 别人的公开歌单：把「为什么没有编辑按钮」说出来，别让人以为是坏的 */}
              {playlist && !isOwner && (
                <span className="rounded-full bg-surface px-2.5 py-1 text-micro text-ink-2">
                  别人的歌单 · 只读
                </span>
              )}
              <span className="rounded-full bg-surface px-2.5 py-1 text-micro text-ink-3">
                {playlist?.is_public ? '公开' : '私有'}
              </span>
              {playlist?.description && <span>{playlist.description}</span>}
              {songs.length > 0 && <span>· {songs.length} 首</span>}
            </div>
          </div>
          <span className="flex-1" />
          {isOwner && (
            <div className="flex shrink-0 gap-2">
              <button
                type="button"
                onClick={() => setEditing(true)}
                className="h-9 rounded-full bg-surface px-4 text-nav transition-colors hover:bg-surface-hover"
              >
                编辑信息
              </button>
              <button
                type="button"
                onClick={() => setConfirmingDelete(true)}
                className="h-9 rounded-full px-4 text-nav text-accent transition-colors hover:bg-accent-soft"
              >
                删除歌单
              </button>
            </div>
          )}
        </div>
      </div>

      {err && <p className="mb-4 rounded-md bg-accent-soft px-4 py-3 text-note text-accent">{err}</p>}

      {error ? (
        <ErrorNote message={error} />
      ) : loading ? (
        <LoadingNote />
      ) : (
        <>
          {/* 拖拽只对属主开放：别人拖了也存不下去（后端 403），不如不给这个手势 */}
          {isOwner && songs.length > 1 && (
            <p className="mb-2.5 text-cap text-ink-4">
              拖动行可以调整顺序，也可以用每行 ⋯ 菜单里的「上移 / 下移」。
            </p>
          )}
          <SongTable
            songs={rows}
            onRemove={isOwner ? (s) => void removeSong(s) : undefined}
            onReorder={isOwner ? move : undefined}
          />
        </>
      )}

      {editing && playlist && (
        <EditDialog
          playlist={playlist}
          onClose={() => setEditing(false)}
          onDone={() => {
            setEditing(false);
            reload();
          }}
        />
      )}

      {confirmingDelete && playlist && (
        <DeleteDialog
          playlist={playlist}
          busy={busy}
          onClose={() => setConfirmingDelete(false)}
          onConfirm={() => void removePlaylist()}
        />
      )}
    </div>
  );
}

/**
 * 改名 / 改描述 / 改可见性。**三个字段一起发** —— 这张表就是「一次改完」，
 * 没必要学后端那样逐字段缺席即不动。
 */
function EditDialog({
  playlist,
  onClose,
  onDone,
}: {
  playlist: Playlist;
  onClose: () => void;
  onDone: () => void;
}) {
  const [name, setName] = useState(playlist.name);
  const [description, setDescription] = useState(playlist.description ?? '');
  const [isPublic, setIsPublic] = useState(playlist.is_public);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const canSubmit = name.trim().length > 0;

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (!canSubmit || busy) return;
    setBusy(true);
    setErr(null);
    try {
      const trimmed = description.trim();
      await api.playlists.update(playlist.id, {
        name: name.trim(),
        // 空描述发 **null 而不是空串**：后端的 null 落成 NULL，空串会存成 ''。
        // 两种「没有描述」在库里长得不一样，没必要引入第二种。
        description: trimmed === '' ? null : trimmed,
        is_public: isPublic,
      });
      onDone();
    } catch (e2) {
      setErr(messageOf(e2));
    } finally {
      setBusy(false);
    }
  }

  const INPUT =
    'h-9 w-full rounded-lg border-0 bg-surface px-3 text-nav text-ink outline-0 placeholder:text-ink-4';

  return (
    <Dialog title="编辑歌单" onClose={onClose}>
      <form onSubmit={submit} className="p-4">
        <div className="flex flex-col gap-2">
          <input
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="歌单名"
            className={INPUT}
          />
          <input
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            placeholder="描述（可留空）"
            className={INPUT}
          />
          <div className="flex rounded-full bg-surface p-0.5">
            {(
              [
                [false, '私有'],
                [true, '公开'],
              ] as const
            ).map(([val, label]) => (
              <button
                key={label}
                type="button"
                onClick={() => setIsPublic(val)}
                className={[
                  'flex-1 rounded-full px-3 py-1.5 text-cap transition-colors',
                  isPublic === val ? 'bg-surface-press text-ink' : 'text-ink-3 hover:text-ink',
                ].join(' ')}
              >
                {label}
              </button>
            ))}
          </div>
        </div>
        <p className="mt-2.5 text-micro leading-4 text-ink-4">
          {isPublic
            ? '所有人都能看到这个歌单，但仍然只有你能改。'
            : '只有你自己能看到这个歌单。'}
        </p>
        {err && <p className="mt-2.5 text-cap text-accent">{err}</p>}
        <div className="mt-4 flex gap-2">
          <button
            type="submit"
            disabled={!canSubmit || busy}
            className="h-9 rounded-full bg-accent px-4 text-nav font-medium text-white transition-colors hover:brightness-110 disabled:opacity-40"
          >
            {busy ? '保存中…' : '保存'}
          </button>
          <button
            type="button"
            onClick={onClose}
            className="h-9 rounded-full px-4 text-nav text-ink-3 transition-colors hover:bg-surface-hover hover:text-ink"
          >
            取消
          </button>
        </div>
      </form>
    </Dialog>
  );
}

/**
 * 删除确认。**必须确认** —— 后端 `playlist_items` 是 `ON DELETE CASCADE`，
 * 条目直接跟着消失，没有回收站、撤不回来。所以文案把后果写清（含「歌曲文件本身
 * 不受影响」这句 —— 那是用户真正会担心的），而不是只问一句「确定吗」。
 */
function DeleteDialog({
  playlist,
  busy,
  onClose,
  onConfirm,
}: {
  playlist: Playlist;
  busy: boolean;
  onClose: () => void;
  onConfirm: () => void;
}) {
  return (
    <Dialog title="删除歌单" onClose={onClose}>
      <div className="p-4">
        <p className="text-nav text-ink-2">确定删除「{playlist.name}」吗？</p>
        <p className="mt-1.5 text-cap leading-4 text-ink-4">
          歌单里的曲目会一起从这个歌单里移除，<b className="font-medium">歌曲文件本身不受影响</b>
          。此操作不可撤销。
        </p>
        <div className="mt-4 flex gap-2">
          <button
            type="button"
            onClick={onConfirm}
            disabled={busy}
            className="h-9 rounded-full bg-accent px-4 text-nav font-medium text-white transition-colors hover:brightness-110 disabled:opacity-40"
          >
            {busy ? '删除中…' : '删除'}
          </button>
          <button
            type="button"
            onClick={onClose}
            className="h-9 rounded-full px-4 text-nav text-ink-3 transition-colors hover:bg-surface-hover hover:text-ink"
          >
            取消
          </button>
        </div>
      </div>
    </Dialog>
  );
}
