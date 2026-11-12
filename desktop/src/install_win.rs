//! Windows: `yggproxy install` / `install-tracker` / `uninstall` (Windows services, Start menu
//! shortcut, PATH) and the service entry points the service manager starts.

use crate::platform::{self, command};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;
use windows_service::service::{
    ServiceAccess, ServiceAction, ServiceActionType, ServiceControl, ServiceControlAccept, ServiceErrorControl,
    ServiceExitCode, ServiceFailureActions, ServiceFailureResetPeriod, ServiceInfo, ServiceStartType, ServiceState,
    ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult, ServiceStatusHandle};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

const SERVICE: &str = "Ratatoskr";
const TRACKER_SERVICE: &str = "RatatoskrTracker";

fn install_dir() -> PathBuf {
    PathBuf::from(std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into())).join("Ratatoskr")
}

fn bin_path() -> PathBuf {
    install_dir().join("yggproxy.exe")
}

fn shortcut_path() -> PathBuf {
    PathBuf::from(std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into()))
        .join(r"Microsoft\Windows\Start Menu\Programs\Ratatoskr.lnk")
}

fn ps(script: &str) -> Result<(), String> {
    let st = command("powershell").args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script])
        .status()
        .map_err(|e| format!("powershell: {e}"))?;
    if st.success() { Ok(()) } else { Err(format!("powershell завершился с ошибкой ({st})")) }
}

/// Not elevated: run the same command again through the UAC prompt and wait for it.
fn elevate(arg: &str) -> Result<bool, String> {
    if platform::is_admin() {
        return Ok(false);
    }
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    println!("Нужны права администратора: подтвердите запрос Windows.");
    ps(&format!(
        "$p = Start-Process -FilePath '{}' -ArgumentList '{arg}','--pause' -Verb RunAs -Wait -PassThru; exit $p.ExitCode",
        me.display()
    ))?;
    Ok(true)
}

/// `--pause` (set by `elevate`): keep the elevated console open so its output can be read.
fn pause_if_asked() {
    if std::env::args().any(|a| a == "--pause") {
        println!("\nНажмите Enter, чтобы закрыть окно.");
        let _ = std::io::stdin().read_line(&mut String::new());
    }
}

fn manager() -> Result<ServiceManager, String> {
    ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)
        .map_err(|e| format!("диспетчер служб: {e}"))
}

/// Stop a service if it exists and wait until it has stopped.
fn stop_service(m: &ServiceManager, name: &str) {
    let Ok(s) = m.open_service(name, ServiceAccess::QUERY_STATUS | ServiceAccess::STOP) else { return };
    let _ = s.stop();
    for _ in 0..100 {
        match s.query_status() {
            Ok(st) if st.current_state != ServiceState::Stopped => std::thread::sleep(Duration::from_millis(200)),
            _ => break,
        }
    }
}

/// Copy this exe to Program Files (skipped when it already runs from there).
fn copy_self() -> Result<PathBuf, String> {
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let dst = bin_path();
    std::fs::create_dir_all(install_dir()).map_err(|e| format!("{}: {e}", install_dir().display()))?;
    let same = std::fs::canonicalize(&me).ok() == std::fs::canonicalize(&dst).ok();
    if !same {
        std::fs::copy(&me, &dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    }
    Ok(dst)
}

fn create_or_update(m: &ServiceManager, name: &str, display: &str, descr: &str, arg: &str, exe: &PathBuf) -> Result<(), String> {
    let info = ServiceInfo {
        name: OsString::from(name),
        display_name: OsString::from(display),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe.clone(),
        launch_arguments: vec![OsString::from(arg)],
        dependencies: vec![],
        account_name: None, // LocalSystem: the VPN needs to create adapters and routes
        account_password: None,
    };
    let access = ServiceAccess::QUERY_STATUS | ServiceAccess::START | ServiceAccess::STOP | ServiceAccess::CHANGE_CONFIG;
    let s = match m.open_service(name, access) {
        Ok(s) => {
            s.change_config(&info).map_err(|e| format!("служба {name}: {e}"))?;
            s
        }
        Err(_) => m.create_service(&info, access).map_err(|e| format!("служба {name}: {e}"))?,
    };
    let _ = s.set_description(descr);
    let restart = ServiceAction { action_type: ServiceActionType::Restart, delay: Duration::from_secs(5) };
    let _ = s.update_failure_actions(ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(86400)),
        reboot_msg: None,
        command: None,
        actions: Some(vec![restart.clone(), restart.clone(), restart]),
    });
    s.start::<&str>(&[]).map_err(|e| format!("запуск службы {name}: {e}"))?;
    Ok(())
}

pub fn install() -> Result<(), String> {
    if elevate("install")? {
        return Ok(());
    }
    let r = (|| {
        let m = manager()?;
        stop_service(&m, SERVICE);
        let exe = copy_self()?;
        create_or_update(&m, SERVICE, "Ratatoskr (YggProxy)", "Интернет через Yggdrasil: VPN-клиент и сервер выхода", "service", &exe)?;
        let dir = install_dir();
        let lnk = shortcut_path();
        let _ = ps(&format!(
            "$s = (New-Object -ComObject WScript.Shell).CreateShortcut('{}'); $s.TargetPath = '{}'; $s.Arguments = 'gui'; \
             $s.WorkingDirectory = '{}'; $s.Description = 'Ratatoskr: интернет через Yggdrasil'; $s.Save()",
            lnk.display(),
            exe.display(),
            dir.display()
        ));
        let _ = ps(&format!(
            "$d = '{}'; $p = [Environment]::GetEnvironmentVariable('Path', 'Machine'); \
             if (($p -split ';') -notcontains $d) {{ [Environment]::SetEnvironmentVariable('Path', $p.TrimEnd(';') + ';' + $d, 'Machine') }}",
            dir.display()
        ));
        println!("Установлено: {}, служба {SERVICE} запущена и стартует вместе с Windows.", exe.display());
        println!("Ярлык «Ratatoskr» — в меню «Пуск». В новой командной строке: yggproxy status · yggproxy connect · yggproxy gui");
        Ok(())
    })();
    if let Err(e) = &r {
        eprintln!("yggproxy: {e}");
    }
    pause_if_asked();
    r
}

pub fn install_tracker() -> Result<(), String> {
    if elevate("install-tracker")? {
        return Ok(());
    }
    let r = (|| {
        let m = manager()?;
        stop_service(&m, TRACKER_SERVICE);
        let exe = copy_self()?;
        create_or_update(&m, TRACKER_SERVICE, "Ratatoskr tracker", "Ищет серверы выхода Ratatoskr в Yggdrasil и раздаёт их список (порт 27081)", "tracker-service", &exe)?;
        println!("Трекер установлен и запущен (служба {TRACKER_SERVICE}). У него свой узел Yggdrasil: yggdrasil-go не нужен.");
        println!("Журнал: {}", crate::config::state_dir().join("tracker.log").display());
        Ok(())
    })();
    if let Err(e) = &r {
        eprintln!("yggproxy: {e}");
    }
    pause_if_asked();
    r
}

pub fn uninstall() -> Result<(), String> {
    if elevate("uninstall")? {
        return Ok(());
    }
    let m = manager()?;
    for name in [SERVICE, TRACKER_SERVICE] {
        stop_service(&m, name);
        if let Ok(s) = m.open_service(name, ServiceAccess::DELETE) {
            let _ = s.delete();
        }
    }
    let _ = std::fs::remove_file(shortcut_path());
    let _ = command("netsh").args(["advfirewall", "firewall", "delete", "rule", "name=Ratatoskr tracker"]).status();
    println!("Службы удалены. Настройки и ключ узла остались в {}.", crate::config::state_dir().parent().unwrap_or(&crate::config::state_dir()).display());
    println!("Файл программы: {} (можно удалить вручную).", bin_path().display());
    pause_if_asked();
    Ok(())
}

/// Start the GUI's install through the UAC prompt (no waiting: the window keeps working).
pub fn install_elevated_detached() {
    if let Ok(me) = std::env::current_exe() {
        let _ = command("powershell")
            .args(["-NoProfile", "-Command", &format!("Start-Process -FilePath '{}' -ArgumentList 'install','--pause' -Verb RunAs", me.display())])
            .spawn();
    }
}

// ------------------------------------------------------------------------------------------
// Service entry points

static KIND: OnceLock<&'static str> = OnceLock::new();
static HANDLE: OnceLock<ServiceStatusHandle> = OnceLock::new();

define_windows_service!(ffi_service_main, service_main);

fn status(state: ServiceState, code: u32) {
    if let Some(h) = HANDLE.get() {
        let _ = h.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: if state == ServiceState::Running { ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN } else { ServiceControlAccept::empty() },
            exit_code: ServiceExitCode::Win32(code),
            checkpoint: 0,
            wait_hint: Duration::from_secs(15),
            process_id: None,
        });
    }
}

fn service_main(_args: Vec<OsString>) {
    let kind = *KIND.get().unwrap_or(&"service");
    let name = if kind == "tracker" { TRACKER_SERVICE } else { SERVICE };
    let Ok(h) = service_control_handler::register(name, move |c| match c {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            status(ServiceState::StopPending, 0);
            platform::request_stop();
            if kind == "tracker" {
                // The tracker has nothing to undo: stop right away.
                status(ServiceState::Stopped, 0);
                std::process::exit(0);
            }
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    }) else {
        return;
    };
    let _ = HANDLE.set(h);
    status(ServiceState::Running, 0);
    let r = if kind == "tracker" { crate::tracker::main() } else { crate::daemon::main() };
    if let Err(e) = &r {
        let _ = std::fs::write(crate::config::state_dir().join("service-error.txt"), e);
    }
    status(ServiceState::Stopped, if r.is_ok() { 0 } else { 1 });
}

/// `yggproxy service` / `tracker-service`: only the Windows service manager starts these.
pub fn run_service(kind: &'static str) -> Result<(), String> {
    let _ = KIND.set(kind);
    let name = if kind == "tracker" { TRACKER_SERVICE } else { SERVICE };
    service_dispatcher::start(name, ffi_service_main)
        .map_err(|e| format!("{e}: эту команду запускает диспетчер служб Windows; для установки службы — yggproxy install"))
}
