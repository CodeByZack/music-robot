import { useState } from 'react';
import { Popover } from '@/components/popover.tsx';
import { RequestsPanel } from '@/components/manage/requests.tsx';
import { UsersPanel } from '@/components/manage/users.tsx';
import { useSession } from '@/lib/session.tsx';

/**
 * 顶栏右上角的管理入口 —— 点歌请求 + 用户，**都在这一层浮层里，不占页面**。
 *
 * 为什么是浮层而不是页面（用户 2026-10-02 明确要求）：
 * 这两件事都是「顺手处理一下」——有人点歌了就去点两下、要加个人就填一张表。
 * 没事的时候它们不该在导航里各占一格。真正的**需求侧入口在搜索页**
 * （搜不到 → 「请求这首歌」），那边是用户主路径，才值得占页面。
 *
 * 顶栏**只加一个图标**：`docs/design.md` §8 明确警告过「顶栏那排图标按钮一多就退回
 * 后台管理系统」，所以这里用 tab 把两个功能收进一个入口，而不是并排两个图标。
 */
export function ManageMenu() {
  const { user } = useSession();
  const isAdmin = user?.role === 'admin';
  const [tab, setTab] = useState<'requests' | 'users'>('requests');

  // 非管理员看不到 tab（他只有一个分节），标题直接用「我的点歌」——
  // 普通用户拿到的列表**永远只有自己提交的**，写「点歌请求」会让他以为能看到全量。
  const active = isAdmin ? tab : 'requests';

  return (
    <Popover
      label={isAdmin ? '点歌请求与用户管理' : '我的点歌'}
      title={isAdmin ? '管理' : '我的点歌'}
      width={360}
      trigger={
        /* 收件箱 —— 「收到的需求」。比握手 / 加号之类更贴语义。 */
        <svg
          className="ico-md"
          viewBox="0 0 16 16"
          fill="none"
          stroke="currentColor"
          strokeWidth={1.5}
          strokeLinecap="round"
          strokeLinejoin="round"
        >
          <path d="M2 9.4 4 3.4h8l2 6v3.2a.9.9 0 0 1-.9.9H2.9a.9.9 0 0 1-.9-.9z" />
          <path d="M2 9.4h3.6l.7 1.9h3.4l.7-1.9H14" />
        </svg>
      }
    >
      {isAdmin ? (
        <div className="flex gap-1 border-b border-line-weak px-2.5 pt-2.5">
          {(
            [
              ['requests', '点歌请求'],
              ['users', '用户'],
            ] as const
          ).map(([id, label]) => (
            <button
              key={id}
              type="button"
              onClick={() => setTab(id)}
              className={[
                'rounded-t-md border-b-2 px-3 py-1.5 text-nav transition-colors',
                tab === id
                  ? 'border-accent text-ink'
                  : 'border-transparent text-ink-3 hover:text-ink',
              ].join(' ')}
            >
              {label}
            </button>
          ))}
        </div>
      ) : (
        <div className="border-b border-line-weak px-3.5 py-2">
          <b className="text-nav font-medium">我的点歌</b>
        </div>
      )}

      {/* 两个面板在同一个位置条件渲染 —— React 看到组件类型变了会**卸载旧的、挂载新的**，
          所以切 tab 回来时列表会重新拉一遍。这正是想要的（数据新鲜），
          也就不需要额外的刷新按钮：关掉重开、切 tab 都会重拉。 */}
      {active === 'requests' ? <RequestsPanel /> : <UsersPanel />}
    </Popover>
  );
}
