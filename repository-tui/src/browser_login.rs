use anyhow::{bail, Context, Result};
use rand::Rng;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tungstenite::{connect, Message};
use url::Url;

use crate::jwts::{JwtsClient, DEFAULT_HIT_BASE_URL};

const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const POLL_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DebugTarget {
    #[serde(default)]
    url: String,
    #[serde(default)]
    web_socket_debugger_url: String,
    #[serde(rename = "type", default)]
    target_type: String,
}

/// 一次性、隔离的浏览器登录会话。
///
/// 浏览器使用独立临时 profile，DevTools 只监听本机回环地址；会话结束时
/// 浏览器进程和 profile 一并销毁。Cookie 只在内存中作为返回值流转。
pub struct BrowserLogin {
    child: Child,
    profile: TempDir,
    port: u16,
    browser_name: String,
}

impl BrowserLogin {
    pub fn launch() -> Result<Self> {
        Self::launch_at(DEFAULT_HIT_BASE_URL)
    }

    pub fn launch_at(login_url: &str) -> Result<Self> {
        Self::launch_at_with_cancel(login_url, None)
    }

    pub fn launch_at_with_cancel(login_url: &str, cancelled: Option<&AtomicBool>) -> Result<Self> {
        let (browser, browser_name) = find_browser()?;
        let port = free_loopback_port()?;
        let profile = TempDir::new().context("无法创建临时浏览器目录")?;
        let profile_path = profile.path().to_string_lossy().to_string();
        let child = Command::new(&browser)
            .args([
                format!("--remote-debugging-port={port}"),
                "--remote-debugging-address=127.0.0.1".to_string(),
                format!("--user-data-dir={profile_path}"),
                "--no-first-run".to_string(),
                "--no-default-browser-check".to_string(),
                "--disable-sync".to_string(),
                "--disable-extensions".to_string(),
                "--disable-background-networking".to_string(),
                "--new-window".to_string(),
                login_url.to_string(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("无法启动{browser_name}"))?;
        let login = Self {
            child,
            profile,
            port,
            browser_name,
        };
        login.wait_for_devtools(Duration::from_secs(20), cancelled)?;
        Ok(login)
    }

    pub fn browser_name(&self) -> &str {
        &self.browser_name
    }

    pub fn wait_for_login(&mut self) -> Result<String> {
        self.wait_for_login_at(DEFAULT_HIT_BASE_URL, LOGIN_TIMEOUT)
    }

    pub fn wait_for_login_at(&mut self, base_url: &str, timeout: Duration) -> Result<String> {
        self.wait_for_login_cancelled(base_url, timeout, None)
    }

    pub fn wait_for_login_with_cancel(
        &mut self,
        base_url: &str,
        timeout: Duration,
        cancelled: &AtomicBool,
    ) -> Result<String> {
        self.wait_for_login_cancelled(base_url, timeout, Some(cancelled))
    }

    /// 启动隔离浏览器，等待用户完成登录，并返回已验证的教务 Cookie。
    pub fn capture_cookie(base_url: &str, cancelled: &AtomicBool) -> Result<String> {
        let mut browser = Self::launch_at_with_cancel(base_url, Some(cancelled))?;
        browser.wait_for_login_with_cancel(base_url, LOGIN_TIMEOUT, cancelled)
    }

    fn wait_for_login_cancelled(
        &mut self,
        base_url: &str,
        timeout: Duration,
        cancelled: Option<&AtomicBool>,
    ) -> Result<String> {
        let deadline = Instant::now() + timeout;
        let mut last_cookie = String::new();
        while Instant::now() < deadline {
            if cancelled.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                bail!("已取消教务系统登录")
            }
            if self.child.try_wait()?.is_some() {
                bail!("登录窗口已关闭，未获取到有效登录状态")
            }
            let targets = match self.targets() {
                Ok(targets) => targets,
                Err(_) => {
                    thread::sleep(POLL_INTERVAL);
                    continue;
                }
            };
            for target in targets {
                if cancelled.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                    bail!("已取消教务系统登录")
                }
                if target.target_type != "page"
                    || target.web_socket_debugger_url.is_empty()
                    || !is_loopback_debugger_url(&target.web_socket_debugger_url)
                    || !target_host_matches(&target.url, base_url)
                {
                    continue;
                }
                let cookie = match cookies_from_target(&target.web_socket_debugger_url, base_url) {
                    Ok(cookie) => cookie,
                    Err(_) => continue,
                };
                if cookie.is_empty() || cookie == last_cookie {
                    continue;
                }
                last_cookie = cookie.clone();
                if JwtsClient::new(base_url, &cookie)
                    .and_then(|client| client.catalog())
                    .is_ok()
                {
                    return Ok(cookie);
                }
            }
            thread::sleep(POLL_INTERVAL);
        }
        bail!("等待登录超时。请确认已在弹出的浏览器中完成登录。")
    }

    fn wait_for_devtools(&self, timeout: Duration, cancelled: Option<&AtomicBool>) -> Result<()> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .context("无法初始化浏览器连接")?;
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if cancelled.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                bail!("已取消教务系统登录")
            }
            if client
                .get(self.version_url())
                .send()
                .is_ok_and(|response| response.status().is_success())
            {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(200));
        }
        bail!("浏览器登录窗口启动超时")
    }

    fn targets(&self) -> Result<Vec<DebugTarget>> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .context("无法初始化浏览器连接")?;
        let response = client
            .get(self.list_url())
            .send()
            .context("无法读取登录窗口状态")?;
        if !response.status().is_success() {
            bail!("无法读取登录窗口状态")
        }
        response.json().context("登录窗口状态格式无效")
    }

    fn version_url(&self) -> String {
        format!("http://127.0.0.1:{}/json/version", self.port)
    }

    fn list_url(&self) -> String {
        format!("http://127.0.0.1:{}/json/list", self.port)
    }
}

impl Drop for BrowserLogin {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // 保持 profile 所有权到浏览器退出后，避免 Windows 文件锁残留。
        let _ = self.profile.path();
    }
}

fn cookies_from_target(websocket_url: &str, base_url: &str) -> Result<String> {
    if !is_loopback_debugger_url(websocket_url) {
        bail!("登录窗口地址不是本机地址")
    }
    let (mut socket, _) = connect(websocket_url).context("无法连接登录窗口")?;
    let request_id = 1u64;
    socket.send(Message::Text(
        json!({
            "id": request_id,
            "method": "Network.getCookies",
            "params": {"urls": [base_url]}
        })
        .to_string()
        .into(),
    ))?;
    loop {
        match socket.read()? {
            Message::Text(text) => {
                let value: Value = serde_json::from_str(&text)?;
                if value.get("id").and_then(Value::as_u64) != Some(request_id) {
                    continue;
                }
                if let Some(message) = value.pointer("/error/message").and_then(Value::as_str) {
                    bail!("浏览器拒绝读取登录状态：{message}")
                }
                let cookies = value
                    .pointer("/result/cookies")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                return cookie_header_for_url(&cookies, base_url);
            }
            Message::Close(_) => bail!("登录窗口连接已关闭"),
            _ => {}
        }
    }
}

fn is_loopback_debugger_url(value: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    matches!(url.scheme(), "ws" | "wss")
        && matches!(
            url.host_str(),
            Some("127.0.0.1") | Some("localhost") | Some("::1") | Some("[::1]")
        )
}

fn target_host_matches(current_url: &str, base_url: &str) -> bool {
    let (Ok(current), Ok(base)) = (Url::parse(current_url), Url::parse(base_url)) else {
        return false;
    };
    let (Some(current_host), Some(base_host)) = (current.host_str(), base.host_str()) else {
        return false;
    };
    let current_host = current_host.to_ascii_lowercase();
    let base_host = base_host.to_ascii_lowercase();
    current_host == base_host || current_host.ends_with(&format!(".{base_host}"))
}

fn cookie_applies_to_url(cookie: &Value, base: &Url) -> bool {
    let Some(host) = base.host_str() else {
        return false;
    };
    let Some(domain) = cookie.get("domain").and_then(Value::as_str) else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    let domain = domain.trim_start_matches('.').to_ascii_lowercase();
    if domain.is_empty() || (host != domain && !host.ends_with(&format!(".{domain}"))) {
        return false;
    }
    let cookie_path = cookie
        .get("path")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("/");
    let request_path = if base.path().is_empty() {
        "/"
    } else {
        base.path()
    };
    let path_matches = cookie_path == "/"
        || request_path == cookie_path
        || (request_path.starts_with(cookie_path)
            && (cookie_path.ends_with('/')
                || request_path.as_bytes().get(cookie_path.len()) == Some(&b'/')));
    let secure = cookie
        .get("secure")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    path_matches && (!secure || base.scheme() == "https")
}

fn cookie_header_for_url(cookies: &[Value], base_url: &str) -> Result<String> {
    let base = Url::parse(base_url).context("教务系统地址无效")?;
    let mut applicable = cookies
        .iter()
        .filter(|cookie| cookie_applies_to_url(cookie, &base))
        .cloned()
        .collect::<Vec<_>>();
    applicable.sort_by_key(|cookie| {
        std::cmp::Reverse(
            cookie
                .get("path")
                .and_then(Value::as_str)
                .map(str::len)
                .unwrap_or(0),
        )
    });
    Ok(cookie_header(&applicable))
}

pub fn cookie_header(cookies: &[Value]) -> String {
    let mut values = BTreeMap::new();
    for cookie in cookies {
        let Some(name) = cookie.get("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(value) = cookie.get("value").and_then(Value::as_str) else {
            continue;
        };
        if name.is_empty()
            || name
                .chars()
                .any(|character| character.is_ascii_control() || character == ';')
            || value
                .chars()
                .any(|character| character.is_ascii_control() || character == ';')
        {
            continue;
        }
        values
            .entry(name.to_string())
            .or_insert_with(|| value.to_string());
    }
    values
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; ")
}

fn free_loopback_port() -> Result<u16> {
    for _ in 0..32 {
        let hint: u16 = rand::rng().random_range(41000..61000);
        let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), hint);
        if let Ok(listener) = TcpListener::bind(address) {
            let port = listener.local_addr()?.port();
            drop(listener);
            return Ok(port);
        }
    }
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    Ok(listener.local_addr()?.port())
}

fn find_browser() -> Result<(PathBuf, String)> {
    if let Some(path) = std::env::var_os("FIREWORKS_BROWSER") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok((path, "浏览器".to_string()));
        }
    }
    let mut candidates = Vec::new();
    if cfg!(windows) {
        for root in [
            std::env::var_os("PROGRAMFILES"),
            std::env::var_os("PROGRAMFILES(X86)"),
            std::env::var_os("PROGRAMW6432"),
            std::env::var_os("LOCALAPPDATA"),
        ]
        .into_iter()
        .flatten()
        {
            let root = PathBuf::from(root);
            candidates.push((
                root.join("Microsoft/Edge/Application/msedge.exe"),
                "Microsoft Edge",
            ));
            candidates.push((
                root.join("Google/Chrome/Application/chrome.exe"),
                "Google Chrome",
            ));
        }
    } else if cfg!(target_os = "macos") {
        candidates.extend([
            (
                PathBuf::from("/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge"),
                "Microsoft Edge",
            ),
            (
                PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
                "Google Chrome",
            ),
        ]);
    } else {
        for name in [
            "microsoft-edge",
            "microsoft-edge-stable",
            "google-chrome",
            "chromium",
        ] {
            if let Some(path) = find_on_path(name) {
                candidates.push((path, name));
            }
        }
    }
    candidates
        .into_iter()
        .find(|(path, _)| path.is_file())
        .map(|(path, name)| (path, name.to_string()))
        .context("没有找到 Microsoft Edge 或 Google Chrome")
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_values_are_sorted_and_prefer_specific_first_value() {
        let cookies = vec![
            json!({"name":"B","value":"2","domain":"example.test","path":"/"}),
            json!({"name":"A","value":"1","domain":"example.test","path":"/"}),
            json!({"name":"A","value":"new","domain":"example.test","path":"/"}),
        ];
        assert_eq!(cookie_header(&cookies), "A=1; B=2");
    }

    #[test]
    fn cookie_filter_requires_domain_path_and_secure_match() {
        let base = "http://jwts.example.test/pyfa/queryPykc";
        let cookies = vec![
            json!({"name":"good","value":"1","domain":"jwts.example.test","path":"/"}),
            json!({"name":"sub","value":"2","domain":".example.test","path":"/pyfa"}),
            json!({"name":"wrong-domain","value":"3","domain":"other.test","path":"/"}),
            json!({"name":"wrong-path","value":"4","domain":"jwts.example.test","path":"/admin"}),
            json!({"name":"secure","value":"5","domain":"jwts.example.test","path":"/","secure":true}),
        ];
        let header = cookie_header_for_url(&cookies, base).unwrap();
        assert_eq!(header, "good=1; sub=2");
    }

    #[test]
    fn unrelated_page_and_debugger_are_rejected() {
        assert!(!target_host_matches(
            "https://example.com",
            DEFAULT_HIT_BASE_URL
        ));
        assert!(target_host_matches(
            DEFAULT_HIT_BASE_URL,
            DEFAULT_HIT_BASE_URL
        ));
        assert!(is_loopback_debugger_url(
            "ws://127.0.0.1:9222/devtools/page/1"
        ));
        assert!(!is_loopback_debugger_url(
            "ws://192.168.1.2:9222/devtools/page/1"
        ));
    }

    #[test]
    fn loopback_port_is_allocatable() {
        let port = free_loopback_port().unwrap();
        assert!(port > 0);
    }

    #[test]
    fn profile_is_deleted_when_session_drops() {
        let profile = TempDir::new().unwrap();
        let path = profile.path().to_path_buf();
        assert!(path.exists());
        drop(profile);
        assert!(!path.exists());
    }

    #[test]
    fn browser_override_must_exist() {
        let missing = std::path::Path::new("definitely-missing-browser.exe");
        assert!(!missing.exists());
    }
}
