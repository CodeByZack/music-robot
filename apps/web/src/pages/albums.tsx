/**
 * 专辑页 —— **做不了，缺后端接口**。
 *
 * 后端只有 `GET /api/albums/{id}`（详情），**没有 `GET /api/albums`（列表）**。
 * 前端没法列出专辑：
 * - `/api/library` 的曲目只给 `album_id`，**不给专辑名**，连名字都显示不出来
 * - 按 album_id 去重能凑出 id 列表，但每个都要再打一次详情接口（N+1）
 *
 * 所以这里如实说明，不摆一个假页面。修法见 HANDOFF 的待确认问题。
 */
export default function AlbumsPage() {
  return (
    <div className="flex-1 overflow-auto px-[22px] pt-2 pb-[130px] max-[1024px]:px-4">
      <div className="pt-2.5 pb-5">
        <h1 className="text-2xl leading-8 font-semibold tracking-[-.2px]">专辑</h1>
        <div className="mt-1 text-[13px] text-ink-3">未实现</div>
      </div>
      <div className="max-w-[74ch] rounded-lg bg-surface p-5 text-[13px] leading-6 text-ink-2">
        <p className="mb-3">
          <b className="text-accent">缺后端接口。</b>
          服务端只有 <code className="text-ink">GET /api/albums/{'{id}'}</code>（专辑详情），
          <b>没有列表接口</b>，所以前端列不出专辑。
        </p>
        <p className="mb-2 text-ink-3">而且连"凑"都凑不出来：</p>
        <ul className="mb-3 list-disc space-y-1 pl-5 text-ink-3">
          <li>
            <code className="text-ink-2">/api/library</code> 的曲目只给{' '}
            <code className="text-ink-2">album_id</code>，<b>不给专辑名</b>
          </li>
          <li>按 id 去重能拿到 id 列表，但每个都要再打一次详情 → N+1 请求</li>
        </ul>
        <p className="text-ink-3">
          需要后端加 <code className="text-ink-2">GET /api/albums?page=&amp;page_size=</code>
          （带曲目数、按 album_artist/year 排序）。已记进 HANDOFF 待确认问题。
        </p>
      </div>
    </div>
  );
}
