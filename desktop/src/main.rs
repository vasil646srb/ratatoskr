//! yggproxy for Linux: one binary for the service (`daemon`), the console commands and the GUI.

mod api;
mod cli;
mod config;
mod daemon;
#[cfg(feature = "gui")]
mod gui;
mod install;
mod tracker;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has_display = std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or(if has_display && cfg!(feature = "gui") { "gui" } else { "status" });
    let res = match cmd {
        "daemon" => daemon::main(),
        "install" => install::install(),
        "tracker" => tracker::main(),
        "install-tracker" => install::install_tracker(),
        "uninstall" => install::uninstall(),
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
    gui::run()
}

#[cfg(not(feature = "gui"))]
fn gui() -> Result<(), String> {
    Err("эта сборка без GUI (серверная): используйте консольные команды, yggproxy help".into())
}
