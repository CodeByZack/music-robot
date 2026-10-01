import { Navigate, Route, Routes, useLocation } from 'react-router';
import PlayerBar from '@/components/player-bar.tsx';
import { Shell } from '@/components/shell.tsx';
import AlbumPage from '@/pages/album.tsx';
import AlbumsPage from '@/pages/albums.tsx';
import ArtistPage from '@/pages/artist.tsx';
import ArtistsPage from '@/pages/artists.tsx';
import FavoritesPage from '@/pages/favorites.tsx';
import HomePage from '@/pages/home.tsx';
import LibraryPage from '@/pages/library.tsx';
import LoginPage from '@/pages/login.tsx';
import NowPage from '@/pages/now.tsx';
import PlaylistPage from '@/pages/playlist.tsx';
import PlaylistsPage from '@/pages/playlists.tsx';
import SettingsPage from '@/pages/settings.tsx';
import { PlayerProvider } from '@/lib/player.tsx';
import { SessionProvider, useSession } from '@/lib/session.tsx';

/** 播放页自带一整套控件，底部悬浮条在那儿是重复的（原型同此处理）。 */
function PlayerBarOrNothing() {
  const { pathname } = useLocation();
  return pathname === '/now' ? null : <PlayerBar />;
}

function Gate() {
  const { user, ready } = useSession();

  // 没问完 /auth/me 之前先别渲染 —— 否则刷新页面会闪一下登录页
  if (!ready) {
    return <div className="grid h-full place-items-center text-[13px] text-ink-4">载入中…</div>;
  }
  if (!user) return <LoginPage />;

  return (
    <PlayerProvider>
      <Shell>
        <Routes>
          <Route path="/" element={<HomePage />} />
          <Route path="/library" element={<LibraryPage />} />
          <Route path="/albums" element={<AlbumsPage />} />
          <Route path="/albums/:id" element={<AlbumPage />} />
          <Route path="/artists" element={<ArtistsPage />} />
          <Route path="/artists/:name" element={<ArtistPage />} />
          <Route path="/playlists" element={<PlaylistsPage />} />
          <Route path="/playlists/:id" element={<PlaylistPage />} />
          <Route path="/favorites" element={<FavoritesPage />} />
          <Route path="/now" element={<NowPage />} />
          <Route path="/settings" element={<SettingsPage />} />
          <Route path="*" element={<Navigate to="/" replace />} />
        </Routes>
        <PlayerBarOrNothing />
      </Shell>
    </PlayerProvider>
  );
}

export default function App() {
  return (
    <SessionProvider>
      <Gate />
    </SessionProvider>
  );
}
