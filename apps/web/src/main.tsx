import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { BrowserRouter } from 'react-router';
import App from '@/app.tsx';
import '@/global.css';

const root = document.getElementById('root');
if (!root) throw new Error('#root 不存在 —— index.html 被改坏了吧');

createRoot(root).render(
  <StrictMode>
    <BrowserRouter>
      <App />
    </BrowserRouter>
  </StrictMode>,
);
