use super::*;
use parking_lot::Mutex;
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

fn table(headers: &[&str], rows: &[Vec<String>], paging: Option<(usize, usize)>) -> String {
    let headers = headers
        .iter()
        .map(|v| format!("<th>{v}</th>"))
        .collect::<String>();
    let rows = rows
        .iter()
        .map(|row| {
            format!(
                "<tr>{}</tr>",
                row.iter()
                    .map(|v| format!("<td>{v}</td>"))
                    .collect::<String>()
            )
        })
        .collect::<String>();
    let paging = paging
        .map(|(count, size)| {
            format!(
                "<input name='pageCount' value='{count}'><input name='pageSize' value='{size}'>"
            )
        })
        .unwrap_or_default();
    format!("<table><tr>{headers}</tr>{rows}</table>{paging}")
}

fn http(status: u16, body: &str) -> String {
    format!("HTTP/1.1 {status} X\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.as_bytes().len())
}

fn server(responses: Vec<String>) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    let handle = thread::spawn(move || {
        for response in responses {
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("mock server accept failed: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0_u8; 4096];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) => panic!("HTTP client closed before sending a complete request"),
                    Err(error) => panic!("HTTP request read failed: {error}"),
                    Ok(size) => {
                        bytes.extend_from_slice(&chunk[..size]);
                        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                            let headers = String::from_utf8_lossy(&bytes[..end]);
                            let length = headers
                                .lines()
                                .find_map(|line| {
                                    line.to_ascii_lowercase()
                                        .strip_prefix("content-length:")
                                        .and_then(|v| v.trim().parse::<usize>().ok())
                                })
                                .unwrap_or(0);
                            if bytes.len() >= end + 4 + length {
                                break;
                            }
                        }
                    }
                }
            }
            captured
                .lock()
                .push(String::from_utf8_lossy(&bytes).into_owned());
            stream.write_all(response.as_bytes()).unwrap();
        }
    });
    (format!("http://{address}"), requests, handle)
}

#[derive(Default)]
struct Provider {
    normal: AtomicUsize,
    refresh: AtomicUsize,
}
impl SessionProvider for Provider {
    fn cookie(&self, _: &str, refresh: bool) -> Result<String> {
        if refresh {
            self.refresh.fetch_add(1, Ordering::SeqCst);
            Ok("S=fresh".into())
        } else {
            self.normal.fetch_add(1, Ordering::SeqCst);
            Ok("S=old".into())
        }
    }
}

fn curriculum_selection() -> CrawlSelection {
    CrawlSelection {
        grade: "2022版".into(),
        college_code: "01".into(),
        college_name: "学院".into(),
        major_code: "0809".into(),
        major_name: "计算机科学与技术【本】".into(),
        kind: PlanKind::Curriculum,
    }
}

#[test]
fn identities_are_source_specific_and_names_do_not_participate() {
    let mut selection = curriculum_selection();
    assert_eq!(selection.plan_id(), "hit:2022%E7%89%88:01:0809");
    selection.kind = PlanKind::Execution;
    selection.grade = "2023".into();
    assert_eq!(selection.plan_id(), "hit:execution:2023:01:0809");
}

#[test]
fn real_major_arrays_keep_full_labels_and_reject_conflicting_codes() {
    let rows = parse_major_response(
        &json!({"pageZydm":["A","B"],"pageZymc":["同名【本】","同名【辅修】"]}),
    )
    .unwrap();
    assert_eq!(rows[0].name, "同名【本】");
    assert_eq!(rows[1].name, "同名【辅修】");
    let error =
        parse_major_response(&json!({"rows":[{"zydm":"A","zymc":"甲"},{"zydm":"A","zymc":"乙"}]}))
            .unwrap_err()
            .to_string();
    assert!(error.contains("多个不同名称"));
}

#[test]
fn course_html_uses_headers_preserves_unknown_duplicate_and_empty_code() {
    let html = table(
        &[
            "课程名称",
            "未知属性",
            "课程代码",
            "学分",
            "总学时",
            "是否考试课",
        ],
        &[
            vec![
                "程序设计".into(),
                "荣誉".into(),
                "CS1".into(),
                "3.5".into(),
                "48".into(),
                "是".into(),
            ],
            vec![
                "程序设计".into(),
                "另一次".into(),
                "CS1".into(),
                "3.5".into(),
                "48".into(),
                "".into(),
            ],
            vec![
                "导论".into(),
                "".into(),
                "".into(),
                "1".into(),
                "16".into(),
                "".into(),
            ],
        ],
        None,
    );
    let page = super::parse::parse_course_html(&html, PlanKind::Execution, "execution-main", None)
        .unwrap();
    assert_eq!(page.courses.len(), 3);
    assert_eq!(page.courses[0]["assessment_method"], "考试");
    assert_eq!(page.courses[0]["credit"], 3.5);
    assert_eq!(page.courses[0]["source_fields"]["未知属性"], "荣誉");
    assert!(page.courses[1].get("assessment_method").is_none());
    assert_eq!(page.courses[2]["course_code"], "");
}

#[test]
fn first_post_has_no_page_number_and_declared_last_page_is_fetched() {
    let first_rows = (0..20)
        .map(|i| vec![format!("C{i}"), format!("课{i}")])
        .collect::<Vec<_>>();
    let first = table(&["课程代码", "课程名称"], &first_rows, Some((2, 20)));
    let second = table(
        &["课程代码", "课程名称"],
        &[vec!["C20".into(), "末页".into()]],
        Some((2, 20)),
    );
    let (base, requests, handle) = server(vec![http(200, &first), http(200, &second)]);
    let plan = JwtsClient::new(&base, "S=x")
        .unwrap()
        .fetch_plan(&curriculum_selection())
        .unwrap();
    handle.join().unwrap();
    assert_eq!(plan.courses.len(), 21);
    let requests = requests.lock();
    assert!(!requests[0].contains("pageNo="));
    assert!(requests[0].contains("pageBbh=2022"));
    assert!(requests[1].contains("pageNo=2"));
    assert!(requests
        .iter()
        .all(|request| !request.to_ascii_lowercase().contains("ticket")));
}

#[test]
fn short_unpaged_table_is_complete_but_repeated_first_page_fails() {
    let short = table(
        &["序号", "课程代码", "课程名称"],
        &[vec!["1".into(), "C1".into(), "课".into()]],
        None,
    );
    let (base, _, handle) = server(vec![http(200, &short)]);
    assert!(JwtsClient::new(&base, "S=x")
        .unwrap()
        .fetch_plan(&curriculum_selection())
        .is_ok());
    handle.join().unwrap();

    let rows = (0..20)
        .map(|i| vec![format!("C{i}"), format!("课{i}")])
        .collect::<Vec<_>>();
    let repeated = table(&["课程代码", "课程名称"], &rows, Some((2, 20)));
    let (base, _, handle) = server(vec![http(200, &repeated), http(200, &repeated)]);
    let error = JwtsClient::new(&base, "S=x")
        .unwrap()
        .fetch_plan(&curriculum_selection())
        .unwrap_err()
        .to_string();
    handle.join().unwrap();
    assert!(error.contains("重复返回首页"));
}

#[test]
fn authentication_failure_refreshes_once_and_retries_same_query() {
    let catalog="<select id='pageNj'><option value='2023'>2023</option></select><select id='pageYxdm'><option value='01'>学院</option></select>";
    let provider = Arc::new(Provider::default());
    let (base, requests, handle) = server(vec![http(403, "forbidden"), http(200, catalog)]);
    let result = JwtsClient::with_session_provider(&base, provider.clone())
        .unwrap()
        .catalog(PlanKind::Execution)
        .unwrap();
    handle.join().unwrap();
    assert_eq!(result.grades[0].code, "2023");
    assert_eq!(provider.normal.load(Ordering::SeqCst), 1);
    assert_eq!(provider.refresh.load(Ordering::SeqCst), 1);
    let requests = requests.lock();
    assert!(requests[0].contains("S=old") && requests[1].contains("S=fresh"));

    let provider = Arc::new(Provider::default());
    let login = "<title>统一身份认证</title><input type='password'>";
    let (base, _, handle) = server(vec![http(200, login), http(200, login)]);
    let error = JwtsClient::with_session_provider(&base, provider.clone())
        .unwrap()
        .catalog(PlanKind::Execution)
        .unwrap_err()
        .to_string();
    handle.join().unwrap();
    assert!(error.contains("重新登录"));
    assert_eq!(provider.refresh.load(Ordering::SeqCst), 1);
}

#[test]
fn expired_session_page_refreshes_once_before_catalog_parsing() {
    let expired = "<html><script>alert('页面过期，请重新登录');</script></html>";
    let url = Url::parse("http://127.0.0.1:1080/zxjh/queryZxkc").unwrap();
    assert!(validate_authenticated_response(200, &url, expired).is_err());
    let catalog = "<select id='pageNj'><option value='2024'>2024</option></select><select id='pageYxdm'><option value='13'>计算学部</option></select>";
    let provider = Arc::new(Provider::default());
    let (base, requests, handle) = server(vec![http(200, expired), http(200, catalog)]);
    let result = JwtsClient::with_session_provider(&base, provider.clone())
        .unwrap()
        .catalog(PlanKind::Execution)
        .unwrap();
    handle.join().unwrap();
    assert_eq!(result.grades[0].code, "2024");
    assert_eq!(result.colleges[0].code, "13");
    assert_eq!(provider.refresh.load(Ordering::SeqCst), 1);
    let requests = requests.lock();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|request| request.starts_with("GET /zxjh/queryZxkc ")));
    assert!(requests[0].contains("S=old") && requests[1].contains("S=fresh"));
}

#[test]
fn module_direction_notes_and_relations_are_preserved() {
    let tree = r#"[{"id":"0","pId":"-2","idd":"FX1","name":"方向"},{"id":"MK1","pId":"0","idd":"FX1","name":"模块"}]"#;
    let nodes = super::parse::parse_module_tree(tree).unwrap();
    assert!(nodes
        .iter()
        .any(|node| node["module_id"] == "MK1" && node["direction_key"] == "FX1"));
    let directions = super::parse::parse_direction_entries(
        "<button onclick=\"queryZxjhyq(this,'DIR9')\">方向</button>",
    )
    .unwrap();
    assert_eq!(directions[0]["idstr"], "DIR9");
    let metadata = super::parse::parse_metadata_html(
        "<table><tr><th>校区</th><td>一校区</td></tr></table><textarea name='bz'>备注</textarea>",
    );
    assert_eq!(metadata["fields"]["bz"], "备注");
    let relation = json!({"module_id":"MK1","direction_key":"FX1"});
    let page = super::parse::parse_course_html(
        &table(
            &["课程代码", "课程名称"],
            &[vec!["C1".into(), "课".into()]],
            None,
        ),
        PlanKind::Execution,
        "execution-module",
        Some(&relation),
    )
    .unwrap();
    assert_eq!(page.courses[0]["relation"]["module_id"], "MK1");
}

#[test]
fn curriculum_unpaged_requirements_are_kept_once_and_changes_abort_capture() {
    let main = |code: &str| {
        table(
            &["序号", "课程代码", "课程名称", "学分"],
            &[vec!["1".into(), code.into(), "程序设计".into(), "3".into()]],
            Some((2, 1)),
        )
    };
    let requirements = |credit: &str| {
        table(
            &["序号", "课程名称", "学分"],
            &[
                vec!["1".into(), "创新实践".into(), credit.into()],
                vec!["2".into(), "创新实践".into(), credit.into()],
            ],
            None,
        )
    };
    let first = format!("{}{}", main("A1"), requirements("1"));
    let second = format!("{}{}", main("A2"), requirements("1"));
    let (base, _, handle) = server(vec![http(200, &first), http(200, &second)]);
    let plan = JwtsClient::new(&base, "S=test")
        .unwrap()
        .fetch_plan(&curriculum_selection())
        .unwrap();
    handle.join().unwrap();
    assert_eq!(plan.courses.len(), 4);
    assert_eq!(
        plan.courses
            .iter()
            .filter(|c| c["course_code"] == "")
            .count(),
        2
    );
    assert_eq!(plan.info["source_capture"]["unpaged_rows"], 2);
    let changed = format!("{}{}", main("A2"), requirements("2"));
    let (base, _, handle) = server(vec![http(200, &first), http(200, &changed)]);
    let result = JwtsClient::new(&base, "S=test")
        .unwrap()
        .fetch_plan(&curriculum_selection());
    handle.join().unwrap();
    assert!(result.unwrap_err().to_string().contains("培养要求发生变化"));
}

#[test]
fn execution_http_capture_contains_official_name_modules_and_requirements() {
    let catalog = "<select name='pageNj'><option value='2024'>2024</option></select><select name='pageYxdm'><option value='13B'>计算学部</option></select>";
    let majors = r#"[{"pageZydm":"13B371","pageZymc":"软件工程【本】"},{"pageZydm":"13BE371","pageZymc":"软件工程【第二学士学位】"}]"#;
    let main = table(
        &[
            "课程代码",
            "课程名称",
            "开课学年",
            "开课学期",
            "学分",
            "总学时",
            "是否考试课",
        ],
        &[vec![
            "A1".into(),
            "程序设计".into(),
            "1".into(),
            "秋季".into(),
            "3".into(),
            "48".into(),
            "是".into(),
        ]],
        None,
    );
    let tree = r#"[{"id":"0","pId":"-2","idd":"0","name":"无"},{"id":"M1","pId":"0","idd":"0","name":"专业核心"}]"#;
    let module = format!(
        "<table><tr><th>要求学分</th><td>7</td></tr></table>{}",
        table(
            &["课程代码", "课程名称", "学分", "考核方式"],
            &[vec![
                "M101".into(),
                "模块新课程".into(),
                "2".into(),
                "考查".into()
            ]],
            None
        )
    );
    let direction = "<table><tr><th>专业方向名称</th></tr><tr onclick=\"queryZxjhyq(this,'scope-direction-7')\"><td>无</td></tr></table>";
    let requirements = r#"{"mkyqList":[{"mkdm":"M1","yqxf":7,"sfbx":"1"}],"lbxfyqList":[{"kclbmc":"创新实践","xf":6}]}"#;
    let notes = "<table><tr><th>年级</th><th>计划类型</th><th>状态</th></tr><tr><td>2024</td><td>常规</td><td>提交</td></tr></table>";
    let minor = table(&["课程代码", "课程名称", "课程归属", "学分"], &[], None);
    let (base, requests, handle) = server(vec![
        http(200, catalog),
        http(200, majors),
        http(200, &main),
        http(200, tree),
        http(200, &module),
        http(200, direction),
        http(200, requirements),
        http(200, notes),
        http(200, &minor),
    ]);
    let client = JwtsClient::new(&base, "S=fixture").unwrap();
    assert_eq!(
        client.catalog(PlanKind::Execution).unwrap().grades[0].code,
        "2024"
    );
    let majors = client.majors(PlanKind::Execution, "13B", "2024").unwrap();
    assert_eq!(majors.len(), 2);
    let plan = client
        .fetch_plan(&CrawlSelection {
            kind: PlanKind::Execution,
            grade: "2024".into(),
            college_code: "13B".into(),
            college_name: "计算学部".into(),
            major_code: majors[0].code.clone(),
            major_name: majors[0].name.clone(),
        })
        .unwrap();
    handle.join().unwrap();
    assert_eq!(plan.plan_id, "hit:execution:2024:13B:13B371");
    assert_eq!(plan.info["major_full_name"], "软件工程【本】");
    assert_eq!(plan.info["program_type"], "本");
    assert_eq!(plan.info["entry_cohort"], "2024");
    assert!(plan.info.get("plan_version").is_none());
    assert_eq!(plan.courses.len(), 2);
    assert_eq!(plan.courses[0]["recommended_year_semester"], "第一学年秋季");
    assert_eq!(plan.courses[1]["course_code"], "M101");
    assert_eq!(plan.courses[1]["relation"]["module_id"], "M1");
    let structure = &plan.info["academic_structure"];
    assert_eq!(structure["module_tree"].as_array().unwrap().len(), 2);
    assert_eq!(
        structure["module_details"][0]["courses"][0]["course_code"],
        "M101"
    );
    assert_eq!(
        structure["direction_requirements"][0]["requirements"]["mkyqList"][0]["yqxf"],
        7
    );
    assert!(structure["notes"].to_string().contains("常规"));
    assert!(structure["double_degree_minor_requirements"]["courses"]
        .as_array()
        .unwrap()
        .is_empty());
    let requests = requests.lock();
    assert!(requests[1].starts_with("POST /pub/queryYxzyList_x "));
    for index in [2, 3, 5, 7, 8] {
        assert!(requests[index].contains("pageNj=2024"));
        assert!(requests[index].contains("pageYxdm=13B"));
        assert!(requests[index].contains("pageZydm=13B371"));
    }
    assert!(!requests[2].contains("pageNo="));
    assert!(requests[4].starts_with("GET /zxjh/queryZxmkkc?"));
    assert!(requests[4].contains("mkdm=M1") && requests[4].contains("zyfx=0"));
    assert!(requests[6].contains("idstr=scope-direction-7"));
}

#[test]
fn real_major_array_and_malformed_rows_have_distinct_outcomes() {
    let rows = parse_major_response(&json!([
        {"pageZydm":"01E041","pageZymc":"自动化【第二学士学位】"},
        {"pageZydm":"01041","pageZymc":"自动化【本】"}
    ]))
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert_ne!(rows[0].code, rows[1].code);
    let malformed="<table><tr><th>课程代码</th><th>课程名称</th><th>学分</th></tr><tr><td>A1</td><td>缺学分列</td></tr></table>";
    assert!(super::parse::parse_course_html(
        malformed,
        PlanKind::Execution,
        "execution-main",
        None
    )
    .is_err());
    assert!(super::parse::parse_module_tree(r#"[{"id":"M1","pId":"M2","idd":"0","name":"循环"},{"id":"M2","pId":"M1","idd":"0","name":"循环"}]"#).is_err());
}

#[test]
fn empty_major_scopes_are_distinct_from_malformed_responses() {
    for kind in [PlanKind::Curriculum, PlanKind::Execution] {
        let (base, requests, handle) = server(vec![http(200, "[]")]);
        let result = JwtsClient::new(&base, "S=scope").unwrap().majors(kind, "01", "2027");
        handle.join().unwrap();
        assert!(result.unwrap().is_empty());
        assert_eq!(requests.lock().len(), 1);
    }
    for malformed in [
        json!([{"pageZydm":"A","pageZymc":"完整专业"},{"pageZydm":"B"}]),
        json!({"pageZydm":["A","B"],"pageZymc":["完整专业",""]}),
        json!([{"unexpected":"不是专业行"}]),
    ] {
        assert!(super::parse::parse_major_response(&malformed, true).is_err());
    }
}

#[test]
fn ambiguous_cross_cohort_labels_do_not_invent_program_identity() {
    let legacy = vec![
        CatalogOption { code: "A".into(), name: "原专业".into() },
        CatalogOption { code: "B".into(), name: "另一专业".into() },
    ];
    let official = vec![
        CatalogOption { code: "A".into(), name: "原专业【本】".into() },
        CatalogOption { code: "A".into(), name: "更名专业【本】".into() },
        CatalogOption { code: "B".into(), name: "另一专业【辅修】".into() },
    ];
    let result = super::parse::merge_major_labels(legacy, official).unwrap();
    assert_eq!(result[0].name, "原专业");
    assert_eq!(result[1].name, "另一专业【辅修】");
}

#[test]
fn complete_unpaged_tables_are_not_limited_to_twenty_rows() {
    for count in [20, 27] {
        let rows = (0..count)
            .map(|index| vec![format!("C{index}"), format!("课程{index}")])
            .collect::<Vec<_>>();
        let body = table(&["课程代码", "课程名称"], &rows, None);
        let (base, requests, handle) = server(vec![http(200, &body)]);
        let client = JwtsClient::new(&base, "S=unpaged").unwrap();
        let capture = client.fetch_course_pages_query(
            "/zxjh/queryZxmkkc",
            PlanKind::Execution,
            &[("nj", "2025"), ("yxdm", "35"), ("zydm", "35E242"), ("mkdm", "M1"), ("zyfx", "0")],
            "execution-module",
            Some(&json!({"module_id":"M1","direction_key":"0"})),
        );
        handle.join().unwrap();
        let capture = capture.unwrap();
        assert_eq!(capture.courses.len(), count);
        assert_eq!(capture.evidence["pages"], json!([count]));
        assert_eq!(capture.courses.last().unwrap()["course_code"], format!("C{}", count - 1));
        assert_eq!(requests.lock().len(), 1);
    }
    let rows = (0..20).map(|index| vec![format!("C{index}"), "课程".into()]).collect::<Vec<_>>();
    let body = table(&["课程代码", "课程名称"], &rows, None);
    let (base, _, handle) = server(vec![http(200, &body)]);
    let plan = JwtsClient::new(&base, "S=unpaged").unwrap().fetch_plan(&curriculum_selection());
    handle.join().unwrap();
    assert_eq!(plan.unwrap().courses.len(), 20);
}

#[test]
fn pagination_controls_without_valid_page_count_are_rejected() {
    for controls in [
        "<input name='pageSize' value='20'>",
        "<input id='pageCount' value='broken'>",
        "<form name='page'><input name='pageNo'></form>",
    ] {
        let body = format!("{}{controls}", table(&["课程代码", "课程名称"], &[vec!["C1".into(), "课程".into()]], None));
        let (base, _, handle) = server(vec![http(200, &body)]);
        let result = JwtsClient::new(&base, "S=paged").unwrap().fetch_plan(&curriculum_selection());
        handle.join().unwrap();
        assert!(result.is_err(), "分页控制缺少总页数不应被当成无分页表");
    }
}

#[test]
fn direction_placeholder_roots_resolve_only_their_own_modules() {
    let tree = r#"[{"id":"0","pId":"-2","idd":"06111","name":"无"},{"id":"0","pId":"-2","idd":"0","name":"无"},{"id":"100002153","pId":"06111","idd":"06111","name":"专业方向限选课"}]"#;
    let nodes = super::parse::parse_module_tree(tree).unwrap();
    let module = nodes.iter().find(|node| node["id"] == "100002153").unwrap();
    assert_eq!(module["pId"], "06111");
    assert_eq!(module["parent_id"], "0");
    assert_eq!(module["direction_key"], "06111");
    assert_eq!(super::parse::module_requests(&nodes), vec![("100002153".into(), "06111".into())]);
    let wrong_direction = r#"[{"id":"0","pId":"-2","idd":"0","name":"无"},{"id":"M1","pId":"06111","idd":"06111","name":"模块"}]"#;
    assert!(super::parse::parse_module_tree(wrong_direction).is_err());
}
