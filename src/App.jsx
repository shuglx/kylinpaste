import { useEffect, useMemo, useRef, useState } from 'react';
import { convertFileSrc, invoke } from '@tauri-apps/api/tauri';
import { listen } from '@tauri-apps/api/event';

// ---------------------------------------------------------------- 图标

const stroke = {
  fill: 'none',
  stroke: 'currentColor',
  strokeWidth: 1.8,
  strokeLinecap: 'round',
  strokeLinejoin: 'round',
};

const IconSearch = () => (
  <svg width="23" height="23" viewBox="0 0 24 24" {...stroke}>
    <circle cx="11" cy="11" r="7" />
    <path d="M16.5 16.5 21 21" />
  </svg>
);

const IconPin = () => (
  <svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor">
    <path d="M16 12V4h1V2H7v2h1v8l-2 2v2h5.2v6h1.6v-6H18v-2z" />
  </svg>
);

const IconGear = () => (
  <svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor">
    <path d="M19.14 12.94c.04-.3.06-.61.06-.94s-.02-.64-.07-.94l2.03-1.58a.49.49 0 0 0 .12-.61l-1.92-3.32a.49.49 0 0 0-.59-.22l-2.39.96a7.03 7.03 0 0 0-1.62-.94l-.36-2.54a.48.48 0 0 0-.48-.41h-3.84c-.24 0-.43.17-.47.41l-.36 2.54c-.59.24-1.13.57-1.62.94l-2.39-.96a.49.49 0 0 0-.59.22L2.74 8.87c-.12.21-.08.47.12.61l2.03 1.58c-.05.3-.09.63-.09.94s.02.64.07.94l-2.03 1.58a.49.49 0 0 0-.12.61l1.92 3.32c.12.22.37.29.59.22l2.39-.96c.5.38 1.03.7 1.62.94l.36 2.54c.05.24.24.41.48.41h3.84c.24 0 .44-.17.47-.41l.36-2.54c.59-.24 1.13-.57 1.62-.94l2.39.96c.22.08.47 0 .59-.22l1.92-3.32a.49.49 0 0 0-.12-.61zM12 15.6A3.6 3.6 0 1 1 15.6 12 3.6 3.6 0 0 1 12 15.6z" />
  </svg>
);

/** 行内/顶栏图标统一规格:18×18、stroke 2(与 ref 项目同源的 Tabler 图标一致) */
const iconStroke = { ...stroke, strokeWidth: 2 };

/** 清理临时记录:Tabler `trash-x`(带 X 的垃圾桶)。
 *  Tabler 没有 broom/毛刷这类图标(`brush` 是长柄笔刷),而 ref 项目给"清空剪贴板历史"
 *  用的就是这个图标,直接沿用(带 X 的垃圾桶和行内那个普通垃圾桶也区分得开)。 */
const IconClean = () => (
  <svg width="18" height="18" viewBox="0 0 24 24" {...iconStroke}>
    <path d="M4 7h16" />
    <path d="M5 7l1 12a2 2 0 0 0 2 2h8a2 2 0 0 0 2 -2l1 -12" />
    <path d="M9 7v-3a1 1 0 0 1 1 -1h4a1 1 0 0 1 1 1v3" />
    <path d="M10 12l4 4m0 -4l-4 4" />
  </svg>
);

/** 收藏:Tabler `star`(描边与实心共用一条路径,只切 fill,收藏时不会跳形) */
const IconStar = ({ filled }) => (
  <svg width="18" height="18" viewBox="0 0 24 24" {...iconStroke} fill={filled ? 'currentColor' : 'none'}>
    <path d="M12 17.75l-6.172 3.245l1.179 -6.873l-5 -4.867l6.9 -1l3.086 -6.253l3.086 6.253l6.9 1l-5 4.867l1.179 6.873z" />
  </svg>
);

/** 分组:Tabler `tag` */
const IconTag = () => (
  <svg width="18" height="18" viewBox="0 0 24 24" {...iconStroke}>
    <path d="M7.5 7.5m-1 0a1 1 0 1 0 2 0a1 1 0 1 0 -2 0" />
    <path d="M3 6v5.172a2 2 0 0 0 .586 1.414l7.71 7.71a2.41 2.41 0 0 0 3.408 0l5.592 -5.592a2.41 2.41 0 0 0 0 -3.408l-7.71 -7.71a2 2 0 0 0 -1.414 -.586h-5.172a3 3 0 0 0 -3 3z" />
  </svg>
);

/** 删除:Tabler `trash` */
const IconTrash = () => (
  <svg width="18" height="18" viewBox="0 0 24 24" {...iconStroke}>
    <path d="M4 7l16 0" />
    <path d="M10 11l0 6" />
    <path d="M14 11l0 6" />
    <path d="M5 7l1 12a2 2 0 0 0 2 2h8a2 2 0 0 0 2 -2l1 -12" />
    <path d="M9 7v-3a1 1 0 0 1 1 -1h4a1 1 0 0 1 1 1v3" />
  </svg>
);

/** 下拉箭头 */
const IconChevron = () => (
  <svg width="12" height="12" viewBox="0 0 24 24" {...stroke} strokeWidth={2.4}>
    <path d="m6 9 6 6 6-6" />
  </svg>
);

const IconDoc = () => (
  <svg width="20" height="20" viewBox="0 0 24 24" {...stroke}>
    <path d="M7 3h7l4 4v13a1 1 0 0 1-1 1H7a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1z" />
    <path d="M14 3v4h4" />
  </svg>
);

const IconImage = () => (
  <svg width="20" height="20" viewBox="0 0 24 24" {...stroke}>
    <rect x="3.5" y="5" width="17" height="14" rx="2.5" />
    <circle cx="9" cy="10" r="1.5" />
    <path d="m5 17 4.5-4.5L14 17l2.5-2.5L20 18" />
  </svg>
);

const IconFolder = () => (
  <svg width="20" height="20" viewBox="0 0 24 24" {...stroke}>
    <path d="M3 7.5A2.5 2.5 0 0 1 5.5 5h3l1.7 2H18.5A2.5 2.5 0 0 1 21 9.5v7A2.5 2.5 0 0 1 18.5 19h-13A2.5 2.5 0 0 1 3 16.5z" />
  </svg>
);

const IconLink = () => (
  <svg width="20" height="20" viewBox="0 0 24 24" {...stroke}>
    <path d="M10.5 13.5a3.6 3.6 0 0 0 5.1 0l3-3a3.6 3.6 0 0 0-5.1-5.1l-.9.9" />
    <path d="M13.5 10.5a3.6 3.6 0 0 0-5.1 0l-3 3a3.6 3.6 0 0 0 5.1 5.1l.9-.9" />
  </svg>
);

// ---------------------------------------------------------------- 常量与工具

const FILTERS = [
  { id: 'all', label: '全部' },
  { id: 'text', label: '文字' },
  { id: 'image', label: '图片' },
  { id: 'files', label: '文件' },
  { id: 'link', label: '链接' },
  { id: 'fav', label: '收藏' },
];

const FAV_KEY = 'kp-favorites';
const PIN_KEY = 'kp-pinned';
const LINK_RE = /(https?:\/\/|www\.)\S+/i;

/** 分组配色:mac 标签那 7 个颜色(从截图上取样得到),循环使用。
 *  第 6 个是"白底灰环",跟系统里一样当作一种可选颜色。 */
const GROUP_COLORS = [
  { dot: '#ee6f6b' }, // 红
  { dot: '#f2a868' }, // 橙
  { dot: '#f8d86b' }, // 黄
  { dot: '#83d085' }, // 绿
  { dot: '#629ef9' }, // 蓝
  { dot: '#ffffff', ring: '#8a8a8a' }, // 白(灰环)
  { dot: '#a4a5a9' }, // 灰
];

/** 圆点样式:白底那种用内阴影画一圈灰环(不影响尺寸) */
const dotStyle = (color) => ({
  background: color.dot,
  boxShadow: color.ring ? `inset 0 0 0 1.5px ${color.ring}` : 'none',
});

/** 缩略图色调:图片/文件各自一类,链接**单独一类**(紫色,不并进文字),其余文字/富文本绿色 */
const kindTone = (rec) => {
  if (rec.kind === 'image') return 'image';
  if (rec.kind === 'files') return 'files';
  if (LINK_RE.test(rec.text || '')) return 'link';
  return rec.kind === 'html' ? 'html' : 'text';
};

const kindIcon = (rec) => {
  if (rec.kind === 'image') return <IconImage />;
  if (rec.kind === 'files') return <IconFolder />;
  if (LINK_RE.test(rec.text || '')) return <IconLink />;
  return <IconDoc />;
};

const lines = (s) => (s || '').split('\n').map((x) => x.trim()).filter(Boolean);

/** 一行记录展示成:标题 + 副标题,副标题是「类型 · 来源应用」(+ 分组小标签)。
 *  标题一律由后端给:
 *  截图 = 「截图「长 × 宽」」,图片文件/文件 = 「文件名「所在目录」」,文字 = 第一行。 */
function rowText(rec) {
  const label =
    rec.kind === 'image'
      ? '图片'
      : rec.kind === 'files'
        ? '文件'
        : LINK_RE.test(rec.text || '')
          ? '链接'
          : '文字';
  const sub = rec.source_app ? `${label} · ${rec.source_app}` : label;

  if (rec.kind === 'image' || rec.kind === 'files') {
    return { title: rec.text || label, sub };
  }
  // 文字/富文本:只显示第一行,超长由 CSS 省略号处理
  return { title: lines(rec.text)[0] || '(空白内容)', sub };
}

function timeAgo(ms) {
  const diff = Date.now() - ms;
  if (diff < 60_000) return '刚刚';
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}分钟前`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}小时前`;
  const d = new Date(ms);
  return `${d.getMonth() + 1}月${d.getDate()}日`;
}

const loadFavorites = () => {
  try {
    return new Set(JSON.parse(localStorage.getItem(FAV_KEY) || '[]'));
  } catch {
    return new Set();
  }
};

// ---------------------------------------------------------------- 组件

/**
 * 行首的图标/缩略图。
 * 图片类记录优先显示缩略图(捕获时生成,长边 240px),没有缩略图就退回原图,
 * 两者都取不到(协议不可用/文件已被清掉)时退回类型图标,不会出现破图。
 */
function Thumb({ rec, assets, children }) {
  const [step, setStep] = useState(0);
  const sources = useMemo(() => {
    if (!assets || rec.kind !== 'image') return [];
    const file = `${(rec.hash || '').slice(0, 16)}.png`;
    const list = [];
    if (assets.thumbs_dir) list.push(convertFileSrc(`${assets.thumbs_dir}/${file}`));
    if (assets.data_dir && rec.image_path) {
      list.push(convertFileSrc(`${assets.data_dir}/${rec.image_path}`));
    }
    return list;
  }, [assets, rec]);

  if (step >= sources.length) {
    return (
      <div className={`thumb t-${kindTone(rec)}`}>
        {kindIcon(rec)}
        {children}
      </div>
    );
  }
  return (
    <div className={`thumb t-${kindTone(rec)} has-img`}>
      {/* 圆角裁切放在内层:数字角标挂在 .thumb 上,不能被缩略图裁掉 */}
      <span className="thumb-clip">
        <img
          src={sources[step]}
          alt=""
          draggable={false}
          onError={() => setStep((s) => s + 1)}
        />
      </span>
      {children}
    </div>
  );
}

/** 破坏性操作的确认框。
 *  不用 window.confirm:不同平台 webview 的原生实现行为不一致(可能直接返回 false),
 *  而且它盖不住"这条记录已收藏/已分组"这种需要说清楚原因的提示。 */
function ConfirmDialog({ title, note, okText, onCancel, onOk }) {
  return (
    <div className="modal-mask" onClick={onCancel}>
      <div className="modal" onClick={(e) => e.stopPropagation()} onMouseDown={(e) => e.stopPropagation()}>
        <div className="modal-title">{title}</div>
        {note && <div className="modal-note">{note}</div>}
        <div className="modal-actions">
          <button className="btn" onClick={onCancel}>
            取消
          </button>
          <button className="btn danger" autoFocus onClick={onOk}>
            {okText || '删除'}
          </button>
        </div>
      </div>
    </div>
  );
}

export default function App() {
  const [items, setItems] = useState([]);
  const [query, setQuery] = useState('');
  const [filter, setFilter] = useState('all');
  const [favorites, setFavorites] = useState(loadFavorites);
  const [pinned, setPinned] = useState(() => localStorage.getItem(PIN_KEY) === '1');
  const [error, setError] = useState(null);
  const [toast, setToast] = useState(null);
  const [assets, setAssets] = useState(null);
  const [groupFilter, setGroupFilter] = useState(null);
  const [menuPos, setMenuPos] = useState(null);
  const [tagPop, setTagPop] = useState(null);
  const [confirm, setConfirm] = useState(null);
  const [, tick] = useState(0);

  const inputRef = useRef(null);

  // 初始加载 + 订阅剪贴板更新事件
  useEffect(() => {
    invoke('cmd_get_history')
      .then((list) => setItems(list || []))
      .catch((e) => console.error('加载历史失败:', e));

    const unlisten = listen('clipboard-updated', (event) => {
      const rec = event.payload;
      setItems((prev) => [rec, ...prev.filter((i) => i.hash !== rec.hash)].slice(0, 500));
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  // 图片资源目录(缩略图/原图的绝对路径,用于 asset 协议加载)
  useEffect(() => {
    invoke('cmd_get_asset_dirs')
      .then((dirs) => setAssets(dirs))
      .catch((e) => console.error('获取图片目录失败:', e));
  }, []);

  // 唤起后直接能打字搜索
  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  // "x分钟前" 随时间自然刷新
  useEffect(() => {
    const timer = window.setInterval(() => tick((n) => n + 1), 30_000);
    return () => window.clearInterval(timer);
  }, []);

  // 图钉状态同步到窗口
  useEffect(() => {
    localStorage.setItem(PIN_KEY, pinned ? '1' : '0');
    invoke('cmd_set_always_on_top', { enabled: pinned }).catch((e) =>
      console.error('置顶失败:', e)
    );
  }, [pinned]);

  const toggleFavorite = (hash) => {
    setFavorites((prev) => {
      const next = new Set(prev);
      if (next.has(hash)) next.delete(hash);
      else next.add(hash);
      localStorage.setItem(FAV_KEY, JSON.stringify([...next]));
      return next;
    });
  };

  // 已有分组(按首次出现的顺序,新复制的排在前面)
  const groups = useMemo(() => {
    const set = new Set();
    items.forEach((rec) => {
      if (rec.group) set.add(rec.group);
    });
    return [...set];
  }, [items]);

  // 分组配色:按分组名排序后依次取色循环(≤7 个分组时颜色互不相同)
  const groupColors = useMemo(() => {
    const map = new Map();
    [...groups].sort().forEach((name, i) => map.set(name, GROUP_COLORS[i % GROUP_COLORS.length]));
    return map;
  }, [groups]);
  const colorOf = (name) => groupColors.get(name) || GROUP_COLORS[0];

  // 正在筛选的分组被删光后自动回到"不筛选"
  useEffect(() => {
    if (groupFilter && !groups.includes(groupFilter)) setGroupFilter(null);
  }, [groups, groupFilter]);

  const visible = useMemo(() => {
    const q = query.trim().toLowerCase();
    return items.filter((rec) => {
      // "文字"不含链接:链接记录只在"链接"分类里出现,两边不重叠
      if (filter === 'text' && (rec.kind !== 'text' && rec.kind !== 'html')) return false;
      if (filter === 'text' && LINK_RE.test(rec.text || '')) return false;
      if (filter === 'image' && rec.kind !== 'image') return false;
      if (filter === 'files' && rec.kind !== 'files') return false;
      if (filter === 'link' && !LINK_RE.test(rec.text || '')) return false;
      if (filter === 'fav' && !favorites.has(rec.hash)) return false;
      if (groupFilter && rec.group !== groupFilter) return false;
      if (!q) return true;
      return (
        (rec.text || '').toLowerCase().includes(q) ||
        (rec.files || []).some((f) => f.toLowerCase().includes(q))
      );
    });
  }, [items, query, filter, favorites, groupFilter]);

  const pasteItem = (id) => {
    if (id == null) return;
    invoke('cmd_paste_item', { id }).catch((e) => {
      console.error('粘贴失败:', e);
      setError(String(e));
      window.setTimeout(() => setError(null), 3000);
    });
  };

  const flash = (msg) => {
    setToast(msg);
    window.setTimeout(() => setToast(null), 2200);
  };

  // ---------------------------------------------------------- 分组编辑

  const openTagPop = (e, rec) => {
    e.stopPropagation();
    const r = e.currentTarget.getBoundingClientRect();
    const width = 232;
    setMenuPos(null);
    setTagPop({
      id: rec.id,
      value: rec.group || '',
      // 列表是可滚动容器,弹层用 fixed 定位贴在按钮下方(否则会被裁掉)
      top: Math.min(r.bottom + 6, window.innerHeight - 132),
      left: Math.max(8, Math.min(r.right - width, window.innerWidth - width - 8)),
    });
  };

  const applyGroup = (id, raw) => {
    const group = (raw || '').trim() || null;
    setTagPop(null);
    setItems((prev) => prev.map((i) => (i.id === id ? { ...i, group } : i)));
    invoke('cmd_set_group', { id, group }).catch((e) => {
      console.error('设置分组失败:', e);
      setError(String(e));
      window.setTimeout(() => setError(null), 3000);
    });
  };

  // ---------------------------------------------------------- 删除 / 清理

  const doDelete = (ids) => {
    setItems((prev) => prev.filter((i) => !ids.includes(i.id)));
    invoke('cmd_delete_records', { ids }).catch((e) => {
      console.error('删除失败:', e);
      setError(String(e));
      window.setTimeout(() => setError(null), 3000);
    });
  };

  /** 普通记录直接删;收藏过或分过组的要多一步确认 */
  const requestDelete = (e, rec) => {
    e.stopPropagation();
    const marks = [favorites.has(rec.hash) && '已收藏', rec.group && `已分组「${rec.group}」`].filter(Boolean);
    if (marks.length === 0) {
      doDelete([rec.id]);
      return;
    }
    setTagPop(null);
    setConfirm({
      ids: [rec.id],
      title: '删除这条记录?',
      note: `这条记录${marks.join('、')},删除后无法恢复。`,
    });
  };

  // 临时记录 = 没收藏也没分组;扫帚只清理这些
  const transient = useMemo(
    () => items.filter((rec) => !favorites.has(rec.hash) && !rec.group),
    [items, favorites]
  );

  const requestSweep = () => {
    setTagPop(null);
    setMenuPos(null);
    if (transient.length === 0) {
      flash('没有可清理的临时记录');
      return;
    }
    setConfirm({
      ids: transient.map((rec) => rec.id),
      title: `清理 ${transient.length} 条临时记录?`,
      note: '已收藏和已分组的记录会保留,其余记录(含图片文件)将被删除。',
      okText: '清理',
    });
  };

  // ---------------------------------------------------------- 键盘

  // 键盘:Esc 收起窗口,数字键 1-9 快速粘贴(搜索框为空时同样生效)
  const visibleRef = useRef(visible);
  visibleRef.current = visible;
  const typingRef = useRef(false);
  typingRef.current = Boolean(query);
  // 有弹层打开时,Esc 交给弹层自己处理,不要顺手把窗口收起来
  const overlayRef = useRef(false);
  overlayRef.current = Boolean(confirm || tagPop || menuPos);
  useEffect(() => {
    const onKey = (e) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        if (overlayRef.current) return;
        invoke('cmd_hide_main').catch(() => {});
        return;
      }
      if (e.ctrlKey || e.altKey || e.metaKey) return;
      const tag = (e.target && e.target.tagName) || '';
      const inField = tag === 'INPUT' || tag === 'TEXTAREA';
      // 分组输入框等其它输入框里打字,一律不要触发粘贴
      if (inField && e.target !== inputRef.current) return;
      if (inField && typingRef.current) return;
      const n = parseInt(e.key, 10);
      if (n >= 1 && n <= 9) {
        const rec = visibleRef.current[n - 1];
        if (rec) pasteItem(rec.id);
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);

  // 点空白处收起弹层(弹层自己会 stopPropagation)
  useEffect(() => {
    if (!tagPop && !menuPos) return undefined;
    const close = () => {
      setTagPop(null);
      setMenuPos(null);
    };
    window.addEventListener('mousedown', close);
    return () => window.removeEventListener('mousedown', close);
  }, [tagPop, menuPos]);

  const openGroupMenu = (e) => {
    if (menuPos) {
      setMenuPos(null);
      return;
    }
    const r = e.currentTarget.getBoundingClientRect();
    const width = 176; // 与 app.css 的 .menu 宽度一致
    setTagPop(null);
    setMenuPos({
      top: r.bottom + 8,
      // 贴右:与"分组"按钮的右边线(以及顶栏图标右边线)对齐,都是窗口内边距 16px 处
      left: Math.max(8, window.innerWidth - width - 16),
    });
  };

  const pickGroup = (g) => {
    setGroupFilter(g);
    setMenuPos(null);
  };

  const editingRec = tagPop ? items.find((i) => i.id === tagPop.id) : null;

  return (
    <div className="app">
      <div className="head" data-tauri-drag-region>
        <div className="search-row">
          <div className="search">
            <IconSearch />
            <input
              ref={inputRef}
              type="text"
              placeholder="搜索"
              value={query}
              spellCheck={false}
              onChange={(e) => setQuery(e.target.value)}
            />
          </div>
          <button
            className={`icon-btn${pinned ? ' on' : ''}`}
            title={pinned ? '取消置顶' : '置顶(始终显示在最前)'}
            onClick={() => setPinned((v) => !v)}
          >
            <IconPin />
          </button>
          <button
            className="icon-btn"
            title={
              transient.length
                ? `清理 ${transient.length} 条临时记录(保留收藏与分组)`
                : '清理临时记录(保留收藏与分组)'
            }
            onClick={requestSweep}
          >
            <IconClean />
          </button>
          <button className="icon-btn" title="配置(暂未实现)" onClick={() => {}}>
            <IconGear />
          </button>
        </div>

        <div className="chips">
          {FILTERS.map((f) => (
            <button
              key={f.id}
              className={`chip${filter === f.id ? ' on' : ''}`}
              onClick={() => setFilter(f.id)}
            >
              {f.label}
            </button>
          ))}
          <button
            className={`chip chip-group${groupFilter ? ' on' : ''}`}
            title="按分组筛选"
            onClick={openGroupMenu}
          >
            {groupFilter || '分组'}
            <IconChevron />
          </button>
        </div>
      </div>

      <main className="list">
        {visible.length === 0 && (
          <div className="empty">
            <strong>{items.length === 0 ? '暂无记录' : '没有匹配的内容'}</strong>
            {items.length === 0 ? '复制点什么' : '换个关键词或分类试试'}
          </div>
        )}

        {visible.slice(0, 200).map((rec, idx) => {
          const { title, sub } = rowText(rec);
          const fav = favorites.has(rec.hash);
          return (
            <div
              key={rec.id}
              className="item"
              onClick={() => pasteItem(rec.id)}
              title="点击粘贴到上一个应用"
            >
              <Thumb rec={rec} assets={assets}>
                {idx < 9 && <span className="num">{idx + 1}</span>}
              </Thumb>
              <div className="main">
                <div className="title">{title}</div>
                <div className="sub">
                  <span className="sub-text">{sub}</span>
                  {rec.group && (
                    <span className="group-pill" title={`分组:${rec.group}`}>
                      <i className="dot" style={dotStyle(colorOf(rec.group))} />
                      {rec.group}
                    </span>
                  )}
                </div>
              </div>
              <span className="time">{timeAgo(rec.created_at)}</span>
              {/* 分组 → 收藏 → 删除(顺序由需求定死:标签在星星左边,删除在星星右边) */}
              <div className="row-actions">
                <button
                  className={`row-btn${rec.group ? ' on' : ''}`}
                  title={rec.group ? `分组:${rec.group}(点击修改)` : '设置分组'}
                  onClick={(e) => openTagPop(e, rec)}
                >
                  <IconTag />
                </button>
                <button
                  className={`star${fav ? ' on' : ''}`}
                  title={fav ? '取消收藏' : '收藏'}
                  onClick={(e) => {
                    e.stopPropagation();
                    toggleFavorite(rec.hash);
                  }}
                >
                  <IconStar filled={fav} />
                </button>
                <button
                  className="row-btn del"
                  title="删除这条记录"
                  onClick={(e) => requestDelete(e, rec)}
                >
                  <IconTrash />
                </button>
              </div>
            </div>
          );
        })}
      </main>

      {/* 分组筛选下拉(用 fixed 定位,避免被分类条的横向滚动裁掉) */}
      {menuPos && (
        <div
          className="menu"
          style={{ top: menuPos.top, left: menuPos.left }}
          onMouseDown={(e) => e.stopPropagation()}
        >
          <button
            className={`menu-item${groupFilter ? '' : ' on'}`}
            onClick={() => pickGroup(null)}
          >
            <i className="dot empty" />
            <span className="menu-text">不筛选</span>
          </button>
          {groups.map((g) => (
            <button
              key={g}
              className={`menu-item${groupFilter === g ? ' on' : ''}`}
              onClick={() => pickGroup(g)}
            >
              <i className="dot" style={dotStyle(colorOf(g))} />
              <span className="menu-text">{g}</span>
            </button>
          ))}
          {groups.length === 0 && <div className="menu-empty">还没有分组,点记录上的标签图标添加</div>}
        </div>
      )}

      {/* 分组编辑弹层 */}
      {tagPop && editingRec && (
        <div
          className="tag-pop"
          style={{ top: tagPop.top, left: tagPop.left }}
          onMouseDown={(e) => e.stopPropagation()}
          onClick={(e) => e.stopPropagation()}
        >
          <input
            className="tag-input"
            autoFocus
            spellCheck={false}
            placeholder="输入或选择分组"
            list="kp-group-options"
            value={tagPop.value}
            onChange={(e) => setTagPop((p) => (p ? { ...p, value: e.target.value } : p))}
            onKeyDown={(e) => {
              e.stopPropagation();
              if (e.key === 'Enter') applyGroup(tagPop.id, tagPop.value);
              else if (e.key === 'Escape') setTagPop(null);
            }}
          />
          <datalist id="kp-group-options">
            {groups.map((g) => (
              <option key={g} value={g} />
            ))}
          </datalist>
          {groups.length > 0 && (
            <div className="tag-known">
              {groups.map((g) => (
                <button key={g} className="tag-chip" onClick={() => applyGroup(tagPop.id, g)}>
                  <i className="dot" style={dotStyle(colorOf(g))} />
                  {g}
                </button>
              ))}
            </div>
          )}
          <div className="tag-actions">
            {editingRec.group && (
              <button className="btn small ghost" onClick={() => applyGroup(tagPop.id, null)}>
                清除分组
              </button>
            )}
            <button className="btn small" onClick={() => applyGroup(tagPop.id, tagPop.value)}>
              确定
            </button>
          </div>
        </div>
      )}

      {confirm && (
        <ConfirmDialog
          title={confirm.title}
          note={confirm.note}
          okText={confirm.okText}
          onCancel={() => setConfirm(null)}
          onOk={() => {
            const ids = confirm.ids;
            setConfirm(null);
            doDelete(ids);
          }}
        />
      )}

      {toast && <div className="toast">{toast}</div>}
      {error && <div className="errorbar">粘贴失败: {error}</div>}
    </div>
  );
}
