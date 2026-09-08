use anyhow::{bail, Context, Result};
use reqwest::blocking::{Client, Response};
use reqwest::header::{HeaderMap, HeaderValue, COOKIE, REFERER, USER_AGENT};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use url::Url;

#[path = "jwts_parse.rs"]
mod parse;

pub const DEFAULT_HIT_BASE_URL: &str = "http://jwts-hit-edu-cn.ivpn.hit.edu.cn:1080";

#[derive(Debug, Copy, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PlanKind {
    #[default]
    Curriculum,
    Execution,
}

impl PlanKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Curriculum => "培养方案",
            Self::Execution => "执行教学计划",
        }
    }

    fn source_name(self) -> &'static str {
        match self {
            Self::Curriculum => "curriculum",
            Self::Execution => "execution",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct CatalogOption {
    pub code: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CurriculumCatalog {
    pub grades: Vec<CatalogOption>,
    pub colleges: Vec<CatalogOption>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CrawlSelection {
    pub grade: String,
    pub college_code: String,
    pub college_name: String,
    pub major_code: String,
    pub major_name: String,
    #[serde(default)]
    pub kind: PlanKind,
}

impl CrawlSelection {
    pub fn plan_id(&self) -> String {
        match self.kind {
            PlanKind::Curriculum => format!(
                "hit:{}:{}:{}",
                encode_identity(&self.grade),
                encode_identity(&self.college_code),
                encode_identity(&self.major_code)
            ),
            PlanKind::Execution => format!(
                "hit:execution:{}:{}:{}",
                encode_identity(&self.grade),
                encode_identity(&self.college_code),
                encode_identity(&self.major_code)
            ),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CandidatePlan {
    pub plan_id: String,
    pub info: Value,
    pub courses: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CandidateSnapshot {
    pub generated_at: String,
    pub base_url: String,
    pub plans: Vec<CandidatePlan>,
}

pub trait SessionProvider: Send + Sync {
    fn cookie(&self, base_url: &str, force_refresh: bool) -> Result<String>;
}

struct FixedSession(String);

impl SessionProvider for FixedSession {
    fn cookie(&self, _base_url: &str, _force_refresh: bool) -> Result<String> {
        Ok(self.0.clone())
    }
}

#[derive(Clone)]
pub struct JwtsClient {
    base_url: Url,
    provider: Arc<dyn SessionProvider>,
    client: Client,
}

impl fmt::Debug for JwtsClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JwtsClient")
            .field("base_url", &self.base_url.origin().ascii_serialization())
            .field("session", &"<redacted>")
            .finish()
    }
}

impl JwtsClient {
    pub fn new(base_url: &str, cookie: &str) -> Result<Self> {
        let cookie = cookie.trim();
        if cookie.is_empty() {
            bail!("教务系统登录信息为空")
        }
        Self::with_session_provider(base_url, Arc::new(FixedSession(cookie.to_string())))
    }

    pub fn with_session_provider(
        base_url: &str,
        provider: Arc<dyn SessionProvider>,
    ) -> Result<Self> {
        let mut base_url = Url::parse(base_url).context("教务系统地址无效")?;
        if !matches!(base_url.scheme(), "http" | "https") || base_url.host_str().is_none() {
            bail!("教务系统地址必须是有效的 HTTP(S) 地址")
        }
        base_url.set_query(None);
        base_url.set_fragment(None);
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            // Explicit Cookie headers must never be replayed through redirects.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("无法初始化网络连接")?;
        Ok(Self {
            base_url,
            provider,
            client,
        })
    }
    pub fn base_url(&self) -> &str {
        self.base_url.as_str()
    }

    pub fn catalog(&self, kind: PlanKind) -> Result<CurriculumCatalog> {
        let path = match kind {
            PlanKind::Curriculum => "/pyfa/queryPykc",
            PlanKind::Execution => "/zxjh/queryZxkc",
        };
        let body = self.request_text(Method::GET, path, &[], &[])?;
        parse::parse_catalog_html(&body, kind)
    }

    pub fn majors(
        &self,
        kind: PlanKind,
        college_code: &str,
        grade: &str,
    ) -> Result<Vec<CatalogOption>> {
        match kind {
            PlanKind::Execution => {
                let scope = [("yxdm", college_code), ("nj", grade)];
                let value = self.request_json(Method::POST, "/pub/queryYxzyList_x", &scope, &[])?;
                parse::parse_major_response(&value, true)
            }
            PlanKind::Curriculum => {
                let legacy_scope = [("yxdm", college_code), ("nj", grade)];
                let legacy =
                    self.request_json(Method::POST, "/pub/queryYxzyList_bbh", &legacy_scope, &[])?;
                let legacy = parse::parse_major_response(&legacy, true)?;
                if legacy.is_empty() {
                    return Ok(legacy);
                }
                // A plan version is never sent as an entry cohort.  Independently enumerate the
                // execution catalog and only enrich codes whose official label is unambiguous.
                let execution = self.catalog(PlanKind::Execution)?;
                let mut official = Vec::new();
                for cohort in execution.grades {
                    let scope = [("yxdm", college_code), ("nj", cohort.code.as_str())];
                    let value =
                        self.request_json(Method::POST, "/pub/queryYxzyList_x", &scope, &[])?;
                    official.extend(parse::parse_major_response(&value, true)?);
                }
                parse::merge_major_labels(legacy, official)
            }
        }
    }

    pub fn fetch_plan(&self, selection: &CrawlSelection) -> Result<CandidatePlan> {
        match selection.kind {
            PlanKind::Curriculum => self.fetch_curriculum(selection),
            PlanKind::Execution => self.fetch_execution(selection),
        }
    }

    fn fetch_curriculum(&self, selection: &CrawlSelection) -> Result<CandidatePlan> {
        let base_fields = vec![
            ("pageBbh", selection.grade.as_str()),
            ("pageYxdm", selection.college_code.as_str()),
            ("pageZydm", selection.major_code.as_str()),
            ("pageKkxn", ""),
            ("pageKkxq", ""),
            ("pageKcmc", ""),
        ];
        let capture = self.fetch_course_pages(
            "/pyfa/queryPykc",
            selection.kind,
            &base_fields,
            "curriculum-main",
            None,
            false,
        )?;
        if capture.courses.is_empty() {
            bail!("培养方案课程表已确认为空；为避免误删，不生成候选方案")
        }
        Ok(self.build_plan(selection, capture.courses, capture.evidence, None))
    }

    fn fetch_execution(&self, selection: &CrawlSelection) -> Result<CandidatePlan> {
        let scope = [
            ("pageNj", selection.grade.as_str()),
            ("pageYxdm", selection.college_code.as_str()),
            ("pageZydm", selection.major_code.as_str()),
            ("pageKkxn", ""),
            ("pageKkxq", ""),
            ("pageKkxn1", ""),
            ("pageKkxq1", ""),
            ("pageKcmc", ""),
        ];
        let main = self.fetch_course_pages(
            "/zxjh/queryZxkc",
            selection.kind,
            &scope,
            "execution-main",
            None,
            true,
        )?;

        let tree_body = self.request_text(Method::POST, "/zxjh/queryMkTree", &scope, &[])?;
        let module_tree = parse::parse_module_tree(&tree_body)?;
        let mut module_details = Vec::new();
        let mut all_courses = main.courses;
        for (module_id, direction_key) in parse::module_requests(&module_tree) {
            let relation = json!({"module_id": module_id, "direction_key": direction_key});
            let query = [
                ("nj", selection.grade.as_str()),
                ("yxdm", selection.college_code.as_str()),
                ("zydm", selection.major_code.as_str()),
                ("mkdm", module_id.as_str()),
                ("zyfx", direction_key.as_str()),
            ];
            // queryZxmkkc is GET; the complete scope remains attached to every page.
            let detail = self.fetch_course_pages_query(
                "/zxjh/queryZxmkkc",
                selection.kind,
                &query,
                "execution-module",
                Some(&relation),
            )?;
            let module_courses = detail.courses.clone();
            all_courses.extend(module_courses.clone());
            module_details.push(json!({
                "module_id": module_id,
                "direction_key": direction_key,
                "metadata": detail.metadata,
                "courses": module_courses,
                "source_capture": detail.evidence
            }));
        }

        let direction_body = self.request_text(Method::POST, "/zxjh/queryZxfx", &scope, &[])?;
        let direction_entries = parse::parse_direction_entries(&direction_body)?;
        let mut direction_requirements = Vec::new();
        for entry in direction_entries {
            let idstr = entry
                .get("idstr")
                .and_then(Value::as_str)
                .context("方向要求缺少标识")?;
            let value =
                self.request_json(Method::POST, "/zxjh/queryZxfxyq", &[("idstr", idstr)], &[])?;
            direction_requirements
                .push(json!({"direction": entry, "requirements": parse::scrub_json(&value)}));
        }
        let notes_body = self.request_text(Method::POST, "/zxjh/queryZxbz", &scope, &[])?;
        let minor = self.fetch_course_pages(
            "/zxjh/queryZxkcSxw",
            selection.kind,
            &scope,
            "execution-double-degree-minor",
            None,
            true,
        )?;
        all_courses.extend(minor.courses.iter().cloned());
        let academic_structure = json!({
            "module_tree": module_tree,
            "module_details": module_details,
            "direction_requirements": direction_requirements,
            "notes": parse::parse_metadata_html(&notes_body),
            "double_degree_minor_requirements": {"courses":minor.courses,"metadata":minor.metadata,"source_capture":minor.evidence}
        });
        Ok(self.build_plan(
            selection,
            all_courses,
            main.evidence,
            Some(academic_structure),
        ))
    }

    fn build_plan(
        &self,
        selection: &CrawlSelection,
        courses: Vec<Value>,
        evidence: Value,
        academic_structure: Option<Value>,
    ) -> CandidatePlan {
        let plan_id = selection.plan_id();
        let (major_name, program_type) = parse::split_major_name(&selection.major_name);
        let mut info = json!({
            "plan_id": plan_id,
            "campus": "hit",
            "source_kind": selection.kind.source_name(),
            "department_code": selection.college_code,
            "school_name": selection.college_name,
            "major_code": selection.major_code,
            "major_name": major_name,
            "major_full_name": selection.major_name,
            "program_type": program_type,
            "source_capture": evidence
        });
        match selection.kind {
            PlanKind::Curriculum => info["plan_version"] = json!(selection.grade),
            PlanKind::Execution => info["entry_cohort"] = json!(selection.grade),
        }
        if let Some(structure) = academic_structure {
            info["academic_structure"] = structure;
        }
        CandidatePlan {
            plan_id,
            info,
            courses,
        }
    }

    fn fetch_course_pages(
        &self,
        endpoint: &str,
        kind: PlanKind,
        scope: &[(&str, &str)],
        source_section: &str,
        relation: Option<&Value>,
        allow_empty: bool,
    ) -> Result<PageCapture> {
        self.fetch_pages_impl(
            endpoint,
            kind,
            scope,
            source_section,
            relation,
            allow_empty,
            false,
        )
    }

    fn fetch_course_pages_query(
        &self,
        endpoint: &str,
        kind: PlanKind,
        scope: &[(&str, &str)],
        source_section: &str,
        relation: Option<&Value>,
    ) -> Result<PageCapture> {
        self.fetch_pages_impl(endpoint, kind, scope, source_section, relation, true, true)
    }

    fn fetch_pages_impl(
        &self,
        endpoint: &str,
        kind: PlanKind,
        scope: &[(&str, &str)],
        source_section: &str,
        relation: Option<&Value>,
        allow_empty: bool,
        use_query: bool,
    ) -> Result<PageCapture> {
        let first = if use_query {
            self.request_text(Method::GET, endpoint, &[], scope)?
        } else {
            self.request_text(Method::POST, endpoint, scope, &[])?
        };
        let first_page = parse::parse_course_html(&first, kind, source_section, relation)?;
        if first_page.courses.is_empty() && !first_page.confirmed_empty {
            bail!("课程页为空但未提供可验证的空表结构")
        }
        if first_page.courses.is_empty() && first_page.unpaged_courses.is_empty() && !allow_empty {
            bail!("课程表已确认为空")
        }
        let page_count = first_page.page_count.unwrap_or(1);
        if page_count == 0 || page_count > 1000 {
            bail!("课程分页页数异常")
        }
        let page_size = first_page.page_size.unwrap_or_else(|| {
            if first_page.page_count.is_some() {
                20
            } else {
                first_page.courses.len().max(1)
            }
        });
        if page_size == 0 || first_page.courses.len() > page_size {
            bail!("课程分页大小与主课程表不一致");
        }
        if page_count > 1 && first_page.courses.len() != page_size {
            bail!("课程分页在首页提前结束");
        }
        let mut pages = vec![first_page.courses.len()];
        let unpaged = first_page.unpaged_courses;
        let headers = first_page.headers;
        let mut courses = first_page.courses;
        let first_fingerprint = parse::course_fingerprint(&courses);
        for page_no in 2..=page_count {
            let page_no_text = page_no.to_string();
            let page_size_text = page_size.to_string();
            let page_count_text = page_count.to_string();
            let paging = [
                ("pageNo", page_no_text.as_str()),
                ("pageSize", page_size_text.as_str()),
                ("pageCount", page_count_text.as_str()),
            ];
            let mut request_scope = scope.to_vec();
            request_scope.extend_from_slice(&paging);
            let body = if use_query {
                self.request_text(Method::GET, endpoint, &[], &request_scope)?
            } else {
                self.request_text(Method::POST, endpoint, &request_scope, &[])?
            };
            let page = parse::parse_course_html(&body, kind, source_section, relation)?;
            if page.headers != headers || page.unpaged_courses != unpaged {
                bail!("翻页期间课程表头或无代码培养要求发生变化");
            }
            if page.courses.len() > page_size
                || page.page_size.is_some_and(|size| size != page_size)
            {
                bail!("课程分页大小发生变化");
            }
            if page.page_count.is_some_and(|value| value != page_count) {
                bail!("课程分页总页数在抓取过程中发生变化")
            }
            if page.courses.is_empty() {
                bail!("课程分页在第 {page_no} 页提前返回空页")
            }
            if parse::course_fingerprint(&page.courses) == first_fingerprint {
                bail!("课程分页重复返回首页，已停止抓取")
            }
            if page_no < page_count && page.courses.len() < page_size {
                bail!("课程分页在第 {page_no} 页提前结束")
            }
            pages.push(page.courses.len());
            courses.extend(page.courses);
        }
        let unpaged_rows = unpaged.len();
        courses.extend(unpaged);
        let rows = courses.len();
        let scope_json = scope
            .iter()
            .map(|(key, value)| ((*key).to_string(), Value::String((*value).to_string())))
            .collect::<serde_json::Map<_, _>>();
        Ok(PageCapture {
            metadata: parse::parse_metadata_html(&first),
            courses,
            evidence: json!({
                "complete": true,
                "endpoint": endpoint,
                "pages": pages,
                "rows": rows,
                "unpaged_rows": unpaged_rows,
                "scope": scope_json,
                "checks": {
                    "authenticated": true,
                    "expected_headers": true,
                    "pagination_consistent": true,
                    "confirmed_empty": rows == 0
                }
            }),
        })
    }

    fn request_json(
        &self,
        method: Method,
        path: &str,
        form: &[(&str, &str)],
        query: &[(&str, &str)],
    ) -> Result<Value> {
        let text = self.request_text(method, path, form, query)?;
        serde_json::from_str(&text).context("教务系统返回的专业或要求数据结构无法识别")
    }

    fn request_text(
        &self,
        method: Method,
        path: &str,
        form: &[(&str, &str)],
        query: &[(&str, &str)],
    ) -> Result<String> {
        self.ensure_allowed_path(path)?;
        let initial = self
            .provider
            .cookie(self.base_url.as_str(), false)
            .context("无法取得教务系统会话")?;
        match self.send_once(method.clone(), path, form, query, &initial)? {
            CheckedResponse::Body(body) => Ok(body),
            CheckedResponse::AuthenticationExpired => {
                let refreshed = self
                    .provider
                    .cookie(self.base_url.as_str(), true)
                    .context("教务系统登录刷新失败，请重新登录后再试")?;
                match self.send_once(method, path, form, query, &refreshed)? {
                    CheckedResponse::Body(body) => Ok(body),
                    CheckedResponse::AuthenticationExpired => {
                        bail!("登录已失效，请重新登录教务系统后再试")
                    }
                }
            }
        }
    }

    fn send_once(
        &self,
        method: Method,
        path: &str,
        form: &[(&str, &str)],
        query: &[(&str, &str)],
        cookie: &str,
    ) -> Result<CheckedResponse> {
        if cookie.trim().is_empty() {
            bail!("教务系统会话为空")
        }
        let url = self.base_url.join(path).context("教务系统地址拼接失败")?;
        if url.origin() != self.base_url.origin() {
            bail!("拒绝向非学校同源地址发送认证信息")
        }
        let mut request = self
            .client
            .request(method, url)
            .headers(self.headers(cookie)?);
        if !form.is_empty() {
            request = request.form(form);
        }
        if !query.is_empty() {
            request = request.query(query);
        }
        let response = request
            .send()
            .map_err(|_| anyhow::anyhow!("无法连接教务系统"))?;
        self.checked_response(response)
    }

    fn checked_response(&self, response: Response) -> Result<CheckedResponse> {
        let status = response.status().as_u16();
        let final_url = response.url().clone();
        let cross_origin = final_url.origin() != self.base_url.origin();
        let body = response
            .text()
            .map_err(|_| anyhow::anyhow!("无法读取教务系统响应"))?;
        if cross_origin || is_authentication_failure(status, &final_url, &body) {
            return Ok(CheckedResponse::AuthenticationExpired);
        }
        if !(200..300).contains(&status) {
            bail!("教务系统暂时不可用（状态码 {status}）")
        }
        Ok(CheckedResponse::Body(body))
    }

    fn headers(&self, cookie: &str) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(
            COOKIE,
            HeaderValue::from_str(cookie).context("教务系统会话格式无效")?,
        );
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static("HIT-Fireworks-Manager/2.0"),
        );
        headers.insert(
            REFERER,
            HeaderValue::from_str(self.base_url.as_str()).context("教务系统地址格式无效")?,
        );
        Ok(headers)
    }

    fn ensure_allowed_path(&self, path: &str) -> Result<()> {
        const ALLOWED: &[&str] = &[
            "/pyfa/queryPykc",
            "/pub/queryYxzyList_bbh",
            "/pub/queryYxzyList_x",
            "/zxjh/queryZxkc",
            "/zxjh/queryMkTree",
            "/zxjh/queryZxmkkc",
            "/zxjh/queryZxfx",
            "/zxjh/queryZxfxyq",
            "/zxjh/queryZxbz",
            "/zxjh/queryZxkcSxw",
        ];
        if !ALLOWED.contains(&path) {
            bail!("拒绝访问非教学计划只读接口")
        }
        Ok(())
    }
}

struct PageCapture {
    courses: Vec<Value>,
    evidence: Value,
    metadata: Value,
}

enum CheckedResponse {
    Body(String),
    AuthenticationExpired,
}

pub fn validate_authenticated_response(status: u16, url: &Url, body: &str) -> Result<()> {
    if is_authentication_failure(status, url, body) {
        bail!("登录已失效，请重新登录教务系统后再试")
    }
    if !(200..300).contains(&status) {
        bail!("教务系统暂时不可用（状态码 {status}）")
    }
    Ok(())
}

fn is_authentication_failure(status: u16, url: &Url, body: &str) -> bool {
    if matches!(status, 401 | 403) || (300..400).contains(&status) {
        return true;
    }
    let url = url.as_str().to_ascii_lowercase();
    let lower = body.to_ascii_lowercase();
    url.contains("login")
        || url.contains("authserver")
        || lower.contains("atrust")
        || lower.contains("统一身份认证")
        || (lower.contains("页面过期") && lower.contains("重新登录"))
        || (lower.contains("登录") && lower.contains("password"))
}

fn encode_identity(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

pub fn parse_catalog_html(body: &str, kind: PlanKind) -> Result<CurriculumCatalog> {
    parse::parse_catalog_html(body, kind)
}

pub fn parse_major_response(value: &Value) -> Result<Vec<CatalogOption>> {
    parse::parse_major_response(value, false)
}

#[cfg(test)]
#[path = "jwts_tests.rs"]
mod tests;
