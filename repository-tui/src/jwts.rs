use anyhow::{bail, Context, Result};
use reqwest::blocking::{Client, Response};
use reqwest::header::{COOKIE, REFERER, USER_AGENT};
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::Duration;
use url::Url;

pub const DEFAULT_HIT_BASE_URL: &str = "http://jwts-hit-edu-cn.ivpn.hit.edu.cn:1080";

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

#[derive(Debug, Clone)]
pub struct JwtsClient {
    base_url: Url,
    cookie: String,
    client: Client,
}

impl JwtsClient {
    pub fn new(base_url: &str, cookie: &str) -> Result<Self> {
        let base_url = Url::parse(base_url).context("教务系统地址无效")?;
        let cookie = cookie.trim().to_string();
        if cookie.is_empty() {
            bail!("请粘贴教务系统登录信息")
        }
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::limited(5))
            .cookie_store(true)
            .build()
            .context("无法初始化网络连接")?;
        Ok(Self {
            base_url,
            cookie,
            client,
        })
    }

    pub fn catalog(&self) -> Result<CurriculumCatalog> {
        let response = self
            .client
            .get(self.url("/pyfa/queryPykc")?)
            .headers(self.headers()?)
            .send()
            .context("无法连接教务系统")?;
        let body = self.checked_text(response)?;
        parse_catalog_html(&body)
    }

    pub fn majors(&self, college_code: &str, grade: &str) -> Result<Vec<CatalogOption>> {
        let response = self
            .client
            .post(self.url("/pub/queryYxzyList_bbh")?)
            .headers(self.headers()?)
            .form(&[("yxdm", college_code), ("nj", grade)])
            .send()
            .context("无法读取专业列表")?;
        let value = self.checked_json(response)?;
        parse_major_response(&value)
    }

    pub fn fetch_plan(&self, selection: &CrawlSelection) -> Result<CandidatePlan> {
        let page_size = 200usize;
        let mut page_no = 1usize;
        let mut courses = Vec::new();
        let mut total: Option<usize> = None;
        let mut plan_info = json!({
            "grade": selection.grade,
            "college_code": selection.college_code,
            "college_name": selection.college_name,
            "major_code": selection.major_code,
            "major_name": selection.major_name
        });
        loop {
            let page_no_text = page_no.to_string();
            let fields = vec![
                ("pageBbh", selection.grade.as_str()),
                ("pageYxdm", selection.college_code.as_str()),
                ("pageZydm", selection.major_code.as_str()),
                ("pageKkxn", ""),
                ("pageKkxq", ""),
                ("pageKcmc", ""),
                ("pageSize", "200"),
                ("pageNo", page_no_text.as_str()),
                ("pageCount", "0"),
            ];
            let response = self
                .client
                .post(self.url("/pyfa/queryPykc")?)
                .headers(self.headers()?)
                .form(&fields)
                .send()
                .context("无法读取课程数据")?;
            let value = self.checked_json(response)?;
            let page = parse_course_page(&value)?;
            if total.is_none() {
                total = page.total;
            }
            if !page.info.is_null() {
                merge_objects(&mut plan_info, &page.info);
            }
            let received = page.courses.len();
            courses.extend(page.courses);
            if received == 0
                || received < page_size
                || total.is_some_and(|value| courses.len() >= value)
            {
                break;
            }
            page_no += 1;
            if page_no > 1000 {
                bail!("课程分页异常，已停止更新")
            }
        }
        if courses.is_empty() {
            bail!("该专业没有返回课程数据")
        }
        let plan_id = format!(
            "HIT-{}-{}-{}",
            selection.grade, selection.college_code, selection.major_code
        );
        plan_info["plan_ID"] = json!(plan_id);
        Ok(CandidatePlan {
            plan_id,
            info: plan_info,
            courses,
        })
    }

    fn url(&self, path: &str) -> Result<Url> {
        self.base_url.join(path).context("教务系统地址拼接失败")
    }

    fn headers(&self) -> Result<reqwest::header::HeaderMap> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            COOKIE,
            self.cookie.parse().context("教务系统登录信息格式无效")?,
        );
        headers.insert(USER_AGENT, "HIT-Fireworks-Manager/1.0".parse().unwrap());
        headers.insert(REFERER, self.base_url.as_str().parse().unwrap());
        Ok(headers)
    }

    fn checked_text(&self, response: Response) -> Result<String> {
        let status = response.status();
        let final_url = response.url().clone();
        let body = response.text().context("无法读取教务系统响应")?;
        validate_authenticated_response(status.as_u16(), &final_url, &body)?;
        Ok(body)
    }

    fn checked_json(&self, response: Response) -> Result<Value> {
        let text = self.checked_text(response)?;
        serde_json::from_str(&text).context("教务系统返回了无法识别的数据")
    }
}

#[derive(Debug, Default)]
struct CoursePage {
    total: Option<usize>,
    courses: Vec<Value>,
    info: Value,
}

pub fn validate_authenticated_response(status: u16, url: &Url, body: &str) -> Result<()> {
    if matches!(status, 401 | 403) {
        bail!("登录已失效，请重新登录教务系统后再试")
    }
    let lower_url = url.as_str().to_lowercase();
    let lower = body.to_lowercase();
    if lower_url.contains("login")
        || lower_url.contains("authserver")
        || lower.contains("atrust")
        || lower.contains("统一身份认证")
        || lower.contains("登录") && lower.contains("password")
    {
        bail!("登录已失效，请重新登录教务系统后再试")
    }
    if !(200..300).contains(&status) {
        bail!("教务系统暂时不可用（状态码 {status}）")
    }
    Ok(())
}

pub fn parse_catalog_html(body: &str) -> Result<CurriculumCatalog> {
    let document = Html::parse_document(body);
    let option_selector = Selector::parse("option").unwrap();
    let select_selector = Selector::parse("select").unwrap();
    let mut grades = Vec::new();
    let mut colleges = Vec::new();
    for select in document.select(&select_selector) {
        let id = select.value().attr("id").unwrap_or("").to_lowercase();
        let name = select.value().attr("name").unwrap_or("").to_lowercase();
        let options = select
            .select(&option_selector)
            .filter_map(|option| {
                let code = option.value().attr("value")?.trim();
                let label = option.text().collect::<String>().trim().to_string();
                (!code.is_empty() && !label.is_empty()).then(|| CatalogOption {
                    code: code.to_string(),
                    name: label,
                })
            })
            .collect::<Vec<_>>();
        if id.contains("bbh") || id.contains("nj") || name.contains("bbh") || name.contains("nj") {
            grades.extend(options);
        } else if id.contains("yx") || name.contains("yx") {
            colleges.extend(options);
        }
    }
    dedup_options(&mut grades);
    dedup_options(&mut colleges);
    if grades.is_empty() || colleges.is_empty() {
        bail!("无法从教务系统页面识别年级和院系")
    }
    Ok(CurriculumCatalog { grades, colleges })
}

pub fn parse_major_response(value: &Value) -> Result<Vec<CatalogOption>> {
    let rows =
        find_array(value, &["list", "rows", "data", "result"]).context("专业列表结构无法识别")?;
    let mut result = rows
        .iter()
        .filter_map(|row| {
            let code = first_string(row, &["zydm", "zjdm", "major_code", "value"])?;
            let name = first_string(row, &["zymc", "zjmc", "major_name", "label"])?;
            Some(CatalogOption { code, name })
        })
        .collect::<Vec<_>>();
    dedup_options(&mut result);
    if result.is_empty() {
        bail!("该院系没有返回专业列表")
    }
    Ok(result)
}

fn parse_course_page(value: &Value) -> Result<CoursePage> {
    let rows =
        find_array(value, &["list", "rows", "data", "result"]).context("课程列表结构无法识别")?;
    let courses = rows.iter().map(normalize_course).collect::<Vec<_>>();
    let total = first_usize(value, &["total", "records", "pageCount", "count"]);
    let info = value
        .get("info")
        .or_else(|| value.get("plan"))
        .cloned()
        .unwrap_or(Value::Null);
    Ok(CoursePage {
        total,
        courses,
        info,
    })
}

fn normalize_course(row: &Value) -> Value {
    let mut result = row.clone();
    if !result.is_object() {
        return json!({"raw": row});
    }
    let mappings = [
        ("course_code", ["course_code", "kcdm", "kch"]),
        ("course_name", ["course_name", "kcmc", "name"]),
        ("credit", ["credit", "xf", "xuefen"]),
        ("assessment_method", ["assessment_method", "khfsm", "khfs"]),
        (
            "recommended_year_semester",
            ["recommended_year_semester", "tjkkxnxq", "xnxq"],
        ),
    ];
    for (target, aliases) in mappings {
        if result.get(target).is_some_and(|value| !value.is_null()) {
            continue;
        }
        if let Some(value) = aliases.iter().find_map(|key| row.get(*key).cloned()) {
            result[target] = value;
        }
    }
    result
}

fn find_array<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Vec<Value>> {
    if let Some(array) = value.as_array() {
        return Some(array);
    }
    for key in keys {
        if let Some(array) = value.get(*key).and_then(Value::as_array) {
            return Some(array);
        }
        if let Some(object) = value.get(*key) {
            if let Some(found) = find_array(object, keys) {
                return Some(found);
            }
        }
    }
    None
}

fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value.get(*key).and_then(|item| match item {
            Value::String(value) if !value.trim().is_empty() => Some(value.trim().to_string()),
            Value::Number(value) => Some(value.to_string()),
            _ => None,
        })
    })
}

fn first_usize(value: &Value, keys: &[&str]) -> Option<usize> {
    for key in keys {
        if let Some(number) = value.get(*key).and_then(Value::as_u64) {
            return Some(number as usize);
        }
        if let Some(text) = value.get(*key).and_then(Value::as_str) {
            if let Ok(number) = text.parse() {
                return Some(number);
            }
        }
    }
    None
}

fn dedup_options(options: &mut Vec<CatalogOption>) {
    let mut values = BTreeMap::new();
    for option in options.drain(..) {
        values.entry(option.code).or_insert(option.name);
    }
    *options = values
        .into_iter()
        .map(|(code, name)| CatalogOption { code, name })
        .collect();
}

fn merge_objects(target: &mut Value, source: &Value) {
    let Some(target) = target.as_object_mut() else {
        return;
    };
    let Some(source) = source.as_object() else {
        return;
    };
    for (key, value) in source {
        target.insert(key.clone(), value.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::thread;

    fn mock_server(
        responses: Vec<String>,
    ) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let handle = thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buffer = [0u8; 16384];
                let size = stream.read(&mut buffer).unwrap();
                captured
                    .lock()
                    .push(String::from_utf8_lossy(&buffer[..size]).to_string());
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        (format!("http://{address}"), requests, handle)
    }

    fn http(body: &str, content_type: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.as_bytes().len()
        )
    }

    #[test]
    fn parses_catalog_options() {
        let html = r#"
        <select id="pageBbh"><option value="2022">2022 版</option></select>
        <select id="pageYxdm"><option value="01">计算学部</option></select>
        "#;
        let catalog = parse_catalog_html(html).unwrap();
        assert_eq!(catalog.grades[0].code, "2022");
        assert_eq!(catalog.colleges[0].name, "计算学部");
    }

    #[test]
    fn rejects_login_page() {
        let url = Url::parse("http://example.test/login").unwrap();
        assert!(validate_authenticated_response(200, &url, "<title>ATrust 登录</title>").is_err());
        let url = Url::parse("http://example.test/pyfa/queryPykc").unwrap();
        assert!(validate_authenticated_response(403, &url, "forbidden").is_err());
    }

    #[test]
    fn parses_major_variants() {
        let value = json!({"rows":[{"zydm":"0809","zymc":"计算机科学与技术"}]});
        let rows = parse_major_response(&value).unwrap();
        assert_eq!(rows[0].code, "0809");
    }

    #[test]
    fn parses_course_page_and_normalizes_fields() {
        let value = json!({"total":1,"rows":[{"kcdm":"CS101","kcmc":"程序设计","xf":"3"}]});
        let page = parse_course_page(&value).unwrap();
        assert_eq!(page.total, Some(1));
        assert_eq!(page.courses[0]["course_code"], json!("CS101"));
        assert_eq!(page.courses[0]["course_name"], json!("程序设计"));
    }
    #[test]
    fn complete_http_flow_sends_cookie_and_paginates() {
        let catalog = r#"<select id="pageBbh"><option value="2022">2022 版</option></select><select id="pageYxdm"><option value="01">计算学部</option></select>"#;
        let majors = r#"{"rows":[{"zydm":"0809","zymc":"计算机科学与技术"}]}"#;
        let page_one_courses = (0..200)
            .map(|index| json!({"kcdm":format!("CS{index:03}"),"kcmc":format!("课程{index}"),"xf":"1"}))
            .collect::<Vec<_>>();
        let page_one =
            json!({"total":201,"rows":page_one_courses,"info":{"school_name":"计算学部"}})
                .to_string();
        let page_two =
            json!({"total":201,"rows":[{"kcdm":"CS200","kcmc":"课程200","xf":"2"}]}).to_string();
        let (base, requests, handle) = mock_server(vec![
            http(catalog, "text/html"),
            http(majors, "application/json"),
            http(&page_one, "application/json"),
            http(&page_two, "application/json"),
        ]);
        let client = JwtsClient::new(&base, "SESSION=secret").unwrap();
        let catalog_result = client.catalog().unwrap();
        assert_eq!(catalog_result.grades[0].code, "2022");
        let majors_result = client.majors("01", "2022").unwrap();
        assert_eq!(majors_result[0].name, "计算机科学与技术");
        let plan = client
            .fetch_plan(&CrawlSelection {
                grade: "2022".into(),
                college_code: "01".into(),
                college_name: "计算学部".into(),
                major_code: "0809".into(),
                major_name: "计算机科学与技术".into(),
            })
            .unwrap();
        assert_eq!(plan.courses.len(), 201);
        assert_eq!(plan.courses[200]["course_code"], json!("CS200"));
        handle.join().unwrap();
        let captured = requests.lock();
        assert_eq!(captured.len(), 4);
        assert!(captured
            .iter()
            .all(|request| request.to_lowercase().contains("cookie: session=secret")));
        assert!(captured[1].contains("yxdm=01"));
        assert!(captured[2].contains("pageNo=1"));
        assert!(captured[3].contains("pageNo=2"));
    }

    #[test]
    fn http_flow_rejects_atrust_login_response() {
        let (base, _requests, handle) =
            mock_server(vec![http("<title>ATrust 登录</title>", "text/html")]);
        let client = JwtsClient::new(&base, "SESSION=expired").unwrap();
        let error = client.catalog().unwrap_err().to_string();
        handle.join().unwrap();
        assert!(error.contains("登录已失效"));
    }
}
