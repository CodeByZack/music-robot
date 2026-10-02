/**
 * 左栏的封面编辑器：预览 + 选择 / 移除。
 *
 * ⚠️ 显示的必须是**文件内嵌封面**，所以走 `/api/songs/{id}/tags/cover`，
 * 而不是通用的 `/api/songs/{id}/cover` —— 后者**优先返回专辑表里那张**
 * （见 `routes::cover` 的数据来源表）。用错端点的话，用户以为在看文件的封面、
 * 实际看的是专辑的，换掉之后界面还不变（这个坑实测踩过）。
 */
import { useEffect, useRef, useState, type ReactNode } from 'react';

/** 与后端 `tags::MAX_COVER_BYTES` 对齐（前端先拦一下，报错更快）。 */
const MAX_COVER_BYTES = 8 * 1024 * 1024;

/**
 * 封面编辑器：预览 + 选择 / 移除，外加一个放「文件信息」的位置。
 *
 * ## 两种布局（同一份 DOM，只靠 flex 方向切）
 *
 * | 宽度 | 形态 |
 * |---|---|
 * | ≥760px | 竖排：大图 → 按钮 → 提示 → `children`（文件信息） |
 * | <760px | 横排：**图在左（104px）、按钮 + 提示 + 文件信息在右** |
 *
 * 为什么窄屏要横排：左栏在那时是满宽（几百像素），方形封面就跟着涨成几百像素高，
 * 把下面的字段全挤下去了 —— 而封面只是个参考图，不值得占那么大地方。
 * 横排之后它只占一行的高度。
 *
 * `children`（页面的封面 / 歌词 / 同步歌词 那三行）交给这里渲染，就是为了让它
 * 在窄屏能落到图的右侧填满空白，宽屏仍自然地在下方 —— 不用写两份 DOM。
 */
export function CoverEditor({
  songId,
  hasCover,
  nonce,
  picked,
  onPick,
  onRemove,
  onRefresh,
  children,
}: {
  songId: number;
  hasCover: boolean;
  /** 变化就重新取图（写盘成功后父组件 +1）。 */
  nonce: number;
  /** 本地选中的新封面（data URL）。 */
  picked: string | null;
  onPick: (dataUrl: string | null) => void;
  onRemove: () => void;
  onRefresh: () => void;
  /** 文件信息（封面 / 歌词 / 同步歌词）。窄屏落到图的右侧。 */
  children?: ReactNode;
}) {
  const fileRef = useRef<HTMLInputElement | null>(null);
  const [err, setErr] = useState<string | null>(null);
  // 文件里那张加载失败（没有内嵌封面时端点给 404）→ 显示占位
  const [fileCoverOk, setFileCoverOk] = useState(true);

  useEffect(() => {
    setFileCoverOk(true);
  }, [songId, nonce]);

  async function choose(file: File | null) {
    setErr(null);
    if (!file) return;
    if (file.size > MAX_COVER_BYTES) {
      setErr(`图片太大（${(file.size / 1048576).toFixed(1)} MB），上限 ${MAX_COVER_BYTES / 1048576} MB`);
      return;
    }
    // readAsDataURL 给的就是 `data:image/jpeg;base64,...`，后端直接收这个形状。
    const url = await new Promise<string>((resolve, reject) => {
      const r = new FileReader();
      r.onload = () => resolve(String(r.result));
      r.onerror = () => reject(new Error('读取图片失败'));
      r.readAsDataURL(file);
    }).catch((e: Error) => {
      setErr(e.message);
      return '';
    });
    if (url) onPick(url);
  }

  const showFileCover = hasCover && fileCoverOk;

  return (
    <div className="flex flex-col gap-2.5 max-[759px]:flex-row max-[759px]:items-start max-[759px]:gap-4">
      {/* 图。窄屏固定 104px：`max-*:` 变体在 Tailwind 产物里排在基础工具类之后，
          所以这里的宽度能盖住下面的 `w-full`（同一个元素上放两个**基础**宽度类才会争，
          那样谁生效取决于产物里的先后，已踩过）。 */}
      <div className="relative aspect-square w-full max-w-[210px] overflow-hidden rounded-xl bg-surface max-[759px]:w-[104px] max-[759px]:max-w-none max-[759px]:shrink-0 max-[759px]:rounded-lg">
        {picked ? (
          // 本地选中的图直接预览（不必等后端），改成什么一目了然
          <img src={picked} alt="新封面预览" className="size-full object-cover" />
        ) : showFileCover ? (
          <img
            src={`/api/songs/${songId}/tags/cover?v=${nonce}`}
            alt="文件内嵌封面"
            className="size-full object-cover"
            onError={() => setFileCoverOk(false)}
          />
        ) : (
          <div className="grid size-full place-items-center px-2 text-center text-cap leading-4 text-ink-4 max-[759px]:text-micro max-[759px]:leading-3.5">
            文件里没有内嵌封面
          </div>
        )}

        {/*
         * 「刷新」本来是混在下面那句说明里的一个链接，看着碎。它其实是
         * 「重新取这张图」—— 那就放到图自己身上。有图时才出现（没图时重取没意义）。
         * 半透明底 + 模糊，在浅色封面上也看得清。
         */}
        {showFileCover && (
          <button
            type="button"
            onClick={onRefresh}
            title="重新取这张封面"
            aria-label="重新取封面"
            className="absolute right-1.5 bottom-1.5 flex size-6 items-center justify-center rounded-full bg-black/55 text-ink-2 backdrop-blur-sm transition-colors hover:bg-black/75 hover:text-ink max-[759px]:size-5"
          >
            <svg
              className="ico-xs"
              viewBox="0 0 16 16"
              fill="none"
              stroke="currentColor"
              strokeWidth={1.6}
              strokeLinecap="round"
              strokeLinejoin="round"
            >
              <path d="M13 8a5 5 0 1 1-1.6-3.7" />
              <path d="M13.2 3.2v2.7h-2.7" />
            </svg>
          </button>
        )}
      </div>

      {/* 右侧（窄屏）/ 下方（宽屏）：按钮 + 提示 + 文件信息 */}
      <div className="flex min-w-0 flex-1 flex-col gap-2">
        <input
          ref={fileRef}
          type="file"
          accept="image/jpeg,image/png,image/gif"
          className="hidden"
          onChange={(e) => {
            void choose(e.target.files?.[0] ?? null);
            // 清空 value：不然选同一张图不会再触发 change
            e.target.value = '';
          }}
        />

        <div className="flex flex-wrap gap-2">
          <button
            type="button"
            onClick={() => fileRef.current?.click()}
            className="h-8 rounded-full bg-surface px-3 text-note text-ink-2 transition-colors hover:bg-surface-hover"
          >
            {picked || hasCover ? '更换封面' : '选择封面'}
          </button>
          {(picked || hasCover) && (
            <button
              type="button"
              onClick={() => {
                setErr(null);
                onPick(null);
                onRemove();
              }}
              className="h-8 rounded-full px-3 text-note text-ink-3 transition-colors hover:bg-surface-hover hover:text-ink"
            >
              移除封面
            </button>
          )}
        </div>

        {err && <p className="text-cap leading-4 text-accent">{err}</p>}
        {picked && <p className="text-cap leading-4 text-accent">预览的是新封面，写入后才生效。</p>}

        {children}

        {/**
         * 说明放**最后一行**（而不是夹在按钮与文件信息中间）。夹在中间时它占两行、
         * 把上下两块隔开，整体看着很乱；放到末尾它就只是一句脚注。
         * 「刷新」已移到图片角上（它本来就是「重取这张图」）。
         */}
        <p className="text-micro leading-4 text-ink-4">列表页可能显示专辑封面</p>
      </div>
    </div>
  );
}
