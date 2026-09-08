use anyhow::{bail, Context, Result};
use parking_lot::Mutex;
use rand::Rng;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tungstenite::client::IntoClientRequest;
use tungstenite::{client, Error as WebSocketError, Message};
use url::Url;

use crate::jwts::{JwtsClient, PlanKind, SessionProvider};

const AUTH_ENTRY_URL: &str = "https://ids.hit.edu.cn/authserver/login?service=https%3A%2F%2Fivpn.hit.edu.cn%3A443%2Fpassport%2Fv1%2Fauth%2Fcas%3FsfDomain%3Dcas93482";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const REFRESH_TIMEOUT: Duration = Duration::from_secs(3 * 60);
const DEVTOOLS_START_TIMEOUT: Duration = Duration::from_secs(20);
const CDP_IO_TIMEOUT: Duration = Duration::from_millis(200);
const CDP_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const TARGET_HTTP_TIMEOUT: Duration = Duration::from_millis(350);
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const VALIDATION_POLL_INTERVAL: Duration = Duration::from_millis(50);
const VALIDATION_RETRY_INTERVAL: Duration = Duration::from_secs(3);
const MAX_JWTS_NAVIGATIONS: usize = 3;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DebugTarget {
    #[serde(default)]
    url: String,
    #[serde(default)]
    web_socket_debugger_url: String,
    #[serde(rename = "type", default)]
    target_type: String,
}

/// 使用本机专用持久 profile 的学校统一身份认证窗口。
///
/// 该类型只拥有并关闭自己启动的浏览器进程。浏览器退出后，专用 profile
/// 仍保留在本机应用数据目录，由 Edge/Chrome 自己管理其中的认证状态。
pub struct BrowserLogin {
    child: Option<Child>,
    profile: ProfileLease,
    port: u16,
    browser_name: String,
    ready: bool,
}

impl fmt::Debug for BrowserLogin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowserLogin")
            .field("browser_name", &self.browser_name)
            .field("ready", &self.ready)
            .finish()
    }
}

trait SessionControl: Send {
    fn current_cookie(&mut self, base_url: &str) -> Result<String>;
    fn refresh_cookie(&mut self, base_url: &str) -> Result<String>;
}

struct BrowserSessionProvider {
    browser: Mutex<Box<dyn SessionControl>>,
}

impl BrowserSessionProvider {
    fn new(browser: impl SessionControl + 'static) -> Self {
        Self {
            browser: Mutex::new(Box::new(browser)),
        }
    }
}

impl fmt::Debug for BrowserSessionProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowserSessionProvider")
            .field("browser", &"专用学校登录窗口")
            .finish()
    }
}

impl SessionProvider for BrowserSessionProvider {
    fn cookie(&self, base_url: &str, force_refresh: bool) -> Result<String> {
        let mut browser = self.browser.lock();
        if force_refresh {
            browser.refresh_cookie(base_url)
        } else {
            browser.current_cookie(base_url)
        }
    }
}

impl BrowserLogin {
    /// 打开持久隔离的学校登录窗口，确认教务查询可用，并返回会自动受控刷新的客户端。
    ///
    /// 用户只需在浏览器中完成人机验证；本方法不会读取密码、日常浏览器 profile，
    /// 也不会把 Cookie 另存为明文文件。
    pub fn authenticated_client(base_url: &str, cancelled: &AtomicBool) -> Result<JwtsClient> {
        let base = validate_base_url(base_url)?;
        let start_url = authentication_start_url(&base)?;
        let mut browser = Self::launch_at_with_cancel(start_url.as_str(), Some(cancelled))?;
        browser.wait_for_authenticated(
            base.as_str(),
            LOGIN_TIMEOUT,
            Some(cancelled),
            AuthenticationAttempt::Initial,
        )?;

        let provider: Arc<dyn SessionProvider> = Arc::new(BrowserSessionProvider::new(browser));
        JwtsClient::with_session_provider(base.as_str(), provider)
    }

    fn launch_at_with_cancel(login_url: &str, cancelled: Option<&AtomicBool>) -> Result<Self> {
        validate_navigation_url(login_url)?;
        check_cancelled(cancelled)?;

        let profile = ProfileLease::acquire()?;
        let (browser, browser_name) = find_browser()?;
        let port = free_loopback_port().context("无法为学校登录窗口分配本机端口")?;
        let mut profile_argument = OsString::from("--user-data-dir=");
        profile_argument.push(profile.path().as_os_str());

        let child = Command::new(&browser)
            .arg(format!("--remote-debugging-port={port}"))
            .arg("--remote-debugging-address=127.0.0.1")
            .arg(profile_argument)
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--disable-sync")
            .arg("--disable-extensions")
            .arg("--disable-background-networking")
            .arg("--new-window")
            .arg(login_url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("无法启动{browser_name}"))?;

        let mut login = Self {
            child: Some(child),
            profile,
            port,
            browser_name,
            ready: false,
        };
        login.wait_for_devtools(DEVTOOLS_START_TIMEOUT, cancelled)?;
        login.ready = true;
        Ok(login)
    }

    fn wait_for_authenticated(
        &mut self,
        base_url: &str,
        timeout: Duration,
        cancelled: Option<&AtomicBool>,
        attempt: AuthenticationAttempt,
    ) -> Result<String> {
        let base = validate_base_url(base_url)?;
        let deadline = Instant::now() + timeout;
        let login_cas_url = login_cas_url(&base)?;
        let mut last_validation: Option<(String, Instant)> = None;
        let mut jwts_navigations = 0usize;

        while Instant::now() < deadline {
            check_cancelled(cancelled)?;
            self.ensure_browser_open()?;

            let targets = match self.targets() {
                Ok(targets) => targets,
                Err(error) => {
                    if self.browser_has_exited()? {
                        bail!("学校登录窗口已关闭，请重新打开更新向导")
                    }
                    if error.to_string().contains("网络") {
                        return Err(error);
                    }
                    sleep_with_cancel(POLL_INTERVAL, cancelled)?;
                    continue;
                }
            };
            let pages = trusted_page_targets(targets, &base)?;
            if pages.is_empty() {
                sleep_with_cancel(POLL_INTERVAL, cancelled)?;
                continue;
            }

            if let Some(target) = pages
                .iter()
                .find(|target| should_continue_to_jwts(&target.url))
            {
                if jwts_navigations >= MAX_JWTS_NAVIGATIONS {
                    bail!("学校统一身份认证未能返回教务系统，请在登录窗口重新完成验证后再试")
                }
                navigate_target(
                    &target.web_socket_debugger_url,
                    login_cas_url.as_str(),
                    cancelled,
                )?;
                jwts_navigations += 1;
                sleep_with_cancel(POLL_INTERVAL, cancelled)?;
                continue;
            }

            let cookie = match cookies_from_target(
                &pages[0].web_socket_debugger_url,
                base.as_str(),
                cancelled,
            ) {
                Ok(cookie) => cookie,
                Err(_) => {
                    sleep_with_cancel(POLL_INTERVAL, cancelled)?;
                    continue;
                }
            };
            if cookie.is_empty() {
                sleep_with_cancel(POLL_INTERVAL, cancelled)?;
                continue;
            }

            let should_validate = last_validation
                .as_ref()
                .is_none_or(|(previous, checked_at)| {
                    previous != &cookie || checked_at.elapsed() >= VALIDATION_RETRY_INTERVAL
                });
            if should_validate {
                last_validation = Some((cookie.clone(), Instant::now()));
                match validate_query_access(cookie.clone(), base.as_str(), cancelled, deadline)? {
                    QueryAccess::Available => return Ok(cookie),
                    QueryAccess::AuthenticationPending => {}
                    QueryAccess::NetworkUnavailable => {
                        bail!("无法连接教务系统，请检查网络后重试")
                    }
                }
            }
            sleep_with_cancel(POLL_INTERVAL, cancelled)?;
        }

        match attempt {
            AuthenticationAttempt::Initial => {
                bail!("等待学校认证超时。请在弹出的学校登录窗口完成验证后重试")
            }
            AuthenticationAttempt::Refresh => {
                bail!("学校登录状态刷新失败。请在当前登录窗口重新完成验证后重试")
            }
        }
    }

    fn refresh_authenticated_cookie(&mut self, base_url: &str) -> Result<String> {
        let base = validate_base_url(base_url)?;
        let start_url = authentication_start_url(&base)?;
        self.navigate_first_page(&base, start_url.as_str(), None)?;
        self.wait_for_authenticated(
            base.as_str(),
            REFRESH_TIMEOUT,
            None,
            AuthenticationAttempt::Refresh,
        )
    }

    fn current_cookie_for(&mut self, base_url: &str) -> Result<String> {
        let base = validate_base_url(base_url)?;
        self.ensure_browser_open()?;
        let pages = trusted_page_targets(self.targets()?, &base)?;
        let target = pages.first().context("学校登录窗口尚未就绪，请稍后重试")?;
        let cookie = cookies_from_target(&target.web_socket_debugger_url, base.as_str(), None)?;
        if cookie.is_empty() {
            bail!("当前教务登录状态不可用，请重新打开学校登录窗口")
        }
        Ok(cookie)
    }

    fn navigate_first_page(
        &mut self,
        base: &Url,
        destination: &str,
        cancelled: Option<&AtomicBool>,
    ) -> Result<()> {
        validate_navigation_url(destination)?;
        self.ensure_browser_open()?;
        let pages = trusted_page_targets(self.targets()?, base)?;
        let target = pages.first().context("学校登录窗口尚未就绪，请稍后重试")?;
        navigate_target(&target.web_socket_debugger_url, destination, cancelled)
    }

    fn wait_for_devtools(
        &mut self,
        timeout: Duration,
        cancelled: Option<&AtomicBool>,
    ) -> Result<()> {
        let client = reqwest::blocking::Client::builder()
            .timeout(TARGET_HTTP_TIMEOUT)
            .build()
            .context("无法初始化学校登录窗口")?;
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            check_cancelled(cancelled)?;
            if self.browser_has_exited()? {
                bail!("学校登录窗口启动失败。专用登录状态可能正被其他窗口使用，请关闭后重试")
            }
            if client
                .get(self.version_url())
                .send()
                .is_ok_and(|response| response.status().is_success())
            {
                return Ok(());
            }
            sleep_with_cancel(Duration::from_millis(100), cancelled)?;
        }
        bail!("学校登录窗口启动超时，请关闭窗口后重试")
    }

    fn targets(&self) -> Result<Vec<DebugTarget>> {
        let client = reqwest::blocking::Client::builder()
            .timeout(TARGET_HTTP_TIMEOUT)
            .build()
            .context("无法初始化学校登录窗口")?;
        let response = client
            .get(self.list_url())
            .send()
            .context("无法读取学校登录窗口状态")?;
        if !response.status().is_success() {
            bail!("无法读取学校登录窗口状态")
        }
        response.json().context("学校登录窗口返回了无法识别的状态")
    }

    fn ensure_browser_open(&mut self) -> Result<()> {
        if self.browser_has_exited()? {
            bail!("学校登录窗口已关闭，请重新打开更新向导")
        }
        Ok(())
    }

    fn browser_has_exited(&mut self) -> Result<bool> {
        match self.child.as_mut() {
            Some(child) => child
                .try_wait()
                .map(|status| status.is_some())
                .context("无法确认学校登录窗口状态"),
            None => Ok(true),
        }
    }

    fn version_url(&self) -> String {
        format!("http://127.0.0.1:{}/json/version", self.port)
    }

    fn list_url(&self) -> String {
        format!("http://127.0.0.1:{}/json/list", self.port)
    }

    fn close_owned_process(&mut self) {
        if let Some(mut child) = self.child.take() {
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
        self.ready = false;
    }
}

impl SessionControl for BrowserLogin {
    fn current_cookie(&mut self, base_url: &str) -> Result<String> {
        self.current_cookie_for(base_url)
    }

    fn refresh_cookie(&mut self, base_url: &str) -> Result<String> {
        self.refresh_authenticated_cookie(base_url)
    }
}

impl Drop for BrowserLogin {
    fn drop(&mut self) {
        self.close_owned_process();
        // ProfileLease 随后只释放管理器锁；专用 profile 本身会保留。
        let _ = self.profile.path();
    }
}

#[derive(Clone, Copy)]
enum AuthenticationAttempt {
    Initial,
    Refresh,
}

enum QueryAccess {
    Available,
    AuthenticationPending,
    NetworkUnavailable,
}

fn validate_query_access(
    cookie: String,
    base_url: &str,
    cancelled: Option<&AtomicBool>,
    authentication_deadline: Instant,
) -> Result<QueryAccess> {
    let base_url = base_url.to_string();
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let result = JwtsClient::new(&base_url, &cookie)
            .and_then(|client| client.catalog(PlanKind::Execution))
            .map(|_| ());
        let _ = sender.send(result);
    });

    loop {
        check_cancelled(cancelled)?;
        if Instant::now() >= authentication_deadline {
            return Ok(QueryAccess::NetworkUnavailable);
        }
        match receiver.recv_timeout(VALIDATION_POLL_INTERVAL) {
            Ok(Ok(())) => return Ok(QueryAccess::Available),
            Ok(Err(error)) if is_authentication_error(&error) => {
                return Ok(QueryAccess::AuthenticationPending)
            }
            Ok(Err(error)) if is_network_error(&error) => {
                return Ok(QueryAccess::NetworkUnavailable)
            }
            Ok(Err(error)) => return Err(error),
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("无法验证教务登录状态，请重试")
            }
        }
    }
}

fn is_authentication_error(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|source| source.to_string() == "登录已失效，请重新登录教务系统后再试")
}

fn is_network_error(error: &anyhow::Error) -> bool {
    error.to_string() == "无法连接教务系统"
        || error.chain().any(|source| {
            source
                .downcast_ref::<reqwest::Error>()
                .is_some_and(|error| error.is_connect() || error.is_timeout() || error.is_request())
        })
}

struct ProfileLease {
    profile_path: PathBuf,
    _lock: TcpListener,
}

impl fmt::Debug for ProfileLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProfileLease")
            .field("persistent", &true)
            .finish()
    }
}

impl ProfileLease {
    fn acquire() -> Result<Self> {
        let application_directory = application_data_directory()?;
        Self::acquire_at(&application_directory)
    }

    fn acquire_at(application_directory: &Path) -> Result<Self> {
        fs::create_dir_all(application_directory).context("无法创建本机学校登录状态目录")?;
        secure_directory(application_directory)?;

        let profile_path = application_directory.join("identity-profile");
        fs::create_dir_all(&profile_path).context("无法创建本机专用学校登录目录")?;
        secure_directory(&profile_path)?;

        let lock =
            TcpListener::bind(profile_lock_address(application_directory)).map_err(|error| {
                if error.kind() == ErrorKind::AddrInUse {
                    anyhow::anyhow!("学校登录状态正由另一个管理器窗口使用。请关闭另一个窗口后重试")
                } else {
                    anyhow::anyhow!("无法锁定本机学校登录状态，请稍后重试")
                }
            })?;

        Ok(Self {
            profile_path,
            _lock: lock,
        })
    }

    fn path(&self) -> &Path {
        &self.profile_path
    }
}

fn profile_lock_address(application_directory: &Path) -> SocketAddr {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    application_directory.hash(&mut hasher);
    let port = 45_000 + (hasher.finish() % 10_000) as u16;
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
}

fn application_data_directory() -> Result<PathBuf> {
    let root = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("USERPROFILE")
                    .map(PathBuf::from)
                    .map(|home| home.join("AppData/Local"))
            })
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|home| home.join(".local/share"))
            })
    }
    .context("无法确定本机学校登录状态保存位置")?;

    Ok(root.join("FireworksRepositoryManager"))
}

#[cfg(unix)]
fn secure_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .context("无法保护本机学校登录状态目录")
}

#[cfg(not(unix))]
fn secure_directory(_path: &Path) -> Result<()> {
    Ok(())
}

fn trusted_page_targets(targets: Vec<DebugTarget>, base: &Url) -> Result<Vec<DebugTarget>> {
    let mut pages = Vec::new();
    for target in targets {
        if target.target_type != "page" || target.web_socket_debugger_url.is_empty() {
            continue;
        }
        if !is_loopback_debugger_url(&target.web_socket_debugger_url) {
            bail!("学校登录窗口的本机连接不安全，请关闭窗口后重试")
        }
        validate_page_url(&target.url, base)?;
        pages.push(target);
    }
    Ok(pages)
}

fn validate_page_url(value: &str, base: &Url) -> Result<()> {
    if value.is_empty()
        || value == "about:blank"
        || value.starts_with("chrome://newtab")
        || value.starts_with("edge://newtab")
    {
        return Ok(());
    }
    if value.starts_with("chrome-error://") {
        bail!("无法打开学校登录页面，请检查网络连接后重试")
    }
    let url = Url::parse(value)
        .map_err(|_| anyhow::anyhow!("学校登录窗口进入了无法识别的页面，请关闭后重试"))?;
    if is_loopback_url(&url) && is_loopback_url(base) {
        return Ok(());
    }
    if is_trusted_school_url(&url) {
        return Ok(());
    }
    bail!("学校登录窗口进入了不受信任的网站，已停止读取登录状态")
}

fn validate_navigation_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).context("学校登录入口无效")?;
    if has_url_credentials(&url) {
        bail!("学校登录入口无效")
    }
    if is_loopback_url(&url) || is_trusted_school_url(&url) {
        return Ok(url);
    }
    bail!("拒绝打开不受信任的学校登录地址")
}

fn validate_base_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).context("教务系统地址无效")?;
    if has_url_credentials(&url) || url.query().is_some() || url.fragment().is_some() {
        bail!("教务系统地址无效")
    }
    if is_loopback_url(&url) {
        return Ok(url);
    }
    let path_is_root = url.path().is_empty() || url.path() == "/";
    let allowed = match normalized_host(&url).as_deref() {
        Some("jwts-hit-edu-cn.ivpn.hit.edu.cn") => {
            url.scheme() == "http" && url.port_or_known_default() == Some(1080)
        }
        Some("jwts.hit.edu.cn") => {
            url.scheme() == "https" && url.port_or_known_default() == Some(443)
        }
        _ => false,
    };
    if allowed && path_is_root {
        Ok(url)
    } else {
        bail!("只允许连接学校教务系统的受控地址")
    }
}

fn authentication_start_url(base: &Url) -> Result<Url> {
    if is_loopback_url(base) {
        login_cas_url(base)
    } else {
        validate_navigation_url(AUTH_ENTRY_URL)
    }
}

fn login_cas_url(base: &Url) -> Result<Url> {
    let mut url = base.clone();
    url.set_path("/loginCAS");
    url.set_query(None);
    url.set_fragment(None);
    validate_navigation_url(url.as_str())
}

fn should_continue_to_jwts(value: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    normalized_host(&url).as_deref() == Some("i-hit-edu-cn.ivpn.hit.edu.cn")
        && url.path().split(';').next() == Some("/cas/login_portal")
}

fn is_trusted_school_url(url: &Url) -> bool {
    if has_url_credentials(url) {
        return false;
    }
    let scheme = url.scheme();
    let port = url.port_or_known_default();
    match normalized_host(url).as_deref() {
        Some("ids.hit.edu.cn") | Some("ivpn.hit.edu.cn") | Some("jwts.hit.edu.cn") => {
            scheme == "https" && port == Some(443)
        }
        Some("ids-hit-edu-cn-s.ivpn.hit.edu.cn") => {
            matches!(scheme, "http" | "https") && port == Some(1080)
        }
        Some("i-hit-edu-cn.ivpn.hit.edu.cn") | Some("jwts-hit-edu-cn.ivpn.hit.edu.cn") => {
            scheme == "http" && port == Some(1080)
        }
        _ => false,
    }
}

fn normalized_host(url: &Url) -> Option<String> {
    url.host_str().map(str::to_ascii_lowercase)
}

fn has_url_credentials(url: &Url) -> bool {
    !url.username().is_empty() || url.password().is_some()
}

fn is_loopback_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && matches!(
            normalized_host(url).as_deref(),
            Some("127.0.0.1") | Some("localhost") | Some("::1") | Some("[::1]")
        )
}

fn navigate_target(
    websocket_url: &str,
    destination: &str,
    cancelled: Option<&AtomicBool>,
) -> Result<()> {
    validate_navigation_url(destination)?;
    cdp_command(
        websocket_url,
        "Page.navigate",
        json!({"url": destination}),
        cancelled,
    )?;
    Ok(())
}

fn cookies_from_target(
    websocket_url: &str,
    base_url: &str,
    cancelled: Option<&AtomicBool>,
) -> Result<String> {
    let base = validate_base_url(base_url)?;
    let response = cdp_command(
        websocket_url,
        "Network.getCookies",
        json!({"urls": [base.as_str()]}),
        cancelled,
    )?;
    let cookies = response
        .pointer("/result/cookies")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    cookie_header_for_url(&cookies, base.as_str())
}

fn cdp_command(
    websocket_url: &str,
    method: &str,
    params: Value,
    cancelled: Option<&AtomicBool>,
) -> Result<Value> {
    let endpoint = debugger_socket_address(websocket_url)?;
    check_cancelled(cancelled)?;
    let stream = TcpStream::connect_timeout(&endpoint, CDP_IO_TIMEOUT)
        .map_err(|_| anyhow::anyhow!("无法连接学校登录窗口"))?;
    stream
        .set_read_timeout(Some(CDP_IO_TIMEOUT))
        .map_err(|_| anyhow::anyhow!("无法设置学校登录窗口读取时限"))?;
    stream
        .set_write_timeout(Some(CDP_IO_TIMEOUT))
        .map_err(|_| anyhow::anyhow!("无法设置学校登录窗口写入时限"))?;

    let request = websocket_url
        .into_client_request()
        .map_err(|_| anyhow::anyhow!("学校登录窗口连接地址无效"))?;
    let (mut socket, _) =
        client(request, stream).map_err(|_| anyhow::anyhow!("无法建立学校登录窗口连接"))?;
    let request_id = 1u64;
    socket
        .send(Message::Text(
            json!({"id": request_id, "method": method, "params": params})
                .to_string()
                .into(),
        ))
        .map_err(|_| anyhow::anyhow!("无法请求学校登录状态"))?;

    let deadline = Instant::now() + CDP_COMMAND_TIMEOUT;
    loop {
        check_cancelled(cancelled)?;
        if Instant::now() >= deadline {
            bail!("读取学校登录状态超时")
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                let value: Value = serde_json::from_str(&text)
                    .map_err(|_| anyhow::anyhow!("学校登录窗口响应格式无效"))?;
                if value.get("id").and_then(Value::as_u64) != Some(request_id) {
                    continue;
                }
                if value.get("error").is_some() {
                    bail!("学校登录窗口拒绝了登录状态请求")
                }
                return Ok(value);
            }
            Ok(Message::Close(_)) => bail!("学校登录窗口连接已关闭"),
            Ok(_) => continue,
            Err(WebSocketError::Io(error))
                if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
            {
                continue
            }
            Err(_) => bail!("无法读取学校登录窗口状态"),
        }
    }
}

fn debugger_socket_address(value: &str) -> Result<SocketAddr> {
    let url = Url::parse(value).map_err(|_| anyhow::anyhow!("学校登录窗口连接地址无效"))?;
    if url.scheme() != "ws" || has_url_credentials(&url) {
        bail!("学校登录窗口连接地址不是本机安全地址")
    }
    let port = url
        .port_or_known_default()
        .context("学校登录窗口连接地址缺少端口")?;
    let address = match normalized_host(&url).as_deref() {
        Some("127.0.0.1") | Some("localhost") => {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
        }
        Some("::1") | Some("[::1]") => SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port),
        _ => bail!("学校登录窗口连接地址不是本机地址"),
    };
    Ok(address)
}

fn is_loopback_debugger_url(value: &str) -> bool {
    debugger_socket_address(value).is_ok()
}

fn cookie_applies_to_url(cookie: &Value, base: &Url) -> bool {
    let Some(host) = normalized_host(base) else {
        return false;
    };
    let Some(raw_domain) = cookie.get("domain").and_then(Value::as_str) else {
        return false;
    };
    let domain = raw_domain.trim_start_matches('.').to_ascii_lowercase();
    if domain.is_empty() {
        return false;
    }
    let domain_matches = if raw_domain.starts_with('.') {
        host == domain || host.ends_with(&format!(".{domain}"))
    } else {
        host == domain
    };
    if !domain_matches {
        return false;
    }

    let cookie_path = cookie
        .get("path")
        .and_then(Value::as_str)
        .filter(|value| value.starts_with('/'))
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
    let unexpired = cookie
        .get("expires")
        .and_then(Value::as_f64)
        .is_none_or(|expires| expires <= 0.0 || expires > unix_time_now());

    path_matches && (!secure || base.scheme() == "https") && unexpired
}

fn cookie_header_for_url(cookies: &[Value], base_url: &str) -> Result<String> {
    let base = validate_base_url(base_url)?;
    let mut applicable = cookies
        .iter()
        .filter(|cookie| cookie_applies_to_url(cookie, &base))
        .filter(|cookie| {
            cookie
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| !authentication_only_cookie(name))
        })
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

fn authentication_only_cookie(name: &str) -> bool {
    name.eq_ignore_ascii_case("CASTGC") || name.eq_ignore_ascii_case("TGC")
}

fn cookie_header(cookies: &[Value]) -> String {
    let mut values = BTreeMap::new();
    for cookie in cookies {
        let Some(name) = cookie.get("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(value) = cookie.get("value").and_then(Value::as_str) else {
            continue;
        };
        if !valid_cookie_name(name) || !valid_cookie_value(value) {
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

fn valid_cookie_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn valid_cookie_value(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| !byte.is_ascii_control() && byte != b';')
}

fn unix_time_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn check_cancelled(cancelled: Option<&AtomicBool>) -> Result<()> {
    if cancelled.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        bail!("已取消学校登录")
    }
    Ok(())
}

fn sleep_with_cancel(duration: Duration, cancelled: Option<&AtomicBool>) -> Result<()> {
    let deadline = Instant::now() + duration;
    loop {
        check_cancelled(cancelled)?;
        let now = Instant::now();
        if now >= deadline {
            return Ok(());
        }
        thread::sleep((deadline - now).min(Duration::from_millis(25)));
    }
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
        for (executable, name) in [
            ("microsoft-edge", "Microsoft Edge"),
            ("microsoft-edge-stable", "Microsoft Edge"),
            ("google-chrome", "Google Chrome"),
            ("google-chrome-stable", "Google Chrome"),
        ] {
            if let Some(path) = find_on_path(executable) {
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
    use crate::jwts::DEFAULT_HIT_BASE_URL;
    use std::sync::atomic::AtomicUsize;
    use tempfile::TempDir;
    use tungstenite::accept;

    #[test]
    fn debugger_endpoint_must_be_an_exact_loopback_websocket() {
        assert!(is_loopback_debugger_url(
            "ws://127.0.0.1:9222/devtools/page/1"
        ));
        assert!(is_loopback_debugger_url(
            "ws://localhost:9222/devtools/page/1"
        ));
        assert!(is_loopback_debugger_url("ws://[::1]:9222/devtools/page/1"));
        assert!(!is_loopback_debugger_url(
            "ws://192.168.1.2:9222/devtools/page/1"
        ));
        assert!(!is_loopback_debugger_url(
            "ws://localhost.example:9222/devtools/page/1"
        ));
        assert!(!is_loopback_debugger_url(
            "wss://127.0.0.1:9222/devtools/page/1"
        ));
    }

    #[test]
    fn base_and_page_hosts_are_exactly_allowlisted() {
        assert!(validate_base_url(DEFAULT_HIT_BASE_URL).is_ok());
        assert!(validate_base_url("https://jwts.hit.edu.cn").is_ok());
        assert!(validate_base_url("http://127.0.0.1:18080/test").is_ok());
        assert!(validate_base_url("http://evil.jwts.hit.edu.cn").is_err());
        assert!(validate_base_url("http://jwts.hit.edu.cn.evil.test").is_err());

        let base = validate_base_url(DEFAULT_HIT_BASE_URL).unwrap();
        assert!(validate_page_url("https://ids.hit.edu.cn/authserver/login", &base).is_ok());
        let error = validate_page_url("https://ids.hit.edu.cn.evil.test/login", &base)
            .unwrap_err()
            .to_string();
        assert!(!error.contains("ids.hit.edu.cn.evil.test"));
    }

    #[test]
    fn only_explicit_authentication_failure_keeps_waiting() {
        let authentication = anyhow::anyhow!("登录已失效，请重新登录教务系统后再试");
        let parse_failure = anyhow::anyhow!("执行教学计划表头不完整");
        let network_failure = anyhow::anyhow!("无法连接教务系统");
        assert!(is_authentication_error(&authentication));
        assert!(!is_authentication_error(&parse_failure));
        assert!(!is_network_error(&parse_failure));
        assert!(is_network_error(&network_failure));
    }
    #[test]
    fn cookie_filter_requires_domain_path_secure_and_expiry_match() {
        let base = "http://127.0.0.1:18080/pyfa/queryPykc";
        let cookies = vec![
            json!({"name":"good","value":"1","domain":"127.0.0.1","path":"/"}),
            json!({"name":"specific","value":"2","domain":"127.0.0.1","path":"/pyfa"}),
            json!({"name":"wrong-domain","value":"3","domain":"localhost","path":"/"}),
            json!({"name":"wrong-path","value":"4","domain":"127.0.0.1","path":"/admin"}),
            json!({"name":"secure","value":"5","domain":"127.0.0.1","path":"/","secure":true}),
            json!({"name":"expired","value":"6","domain":"127.0.0.1","path":"/","expires":1.0}),
        ];
        let header = cookie_header_for_url(&cookies, base).unwrap();
        assert_eq!(header, "good=1; specific=2");
    }

    #[test]
    fn cas_session_cookie_is_never_forwarded_to_jwts() {
        let base = "https://jwts.hit.edu.cn/";
        let cookies = vec![
            json!({"name":"CASTGC","value":"sentinel-cas-secret","domain":".hit.edu.cn","path":"/","secure":true}),
            json!({"name":"JSESSIONID","value":"jwts-session","domain":"jwts.hit.edu.cn","path":"/","secure":true}),
        ];
        let header = cookie_header_for_url(&cookies, base).unwrap();
        assert_eq!(header, "JSESSIONID=jwts-session");
        assert!(!header.contains("sentinel-cas-secret"));
    }

    #[test]
    fn refresh_destinations_are_controlled() {
        let production = validate_base_url(DEFAULT_HIT_BASE_URL).unwrap();
        let start = authentication_start_url(&production).unwrap();
        assert_eq!(start.as_str(), AUTH_ENTRY_URL);
        assert_eq!(
            login_cas_url(&production).unwrap().as_str(),
            "http://jwts-hit-edu-cn.ivpn.hit.edu.cn:1080/loginCAS"
        );

        let local = validate_base_url("http://127.0.0.1:18080/root").unwrap();
        assert_eq!(
            authentication_start_url(&local).unwrap().as_str(),
            "http://127.0.0.1:18080/loginCAS"
        );
        assert!(validate_navigation_url("https://attacker.example/loginCAS").is_err());
    }

    #[test]
    fn cdp_cookie_exchange_uses_the_requested_url_and_redacts_errors() {
        let requested_base = "http://127.0.0.1:18080/pyfa/queryPykc";
        let (endpoint, request_receiver, server) = fake_cdp_server(
            json!({
                "result": {
                    "cookies": [
                        {"name":"JSESSIONID","value":"ok","domain":"127.0.0.1","path":"/"}
                    ]
                }
            }),
            Duration::ZERO,
        );
        assert_eq!(
            cookies_from_target(&endpoint, requested_base, None).unwrap(),
            "JSESSIONID=ok"
        );
        let request = request_receiver.recv().unwrap();
        assert_eq!(request["method"], "Network.getCookies");
        assert_eq!(request["params"]["urls"][0], requested_base);
        server.join().unwrap();

        let (endpoint, _, server) = fake_cdp_server(
            json!({"error":{"message":"sentinel-ticket-secret"}}),
            Duration::ZERO,
        );
        let error = cdp_command(&endpoint, "Network.getCookies", json!({}), None)
            .unwrap_err()
            .to_string();
        assert!(!error.contains("sentinel-ticket-secret"));
        server.join().unwrap();
    }

    #[test]
    fn cdp_navigation_waits_beyond_one_read_poll() {
        let (endpoint, receiver, server) = fake_cdp_server(
            json!({"result":{"frameId":"school-frame"}}),
            Duration::from_millis(750),
        );
        let result = navigate_target(&endpoint, "http://127.0.0.1:18080/loginCAS", None);
        server.join().unwrap();
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(receiver.recv().unwrap()["method"], "Page.navigate");
    }

    #[test]
    fn cdp_wait_remains_cancellable_during_navigation() {
        let (endpoint, receiver, server) = fake_cdp_server(
            json!({"result":{"frameId":"school-frame"}}),
            Duration::from_secs(1),
        );
        let cancelled = Arc::new(AtomicBool::new(false));
        let setter = Arc::clone(&cancelled);
        let worker = thread::spawn(move || {
            receiver.recv().unwrap();
            setter.store(true, Ordering::Relaxed);
        });
        let started = Instant::now();
        let result = navigate_target(
            &endpoint,
            "http://127.0.0.1:18080/loginCAS",
            Some(cancelled.as_ref()),
        );
        assert!(result.unwrap_err().to_string().contains("取消"));
        assert!(started.elapsed() < Duration::from_millis(800));
        worker.join().unwrap();
        server.join().unwrap();
    }

    #[test]
    fn cdp_read_timeout_is_bounded() {
        let (endpoint, _, server) = fake_cdp_server(
            json!({"result":{"cookies":[]}}),
            CDP_COMMAND_TIMEOUT + Duration::from_millis(500),
        );
        let started = Instant::now();
        let result = cdp_command(&endpoint, "Network.getCookies", json!({}), None);
        assert!(result.is_err());
        assert!(started.elapsed() >= CDP_COMMAND_TIMEOUT);
        assert!(started.elapsed() < CDP_COMMAND_TIMEOUT + Duration::from_secs(1));
        server.join().unwrap();
    }

    #[test]
    fn profile_is_persistent_but_exclusively_leased() {
        let root = TempDir::new().unwrap();
        let application_directory = root.path().join("app-data");
        let lease = ProfileLease::acquire_at(&application_directory).unwrap();
        let profile_path = lease.path().to_path_buf();
        assert!(profile_path.exists());
        assert!(ProfileLease::acquire_at(&application_directory).is_err());

        drop(lease);
        assert!(profile_path.exists());
        assert!(ProfileLease::acquire_at(&application_directory).is_ok());
    }

    #[test]
    fn debug_metadata_does_not_expose_profile_or_port() {
        let root = TempDir::new().unwrap();
        let application_directory = root.path().join("sentinel-profile-secret");
        let profile = ProfileLease::acquire_at(&application_directory).unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .arg("--list")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let browser = BrowserLogin {
            child: Some(child),
            profile,
            port: 54321,
            browser_name: "Microsoft Edge".to_string(),
            ready: true,
        };
        let debug = format!("{browser:?}");
        assert_eq!(
            debug,
            "BrowserLogin { browser_name: \"Microsoft Edge\", ready: true }"
        );
        assert!(!debug.contains("sentinel-profile-secret"));
        assert!(!debug.contains("54321"));
    }

    struct FakeSessionControl {
        drops: Arc<AtomicUsize>,
        refreshes: Arc<AtomicUsize>,
    }

    impl SessionControl for FakeSessionControl {
        fn current_cookie(&mut self, _base_url: &str) -> Result<String> {
            Ok("JSESSIONID=current".to_string())
        }

        fn refresh_cookie(&mut self, _base_url: &str) -> Result<String> {
            self.refreshes.fetch_add(1, Ordering::SeqCst);
            Ok("JSESSIONID=refreshed".to_string())
        }
    }

    impl Drop for FakeSessionControl {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn provider_arc_keeps_browser_alive_and_force_refreshes_once() {
        let drops = Arc::new(AtomicUsize::new(0));
        let refreshes = Arc::new(AtomicUsize::new(0));
        let provider = Arc::new(BrowserSessionProvider::new(FakeSessionControl {
            drops: Arc::clone(&drops),
            refreshes: Arc::clone(&refreshes),
        }));
        let session_provider: Arc<dyn SessionProvider> = provider.clone();
        assert_eq!(
            session_provider
                .cookie(DEFAULT_HIT_BASE_URL, false)
                .unwrap(),
            "JSESSIONID=current"
        );
        assert_eq!(
            session_provider.cookie(DEFAULT_HIT_BASE_URL, true).unwrap(),
            "JSESSIONID=refreshed"
        );
        assert_eq!(refreshes.load(Ordering::SeqCst), 1);
        drop(provider);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(session_provider);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cancellation_is_observed_within_one_poll_window() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let setter = Arc::clone(&cancelled);
        let worker = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            setter.store(true, Ordering::Relaxed);
        });
        let started = Instant::now();
        let result = sleep_with_cancel(Duration::from_secs(5), Some(cancelled.as_ref()));
        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_millis(500));
        worker.join().unwrap();
    }

    #[test]
    fn browser_process_fixture() {
        if std::env::var_os("FIREWORKS_BROWSER_TEST_CHILD").is_some() {
            thread::sleep(Duration::from_secs(5));
        }
    }

    #[test]
    fn drop_only_stops_the_owned_browser_process() {
        let mut unrelated = spawn_test_child();
        let owned = spawn_test_child();
        thread::sleep(Duration::from_millis(300));

        let root = TempDir::new().unwrap();
        let application_directory = root.path().join("app-data");
        let profile = ProfileLease::acquire_at(&application_directory).unwrap();
        let browser = BrowserLogin {
            child: Some(owned),
            profile,
            port: 54321,
            browser_name: "测试浏览器".to_string(),
            ready: true,
        };
        let started = Instant::now();
        drop(browser);
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(unrelated.try_wait().unwrap().is_none());
        unrelated.kill().unwrap();
        unrelated.wait().unwrap();
        assert!(application_directory.join("identity-profile").exists());
    }

    fn spawn_test_child() -> Child {
        Command::new(std::env::current_exe().unwrap())
            .arg("browser_process_fixture")
            .arg("--nocapture")
            .env("FIREWORKS_BROWSER_TEST_CHILD", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    fn fake_cdp_server(
        response_body: Value,
        delay: Duration,
    ) -> (String, mpsc::Receiver<Value>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (sender, receiver) = mpsc::channel();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = accept(stream).unwrap();
            let message = socket.read().unwrap();
            let Message::Text(text) = message else {
                panic!("expected text command")
            };
            let request: Value = serde_json::from_str(&text).unwrap();
            let _ = sender.send(request.clone());
            thread::sleep(delay);
            let mut response = response_body;
            response["id"] = request["id"].clone();
            let _ = socket.send(Message::Text(response.to_string().into()));
        });
        (
            format!("ws://127.0.0.1:{port}/devtools/page/test"),
            receiver,
            server,
        )
    }
}
