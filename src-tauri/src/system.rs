//! 单实例(unix socket)与开机自启(XDG autostart)
//! tauri v1 无对应插件,手工实现

use std::io::Write;
use std::os::unix::net::{UnixListener, UnixStream};
use tauri::AppHandle;

// ---------- 单实例 ----------

/// socket 路径:优先 XDG_RUNTIME_DIR(per-user、0700、登出自动清理),
/// 回退到用户缓存目录。均为单用户目录,无需 uid 后缀。
fn sock_path() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        let p = std::path::PathBuf::from(dir).join("kylinpaste.sock");
        if p.is_absolute() {
            return p;
        }
    }
    dirs::cache_dir()
        .unwrap_or_else(|| std::env::temp_dir())
        .join("kylinpaste-instance.sock")
}

/// 尝试成为唯一实例。
/// 返回 true = 本进程继续运行(首实例或单实例检测降级);
/// 返回 false = 确认已有活实例(已请求其显示窗口),本进程应退出。
///
/// 注意:任何绑定异常都不得让应用退出——单实例只是辅助功能。
pub fn acquire_single_instance(app: AppHandle) -> bool {
    let path = sock_path();

    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(first_err) => {
            // 绑定失败:先探活,区分"真有实例"和"陈旧 socket 残留"
            match UnixStream::connect(&path) {
                Ok(mut s) => {
                    // 有活实例:请求其显示主窗口
                    let _ = s.write_all(b"show");
                    return false;
                }
                Err(_) => {
                    // 无活实例:清理陈旧 socket 文件后重试
                    let _ = std::fs::remove_file(&path);
                    match UnixListener::bind(&path) {
                        Ok(l) => l,
                        Err(e) => {
                            eprintln!(
                                "[单实例] 绑定 {path:?} 失败({first_err} / {e}),单实例检测本次禁用"
                            );
                            return true;
                        }
                    }
                }
            }
        }
    };

    // 首实例:监听后续实例的唤起请求
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            if let Ok(mut s) = stream {
                let _ = s.write_all(b"show");
                crate::toggle_main_window(&app);
            }
        }
    });
    true
}

// ---------- 开机自启(XDG autostart) ----------

fn autostart_desktop_path() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|d| d.join("autostart").join("kylinpaste.desktop"))
}

pub fn set_autostart(enable: bool) -> Result<bool, String> {
    let path = autostart_desktop_path().ok_or("无法定位用户配置目录")?;
    if enable {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let content = format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=KylinPaste\n\
             Comment=Clipboard manager\n\
             Exec={}\n\
             Icon=kylinpaste\n\
             Terminal=false\n\
             X-GNOME-Autostart-enabled=true\n",
            exe.display()
        );
        std::fs::write(&path, content).map_err(|e| e.to_string())?;
    } else {
        let _ = std::fs::remove_file(&path);
    }
    crate::klog!(
        "[自启] {} -> {}({})",
        if enable { "开启" } else { "关闭" },
        path.display(),
        if is_autostart() { "文件在位" } else { "文件已移除" }
    );
    Ok(is_autostart())
}

/// 自启动文件的完整路径(界面反馈给用户,方便核对)
pub fn autostart_path_str() -> Option<String> {
    autostart_desktop_path().map(|p| p.to_string_lossy().into_owned())
}

pub fn is_autostart() -> bool {
    autostart_desktop_path()
        .map(|p| p.exists())
        .unwrap_or(false)
}

#[tauri::command]
pub fn cmd_get_autostart() -> bool {
    is_autostart()
}

#[tauri::command]
pub fn cmd_set_autostart(enabled: bool) -> Result<bool, String> {
    set_autostart(enabled)
}
