import { useEffect, useState } from 'react';
import { api } from '@/lib/client.ts';

/**
 * 封面。库里**不是每首都内嵌封面** —— 实测 8 首里 5 首有、3 首没有，
 * 端点对没有的返回 404。所以必须有 `onError` 兜底，否则满屏破图。
 *
 * ⚠️ 走 `<img src>` 的裸 GET：后端媒体端点用 `require_auth_media`（认登录 cookie），
 * 所以这里带不了 Authorization 头也没关系。
 */
export default function Cover({
  id,
  className = 'size-10',
  rounded = 'rounded-md',
  glyphClass = 'text-base',
}: {
  id: number;
  /** 尺寸相关的 class（宽高 / 圆角由调用处给）。 */
  className?: string;
  /** 圆角单独给 —— 因为要跟外层的 overflow 配合。 */
  rounded?: string;
  /** 占位字形的大小。 */
  glyphClass?: string;
}) {
  const [ok, setOk] = useState(true);
  // 换歌 / 换曲子时重置，否则下一首会沿用上一首的「破了」状态
  useEffect(() => setOk(true), [id]);

  return (
    <div className={`relative ${className} ${rounded} overflow-hidden bg-surface`}>
      {ok ? (
        <img
          src={api.library.coverUrl(id)}
          alt=""
          loading="lazy"
          draggable={false}
          onError={() => setOk(false)}
          className="size-full object-cover"
        />
      ) : (
        <span className={`grid size-full place-items-center text-ink-4 ${glyphClass}`}>♪</span>
      )}
    </div>
  );
}
