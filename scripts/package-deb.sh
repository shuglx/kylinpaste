#!/usr/bin/env bash
# 手工打包 deb(在 arm64 构建机上执行)
# 依赖: dpkg-deb(宿主机为 arm64 时无需交叉处理)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

VERSION="$(node -p "require('./package.json').version")"
ARCH="arm64"
BIN="src-tauri/target/release/kylinpaste"
STAGE="dist-deb/kylinpaste"
OUT="dist-deb/kylinpaste_${VERSION}_${ARCH}.deb"

if [ ! -f "$BIN" ]; then
  echo "错误: 未找到 $BIN,请先在 focal arm64 容器内执行 cargo build --release" >&2
  exit 1
fi

rm -rf "$STAGE"
mkdir -p \
  "$STAGE/DEBIAN" \
  "$STAGE/usr/bin" \
  "$STAGE/usr/share/applications" \
  "$STAGE/usr/share/icons/hicolor/32x32/apps" \
  "$STAGE/usr/share/icons/hicolor/128x128/apps" \
  "$STAGE/usr/share/icons/hicolor/256x256/apps" \
  "$STAGE/usr/share/icons/hicolor/512x512/apps"

cp "$BIN" "$STAGE/usr/bin/kylinpaste"
cp src-tauri/icons/32x32.png   "$STAGE/usr/share/icons/hicolor/32x32/apps/kylinpaste.png"
cp src-tauri/icons/128x128.png "$STAGE/usr/share/icons/hicolor/128x128/apps/kylinpaste.png"
cp src-tauri/icons/256x256.png "$STAGE/usr/share/icons/hicolor/256x256/apps/kylinpaste.png"
cp src-tauri/icons/512x512.png "$STAGE/usr/share/icons/hicolor/512x512/apps/kylinpaste.png"

cat > "$STAGE/usr/share/applications/kylinpaste.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=KylinPaste
Name[zh_CN]=麒麟剪贴板
Comment=Clipboard history manager
Comment[zh_CN]=剪贴板历史管理
Exec=/usr/bin/kylinpaste
Icon=kylinpaste
Terminal=false
Categories=Utility;
EOF

# 运行时依赖均为麒麟 V10 SP1(Ubuntu 20.04 基座)自带,
# 内网环境无法联网装包,故必须与目标机现状一致
cat > "$STAGE/DEBIAN/control" <<EOF
Package: kylinpaste
Version: ${VERSION}
Section: utils
Priority: optional
Architecture: ${ARCH}
Depends: libwebkit2gtk-4.0-37, libgtk-3-0, libxkbcommon0
Maintainer: ryan <ryan@localhost>
Description: KylinPaste clipboard history manager for Kylin V10 (arm64)
 剪贴板历史管理工具:全类型记录、搜索、快捷粘贴、收藏分组。
EOF

dpkg-deb --build --root-owner-group "$STAGE" "$OUT"
echo "打包完成: $OUT"
