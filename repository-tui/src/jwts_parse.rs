use super::{CatalogOption, CurriculumCatalog, PlanKind};
use anyhow::{bail, Context, Result};
use scraper::{Html, Selector};
use serde_json::{json, Map, Number, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug)]
pub(crate) struct CoursePage {
    pub courses: Vec<Value>,
    pub unpaged_courses: Vec<Value>,
    pub page_count: Option<usize>,
    pub page_size: Option<usize>,
    pub confirmed_empty: bool,
    pub headers: Vec<String>,
}

pub(crate) fn parse_catalog_html(body: &str, kind: PlanKind) -> Result<CurriculumCatalog> {
    let document = Html::parse_document(body);
    let select_selector = Selector::parse("select").expect("static selector");
    let option_selector = Selector::parse("option").expect("static selector");
    let mut grades = Vec::new();
    let mut colleges = Vec::new();
    for select in document.select(&select_selector) {
        let id = select.value().attr("id").unwrap_or("").to_ascii_lowercase();
        let name = select
            .value()
            .attr("name")
            .unwrap_or("")
            .to_ascii_lowercase();
        let key = format!("{id} {name}");
        let options = select
            .select(&option_selector)
            .filter_map(|option| {
                let code = option.value().attr("value")?.trim();
                let label = clean_text(option.text().collect::<String>());
                (!code.is_empty() && !label.is_empty()).then(|| CatalogOption {
                    code: code.to_string(),
                    name: label,
                })
            })
            .collect::<Vec<_>>();
        let is_grade = match kind {
            PlanKind::Curriculum => key.contains("bbh"),
            PlanKind::Execution => key.contains("nj") || key.contains("grade"),
        };
        if is_grade {
            grades.extend(options);
        } else if key.contains("yxdm") || key.contains("college") || key.contains("department") {
            colleges.extend(options);
        }
    }
    grades = checked_options(grades, "版本/年级")?;
    colleges = checked_options(colleges, "院系")?;
    if grades.is_empty() || colleges.is_empty() {
        bail!(
            "无法从教务系统页面识别{}和院系",
            match kind {
                PlanKind::Curriculum => "方案版本",
                PlanKind::Execution => "入学年级",
            }
        );
    }
    Ok(CurriculumCatalog { grades, colleges })
}

pub(crate) fn parse_major_response(value: &Value, allow_empty: bool) -> Result<Vec<CatalogOption>> {
    let mut options = Vec::new();
    if let (Some(codes), Some(names)) = (
        value.get("pageZydm").and_then(Value::as_array),
        value.get("pageZymc").and_then(Value::as_array),
    ) {
        if codes.len() != names.len() {
            bail!("专业代码与专业名称数量不一致");
        }
        for (code, name) in codes.iter().zip(names) {
            let code = scalar_text(code).context("专业代码结构无法识别")?;
            let name = scalar_text(name).context("专业名称结构无法识别")?;
            if code.is_empty() || name.is_empty() {
                bail!("专业列表包含缺少代码或名称的记录");
            }
            options.push(CatalogOption { code, name });
        }
    } else if let Some(rows) = find_array(value, &["list", "rows", "data", "result"]) {
        for row in rows {
            if let (Some(code), Some(name)) = (
                first_string(
                    row,
                    &["pageZydm", "zydm", "zjdm", "major_code", "value", "code"],
                ),
                first_string(
                    row,
                    &["pageZymc", "zymc", "zjmc", "major_name", "label", "name"],
                ),
            ) {
                options.push(CatalogOption { code, name });
            } else {
                bail!("专业列表包含缺少代码或名称的记录");
            }
        }
    } else {
        bail!("专业列表结构无法识别");
    }
    let options = checked_options(options, "专业")?;
    if options.is_empty() && !allow_empty {
        bail!("该院系没有返回专业列表");
    }
    Ok(options)
}

pub(crate) fn merge_major_labels(
    legacy: Vec<CatalogOption>,
    official: Vec<CatalogOption>,
) -> Result<Vec<CatalogOption>> {
    let mut labels = BTreeMap::<String, BTreeSet<String>>::new();
    for item in official {
        labels.entry(item.code).or_default().insert(item.name);
    }
    let official = labels
        .into_iter()
        .filter_map(|(code, names)| {
            (names.len() == 1).then(|| (code, names.into_iter().next().unwrap()))
        })
        .collect::<BTreeMap<_, _>>();
    Ok(legacy
        .into_iter()
        .map(|item| CatalogOption {
            name: official.get(&item.code).cloned().unwrap_or(item.name),
            code: item.code,
        })
        .collect())
}

fn checked_options(options: Vec<CatalogOption>, what: &str) -> Result<Vec<CatalogOption>> {
    let mut names_by_code = BTreeMap::<String, String>::new();
    let mut result = Vec::new();
    for option in options {
        if let Some(previous) = names_by_code.get(&option.code) {
            if previous != &option.name {
                bail!(
                    "{what}代码 {} 对应多个不同名称：{} / {}",
                    option.code,
                    previous,
                    option.name
                );
            }
            continue;
        }
        names_by_code.insert(option.code.clone(), option.name.clone());
        result.push(option);
    }
    Ok(result)
}

pub(crate) fn parse_course_html(
    body: &str,
    kind: PlanKind,
    source_section: &str,
    relation: Option<&Value>,
) -> Result<CoursePage> {
    let document = Html::parse_document(body);
    let table_selector = Selector::parse("table").expect("static selector");
    let row_selector = Selector::parse("tr").expect("static selector");
    let th_selector = Selector::parse("th").expect("static selector");
    let cell_selector = Selector::parse("td").expect("static selector");
    let mut chosen: Option<(Vec<String>, Vec<Vec<String>>)> = None;
    let mut explicit_empty = false;
    let mut unpaged_courses = Vec::new();

    for table in document.select(&table_selector) {
        let rows = table.select(&row_selector).collect::<Vec<_>>();
        let mut headers = Vec::new();
        let mut data_start = 0;
        for (index, row) in rows.iter().enumerate() {
            let candidate = row
                .select(&th_selector)
                .map(|cell| clean_text(cell.text().collect::<String>()))
                .collect::<Vec<_>>();
            if !candidate.is_empty() {
                headers = candidate;
                data_start = index + 1;
                break;
            }
            let candidate = row
                .select(&cell_selector)
                .map(|cell| clean_text(cell.text().collect::<String>()))
                .collect::<Vec<_>>();
            if candidate
                .iter()
                .any(|value| canonical_header(value).is_some())
            {
                headers = candidate;
                data_start = index + 1;
                break;
            }
        }
        if headers.is_empty() || !looks_like_course_headers(&headers, kind) {
            continue;
        }
        let mut parsed_rows = Vec::new();
        for row in rows.into_iter().skip(data_start) {
            let cells = row
                .select(&cell_selector)
                .map(|cell| clean_text(cell.text().collect::<String>()))
                .collect::<Vec<_>>();
            if cells.is_empty() {
                continue;
            }
            if cells.len() == 1 && is_empty_message(&cells[0]) {
                explicit_empty = true;
                continue;
            }
            if cells.len() != headers.len() {
                bail!("课程行列数与表头不一致，不能将截断数据作为完整计划");
            }
            if cells.iter().all(|cell| cell.is_empty()) {
                continue;
            }
            parsed_rows.push(cells);
        }
        if headers
            .iter()
            .any(|header| canonical_header(header) == Some("course_code"))
        {
            if chosen.is_some() {
                bail!("课程响应包含多个主课程表，无法确定分页范围");
            }
            chosen = Some((headers, parsed_rows));
        } else if kind == PlanKind::Curriculum {
            unpaged_courses.extend(parsed_rows.iter().map(|cells| {
                course_from_cells(&headers, cells, kind, "curriculum-requirements", None)
            }));
        }
    }

    let Some((headers, rows)) = chosen else {
        bail!("课程响应缺少预期表头，无法确认数据完整性");
    };
    let courses = rows
        .iter()
        .map(|cells| course_from_cells(&headers, cells, kind, source_section, relation))
        .collect::<Vec<_>>();
    let page_count = named_number(&document, &["pageCount", "page_count", "totalPage"]);
    let page_size = named_number(&document, &["pageSize", "page_size", "limit"]);
    let paging_controls = Selector::parse("input,select,form,[data-page-count],[data-page-size]")
        .expect("static selector");
    let has_paging_controls = document.select(&paging_controls).any(|element| {
        let attributes = element.value();
        attributes.attr("data-page-count").is_some()
            || attributes.attr("data-page-size").is_some()
            || [attributes.attr("name"), attributes.attr("id")]
                .into_iter()
                .flatten()
                .any(|name| {
                    ["pageNo", "pageCount", "page_count", "totalPage", "pageSize", "page_size", "limit"]
                        .iter()
                        .any(|expected| name.eq_ignore_ascii_case(expected))
                        || (attributes.name() == "form" && name.eq_ignore_ascii_case("page"))
                })
    });
    if has_paging_controls && page_count.is_none() {
        bail!("课程分页控件缺少有效总页数，无法确认抓取完整");
    }
    Ok(CoursePage {
        confirmed_empty: courses.is_empty() && (explicit_empty || rows.is_empty()),
        courses,
        unpaged_courses,
        page_count,
        page_size,
        headers,
    })
}

fn looks_like_course_headers(headers: &[String], kind: PlanKind) -> bool {
    let canonical = headers
        .iter()
        .filter_map(|header| canonical_header(header))
        .collect::<BTreeSet<_>>();
    if !canonical.contains("course_name") {
        return false;
    }
    match kind {
        PlanKind::Curriculum => canonical.contains("course_code") || canonical.contains("sequence"),
        PlanKind::Execution => canonical.contains("course_code"),
    }
}

fn course_from_cells(
    headers: &[String],
    cells: &[String],
    kind: PlanKind,
    source_section: &str,
    relation: Option<&Value>,
) -> Value {
    let mut result = Map::new();
    let mut unknown = Map::new();
    let mut unknown_counts = BTreeMap::<String, usize>::new();
    for (index, header) in headers.iter().enumerate() {
        let value = cells.get(index).cloned().unwrap_or_default();
        match canonical_header(header) {
            Some("sequence") => {}
            Some("course_code") => {
                result.insert("course_code".into(), Value::String(value));
            }
            Some("course_name") => insert_text(&mut result, "course_name", &value),
            Some("course_english_name") => insert_text(&mut result, "course_english_name", &value),
            Some("academic_year") => insert_text(&mut result, "academic_year", &value),
            Some("semester") => insert_text(&mut result, "semester", &value),
            Some("offering_department") => insert_text(&mut result, "offering_college", &value),
            Some("course_nature") => insert_text(&mut result, "course_nature", &value),
            Some("course_category") => insert_text(&mut result, "course_category", &value),
            Some("major_direction") => insert_text(&mut result, "major_direction", &value),
            Some("credit") => insert_number_or_text(&mut result, "credit", &value),
            Some("total_hours") => insert_number_or_text(&mut result, "total_hours", &value),
            Some("assessment_method") => insert_text(&mut result, "assessment_method", &value),
            Some("is_exam") => {
                if is_explicit_yes(&value) {
                    result.insert("assessment_method".into(), Value::String("考试".into()));
                }
                if !value.is_empty() {
                    result.insert("is_exam_course".into(), Value::String(value));
                }
            }
            _ => {
                if !header.is_empty() && !value.is_empty() {
                    let count = unknown_counts.entry(header.clone()).or_default();
                    *count += 1;
                    let key = if *count == 1 {
                        header.clone()
                    } else {
                        format!("{header}#{}", *count)
                    };
                    unknown.insert(key, Value::String(value));
                }
            }
        }
    }
    result
        .entry("course_code")
        .or_insert_with(|| Value::String(String::new()));
    result.insert(
        "source_section".into(),
        Value::String(source_section.into()),
    );
    if let Some(relation) = relation {
        result.insert("relation".into(), relation.clone());
    }
    if let (Some(year), Some(term)) = (
        result.get("academic_year").and_then(Value::as_str),
        result.get("semester").and_then(Value::as_str),
    ) {
        let year_label = match year {
            "1" => "第一学年",
            "2" => "第二学年",
            "3" => "第三学年",
            "4" => "第四学年",
            "5" => "第五学年",
            _ => year,
        };
        result.insert(
            "recommended_year_semester".into(),
            Value::String(format!("{year_label}{term}")),
        );
    }
    if !unknown.is_empty() {
        result.insert("source_fields".into(), Value::Object(unknown));
    }
    if kind == PlanKind::Execution
        && result.get("assessment_method") == Some(&Value::String(String::new()))
    {
        result.remove("assessment_method");
    }
    Value::Object(result)
}

fn canonical_header(header: &str) -> Option<&'static str> {
    let key = header
        .chars()
        .filter(|ch| !ch.is_whitespace() && !matches!(ch, '（' | '）' | '(' | ')' | ':' | '：'))
        .collect::<String>()
        .to_ascii_lowercase();
    match key.as_str() {
        "序号" | "序" => Some("sequence"),
        "课程代码" | "课程编号" | "课号" | "kcdm" => Some("course_code"),
        "课程名称" | "课程名" | "kcmc" => Some("course_name"),
        "课程英文名" | "英文名称" | "英文名" => Some("course_english_name"),
        "开课学年" | "学年" => Some("academic_year"),
        "开课学期" | "学期" => Some("semester"),
        "开课院系" | "开课单位" | "院系" => Some("offering_department"),
        "课程性质" | "性质" => Some("course_nature"),
        "课程类别" | "类别" => Some("course_category"),
        "专业方向" | "方向" => Some("major_direction"),
        "学分" => Some("credit"),
        "总学时" | "学时" => Some("total_hours"),
        "考核方式" | "考核形式" => Some("assessment_method"),
        "是否考试课" | "考试课" => Some("is_exam"),
        _ => None,
    }
}

pub(crate) fn course_fingerprint(courses: &[Value]) -> String {
    serde_json::to_string(courses).unwrap_or_default()
}

pub(crate) fn parse_module_tree(body: &str) -> Result<Vec<Value>> {
    let value: Value = serde_json::from_str(body).context("模块树不是有效 JSON")?;
    let rows = value.as_array().context("模块树不是节点数组")?;
    let mut nodes = Vec::with_capacity(rows.len());
    let mut identities = BTreeSet::new();
    for row in rows {
        let id = first_string(row, &["id"]).context("模块节点缺少代码")?;
        let direction = first_string(row, &["idd"]).context("模块节点缺少方向代码")?;
        let name = first_string(row, &["name"]).context("模块节点缺少名称")?;
        let parent = row.get("pId").and_then(scalar_text);
        if !identities.insert((id.clone(), direction.clone())) {
            bail!("模块节点身份重复");
        }
        let mut node = scrub_json(row);
        if !node.is_object() {
            bail!("模块节点格式错误");
        }
        node["module_id"] = if parent
            .as_deref()
            .is_some_and(|p| p != "-2" && p != "-1" && !p.is_empty())
        {
            json!(id)
        } else {
            Value::Null
        };
        node["direction_key"] = json!(direction);
        node["parent_id"] = json!(parent);
        node["name"] = json!(name);
        nodes.push(node);
    }
    let placeholder_directions = nodes
        .iter()
        .filter(|node| node["module_id"].is_null() && node["id"].as_str() == Some("0"))
        .filter_map(|node| node["direction_key"].as_str().map(str::to_string))
        .collect::<BTreeSet<_>>();
    for node in &mut nodes {
        if node["module_id"].is_null() {
            continue;
        }
        let parent = node["parent_id"].as_str().context("模块缺少父节点")?;
        let direction = node["direction_key"].as_str().unwrap_or("");
        if !identities.contains(&(parent.to_string(), direction.to_string()))
            && parent == direction
            && placeholder_directions.contains(direction)
        {
            node["parent_id"] = json!("0");
        }
    }
    for node in &nodes {
        if node["module_id"].is_null() {
            continue;
        }
        let parent = node["parent_id"].as_str().context("模块缺少父节点")?;
        let direction = node["direction_key"].as_str().unwrap_or("");
        if !identities.contains(&(parent.to_string(), direction.to_string())) {
            bail!("模块树引用缺失的父节点");
        }
        let mut seen = BTreeSet::new();
        let mut cursor = node;
        while !cursor["module_id"].is_null() {
            let id = cursor["module_id"].as_str().unwrap_or("");
            if !seen.insert(id) {
                bail!("模块树存在循环");
            }
            let parent = cursor["parent_id"].as_str().unwrap_or("");
            cursor = nodes
                .iter()
                .find(|n| {
                    n["id"].as_str() == Some(parent)
                        && n["direction_key"].as_str() == Some(direction)
                })
                .context("模块树父节点缺失")?;
        }
    }
    Ok(nodes)
}

pub(crate) fn module_requests(nodes: &[Value]) -> Vec<(String, String)> {
    let mut seen = BTreeSet::new();
    let mut result = Vec::new();
    for node in nodes {
        let Some(module_id) = node.get("module_id").and_then(Value::as_str) else {
            continue;
        };
        if module_id.is_empty() {
            continue;
        }
        let direction = node
            .get("direction_key")
            .and_then(Value::as_str)
            .unwrap_or("");
        if seen.insert((module_id.to_string(), direction.to_string())) {
            result.push((module_id.to_string(), direction.to_string()));
        }
    }
    result
}

pub(crate) fn parse_direction_entries(body: &str) -> Result<Vec<Value>> {
    let document = Html::parse_document(body);
    let selector = Selector::parse("[onclick]").expect("static selector");
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    for element in document.select(&selector) {
        let onclick = element.value().attr("onclick").unwrap_or("");
        let Some(idstr) = extract_call_args(onclick, "queryZxjhyq").last().cloned() else {
            continue;
        };
        if idstr.is_empty() || !seen.insert(idstr.clone()) {
            continue;
        }
        result
            .push(json!({"idstr": idstr, "name": clean_text(element.text().collect::<String>())}));
    }
    let lower = body.to_lowercase();
    let has_direction_table = document
        .select(&Selector::parse("th").unwrap())
        .any(|h| clean_text(h.text().collect()).contains("专业方向"));
    if result.is_empty()
        && !has_direction_table
        && !lower.contains("暂无")
        && !lower.contains("无方向")
        && !lower.contains("empty")
    {
        bail!("执行计划方向要求入口结构无法识别");
    }
    Ok(result)
}

pub(crate) fn parse_metadata_html(body: &str) -> Value {
    let document = Html::parse_document(body);
    let table_selector = Selector::parse("table").expect("static selector");
    let row_selector = Selector::parse("tr").expect("static selector");
    let cell_selector = Selector::parse("th,td").expect("static selector");
    let field_selector =
        Selector::parse("input[name],select[name],textarea[name]").expect("static selector");
    let text_selector = Selector::parse("pre, .panel-body, .form-control-static, #bz, .bz")
        .expect("static selector");
    let mut tables = Vec::new();
    for table in document.select(&table_selector) {
        if table.select(&field_selector).next().is_some()
            || table.select(&table_selector).next().is_some()
            || table
                .select(&Selector::parse("th").unwrap())
                .next()
                .is_none()
        {
            continue;
        }
        let rows = table
            .select(&row_selector)
            .filter_map(|row| {
                let cells = row
                    .select(&cell_selector)
                    .map(|cell| clean_text(cell.text().collect::<String>()))
                    .collect::<Vec<_>>();
                (!cells.is_empty() && cells.iter().any(|cell| !cell.is_empty()))
                    .then_some(Value::Array(cells.into_iter().map(Value::String).collect()))
            })
            .collect::<Vec<_>>();
        if !rows.is_empty() {
            tables.push(Value::Array(rows));
        }
    }
    let mut fields = Map::new();
    for field in document.select(&field_selector) {
        let Some(name) = field.value().attr("name") else {
            continue;
        };
        if is_sensitive_key(name) {
            continue;
        }
        if field.value().name() == "select" || name.starts_with("page") || name == "path_id" {
            continue;
        }
        let value = field
            .value()
            .attr("value")
            .map(str::to_string)
            .unwrap_or_else(|| clean_text(field.text().collect::<String>()));
        if !value.is_empty() {
            fields.insert(name.to_string(), Value::String(value));
        }
    }
    let texts = document
        .select(&text_selector)
        .map(|node| clean_text(node.text().collect::<String>()))
        .filter(|text| !text.is_empty())
        .map(Value::String)
        .collect::<Vec<_>>();
    json!({"tables": tables, "fields": fields, "text": texts})
}

pub(crate) fn scrub_json(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .filter_map(|(key, value)| {
                    (!is_sensitive_key(key)).then(|| (key.clone(), scrub_json(value)))
                })
                .collect(),
        ),
        Value::Array(array) => Value::Array(array.iter().map(scrub_json).collect()),
        _ => value.clone(),
    }
}

pub(crate) fn split_major_name(full: &str) -> (String, Option<String>) {
    let full = full.trim();
    if full.ends_with('】') {
        if let Some(open) = full.rfind('【') {
            let name = full[..open].trim();
            let kind = full[open + '【'.len_utf8()..full.len() - '】'.len_utf8()].trim();
            if !name.is_empty() && !kind.is_empty() {
                return (name.to_string(), Some(kind.to_string()));
            }
        }
    }
    (full.to_string(), None)
}

fn named_number(document: &Html, names: &[&str]) -> Option<usize> {
    let selector = Selector::parse("input,select,[data-page-count],[data-page-size]")
        .expect("static selector");
    for element in document.select(&selector) {
        let identity = element
            .value()
            .attr("name")
            .or_else(|| element.value().attr("id"))
            .unwrap_or("");
        for name in names {
            let attr_value = if identity.eq_ignore_ascii_case(name) {
                element.value().attr("value")
            } else if *name == "pageCount" {
                element.value().attr("data-page-count")
            } else if *name == "pageSize" {
                element.value().attr("data-page-size")
            } else {
                None
            };
            if let Some(number) = attr_value.and_then(|value| value.trim().parse::<usize>().ok()) {
                return Some(number);
            }
        }
    }
    None
}

fn extract_call_args(script: &str, function: &str) -> Vec<String> {
    let Some(start) = script.find(function) else {
        return Vec::new();
    };
    let Some(open_rel) = script[start + function.len()..].find('(') else {
        return Vec::new();
    };
    let rest = &script[start + function.len() + open_rel + 1..];
    let Some(close) = rest.find(')') else {
        return Vec::new();
    };
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    for ch in rest[..close].chars() {
        match quote {
            Some(marker) if ch == marker => {
                if !current.trim().is_empty() {
                    args.push(current.trim().to_string());
                }
                current.clear();
                quote = None;
            }
            Some(_) => current.push(ch),
            None if ch == '\'' || ch == '"' => quote = Some(ch),
            None => {}
        }
    }
    args.into_iter()
        .filter(|arg| !arg.eq_ignore_ascii_case("this"))
        .collect()
}

fn insert_text(result: &mut Map<String, Value>, key: &str, value: &str) {
    if !value.is_empty() {
        result.insert(key.into(), Value::String(value.into()));
    }
}

fn insert_number_or_text(result: &mut Map<String, Value>, key: &str, value: &str) {
    if value.is_empty() {
        return;
    }
    if let Ok(number) = value.parse::<i64>() {
        result.insert(key.into(), Value::Number(number.into()));
    } else if let Ok(number) = value.parse::<f64>() {
        if let Some(number) = Number::from_f64(number) {
            result.insert(key.into(), Value::Number(number));
        }
    } else {
        result.insert(key.into(), Value::String(value.into()));
    }
}

fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(scalar_text))
        .filter(|value| !value.is_empty())
}

fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.trim().to_string()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn find_array<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Vec<Value>> {
    if let Some(array) = value.as_array() {
        return Some(array);
    }
    for key in keys {
        if let Some(array) = value.get(*key).and_then(Value::as_array) {
            return Some(array);
        }
        if let Some(nested) = value.get(*key) {
            if let Some(array) = find_array(nested, keys) {
                return Some(array);
            }
        }
    }
    None
}

fn clean_text(text: String) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_empty_message(value: &str) -> bool {
    let value = value.trim().to_lowercase();
    value.is_empty()
        || value.contains("暂无")
        || value.contains("没有数据")
        || value.contains("无数据")
        || value.contains("empty")
}

fn is_explicit_yes(value: &str) -> bool {
    matches!(
        value.trim().to_lowercase().as_str(),
        "是" | "yes" | "y" | "1" | "考试"
    )
}

fn is_sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("ticket")
        || key.contains("cookie")
        || key.contains("password")
        || key == "username"
        || key == "userid"
        || key == "user_id"
        || key == "xh"
}
