import React from 'react';
import { createRoot } from 'react-dom/client';
import App from './App.jsx';
import './app.css';

// 平台标记:Linux 上窗口外扩一圈透明边缘用于自绘阴影,mac 用系统阴影不需要
const isMac = /Mac/i.test(navigator.platform);
document.body.classList.add(isMac ? 'platform-mac' : 'platform-linux');

createRoot(document.getElementById('root')).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);
