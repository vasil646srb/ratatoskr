//! yggproxy for Linux and Windows: one binary for the service (`daemon`), the console commands
//! and the GUI.

mod api;
mod cli;
mod config;
mod daemon;
#[cfg(feature = "gui")]
mod gui;
#[cfg(unix)]
mod install;
#[cfg(windows)]
#[path = "install_win.rs"]
mod install;
mod platform;
mod peerstats;
mod tracker;
mod tracker_client;
mod ygg_net;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has_display = cfg!(windows) || std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or(if has_display && cfg!(feature = "gui") { "gui" } else { "status" });
    let res = match cmd {
        "daemon" => daemon::main(),
        "install" => install::install(),
        "tracker" => tracker::main(),
        "install-tracker" => install::install_tracker(),
        "uninstall" => install::uninstall(),
        #[cfg(windows)]
        "service" => install::run_service("service"),
        #[cfg(windows)]
        "tracker-service" => install::run_service("tracker"),
        "gui" => gui(),
        "version" | "--version" => {
            println!("yggproxy {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => cli::run(&args),
    };
    if let Err(e) = res {
        eprintln!("yggproxy: {e}");
        std::process::exit(1);
    }
}

#[cfg(feature = "gui")]
fn gui() -> Result<(), String> {
    // Windows: started from the Start menu or by double-click, the console window was made just
    // for us; let it go so only the app window stays (from cmd it keeps working).
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Console::{FreeConsole, GetConsoleProcessList};
        let mut ids = [0u32; 4];
        if GetConsoleProcessList(ids.as_mut_ptr(), 4) <= 1 {
            FreeConsole();
        }
    }
    let r = gui::run();
    // Windows: the console is gone (or was never seen): say what happened in a message box.
    #[cfg(windows)]
    if let Err(e) = &r {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
        let text = format!(
            "Не удалось открыть окно Ratatoskr:\n{e}\n\nСлужба и консольные команды работают без окна: \
             yggproxy status, yggproxy connect, yggproxy server on.\n\nМожно попробовать другой способ отрисовки: \
             переменная окружения YGGPROXY_RENDERER=glow."
        );
        let w = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
        let (t, c) = (w(&text), w("Ratatoskr"));
        unsafe { MessageBoxW(0, t.as_ptr(), c.as_ptr(), MB_OK | MB_ICONERROR) };
    }
    r
}

#[cfg(not(feature = "gui"))]
fn gui() -> Result<(), String> {
    Err("эта сборка без GUI (серверная): используйте консольные команды, yggproxy help".into())
}
