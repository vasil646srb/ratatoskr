//! New versions from the project's GitHub releases: the service checks twice a day and the
//! window/console offer them; `yggproxy update` checks now and, on Linux as root, installs.

use serde_json::{json, Value};

const API: &str = "https://api.github.com/repos/vasil646srb/ratatoskr/releases/latest";
pub const PAGE: &str = "https://github.com/vasil646srb/ratatoskr/releases/latest";

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// The release file this build is updated from.
pub fn asset() -> &'static str {
    if cfg!(windows) {
        "yggproxy-windows-x86_64.exe"
    } else if cfg!(target_os = "android") {
        "yggproxy-android-arm64"
    } else if cfg!(feature = "gui") {
        "yggproxy-desktop-linux-x86_64"
    } else {
        "yggproxy-server-linux-x86_64"
    }
}

/// "0.1.12" > "0.1.9": numbers compared one by one.
pub fn newer(a: &str, b: &str) -> bool {
    let n = |s: &str| -> Vec<u64> { s.trim_start_matches('v').split('.').map(|p| p.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap_or(0)).collect() };
    let (x, y) = (n(a), n(b));
    for i in 0..x.len().max(y.len()) {
        let (p, q) = (x.get(i).copied().unwrap_or(0), y.get(i).copied().unwrap_or(0));
        if p != q {
            return p > q;
        }
    }
    false
}

/// The latest release: `{version, url (our file), page, notes}`.
pub fn latest() -> Result<Value, String> {
    let v: Value = serde_json::from_str(&crate::platform::download_text(API)?).map_err(|e| format!("GitHub: {e}"))?;
    let version = v["tag_name"].as_str().ok_or("GitHub: нет версии")?.trim_start_matches('v').to_string();
    let url = v["assets"].as_array().into_iter().flatten()
        .find(|a| a["name"].as_str() == Some(asset()))
        .and_then(|a| a["browser_download_url"].as_str())
        .map(String::from);
    let notes = v["body"].as_str().unwrap_or("").split("## Установка").next().unwrap_or("").trim().to_string();
    Ok(json!({"version": version, "url": url, "page": v["html_url"].as_str().unwrap_or(PAGE), "notes": notes}))
}

/// The latest release if it is newer than this build.
pub fn available() -> Result<Option<Value>, String> {
    let r = latest()?;
    Ok(newer(r["version"].as_str().unwrap_or("0"), CURRENT).then_some(r))
}

/// Open a link in the browser (the window's «Скачать»).
#[allow(dead_code)]
pub fn open_url(url: &str) {
    #[cfg(windows)]
    let _ = crate::platform::command("explorer").arg(url).spawn();
    #[cfg(not(windows))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// `yggproxy update`: check; on Linux as root download the new binary and install it (service
/// restarted, the tracker too when it runs).
pub fn run() -> Result<(), String> {
    println!("установлена версия {CURRENT}, проверяю GitHub…");
    let Some(r) = available()? else {
        println!("это последняя версия");
        return Ok(());
    };
    let ver = r["version"].as_str().unwrap_or("?");
    println!("доступна версия {ver}\n");
    let notes = r["notes"].as_str().unwrap_or("");
    if !notes.is_empty() {
        println!("{notes}\n");
    }
    let url = r["url"].as_str().map(String::from);
    #[cfg(all(unix, not(target_os = "android")))]
    if crate::platform::is_admin() {
        let url = url.ok_or("в релизе нет файла для этой сборки")?;
        println!("скачиваю {url}");
        let bytes = download(&url)?;
        let tmp = std::env::temp_dir().join(format!("yggproxy-{ver}"));
        std::fs::write(&tmp, &bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
        let out = std::process::Command::new(&tmp).arg("version").output().map_err(|e| format!("новый файл не запускается: {e}"))?;
        if !String::from_utf8_lossy(&out.stdout).contains(ver) {
            let _ = std::fs::remove_file(&tmp);
            return Err("скачанный файл не той версии — обновление отменено".into());
        }
        let ok = std::process::Command::new(&tmp).arg("install").status().map(|s| s.success()).unwrap_or(false);
        let _ = std::fs::remove_file(&tmp);
        if !ok {
            return Err("установка не удалась".into());
        }
        let _ = std::process::Command::new("systemctl").args(["try-restart", "yggproxy-tracker"]).status();
        println!("обновлено до {ver}");
        return Ok(());
    }
    match url {
        Some(u) if cfg!(windows) => println!("Скачайте и запустите: {u}"),
        Some(u) if cfg!(target_os = "android") => println!(
            "pkill -f 'yggproxy tracker'\nwget -O $PREFIX/bin/yggproxy {u}\nchmod +x $PREFIX/bin/yggproxy && nohup yggproxy tracker >> ~/tracker.log 2>&1 &"),
        Some(_) => println!("Установить: sudo yggproxy update"),
        None => println!("Страница релиза: {}", r["page"].as_str().unwrap_or(PAGE)),
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "android")))]
fn download(url: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(15))
        .timeout_read(std::time::Duration::from_secs(120))
        .user_agent(concat!("yggproxy/", env!("CARGO_PKG_VERSION")))
        .build();
    let resp = agent.get(url).call().map_err(|e| e.to_string())?;
    let mut b = Vec::new();
    resp.into_reader().take(200 << 20).read_to_end(&mut b).map_err(|e| e.to_string())?;
    Ok(b)
}

#[cfg(test)]
mod tests {
    #[test]
    fn versions() {
        assert!(super::newer("0.1.12", "0.1.9"));
        assert!(super::newer("v0.2.0", "0.1.12"));
        assert!(!super::newer("0.1.12", "0.1.12"));
        assert!(!super::newer("0.1.11", "0.1.12"));
    }
}
