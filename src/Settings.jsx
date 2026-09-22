import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/tauri';
import { open as openUrl } from '@tauri-apps/api/shell';
import {
  IconChevron,
  IconChevronLeft,
  IconDoc,
  IconExternalLink,
  IconInfo,
  IconSliders,
} from './icons.jsx';

const GITHUB_URL = 'https://github.com/shuglx/kylinpaste';
const KEEP_OPTIONS = [100, 300, 500, 1000];

const isMac = () => /mac/i.test(navigator.platform || navigator.userAgent);

/**
 * 卡片里的一行:左标签(可带说明)+ 右控件。
 *
 * 说明文字**点一下问号才展开**(不用原生 title:悬停既不弹提示、鼠标也不会变成问号),
 * `note` 则是常驻的灰色小字。
 * 注意:`hint`/`note` 与 `onClick` 不要同时用(会渲染出嵌套 button)。
 */
function Row({ label, hint, note, children, onClick, className = '' }) {
  const [openHint, setOpenHint] = useState(false);
  const Tag = onClick ? 'button' : 'div';
  return (
    <div className="row-block">
      <Tag className={`row ${className}`} onClick={onClick}>
        <span className="row-label">
          {label}
          {hint && (
            <span
              className={`hint-btn${openHint ? ' on' : ''}`}
              role="button"
              tabIndex={0}
              aria-label={hint}
              onClick={(e) => {
                e.stopPropagation();
                setOpenHint((v) => !v);
              }}
              onKeyDown={(e) => {
                if (e.key === 'Enter' || e.key === ' ') {
                  e.preventDefault();
                  e.stopPropagation();
                  setOpenHint((v) => !v);
                }
              }}
            >
              ?
            </span>
          )}
        </span>
        <span className="row-ctrl">{children}</span>
      </Tag>
      {hint && openHint && <div className="row-note">{hint}</div>}
      {note && <div className="row-note">{note}</div>}
    </div>
  );
}

function Switch({ checked, onChange, label }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      className={`switch${checked ? ' on' : ''}`}
      onClick={() => onChange(!checked)}
    >
      <i />
    </button>
  );
}

/** 下拉选择(用列表页同一套 .menu 样式) */
function Select({ value, options, onChange, width = 140 }) {
  const [open, setOpen] = useState(false);
  const [pos, setPos] = useState(null);
  const current = options.find((o) => o.value === value);

  useEffect(() => {
    if (!open) return undefined;
    const close = () => setOpen(false);
    window.addEventListener('mousedown', close);
    return () => window.removeEventListener('mousedown', close);
  }, [open]);

  const toggle = (e) => {
    if (open) {
      setOpen(false);
      return;
    }
    const rect = e.currentTarget.getBoundingClientRect();
    setPos({
      top: rect.bottom + 6,
      left: Math.max(8, Math.min(rect.right - width, window.innerWidth - width - 8)),
    });
    setOpen(true);
  };

  return (
    <>
      <button type="button" className="select" onClick={toggle}>
        <span className="select-value">{current ? current.label : String(value)}</span>
        <IconChevron />
      </button>
      {open && pos && (
        <div
          className="menu"
          style={{ top: pos.top, left: pos.left, width }}
          onMouseDown={(e) => e.stopPropagation()}
        >
          {options.map((opt) => (
            <button
              key={opt.value}
              type="button"
              className={`menu-item${opt.value === value ? ' on' : ''}`}
              onClick={() => {
                onChange(opt.value);
                setOpen(false);
              }}
            >
              <span className="menu-text">{opt.label}</span>
            </button>
          ))}
        </div>
      )}
    </>
  );
}

/** `KeyboardEvent` → 加速键字符串(如 Ctrl+Alt+V);只按修饰键时返回 null */
function accelOf(event) {
  const parts = [];
  const mac = isMac();
  if (mac) {
    if (event.metaKey) parts.push('Cmd');
    if (event.ctrlKey) parts.push('Ctrl');
    if (event.altKey) parts.push('Alt');
    if (event.shiftKey) parts.push('Shift');
  } else {
    if (event.ctrlKey) parts.push('Ctrl');
    if (event.altKey) parts.push('Alt');
    if (event.shiftKey) parts.push('Shift');
    if (event.metaKey) parts.push('Super');
  }

  const key = mainKeyOf(event);
  if (!key) return null;
  parts.push(key);
  return parts.join('+');
}

function mainKeyOf(event) {
  const key = event.key;
  if (!key || ['Control', 'Alt', 'Shift', 'Meta', 'AltGraph', 'CapsLock'].includes(key)) {
    return null;
  }
  if (key === ' ') return 'Space';
  if (key === 'Escape') return 'Esc'; // 调用方会先把 Esc 当作"取消"
  if (key.startsWith('Arrow')) {
    return { ArrowUp: 'Up', ArrowDown: 'Down', ArrowLeft: 'Left', ArrowRight: 'Right' }[key];
  }
  if (/^F\d{1,2}$/.test(key)) return key;
  if (key.length === 1) return key.toUpperCase();
  return key; // Tab / Enter / Backspace / Delete / Home / End / PageUp / PageDown
}

/** 快捷键录入:点一下进入"请按键"状态,按下的组合直接生效 */
function HotkeyRecorder({ value, onCommit, onRecordingChange, t }) {
  const [recording, setRecording] = useState(false);
  const [error, setError] = useState(null);
  const commitRef = useRef(onCommit);
  commitRef.current = onCommit;
  const notifyRef = useRef(onRecordingChange);
  notifyRef.current = onRecordingChange;

  // 录入期间暂停全局热键:被动抓键(X11 XGrabKey)会把按键事件投递给抓键方,
  // webview 根本收不到 —— 不暂停的话,当前正在生效的那个组合永远录不进去。
  // 组件卸载(关设置页)时兜底恢复。
  useEffect(() => {
    invoke('cmd_set_hotkey_recording', { recording }).catch(() => {});
    notifyRef.current?.();
    return () => {
      if (recording) {
        invoke('cmd_set_hotkey_recording', { recording: false }).catch(() => {});
        notifyRef.current?.();
      }
    };
  }, [recording]);

  useEffect(() => {
    if (!recording) return undefined;
    const onKey = (event) => {
      // 捕获阶段拦下来:别让设置页/列表页的键盘逻辑(数字键粘贴、Esc 收起窗口)参与
      event.preventDefault();
      event.stopPropagation();
      if (event.key === 'Escape') {
        setRecording(false);
        setError(null);
        return;
      }
      const accel = accelOf(event);
      if (!accel) return; // 只按了修饰键,继续等
      if (!accel.includes('+')) {
        setError(t.hotkeyNeedModifier);
        return;
      }
      setRecording(false);
      setError(null);
      commitRef.current(accel);
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [recording, t]);

  return (
    <span className="hotkey-cell">
      <button
        type="button"
        className={`hotkey${recording ? ' on' : ''}`}
        onClick={() => {
          setRecording(true);
          setError(null);
        }}
      >
        {recording ? t.hotkeyRecording : value}
      </button>
      {error && <span className="hotkey-error">{error}</span>}
    </span>
  );
}

export default function Settings({
  settings,
  onChange,
  onClose,
  t,
  onLanguageChange,
  counts,
}) {
  const [tab, setTab] = useState('general');
  const [appInfo, setAppInfo] = useState(null);
  const [autostart, setAutostart] = useState(false);
  const [notice, setNotice] = useState(null);
  const [hotkeyStatus, setHotkeyStatus] = useState(null);
  const [logPath, setLogPath] = useState(null);
  const [storageFiles, setStorageFiles] = useState([]);

  useEffect(() => {
    invoke('cmd_get_app_info').then(setAppInfo).catch(() => {});
    invoke('cmd_get_autostart').then(setAutostart).catch(() => {});
    invoke('cmd_get_log_path').then(setLogPath).catch(() => {});
    invoke('cmd_get_storage_files').then(setStorageFiles).catch(() => {});
  }, []);

  /** 打开"存储"页里的文件(后端只放行它自己报出来的那几个路径) */
  const openStorageFile = (name) => {
    const file = storageFiles.find((f) => f.name === name);
    if (!file) return;
    invoke('cmd_open_path', { path: file.path }).catch((e) => fail(t.openPathFailed(e)));
  };
  const fileNameOf = (name) => storageFiles.find((f) => f.name === name)?.name || '—';

  /** 全局热键的注册状态:没绑上/被占用时在这里提示,而不是让用户对着没反应的快捷键干瞪眼 */
  const refreshHotkeyStatus = () => {
    invoke('cmd_get_hotkey_status').then(setHotkeyStatus).catch(() => {});
  };
  useEffect(() => {
    refreshHotkeyStatus();
  }, [settings.hotkey]);

  const flash = (msg) => {
    setNotice(msg);
    window.setTimeout(() => setNotice(null), 2200);
  };

  const fail = (e) => {
    console.error(e);
    setNotice(String(e));
    window.setTimeout(() => setNotice(null), 3200);
  };

  /** 保存设置:失败(比如热键被占用)时上层会回滚,这里把错误弹给用户 */
  const commit = (patch, successMessage) => {
    const result = onChange({ ...settings, ...patch });
    if (result && typeof result.then === 'function') {
      result
        .then(() => {
          if (successMessage) flash(successMessage);
        })
        .catch((e) => fail(String(e)))
        .then(refreshHotkeyStatus); // 成功失败都刷新:失败时前端已回滚,状态要跟着回去
    }
  };

  const copyLogPath = () => {
    invoke('cmd_copy_log_path')
      .then(() => flash(t.pathCopied))
      .catch((e) => fail(String(e)));
  };

  const toggleAutostart = (next) => {
    setAutostart(next); // 先动界面,失败再回滚
    invoke('cmd_set_autostart', { enabled: next })
      .then((actual) => setAutostart(!!actual))
      .catch((e) => {
        setAutostart(!next);
        fail(t.autostartFailed(e));
      });
  };

  return (
    <div className="settings">
      <aside className="side">
        <button
          type="button"
          className={`side-item${tab === 'general' ? ' on' : ''}`}
          onClick={() => setTab('general')}
        >
          <IconSliders />
          {t.tabGeneral}
        </button>
        <button
          type="button"
          className={`side-item${tab === 'storage' ? ' on' : ''}`}
          onClick={() => setTab('storage')}
        >
          <IconDoc />
          {t.tabStorage}
        </button>
        <button
          type="button"
          className={`side-item${tab === 'about' ? ' on' : ''}`}
          onClick={() => setTab('about')}
        >
          <IconInfo />
          {t.tabAbout}
        </button>
      </aside>

      <section className="pane">
        <header className="pane-head" data-tauri-drag-region>
          <button type="button" className="icon-btn" title={t.back} onClick={onClose}>
            <IconChevronLeft />
          </button>
          <h1>
            {tab === 'general' ? t.tabGeneral : tab === 'storage' ? t.tabStorage : t.tabAbout}
          </h1>
        </header>

        <div className="pane-body">
          {tab === 'general' ? (
            <>
              <div className="group-title">{t.sectionGeneral}</div>
              <div className="card">
                <Row label={t.autostart}>
                  <Switch checked={autostart} onChange={toggleAutostart} label={t.autostart} />
                </Row>
                <Row label={t.quickPaste} hint={t.quickPasteTip}>
                  <Switch
                    checked={settings.quick_paste}
                    onChange={(v) => commit({ quick_paste: v })}
                    label={t.quickPaste}
                  />
                </Row>
              </div>

              <div className="group-title">{t.sectionHotkey}</div>
              <div className="card">
                <Row label={t.hotkeyToggle}>
                  <HotkeyRecorder
                    value={settings.hotkey}
                    t={t}
                    onRecordingChange={refreshHotkeyStatus}
                    onCommit={(accel) => commit({ hotkey: accel }, t.hotkeySaved)}
                  />
                </Row>
                {hotkeyStatus && !hotkeyStatus.ok && (
                  <div className="hotkey-warn">
                    <div>{t.hotkeyStatusFailed(hotkeyStatus.message)}</div>
                    <div className="hotkey-warn-tip">{t.hotkeyStatusTip}</div>
                  </div>
                )}
                {hotkeyStatus && hotkeyStatus.ok && hotkeyStatus.wayland && (
                  <div className="hotkey-warn soft">{t.hotkeyWayland}</div>
                )}
              </div>

              <div className="group-title">{t.sectionI18n}</div>
              <div className="card">
                <Row label={t.language}>
                  <Select
                    value={settings.language}
                    options={[
                      { value: 'zh', label: t.langZh },
                      { value: 'en', label: t.langEn },
                    ]}
                    onChange={(v) => {
                      commit({ language: v });
                      onLanguageChange?.(v);
                    }}
                    width={140}
                  />
                </Row>
              </div>
            </>
          ) : tab === 'storage' ? (
            <>
              <div className="group-title">{t.sectionStorage}</div>
              <div className="card">
                <Row label={t.maxItems} hint={t.maxItemsHint}>
                  <Select
                    value={settings.max_items}
                    options={KEEP_OPTIONS.map((n) => ({ value: n, label: String(n) }))}
                    onChange={(v) => commit({ max_items: v })}
                    width={110}
                  />
                </Row>
              </div>

              <div className="group-title">{t.sectionStorageUsage}</div>
              <div className="card">
                <Row label={t.storageTransient}>
                  <span className="storage-num">{t.storageCount(counts?.transient ?? 0)}</span>
                </Row>
                <Row label={t.storageGrouped}>
                  <span className="storage-num">{t.storageCount(counts?.grouped ?? 0)}</span>
                </Row>
                <Row label={t.storageFavorite}>
                  <span className="storage-num">{t.storageCount(counts?.favorite ?? 0)}</span>
                </Row>
              </div>

              <div className="group-title">{t.sectionStorageFiles}</div>
              <div className="card">
                <Row label={t.storageRecordsFile} onClick={() => openStorageFile('history.json')}>
                  <span className="link-value" title={t.storageRecordsTip}>
                    {fileNameOf('history.json')}
                    <IconExternalLink />
                  </span>
                </Row>
                <Row
                  label={t.storageSettingsFile}
                  onClick={() => openStorageFile('settings.json')}
                >
                  <span className="link-value">
                    {fileNameOf('settings.json')}
                    <IconExternalLink />
                  </span>
                </Row>
              </div>

              {/* 隐私说明常驻展示(原来挤在"最大条目数"的 tooltip 里) */}
              <div className="storage-note">{t.storagePrivacy}</div>
            </>
          ) : (
            <>
              <div className="group-title">{t.sectionAbout}</div>
              <div className="card about-card">
                <img className="about-logo" src="/icon.png" alt="" />
                <div className="about-text">
                  <div className="about-name">{appInfo?.name || t.appName}</div>
                  <div className="about-version">
                    {t.version} {appInfo?.version || '—'}
                  </div>
                </div>
              </div>

              <div className="group-title">{t.sectionLinks}</div>
              <div className="card">
                <Row
                  label={t.githubRepo}
                  onClick={() => openUrl(GITHUB_URL).catch((e) => fail(e))}
                >
                  <span className="link-value">
                    github.com/shuglx/kylinpaste
                    <IconExternalLink />
                  </span>
                </Row>
                <Row label={`${t.logFile}（${t.copyPath}）`} onClick={copyLogPath}>
                  <span
                    className="link-value log-path"
                    title={logPath || ''}
                  >
                    {logPath ? 'kylinpaste.log' : '—'}
                    <IconExternalLink />
                  </span>
                </Row>
              </div>
            </>
          )}
        </div>
      </section>

      {notice && <div className="toast">{notice}</div>}
    </div>
  );
}
