/**
 * 歌手页 —— **做不了，缺后端接口**。
 *
 * 后端只有 `GET /api/artists/{name}`（详情，**参数是名字不是 id**），
 * 没有 `GET /api/artists`（列表）。所以前端列不出歌手。
 *
 * 另外 `/api/library` 的曲目只给 `artists` 字符串（可能是 `A / B` 这种多歌手），
 * 在客户端切分去重并不等价于后端的歌手实体 —— 那样做出来的列表和点进去的详情
 * 对不上（详情的路由参数是名字，还要 URL 编码）。宁可不做。
 */
export default function ArtistsPage() {
  return (
    <div className="flex-1 overflow-auto px-[22px] pt-2 pb-[130px] max-[1024px]:px-4">
      <div className="pt-2.5 pb-5">
        <h1 className="text-2xl leading-8 font-semibold tracking-[-.2px]">歌手</h1>
        <div className="mt-1 text-[13px] text-ink-3">未实现</div>
      </div>
      <div className="max-w-[74ch] rounded-lg bg-surface p-5 text-[13px] leading-6 text-ink-2">
        <p className="mb-3">
          <b className="text-accent">缺后端接口。</b>
          服务端只有 <code className="text-ink">GET /api/artists/{'{name}'}</code>（歌手详情，
          参数是<b>名字不是 id</b>），<b>没有列表接口</b>。
        </p>
        <p className="mb-2 text-ink-3">也不建议在前端硬凑：</p>
        <ul className="mb-3 list-disc space-y-1 pl-5 text-ink-3">
          <li>
            曲目里的 <code className="text-ink-2">artists</code> 是字符串（可能是{' '}
            <code className="text-ink-2">A / B</code> 这种多歌手），客户端切分去重
            <b>不等价于后端的歌手实体</b>
          </li>
          <li>凑出来的列表和点进去的详情会对不上</li>
        </ul>
        <p className="text-ink-3">
          需要后端加 <code className="text-ink-2">GET /api/artists?page=&amp;page_size=</code>
          （带曲目数 / 专辑数）。已记进 HANDOFF 待确认问题。
        </p>
      </div>
    </div>
  );
}
