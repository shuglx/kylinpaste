// 图标集中放这里(App 与设置界面共用)。
//
// 除特别说明外,路径都取自 ref 项目同源的 **Tabler Icons**(`@tabler/icons-webfont`),
// 统一 18×18 + stroke 2;少数自定义图标在注释里标注了。

export const stroke = {
  fill: 'none',
  stroke: 'currentColor',
  strokeWidth: 1.8,
  strokeLinecap: 'round',
  strokeLinejoin: 'round',
};

/** 行内/顶栏图标统一规格(与 Tabler 的设计一致) */
export const iconStroke = { ...stroke, strokeWidth: 2 };

export const IconSearch = () => (
  <svg width="28" height="28" viewBox="0 0 24 24" {...stroke}>
    <circle cx="11" cy="11" r="7" />
    <path d="M16.5 16.5 21 21" />
  </svg>
);

export const IconPin = () => (
  <svg width="22" height="22" viewBox="0 0 24 24" fill="currentColor">
    <path d="M16 12V4h1V2H7v2h1v8l-2 2v2h5.2v6h1.6v-6H18v-2z" />
  </svg>
);

export const IconGear = () => (
  <svg width="22" height="22" viewBox="0 0 24 24" fill="currentColor">
    <path d="M19.14 12.94c.04-.3.06-.61.06-.94s-.02-.64-.07-.94l2.03-1.58a.49.49 0 0 0 .12-.61l-1.92-3.32a.49.49 0 0 0-.59-.22l-2.39.96a7.03 7.03 0 0 0-1.62-.94l-.36-2.54a.48.48 0 0 0-.48-.41h-3.84c-.24 0-.43.17-.47.41l-.36 2.54c-.59.24-1.13.57-1.62.94l-2.39-.96a.49.49 0 0 0-.59.22L2.74 8.87c-.12.21-.08.47.12.61l2.03 1.58c-.05.3-.09.63-.09.94s.02.64.07.94l-2.03 1.58a.49.49 0 0 0-.12.61l1.92 3.32c.12.22.37.29.59.22l2.39-.96c.5.38 1.03.7 1.62.94l.36 2.54c.05.24.24.41.48.41h3.84c.24 0 .44-.17.47-.41l.36-2.54c.59-.24 1.13-.57 1.62-.94l2.39.96c.22.08.47 0 .59-.22l1.92-3.32a.49.49 0 0 0-.12-.61zM12 15.6A3.6 3.6 0 1 1 15.6 12 3.6 3.6 0 0 1 12 15.6z" />
  </svg>
);

/** 清理临时记录:Tabler `trash-x`(ref 项目给"清空剪贴板历史"用的就是这个) */
export const IconClean = () => (
  <svg width="22" height="22" viewBox="0 0 24 24" {...iconStroke}>
    <path d="M4 7h16" />
    <path d="M5 7l1 12a2 2 0 0 0 2 2h8a2 2 0 0 0 2 -2l1 -12" />
    <path d="M9 7v-3a1 1 0 0 1 1 -1h4a1 1 0 0 1 1 1v3" />
    <path d="M10 12l4 4m0 -4l-4 4" />
  </svg>
);

/** 收藏:Tabler `star`(描边与实心共用一条路径,只切 fill,收藏时不会跳形) */
export const IconStar = ({ filled }) => (
  <svg width="18" height="18" viewBox="0 0 24 24" {...iconStroke} fill={filled ? 'currentColor' : 'none'}>
    <path d="M12 17.75l-6.172 3.245l1.179 -6.873l-5 -4.867l6.9 -1l3.086 -6.253l3.086 6.253l6.9 1l-5 4.867l1.179 6.873z" />
  </svg>
);

/** 分组:Tabler `tag` */
export const IconTag = () => (
  <svg width="18" height="18" viewBox="0 0 24 24" {...iconStroke}>
    <path d="M7.5 7.5m-1 0a1 1 0 1 0 2 0a1 1 0 1 0 -2 0" />
    <path d="M3 6v5.172a2 2 0 0 0 .586 1.414l7.71 7.71a2.41 2.41 0 0 0 3.408 0l5.592 -5.592a2.41 2.41 0 0 0 0 -3.408l-7.71 -7.71a2 2 0 0 0 -1.414 -.586h-5.172a3 3 0 0 0 -3 3z" />
  </svg>
);

/** 删除:Tabler `trash` */
export const IconTrash = () => (
  <svg width="18" height="18" viewBox="0 0 24 24" {...iconStroke}>
    <path d="M4 7l16 0" />
    <path d="M10 11l0 6" />
    <path d="M14 11l0 6" />
    <path d="M5 7l1 12a2 2 0 0 0 2 2h8a2 2 0 0 0 2 -2l1 -12" />
    <path d="M9 7v-3a1 1 0 0 1 1 -1h4a1 1 0 0 1 1 1v3" />
  </svg>
);

/** 下拉箭头:Tabler `chevron-down` */
export const IconChevron = () => (
  <svg width="12" height="12" viewBox="0 0 24 24" {...iconStroke} strokeWidth={2.4}>
    <path d="m6 9 6 6 6-6" />
  </svg>
);

/** 返回:Tabler `chevron-left` */
export const IconClose = () => (
  <svg width="18" height="18" viewBox="0 0 24 24" {...iconStroke}>
    <path d="M18 6l-12 12" />
    <path d="M6 6l12 12" />
  </svg>
);

export const IconChevronLeft = () => (
  <svg width="18" height="18" viewBox="0 0 24 24" {...iconStroke}>
    <path d="M15 6l-6 6l6 6" />
  </svg>
);

/** 通用(设置侧栏):自定义"调节杆" */
export const IconSliders = () => (
  <svg width="18" height="18" viewBox="0 0 24 24" {...iconStroke}>
    <path d="M4 7h6M16 7h4M4 17h4M14 17h6" />
    <circle cx="14" cy="7" r="2" />
    <circle cx="10" cy="17" r="2" />
  </svg>
);

/** 关于:Tabler `info-circle` */
export const IconInfo = () => (
  <svg width="18" height="18" viewBox="0 0 24 24" {...iconStroke}>
    <circle cx="12" cy="12" r="9" />
    <path d="M12 16v-4" />
    <path d="M12 8h.01" />
  </svg>
);

/** 外链:Tabler `external-link` */
export const IconExternalLink = () => (
  <svg width="14" height="14" viewBox="0 0 24 24" {...iconStroke} strokeWidth={2.2}>
    <path d="M12 6h-6a2 2 0 0 0 -2 2v10a2 2 0 0 0 2 2h10a2 2 0 0 0 2 -2v-6" />
    <path d="M11 13l9 -9" />
    <path d="M15 4h5v5" />
  </svg>
);

/** 键盘:Tabler `keyboard`(设置里"快捷键"一节) */
export const IconKeyboard = () => (
  <svg width="16" height="16" viewBox="0 0 24 24" {...iconStroke}>
    <rect x="2" y="6" width="20" height="12" rx="2" />
    <path d="M6 10h0M10 10h0M14 10h0M18 10h0M8 14h8" />
  </svg>
);

/** 记录类型图标 */
export const IconDoc = () => (
  <svg width="20" height="20" viewBox="0 0 24 24" {...stroke}>
    <path d="M7 3h7l4 4v13a1 1 0 0 1-1 1H7a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1z" />
    <path d="M14 3v4h4" />
  </svg>
);

export const IconImage = () => (
  <svg width="20" height="20" viewBox="0 0 24 24" {...stroke}>
    <rect x="3.5" y="5" width="17" height="14" rx="2.5" />
    <circle cx="9" cy="10" r="1.5" />
    <path d="m5 17 4.5-4.5L14 17l2.5-2.5L20 18" />
  </svg>
);

export const IconFolder = () => (
  <svg width="20" height="20" viewBox="0 0 24 24" {...stroke}>
    <path d="M3 7.5A2.5 2.5 0 0 1 5.5 5h3l1.7 2H18.5A2.5 2.5 0 0 1 21 9.5v7A2.5 2.5 0 0 1 18.5 19h-13A2.5 2.5 0 0 1 3 16.5z" />
  </svg>
);

export const IconLink = () => (
  <svg width="20" height="20" viewBox="0 0 24 24" {...stroke}>
    <path d="M10.5 13.5a3.6 3.6 0 0 0 5.1 0l3-3a3.6 3.6 0 0 0-5.1-5.1l-.9.9" />
    <path d="M13.5 10.5a3.6 3.6 0 0 0-5.1 0l-3 3a3.6 3.6 0 0 0 5.1 5.1l.9-.9" />
  </svg>
);
