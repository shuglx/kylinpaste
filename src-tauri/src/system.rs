//! 单实例(unix socket)与开机自启(XDG autostart)
//! tauri v1 无对应插件,手工实现

use std::io::Write;
use std::os::unix::net::{UnixListener, UnixStream};
use tauri::AppHandle;

// ---------- 单实例 ----------

/// Linux 抽象命名空间 socket,不落盘
#[cfg(target_os = "linux")]
const SOCK_ADDR: &str = "\0kylinpaste.instance.sock";

#[cfg(not(target_os = "linux"))]
fn sock_path() -> std::path::PathBuf {
    std::env::temp_dir().join("kylinpaste-instance.sock")
}

/// 尝试成为唯一实例。true = 本进程是首实例;false = 已有实例(已请求其显示窗口)。
pub fn acquire_single_instance(app: AppHandle) -> bool {
    #[cfg(target_os = "linux")]
    let bind = || UnixListener::bind(SOCK_ADDR);
    #[cfg(not(target_os = "linux"))]
    let bind = || {
        let p = sock_path();
        let _ = std::fs::remove_file(&p);
        UnixListener::bind(p)
    };

    match bind() {
        Ok(listener) => {
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
        Err(_) => {
            // 已有实例:通知它显示主窗口
            #[cfg(target_os = "linux")]
            let connect = || UnixStream::connect(SOCK_ADDR);
            #[cfg(not(target_os = "linux"))]
            let connect = || UnixStream::connect(sock_path());
            if let Ok(mut s) = connect() {
                let _ = s.write_all(b"show");
            }
            false
        }
    }
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
    Ok(is_autostart())
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
