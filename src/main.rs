// GUI default: no console window. config.json `"console": true` allocates one.
#![cfg_attr(windows, windows_subsystem = "windows")]

use anyhow::Result;
use std::io::Write;
use tracing_subscriber::EnvFilter;

mod app;

fn main() {
    // Kill any inherited console flash *before* config load / GUI init.
    // (windows_subsystem=windows normally has no console; this is belt-and-suspenders.)
    let force_headless_early = std::env::args().any(|a| a == "--headless" || a == "-H")
        || std::env::var_os("OHMYCOPY_HEADLESS").is_some();
    let force_console_early = std::env::args().any(|a| a == "--console")
        || std::env::var_os("OHMYCOPY_CONSOLE").is_some();
    if !force_headless_early && !force_console_early {
        ohmycopy::console_win::hide_early();
    }

    // Always capture a useful panic location (even without RUST_BACKTRACE).
    std::panic::set_hook(Box::new(|info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown".into());
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "Box<Any>".into()
        };

        let msg = format!(
            "OhMyCopy {} panicked\n  location: {}\n  message:  {}\n",
            env!("CARGO_PKG_VERSION"),
            location,
            payload
        );

        let _ = write_crash_log(&msg);
        // Ensure a console exists so the user can see the panic if possible.
        ohmycopy::console_win::set_visible(true);
        eprintln!("{msg}");
        if std::env::var_os("RUST_BACKTRACE").is_some() {
            eprintln!("{info}");
        } else {
            eprintln!("提示: 设置环境变量 RUST_BACKTRACE=1 可显示完整调用栈");
            eprintln!("崩溃日志也会写入 exe 目录下的 crash.log");
        }
        ohmycopy::console_win::error_message_box("OhMyCopy 崩溃", &msg);
    }));

    if let Err(e) = real_main() {
        let msg = format!("OhMyCopy 启动失败: {e:?}");
        let _ = write_crash_log(&format!("{msg}\n"));
        ohmycopy::console_win::set_visible(true);
        eprintln!("{msg}");
        eprintln!("按 Enter 退出…");
        let _ = std::io::stdin().read_line(&mut String::new());
        ohmycopy::console_win::error_message_box("OhMyCopy 启动失败", &format!("{e:#}"));
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let force_headless = std::env::args().any(|a| a == "--headless" || a == "-H")
        || std::env::var_os("OHMYCOPY_HEADLESS").is_some();
    let force_console = std::env::args().any(|a| a == "--console")
        || std::env::var_os("OHMYCOPY_CONSOLE").is_some();

    // Load config first so we know whether to show a console.
    let cfg = ohmycopy::config::Config::load_or_create()?;
    // Headless / explicit console: show black console. Default GUI: never.
    let show_console = force_console || force_headless || cfg.console;
    if show_console {
        ohmycopy::console_win::set_visible(true);
    } else {
        // Hide again after config load (in case anything reattached).
        ohmycopy::console_win::hide_early();
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        console = show_console,
        "OhMyCopy starting"
    );

    app::run_with_config(cfg, force_headless)
}

fn write_crash_log(msg: &str) -> Result<()> {
    let dir = ohmycopy::config::Config::config_dir().unwrap_or_else(|_| {
        std::env::temp_dir().join("OhMyCopy")
    });
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("crash.log");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    writeln!(f, "---- {ts} ----")?;
    write!(f, "{msg}")?;
    Ok(())
}
