import { useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/tauri';
import { listen } from '@tauri-apps/api/event';

const KIND_LABEL = { text: '文本', html: '富文本', image: '图片', files: '文件' };

function formatTime(ms) {
  const d = new Date(ms);
  const p = (n) => String(n).padStart(2, '0');
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

export default function App() {
  const [items, setItems] = useState([]);
  const [query, setQuery] = useState('');
  const [error, setError] = useState(null);
  const [theme, setTheme] = useState(() => {
    const saved = localStorage.getItem('kp-theme');
    if (saved === 'light' || saved === 'dark') return saved;
    return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
  });

  // 初始加载 + 订阅剪贴板更新事件
  useEffect(() => {
    invoke('cmd_get_history')
      .then((list) => setItems(list || []))
      .catch((e) => console.error('加载历史失败:', e));

    // 按哈希去重:重复内容只保留置顶的最新一条
    const unlisten = listen('clipboard-updated', (event) => {
      const rec = event.payload;
      setItems((prev) => [rec, ...prev.filter((i) => i.hash !== rec.hash)].slice(0, 500));
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  // 主题应用与持久化(F1)
  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    localStorage.setItem('kp-theme', theme);
  }, [theme]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return items;
    return items.filter(
      (i) =>
        (i.text || '').toLowerCase().includes(q) ||
        (i.files || []).some((f) => f.toLowerCase().includes(q))
    );
  }, [items, query]);

  const pasteItem = (id) => {
    if (id == null) return;
    invoke('cmd_paste_item', { id }).catch((e) => {
      console.error('粘贴失败:', e);
      setError(String(e));
      window.setTimeout(() => setError(null), 3000);
    });
  };

  // 数字键 1-9 快速粘贴(B2)
  const filteredRef = useRef(filtered);
  filteredRef.current = filtered;
  useEffect(() => {
    const onKey = (e) => {
      if (e.ctrlKey || e.altKey || e.metaKey) return;
      const tag = (e.target && e.target.tagName) || '';
      if (tag === 'INPUT' || tag === 'TEXTAREA') return;
      const n = parseInt(e.key, 10);
      if (n >= 1 && n <= 9) {
        const rec = filteredRef.current[n - 1];
        if (rec) pasteItem(rec.id);
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);

  const clearAll = () => {
    invoke('cmd_clear_history').catch((e) => console.error(e));
    setItems([]);
  };

  return (
    <div className="app">
      <header className="toolbar">
        <span className="logo">KylinPaste</span>
        <button className="btn" title="切换明暗主题" onClick={() => setTheme(theme === 'dark' ? 'light' : 'dark')}>
          {theme === 'dark' ? '☀' : '☾'}
        </button>
        <button className="btn" title="清空历史" onClick={clearAll}>
          清空
        </button>
      </header>

      <div className="searchbar">
        <input
          type="text"
          placeholder="搜索剪贴板历史…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
      </div>

      <main className="list">
        {filtered.length === 0 && (
          <div className="empty">暂无记录,复制点内容试试 (Ctrl+C)</div>
        )}
        {filtered.slice(0, 100).map((rec, idx) => (
          <div
            key={rec.id}
            className="item"
            onClick={() => pasteItem(rec.id)}
            title="点击粘贴到上一个应用"
          >
            <span className={`num${idx < 9 ? '' : ' hidden'}`}>{idx < 9 ? idx + 1 : ''}</span>
            <span className="kind">{KIND_LABEL[rec.kind] || rec.kind}</span>
            <span className="content">{rec.text || ''}</span>
            <span className="time">{formatTime(rec.created_at)}</span>
          </div>
        ))}
      </main>

      <footer className="statusbar">
        {filtered.length} 条记录 · 数字键 1-9 快速粘贴 · Ctrl+Alt+V 唤起/隐藏
      </footer>

      {error && <div className="errorbar">粘贴失败: {error}</div>}
    </div>
  );
}
