import { useEffect, useMemo, useRef, useState } from 'react';
import { convertFileSrc, invoke } from '@tauri-apps/api/tauri';
import { listen } from '@tauri-apps/api/event';
import Settings from './Settings.jsx';
import { translations } from './i18n.js';
import {
  IconChevron,
  IconClean,
  IconDoc,
  IconFolder,
  IconGear,
  IconImage,
  IconLink,
  IconPin,
  IconSearch,
  IconStar,
  IconTag,
  IconTrash,
} from './icons.jsx';

// ---------------------------------------------------------------- 常量与工具

/** 旧版把收藏记在 localStorage 的 key(新版本挪到记录里,启动时迁移一次) */
const FAV_KEY = 'kp-favorites';
const PIN_KEY = 'kp-pinned';
const LINK_RE = /(https?:\/\/|www\.)\S+/i;
/** 快捷粘贴的修饰键:mac 上是 ⌘,其它平台是 Ctrl */
const IS_MAC = /mac/i.test(navigator.platform || navigator.userAgent);

/** 设置还没从后端取回来时先用这套,保证界面能立刻渲染(与后端 settings.rs 的默认值一致) */
const DEFAULT_SETTINGS = {
  hotkey: /mac/i.test(navigator.platform || navigator.userAgent) ? 'Cmd+Shift+V' : 'Ctrl+Shift+V',
  max_items: 500,
  language: 'zh',
  quick_paste: true,
};

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
function rowText(rec, t) {
  const label =
    rec.kind === 'image'
      ? t.subtitleImage
      : rec.kind === 'files'
        ? t.subtitleFiles
        : LINK_RE.test(rec.text || '')
          ? t.subtitleLink
          : t.subtitleText;
  const sub = rec.source_app ? `${label} · ${rec.source_app}` : label;

  if (rec.kind === 'image' || rec.kind === 'files') {
    return { title: rec.text || label, sub };
  }
  // 文字/富文本:只显示第一行,超长由 CSS 省略号处理
  return { title: lines(rec.text)[0] || t.blank, sub };
}

function timeAgo(ms, t) {
  const diff = Date.now() - ms;
  if (diff < 60_000) return t.justNow;
  if (diff < 3_600_000) return t.minutesAgo(Math.floor(diff / 60_000));
  if (diff < 86_400_000) return t.hoursAgo(Math.floor(diff / 3_600_000));
  const d = new Date(ms);
  return t.dateMd(d.getMonth() + 1, d.getDate());
}

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
function ConfirmDialog({ title, note, okText, cancelText, onCancel, onOk }) {
  return (
    <div className="modal-mask" onClick={onCancel}>
      <div className="modal" onClick={(e) => e.stopPropagation()} onMouseDown={(e) => e.stopPropagation()}>
        <div className="modal-title">{title}</div>
        {note && <div className="modal-note">{note}</div>}
        <div className="modal-actions">
          <button className="btn" onClick={onCancel}>
            {cancelText}
          </button>
          <button className="btn danger" autoFocus onClick={onOk}>
            {okText}
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
  const [pinned, setPinned] = useState(() => localStorage.getItem(PIN_KEY) === '1');
  const [error, setError] = useState(null);
  const [toast, setToast] = useState(null);
  const [assets, setAssets] = useState(null);
  const [groupFilter, setGroupFilter] = useState(null);
  const [menuPos, setMenuPos] = useState(null);
  const [tagPop, setTagPop] = useState(null);
  const [confirm, setConfirm] = useState(null);
  const [settings, setSettings] = useState(null);
  const [view, setView] = useState('list');
  const [modDown, setModDown] = useState(false);
  const [, tick] = useState(0);

  const inputRef = useRef(null);
  const t = useMemo(() => translations((settings || DEFAULT_SETTINGS).language), [settings]);
  const quickPaste = (settings || DEFAULT_SETTINGS).quick_paste;

  // 初始加载(含旧版 localStorage 收藏的一次性迁移)+ 订阅剪贴板更新事件
  useEffect(() => {
    const migrateFavorites = async () => {
      try {
        const raw = localStorage.getItem(FAV_KEY);
        const hashes = raw ? JSON.parse(raw) : [];
        if (Array.isArray(hashes) && hashes.length) {
          const marked = await invoke('cmd_import_favorites', { hashes });
          console.log(`[收藏] 已把 ${marked} 条 localStorage 收藏迁移到记录里`);
        }
      } catch (e) {
        console.warn('收藏迁移失败（忽略）：', e);
      }
      localStorage.removeItem(FAV_KEY);
      invoke('cmd_get_history')
        .then((list) => setItems(list || []))
        .catch((e) => console.error('加载历史失败：', e));
    };

    migrateFavorites();
    invoke('cmd_get_settings')
      .then((s) => setSettings(s))
      .catch((e) => console.error('加载设置失败：', e));

    const unlisten = listen('clipboard-updated', (event) => {
      const rec = event.payload;
      setItems((prev) => [rec, ...prev.filter((i) => i.hash !== rec.hash)].slice(0, 1200));
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  // 图片资源目录(缩略图/原图的绝对路径,用于 asset 协议加载)
  useEffect(() => {
    invoke('cmd_get_asset_dirs')
      .then((dirs) => setAssets(dirs))
      .catch((e) => console.error('获取图片目录失败：', e));
  }, []);

  // 把 WebView 信息写进日志:麒麟上要靠它确认 WebKitGTK 版本(判断 CSS 支持范围)
  useEffect(() => {
    invoke('cmd_log', {
      message: `UA=${navigator.userAgent} | platform=${navigator.platform} | lang=${navigator.language}`,
    }).catch(() => {});
  }, []);

  // 唤起后直接能打字搜索
  useEffect(() => {
    if (view === 'list') inputRef.current?.focus();
  }, [view]);

  // "x分钟前" 随时间自然刷新
  useEffect(() => {
    const timer = window.setInterval(() => tick((n) => n + 1), 30_000);
    return () => window.clearInterval(timer);
  }, []);

  // 图钉状态同步到窗口
  useEffect(() => {
    localStorage.setItem(PIN_KEY, pinned ? '1' : '0');
    invoke('cmd_set_always_on_top', { enabled: pinned }).catch((e) =>
      console.error('置顶失败：', e)
    );
  }, [pinned]);

  /** 保存设置:先乐观更新界面,后端返回的才是最终值(热键注册失败会退回去并抛错) */
  const persistSettings = async (next) => {
    const previous = settings || DEFAULT_SETTINGS;
    setSettings(next);
    try {
      const saved = await invoke('cmd_set_settings', { settings: next });
      setSettings(saved);
      return saved;
    } catch (e) {
      setSettings(previous);
      throw e;
    }
  };

  const toggleFavorite = (rec) => {
    const favorite = !rec.favorite;
    setItems((prev) => prev.map((i) => (i.id === rec.id ? { ...i, favorite } : i)));
    invoke('cmd_set_favorite', { id: rec.id, favorite }).catch((e) => {
      console.error('收藏失败：', e);
      setItems((prev) => prev.map((i) => (i.id === rec.id ? { ...i, favorite: !favorite } : i)));
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
      if (filter === 'fav' && !rec.favorite) return false;
      if (groupFilter && rec.group !== groupFilter) return false;
      if (!q) return true;
      return (
        (rec.text || '').toLowerCase().includes(q) ||
        (rec.files || []).some((f) => f.toLowerCase().includes(q))
      );
    });
  }, [items, query, filter, groupFilter]);

  const pasteItem = (id) => {
    if (id == null) return;
    invoke('cmd_paste_item', { id }).catch((e) => {
      console.error('粘贴失败：', e);
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
      console.error('设置分组失败：', e);
      setError(String(e));
      window.setTimeout(() => setError(null), 3000);
    });
  };

  // ---------------------------------------------------------- 删除 / 清理

  const doDelete = (ids) => {
    setItems((prev) => prev.filter((i) => !ids.includes(i.id)));
    invoke('cmd_delete_records', { ids }).catch((e) => {
      console.error('删除失败：', e);
      setError(String(e));
      window.setTimeout(() => setError(null), 3000);
    });
  };

  /** 普通记录直接删;收藏过或分过组的要多一步确认 */
  const requestDelete = (e, rec) => {
    e.stopPropagation();
    const marks = [
      rec.favorite && t.markFav,
      rec.group && t.markGroup(rec.group),
    ].filter(Boolean);
    if (marks.length === 0) {
      doDelete([rec.id]);
      return;
    }
    setTagPop(null);
    setConfirm({
      ids: [rec.id],
      title: t.confirmDeleteTitle,
      note: t.confirmDeleteNote(marks.join('、')),
    });
  };

  // 临时记录 = 没收藏也没分组;清理按钮只清这些
  const transient = useMemo(() => items.filter((rec) => !rec.favorite && !rec.group), [items]);

  const requestSweep = () => {
    setTagPop(null);
    setMenuPos(null);
    if (transient.length === 0) {
      flash(t.nothingToClean);
      return;
    }
    setConfirm({
      ids: transient.map((rec) => rec.id),
      title: t.confirmCleanTitle(transient.length),
      note: t.confirmCleanNote,
      okText: t.clean,
    });
  };

  // ---------------------------------------------------------- 键盘

  // 键盘:Esc 收起窗口,Ctrl/Cmd+数字键 1-9 快速粘贴("便捷粘贴"打开时生效)
  const visibleRef = useRef(visible);
  visibleRef.current = visible;
  const quickPasteRef = useRef(true);
  quickPasteRef.current = quickPaste;
  const viewRef = useRef(view);
  viewRef.current = view;
  const modDownRef = useRef(false);
  modDownRef.current = modDown;

  /**
   * 等 ⌘ 松开之后再执行(⌘ 没按着就立刻执行)。
   *
   * 为什么必须等:tao(macOS)覆写了 `sendEvent:` 来处理"按住 ⌘ 收不到 keyUp"这个系统行为,
   * 但它会把 ⌘ 的 keyUp 转发给 `[NSApp keyWindow]`;如果这时我们的窗口已经隐藏
   * (keyWindow 为 nil),就会 `msg_send![nil, ...]` 直接 abort(tao app.rs:54,实测崩溃)。
   * 所以任何"让窗口消失"的动作(快捷粘贴、点击粘贴、Esc 收起)都等 ⌘ 松开再做。
   *
   * 等待期间又触发时**以最后一次为准**(按住 ⌘ 连按数字键 → 粘贴最后按的那条)。
   */
  const pendingActionRef = useRef(null);
  const waitingReleaseRef = useRef(false);
  const runAfterModifierRelease = (action) => {
    if (!IS_MAC || !modDownRef.current) {
      action();
      return;
    }
    pendingActionRef.current = action;
    if (waitingReleaseRef.current) return;
    waitingReleaseRef.current = true;

    const finish = () => {
      window.removeEventListener('keyup', onKeyUp, true);
      window.removeEventListener('blur', finish);
      waitingReleaseRef.current = false;
      const pending = pendingActionRef.current;
      pendingActionRef.current = null;
      if (pending) pending();
    };
    const onKeyUp = (event) => {
      if (!event.metaKey) finish();
    };
    // 兜底:万一 ⌘ 的 keyUp 没落到我们这(比如先点了别的应用),窗口失焦时也执行掉
    window.addEventListener('keyup', onKeyUp, true);
    window.addEventListener('blur', finish);
  };
  // 有弹层打开时,Esc 交给弹层自己处理,不要顺手把窗口收起来
  const overlayRef = useRef(false);
  overlayRef.current = Boolean(confirm || tagPop || menuPos);
  useEffect(() => {
    const onKey = (e) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        if (overlayRef.current) return;
        runAfterModifierRelease(() => invoke('cmd_hide_main').catch(() => {}));
        return;
      }

      // 快捷粘贴:Ctrl(mac 上是 ⌘)+ 数字键 1-9(此时角标也是亮的)
      const mod = IS_MAC ? e.metaKey : e.ctrlKey;
      if (mod) {
        if (e.altKey || e.shiftKey) return;
        const n = parseInt(e.key, 10);
        if (n >= 1 && n <= 9) {
          e.preventDefault();
          if (!quickPasteRef.current) return;
          const rec = visibleRef.current[n - 1];
          if (rec) {
            runAfterModifierRelease(() =>
              invoke('cmd_paste_item', { id: rec.id }).catch((err) => {
                console.error('粘贴失败：', err);
                setError(String(err));
                window.setTimeout(() => setError(null), 3000);
              })
            );
          }
        }
        return;
      }

      // 其它带修饰键的组合不参与
      if (e.ctrlKey || e.altKey || e.metaKey) return;
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);

  // 按住 Ctrl(⌘)时亮出数字角标,松开就收起;同时用它决定快捷键是否生效
  useEffect(() => {
    const MODIFIER_KEY = IS_MAC ? 'Meta' : 'Control';
    // 修饰键"自己"的事件:老版 WebKitGTK(麒麟)在这类事件里给出的
    // e.ctrlKey/e.metaKey 可能是错的(按下时是 false),不能只信事件标志
    const isSelf = (e) =>
      e.key === MODIFIER_KEY ||
      e.code === `${MODIFIER_KEY}Left` ||
      e.code === `${MODIFIER_KEY}Right` ||
      e.keyCode === (IS_MAC ? 91 : 17) ||
      (IS_MAC && e.keyCode === 93);
    const onDown = (e) => {
      if (isSelf(e)) {
        setModDown(true);
        return;
      }
      setModDown(IS_MAC ? e.metaKey : e.ctrlKey);
    };
    const onUp = (e) => {
      if (isSelf(e)) {
        setModDown(false);
        return;
      }
      setModDown(IS_MAC ? e.metaKey : e.ctrlKey);
    };
    const reset = () => setModDown(false);
    window.addEventListener('keydown', onDown);
    window.addEventListener('keyup', onUp);
    window.addEventListener('blur', reset);
    return () => {
      window.removeEventListener('keydown', onDown);
      window.removeEventListener('keyup', onUp);
      window.removeEventListener('blur', reset);
    };
  }, []);

  // 每次呼出窗口都把焦点放回搜索框:直接就能打字筛选
  useEffect(() => {
    const unlisten = listen('tauri://focus', () => {
      if (viewRef.current === 'list') inputRef.current?.focus();
    });
    return () => {
      unlisten.then((f) => f());
    };
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
    const width = 152; // 与 app.css 的 .menu 宽度一致
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

  // ---------------------------------------------------------- 设置界面

  if (view === 'settings') {
    return (
      <div className="app">
        <Settings
          settings={settings || DEFAULT_SETTINGS}
          onChange={persistSettings}
          onClose={() => setView('list')}
          t={t}
        />
      </div>
    );
  }

  const FILTERS = [
    { id: 'all', label: t.filterAll },
    { id: 'text', label: t.filterText },
    { id: 'image', label: t.filterImage },
    { id: 'files', label: t.filterFiles },
    { id: 'link', label: t.filterLink },
    { id: 'fav', label: t.filterFav },
  ];

  return (
    <div className="app">
      <div className="head" data-tauri-drag-region>
        <div className="search-row">
          <div className="search">
            <IconSearch />
            <input
              ref={inputRef}
              type="text"
              placeholder={t.search}
              value={query}
              spellCheck={false}
              onChange={(e) => setQuery(e.target.value)}
            />
          </div>
          <button
            className={`icon-btn${pinned ? ' on' : ''}`}
            title={pinned ? t.unpin : t.pin}
            onClick={() => setPinned((v) => !v)}
          >
            <IconPin />
          </button>
          <button
            className="icon-btn"
            title={transient.length ? t.cleanTipCount(transient.length) : t.cleanTip}
            onClick={requestSweep}
          >
            <IconClean />
          </button>
          <button className="icon-btn" title={t.settings} onClick={() => setView('settings')}>
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
            title={t.groupFilterTitle}
            onClick={openGroupMenu}
          >
            {groupFilter || t.group}
            <IconChevron />
          </button>
        </div>
      </div>

      <main className="list">
        {visible.length === 0 && (
          <div className="empty">
            <strong>{items.length === 0 ? t.emptyTitle : t.noMatchTitle}</strong>
            {items.length === 0 ? t.emptyHint : t.noMatchHint}
          </div>
        )}

        {visible.slice(0, 200).map((rec, idx) => {
          const { title, sub } = rowText(rec, t);
          const fav = Boolean(rec.favorite);
          return (
            <div
              key={rec.id}
              className="item"
              onClick={() => runAfterModifierRelease(() => pasteItem(rec.id))}
              title={t.pasteHint}
            >
              <Thumb rec={rec} assets={assets}>
                {/* 角标只在按住 Ctrl/⌘ 时出现(不给悬停显示,避免误以为数字键直接可用) */}
                {modDown && quickPaste && idx < 9 && <span className="num">{idx + 1}</span>}
              </Thumb>
              <div className="main">
                <div className="title">{title}</div>
                <div className="sub">
                  <span className="sub-text">{sub}</span>
                  {rec.group && (
                    <span className="group-pill" title={t.groupTip(rec.group)}>
                      <i className="dot" style={dotStyle(colorOf(rec.group))} />
                      {rec.group}
                    </span>
                  )}
                </div>
              </div>
              <span className="time">{timeAgo(rec.created_at, t)}</span>
              {/* 分组 → 收藏 → 删除(顺序由需求定死:标签在星星左边,删除在星星右边) */}
              <div className="row-actions">
                <button
                  className={`row-btn${rec.group ? ' on' : ''}`}
                  title={rec.group ? t.groupTip(rec.group) : t.setGroup}
                  onClick={(e) => openTagPop(e, rec)}
                >
                  <IconTag />
                </button>
                <button
                  className={`star${fav ? ' on' : ''}`}
                  title={fav ? t.unfav : t.fav}
                  onClick={(e) => {
                    e.stopPropagation();
                    toggleFavorite(rec);
                  }}
                >
                  <IconStar filled={fav} />
                </button>
                <button
                  className="row-btn del"
                  title={t.deleteOne}
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
          {/* "不筛选"没有颜色点,文字居中(与下面带彩点的分组行区分开) */}
          <button
            className={`menu-item center${groupFilter ? '' : ' on'}`}
            onClick={() => pickGroup(null)}
          >
            <span className="menu-text">{t.groupNoFilter}</span>
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
          {groups.length === 0 && <div className="menu-empty">{t.groupEmpty}</div>}
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
            placeholder={t.groupInputPlaceholder}
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
                {t.clearGroup}
              </button>
            )}
            <button className="btn small" onClick={() => applyGroup(tagPop.id, tagPop.value)}>
              {t.confirm}
            </button>
          </div>
        </div>
      )}

      {confirm && (
        <ConfirmDialog
          title={confirm.title}
          note={confirm.note}
          okText={confirm.okText || t.delete}
          cancelText={t.cancel}
          onCancel={() => setConfirm(null)}
          onOk={() => {
            const ids = confirm.ids;
            setConfirm(null);
            doDelete(ids);
          }}
        />
      )}

      {toast && <div className="toast">{toast}</div>}
      {error && <div className="errorbar">{t.pasteFailed(error)}</div>}
    </div>
  );
}
