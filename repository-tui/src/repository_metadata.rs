//! 课程仓库展示投影；编码与 scripts/repository_description.py 一致。
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt::Write;

use anyhow::{bail, Result};

const LIMIT: usize = 350;
const UNKNOWN: &str = "教务未提供名称";
const RESERVED: [char; 5] = ['｜', '{', '}', ';', '='];
const GUIDANCE: &str = concat!(
    "# 薪火笔记社课程资料物理仓\n\n",
    "本仓库由 [HIT-Fireworks 课程注册表 v2]",
    "(https://github.com/HIT-Fireworks/fireworks-course-registry-v2) 统一维护身份与索引。\n\n",
    "## 维护约定\n\n",
    "- GitHub description 按稳定顺序展示尽可能多的完整代码→原始课程名条目。\n",
    "- 超过 350 字时以“尚余 N 项”明确标记截断；不得在代码或课程名中间硬截断。\n",
    "- 完整课程映射始终以本 README 与 Registry 为准。\n",
    "- 文件库存、容量和内容分类属于 Registry 或审计索引，不写入 description。\n",
    "- 普通内容增删不应触发 description 更新；课程代码成员或原始课程名变化时才更新。\n",
    "- 仓库是唯一资源边界；需要独立资料边界时应拆分仓库，不设置资源组中间层。\n",
    "- 资料按预设中文分类存放：教材、笔记、课件、试卷、作业、实验、软件、教程、模板、项目、其他；只创建实际需要的分类。\n",
    "- 不得自行新增根级分类目录；新增分类须先统一调整管理规则。根目录仅保留 README、LICENSE 和仓库配置。\n",
    "- 分类只区分资料用途，不是资源组；软件包、代码项目与多文件文档放入对应分类并保留完整内部结构。\n",
    "- 不创建资源组或空目录占位文件；课程共仓不代表自动共享全部文件。\n",
    "- 资料归属必须遵守完整课程代码、文件路由与物理仓库契约。\n\n",
);

struct Course<'a> {
    code: &'a str,
    prefix: &'a str,
    suffix: &'a str,
    name: &'a str,
}

// ASCII 标识符的 re.split(r"(\d+)", code) 自然键；数值段不受整数溢出影响。
fn natural_cmp(left: &str, right: &str) -> Ordering {
    fn key_cmp(mut left: &[u8], mut right: &[u8]) -> Ordering {
        loop {
            let lt = left.iter().position(u8::is_ascii_digit).unwrap_or(left.len());
            let rt = right.iter().position(u8::is_ascii_digit).unwrap_or(right.len());
            let order = left[..lt].iter().map(u8::to_ascii_lowercase)
                .cmp(right[..rt].iter().map(u8::to_ascii_lowercase));
            if order != Ordering::Equal {
                return order;
            }
            left = &left[lt..];
            right = &right[rt..];
            if left.is_empty() || right.is_empty() {
                return left.len().cmp(&right.len());
            }
            let ld = left.iter().position(|c| !c.is_ascii_digit()).unwrap_or(left.len());
            let rd = right.iter().position(|c| !c.is_ascii_digit()).unwrap_or(right.len());
            let ln = &left[..ld];
            let rn = &right[..rd];
            let ln = &ln[ln.iter().position(|c| *c != b'0').unwrap_or(ln.len())..];
            let rn = &rn[rn.iter().position(|c| *c != b'0').unwrap_or(rn.len())..];
            let order = ln.len().cmp(&rn.len()).then_with(|| ln.cmp(rn));
            if order != Ordering::Equal {
                return order;
            }
            left = &left[ld..];
            right = &right[rd..];
        }
    }
    key_cmp(left.as_bytes(), right.as_bytes()).then_with(|| left.as_bytes().cmp(right.as_bytes()))
}

fn split_code(code: &str) -> Result<(&str, &str)> {
    // 保留分隔符不可逆；反引号、竖线及控制字符会破坏 Markdown 单元格。
    if code.is_empty() || code.chars().any(|c| RESERVED.contains(&c) || c == '`' || c == '|' || c.is_control()) {
        bail!("非法课程代码：{code:?}");
    }
    let bytes = code.as_bytes();
    let mut end = bytes.iter().take_while(|c| c.is_ascii_digit()).count();
    let letters_start = end;
    while end < bytes.len() && bytes[end].is_ascii_alphabetic() {
        end += 1;
    }
    if end == bytes.len() && end > letters_start + 1 {
        // Python [A-Za-z]+ 回退一个字母，为 .+ 保留非空后缀。
        end -= 1;
    }
    if end == letters_start || end == bytes.len() {
        bail!("课程代码不符合旧映射拆分规则 [0-9]*[A-Za-z]+ 加非空后缀：{code:?}");
    }
    Ok((&code[..end], &code[end..]))
}

fn courses(mapping: &BTreeMap<String, String>) -> Result<Vec<Course<'_>>> {
    let mut normalized: BTreeMap<&str, Course<'_>> = BTreeMap::new();
    for (raw_code, raw_name) in mapping {
        let code = raw_code.trim();
        let name = raw_name.trim();
        let (prefix, suffix) = split_code(code)?;
        if let Some(previous) = normalized.get(code) {
            if previous.name != name {
                bail!("同一课程代码对应多个原始名称：{code}");
            }
            continue;
        }
        normalized.insert(code, Course { code, prefix, suffix, name });
    }
    let mut entries: Vec<_> = normalized.into_values().collect();
    entries.sort_by(|left, right| natural_cmp(left.code, right.code));
    Ok(entries)
}

fn displayed_name<'a>(course: &Course<'a>) -> &'a str {
    if course.name.is_empty() { UNKNOWN } else { course.name }
}

fn encode(entries: &[Course<'_>]) -> String {
    let mut groups: BTreeMap<&str, Vec<&Course<'_>>> = BTreeMap::new();
    for entry in entries {
        groups.entry(entry.prefix).or_default().push(entry);
    }
    let mut groups: Vec<_> = groups.into_iter().collect();
    groups.sort_by(|(left, _), (right, _)| natural_cmp(left, right));
    let mut encoded = String::new();
    for (group_index, (prefix, members)) in groups.into_iter().enumerate() {
        if group_index != 0 {
            encoded.push(';');
        }
        encoded.push_str(prefix);
        encoded.push('{');
        for (entry_index, entry) in members.into_iter().enumerate() {
            if entry_index != 0 {
                encoded.push(';');
            }
            encoded.push_str(entry.suffix);
            encoded.push('=');
            encoded.push_str(displayed_name(entry));
        }
        encoded.push('}');
    }
    encoded
}

/// 生成最多 350 个 Unicode 标量的完整条目投影；一项都放不下时返回错误。
pub fn description(title: &str, mapping: &BTreeMap<String, String>) -> Result<String> {
    let title = title.trim();
    let title = title.strip_suffix(" / 无资料课程").map(str::trim_end).unwrap_or(title);
    if title.is_empty() || title.contains('｜') {
        bail!("课程仓缺少合法稳定上位语义");
    }
    let title_length = title.chars().count();
    let entries = courses(mapping)?;
    if entries.is_empty() {
        if title_length > LIMIT {
            bail!("仓库标题超过 350 字限制");
        }
        return Ok(title.to_owned());
    }
    let mut groups = BTreeMap::new();
    let mut encoded_length = 0;
    let mut best = 0;
    for (index, entry) in entries.iter().enumerate() {
        if entry.name.chars().any(|c| RESERVED.contains(&c)) {
            bail!("原始课程名含保留分隔符：{}={:?}", entry.code, entry.name);
        }
        let group_count = groups.len();
        if groups.insert(entry.prefix, ()).is_none() {
            encoded_length += entry.prefix.chars().count() + 2 + usize::from(group_count != 0);
        } else {
            encoded_length += 1;
        }
        encoded_length += entry.suffix.chars().count() + 1 + displayed_name(entry).chars().count();
        let count = index + 1;
        let remaining = entries.len() - count;
        // 累计精确长度，不重建前缀；检查全部候选，避免余量位数变化漏掉可行项。
        let marker_length = if remaining == 0 { 0 } else { 6 + remaining.ilog10() as usize };
        if title_length + 1 + encoded_length + marker_length <= LIMIT {
            best = count;
        }
    }
    if best == 0 {
        bail!("350 字限制内无法容纳任一完整课程映射条目");
    }
    let mut result = format!("{title}｜{}", encode(&entries[..best]));
    if best < entries.len() {
        write!(result, "｜…尚余{}项", entries.len() - best)?;
    }
    Ok(result)
}

/// 返回维护约定与全部映射，不读取库存，不修改输入；调用方负责安全合并 README。
pub fn readme(mapping: &BTreeMap<String, String>) -> Result<String> {
    let entries = courses(mapping)?;
    let mut result = GUIDANCE.to_owned();
    if !entries.is_empty() {
        result.push_str("## 课程代码与原始课程名\n\n| 课程代码 | 原始课程名 |\n|---|---|\n");
        for entry in &entries {
            let name = displayed_name(entry)
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('\\', "&#92;")
                .replace('|', "\\|")
                .replace("\r\n", "<br>")
                .replace(['\r', '\n'], "<br>");
            writeln!(result, "| `{}` | {name} |", entry.code)?;
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping(items: &[(&str, &str)]) -> BTreeMap<String, String> {
        items.iter().map(|(code, name)| (code.to_string(), name.to_string())).collect()
    }

    fn decoded(description: &str) -> BTreeMap<String, String> {
        let encoded = description.split('｜').nth(1).unwrap();
        let mut result = BTreeMap::new();
        for group in encoded.split("};") {
            let group = group.strip_suffix('}').unwrap_or(group);
            let (prefix, body) = group.split_once('{').unwrap();
            for entry in body.split(';') {
                let (suffix, name) = entry.split_once('=').unwrap();
                assert!(!suffix.is_empty());
                assert!(result.insert(format!("{prefix}{suffix}"), name.to_owned()).is_none());
            }
        }
        result
    }

    #[test]
    fn short_mapping_matches_python_encoding_exactly() {
        let input = mapping(&[("22CS14002", " 算法设计 "), ("13SE11206400", "软件工程"), ("22CS14001", "程序设计")]);
        assert_eq!(description(" 计算机课程 / 无资料课程 ", &input).unwrap(), "计算机课程｜13SE{11206400=软件工程};22CS{14001=程序设计;14002=算法设计}");
    }

    #[test]
    fn natural_order_handles_mixed_prefixes_large_numbers_and_ties() {
        let input = mapping(&[("A10", "十"), ("A2", "二"), ("CS32123", "计算机"), ("22CS14001", "程序"), ("A02", "零二"), ("AD14001", "设计"), ("ELEC21041", "电子")]);
        assert_eq!(description("课程", &input).unwrap(), "课程｜22CS{14001=程序};A{02=零二;2=二;10=十};AD{14001=设计};CS{32123=计算机};ELEC{21041=电子}");
        assert_eq!(natural_cmp("A999999999999999999999999999999", "A1000000000000000000000000000000"), Ordering::Less);
        assert_eq!(natural_cmp("a2", "A2"), Ordering::Greater);
        assert_eq!(split_code("ABC").unwrap(), ("AB", "C"));
    }

    #[test]
    fn duplicate_names_keep_distinct_complete_codes() {
        let input = mapping(&[("A1", "同名课程"), ("B1", "同名课程")]);
        let result = description("课程", &input).unwrap();
        assert_eq!(result, "课程｜A{1=同名课程};B{1=同名课程}");
        assert_eq!(decoded(&result), input);
    }

    #[test]
    fn truncation_is_reversible_maximal_and_reports_exact_remaining() {
        let input: BTreeMap<_, _> = (1..=100).map(|index| (format!("A{index}"), "完整课程😀名称".repeat(3))).collect();
        let result = description("课程", &input).unwrap();
        assert!(result.chars().count() <= LIMIT);
        let shown = decoded(&result);
        assert_eq!(result.split('｜').last().unwrap(), format!("…尚余{}项", input.len() - shown.len()));
        for (code, name) in &shown {
            assert_eq!(input.get(code), Some(name));
        }
        let all = courses(&input).unwrap();
        let expected: BTreeMap<_, _> = all[..shown.len()].iter().map(|entry| (entry.code.to_owned(), entry.name.to_owned())).collect();
        assert_eq!(shown, expected);
        for count in shown.len() + 1..=all.len() {
            let mut candidate = format!("课程｜{}", encode(&all[..count]));
            if count < all.len() {
                write!(candidate, "｜…尚余{}项", all.len() - count).unwrap();
            }
            assert!(candidate.chars().count() > LIMIT);
        }
    }

    #[test]
    fn unicode_limit_includes_exact_350_scalar_boundary() {
        let input = mapping(&[("A1", "😀")]);
        let title = "中".repeat(343);
        let result = description(&title, &input).unwrap();
        assert_eq!(result.chars().count(), 350);
        assert_eq!(result, format!("{title}｜A{{1=😀}}"));
        assert!(description(&"中".repeat(344), &input).is_err());
    }

    #[test]
    fn unknown_names_are_display_only_and_empty_mapping_has_clean_title() {
        let input = mapping(&[("A1", " \t ")]);
        assert_eq!(description("课程", &input).unwrap(), "课程｜A{1=教务未提供名称}");
        assert!(readme(&input).unwrap().contains("| `A1` | 教务未提供名称 |\n"));
        assert_eq!(input["A1"], " \t ");
        assert_eq!(description(" 课程 / 无资料课程 ", &BTreeMap::new()).unwrap(), "课程");
        assert_eq!(description(" 课程 / 开课单位未标注课程 ", &BTreeMap::new()).unwrap(), "课程 / 开课单位未标注课程");
        assert_eq!(readme(&BTreeMap::new()).unwrap(), GUIDANCE);
    }

    #[test]
    fn invalid_codes_reserved_names_and_ambiguous_titles_are_rejected() {
        for code in ["", "123", "A", "_A1", "A`1", "A|1", "A;1", "A\n1"] {
            let input = mapping(&[(code, "名称")]);
            assert!(description("课程", &input).is_err(), "{code:?}");
            assert!(readme(&input).is_err(), "{code:?}");
        }
        for separator in RESERVED {
            assert!(description("课程", &mapping(&[("A1", &format!("名{separator}称"))])).is_err());
        }
        for title in ["", "  ", "课程｜映射"] {
            assert!(description(title, &BTreeMap::new()).is_err(), "{title:?}");
        }
        assert!(description(&"中".repeat(351), &BTreeMap::new()).is_err());
        assert!(description("课程", &mapping(&[("A1", "一"), (" A1 ", "二")])).is_err());
        let mut input = mapping(&[("A1", &"长".repeat(400))]);
        input.insert("B1".into(), "尾部;非法".into());
        assert!(description("课程", &input).unwrap_err().to_string().contains("B1"));
    }

    #[test]
    fn readme_keeps_every_mapping_and_protects_table_structure() {
        let mut input: BTreeMap<_, _> = (1..=120).map(|index| (format!("A{index}"), format!("原名{index}"))).collect();
        input.insert("B1".to_owned(), "  甲|乙\r\n丙\r丁\n戊  ".to_owned());
        let result = readme(&input).unwrap();
        assert!(result.starts_with(GUIDANCE));
        let rows: Vec<_> = result.lines().filter(|line| line.starts_with("| `")).collect();
        let mut expected: Vec<_> = (1..=120).map(|index| format!("| `A{index}` | 原名{index} |")).collect();
        expected.push("| `B1` | 甲\\|乙<br>丙<br>丁<br>戊 |".to_owned());
        assert_eq!(rows, expected);
        assert_eq!(rows.len(), input.len());
    }

    #[test]
    fn oversized_name_fails_description_but_readme_preserves_it() {
        let name = "非常长的原始课程名称😀".repeat(50);
        let input = mapping(&[("A1", &name)]);
        assert!(description("课程", &input).is_err());
        assert_eq!(readme(&input).unwrap(), format!("{GUIDANCE}## 课程代码与原始课程名\n\n| 课程代码 | 原始课程名 |\n|---|---|\n| `A1` | {name} |\n"));
    }
}
