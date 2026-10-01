import { Navigate, Route, Routes } from 'react-router';
import { Shell } from '@/components/shell.tsx';
import LibraryPage from '@/pages/library.tsx';
import LoginPage from '@/pages/login.tsx';
import { SessionProvider, useSession } from '@/lib/session.tsx';

/** 还没做的页面先用它占位 —— 比空白页好，也比假装做好了诚实。 */
function Todo({ what }: { what: string }) {
  return (
    <>
      <header className="h-[58px] shrink-0" />
      <div className="flex-1 px-[22px]">
        <h1 className="pt-2.5 text-2xl leading-8 font-semibold">{what}</h1>
        <p className="mt-3 max-w-[60ch] text-[13px] leading-5 text-ink-3">
          还没做。原型见 <code className="text-ink-2">apps/web/design/prototype.html</code>。
        </p>
      </div>
    </>
  );
}

function Gate() {
  const { user, ready } = useSession();

  // 没问完 /auth/me 之前先别渲染 —— 否则刷新页面会闪一下登录页
  if (!ready) {
    return <div className="grid h-full place-items-center text-[13px] text-ink-4">载入中…</div>;
  }
  if (!user) return <LoginPage />;

  return (
    <Shell>
      <Routes>
        <Route path="/" element={<Todo what="首页" />} />
        <Route path="/library" element={<LibraryPage />} />
        <Route path="/albums" element={<Todo what="专辑" />} />
        <Route path="/artists" element={<Todo what="歌手" />} />
        <Route path="/playlists" element={<Todo what="歌单" />} />
        <Route path="/favorites" element={<Todo what="收藏" />} />
        <Route path="/settings" element={<Todo what="设置" />} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Routes>
    </Shell>
  );
}

export default function App() {
  return (
    <SessionProvider>
      <Gate />
    </SessionProvider>
  );
}
