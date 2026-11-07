//! Проверка обновлений по последнему релизу на GitHub. Kadro ничего не скачивает сам,
//! а предлагает открыть страницу релиза.

use dispatch2::DispatchQueue;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSApplication, NSWorkspace};
use objc2_foundation::{NSString, NSURL, ns_string};

use crate::settings::{self, Settings};
use crate::{output, upload};

const LATEST_RELEASE: &str = "https://api.github.com/repos/binido/Kadro/releases/latest";
/// Автоматическая проверка не чаще раза в сутки.
const CHECK_INTERVAL: u64 = 86_400;

/// Версия и страница последнего релиза из ответа GitHub API.
pub fn parse_release(json: &str) -> Option<(String, String)> {
    let field = |name: &str| {
        let key = format!("\"{name}\"");
        let after = &json[json.find(&key)? + key.len()..];
        let start = after.find('"')? + 1;
        let end = after[start..].find('"')? + start;
        Some(after[start..end].to_string())
    };
    Some((field("tag_name")?.trim_start_matches('v').to_string(), field("html_url")?))
}

/// Новее ли версия `latest`, чем `current`. Версии сравниваются по числам через точку.
pub fn is_newer(latest: &str, current: &str) -> bool {
    let numbers = |version: &str| -> Vec<u64> {
        version
            .split('.')
            .map(|part| part.chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0))
            .collect()
    };
    let (latest, current) = (numbers(latest), numbers(current));
    let length = latest.len().max(current.len());
    let pad = |mut v: Vec<u64>| {
        v.resize(length, 0);
        v
    };
    pad(latest) > pad(current)
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Проверка при запуске: раз в сутки, если не выключена. Сообщает только о новой версии.
pub fn check_on_launch() {
    if !Settings::load().check_updates {
        return;
    }
    let last = settings::string(settings::LAST_UPDATE_CHECK).and_then(|t| t.parse::<u64>().ok()).unwrap_or(0);
    if now().saturating_sub(last) >= CHECK_INTERVAL {
        check(false);
    }
}

/// Спрашивает GitHub о последнем релизе. `manual` - проверку попросил пользователь,
/// тогда и «обновлений нет», и ошибка показываются.
pub fn check(manual: bool) {
    output::in_background(move || {
        let result =
            upload::send("GET", LATEST_RELEASE, &[("Accept".into(), "application/vnd.github+json".into())], b"")
                .and_then(|body| {
                    parse_release(&String::from_utf8_lossy(&body))
                        .ok_or_else(|| "GitHub answered without a release".into())
                });
        DispatchQueue::main().exec_async(move || {
            let Some(mtm) = MainThreadMarker::new() else { return };
            settings::set_string(settings::LAST_UPDATE_CHECK, &now().to_string());
            let current = env!("CARGO_PKG_VERSION");
            match result {
                Ok((latest, page)) if is_newer(&latest, current) => offer(mtm, &latest, &page),
                Ok(_) if manual => {
                    tell(mtm, "Kadro is up to date", &format!("You have the latest version, {current}."));
                }
                Err(message) if manual => tell(mtm, "Couldn't check for updates", &message),
                _ => {}
            }
        });
    });
}

fn offer(mtm: MainThreadMarker, version: &str, page: &str) {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(&format!("Kadro {version} is available")));
    alert.setInformativeText(&NSString::from_str(&format!(
        "You have {}. The release page has the installer and what changed.",
        env!("CARGO_PKG_VERSION")
    )));
    alert.addButtonWithTitle(ns_string!("Open Release Page"));
    alert.addButtonWithTitle(ns_string!("Later"));
    NSApplication::sharedApplication(mtm).activate();
    if alert.runModal() == NSAlertFirstButtonReturn
        && let Some(url) = NSURL::URLWithString(&NSString::from_str(page))
    {
        NSWorkspace::sharedWorkspace().openURL(&url);
    }
}

fn tell(mtm: MainThreadMarker, title: &str, message: &str) {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(message));
    NSApplication::sharedApplication(mtm).activate();
    alert.runModal();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_version_and_page_are_read_from_the_api_answer() {
        let json = r#"{"url":"https://api.github.com/x","html_url":"https://github.com/binido/Kadro/releases/tag/v0.12.0","id":1,"tag_name":"v0.12.0","name":"Kadro v0.12.0"}"#;
        assert_eq!(
            parse_release(json),
            Some(("0.12.0".into(), "https://github.com/binido/Kadro/releases/tag/v0.12.0".into()))
        );
        assert_eq!(parse_release(r#"{"message":"Not Found"}"#), None);
    }

    #[test]
    fn versions_compare_by_numbers() {
        assert!(is_newer("0.12.0", "0.9.0"));
        assert!(is_newer("1.0", "0.13.2"));
        assert!(is_newer("0.13.1", "0.13"));
        assert!(!is_newer("0.12.0", "0.12.0"));
        assert!(!is_newer("0.11.9", "0.12.0"));
    }
}
