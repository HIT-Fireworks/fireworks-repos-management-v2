use anyhow::{bail, ensure, Context, Result};
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{json, Map, Number, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fmt;
use std::fs::{self, File, Metadata};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

const MARKER: &str = "$fireworks_shards";
const INLINE_LIMIT: usize = 8 * 1024 * 1024;
const PART_LIMIT: usize = 4 * 1024 * 1024;
const MAX_DEPTH: usize = 128;

/// 读取旧普通 JSON 或内容寻址分片，返回完整逻辑值。
pub fn read(path: &Path) -> Result<Value> {
    let store = Store::for_path(path, false)?;
    reject_link(path, false).context("根 JSON 不是安全的常规文件")?;
    let file = File::open(path).context("无法打开根 JSON")?;
    let root_bytes = file.metadata().context("无法检查根 JSON 长度")?.len();
    let mut value = parse(BufReader::new(file)).context("根 JSON 解析失败")?;
    let mut stack = HashSet::new();
    let mut found_shards = false;
    decode(&store, &mut value, 0, &mut stack, &mut found_shards)?;
    ensure!(
        !found_shards || root_bytes <= INLINE_LIMIT as u64,
        "分片根 JSON 超过 8MiB 限制"
    );
    Ok(value)
}

/// 逐分片计算完整逻辑 JSON 的规范哈希，不再展开整份预览副本。
pub fn canonical_sha256(path: &Path) -> Result<String> {
    let store = Store::for_path(path, false)?;
    reject_link(path, false)?;
    let file = File::open(path)?;
    let root_bytes = file.metadata()?.len();
    let value = parse(BufReader::new(file))?;
    let mut writer = CanonicalHashWriter(Sha256::new());
    let mut stack = HashSet::new();
    let mut found = false;
    write_canonical(&store, value, 0, &mut stack, &mut found, &mut writer)?;
    ensure!(!found || root_bytes <= INLINE_LIMIT as u64, "分片根 JSON 超过 8MiB 限制");
    Ok(format!("{:x}", writer.0.finalize()))
}

struct CanonicalHashWriter(Sha256);

impl Write for CanonicalHashWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}

fn write_canonical(
    store: &Store, value: Value, depth: usize, stack: &mut HashSet<String>,
    found: &mut bool, writer: &mut impl Write,
) -> Result<()> {
    ensure!(depth < MAX_DEPTH, "JSON 嵌套深度超过安全限制");
    if value.as_object().is_some_and(|object| object.contains_key(MARKER)) {
        *found = true;
        return write_canonical_envelope(store, value, depth, stack, found, writer);
    }
    match value {
        Value::Array(values) => {
            writer.write_all(b"[")?;
            for (index, value) in values.into_iter().enumerate() {
                if index != 0 { writer.write_all(b",")?; }
                write_canonical(store, value, depth + 1, stack, found, writer)?;
            }
            writer.write_all(b"]")?;
        }
        Value::Object(values) => {
            writer.write_all(b"{")?;
            for (index, (key, value)) in values.into_iter().enumerate() {
                if index != 0 { writer.write_all(b",")?; }
                serde_json::to_writer(&mut *writer, &key)?;
                writer.write_all(b":")?;
                write_canonical(store, value, depth + 1, stack, found, writer)?;
            }
            writer.write_all(b"}")?;
        }
        value => serde_json::to_writer(writer, &value)?,
    }
    Ok(())
}

fn write_canonical_envelope(
    store: &Store, envelope: Value, depth: usize, stack: &mut HashSet<String>,
    found: &mut bool, writer: &mut impl Write,
) -> Result<()> {
    let object = envelope.as_object().context("分片引用必须是对象")?;
    ensure!(object.len() == 3 && object.get(MARKER).and_then(Value::as_u64) == Some(1), "无效的分片版本或引用字段");
    let kind = object.get("kind").and_then(Value::as_str).context("分片 kind 必须是字符串")?;
    ensure!(kind == "array" || kind == "object", "无效的分片 kind");
    let parts = object.get("parts").and_then(Value::as_array).context("分片 parts 必须是数组")?;
    ensure!(!parts.is_empty(), "分片 parts 不能为空");
    let mut fields = std::collections::BTreeMap::new();
    let mut first = true;
    if kind == "array" { writer.write_all(b"[")?; }
    for part in parts {
        let reference = part.as_object().context("分片条目必须是对象")?;
        ensure!(reference.len() == 2 && reference.contains_key("sha256") && reference.contains_key("bytes"), "分片条目只能包含 sha256 和 bytes，禁止 path 等字段");
        let digest = reference.get("sha256").and_then(Value::as_str).context("分片 sha256 必须是字符串")?;
        validate_digest(digest)?;
        let expected = reference.get("bytes").and_then(Value::as_u64).context("分片 bytes 必须是非负整数")?;
        ensure!(stack.len() < MAX_DEPTH, "分片引用深度超过安全限制");
        ensure!(stack.insert(digest.to_owned()), "检测到循环分片引用 {digest}");
        let result = (|| -> Result<()> {
            let bytes = store.part_bytes(digest, expected)?;
            let value = parse(bytes.as_slice()).with_context(|| format!("分片 {digest} JSON 无效"))?;
            ensure!(!value.as_object().is_some_and(|map| map.contains_key(MARKER)), "分片 {digest} 顶层必须是原始容器，而不是另一个引用");
            match (kind, value) {
                ("array", Value::Array(values)) => {
                    for value in values {
                        if !first { writer.write_all(b",")?; }
                        first = false;
                        write_canonical(store, value, depth + 1, stack, found, writer)?;
                    }
                }
                ("object", Value::Object(values)) => {
                    for (key, value) in values {
                        ensure!(!fields.contains_key(&key), "对象分片 {digest} 出现重复键");
                        // 仅保留未展开成员及其来源；排序不展开成员引用的子树。
                        fields.insert(key, (value, digest.to_owned()));
                    }
                }
                _ => bail!("分片 {digest} 容器类型与 kind 不符"),
            }
            Ok(())
        })();
        stack.remove(digest);
        result?;
    }
    if kind == "array" {
        writer.write_all(b"]")?;
    } else {
        writer.write_all(b"{")?;
        for (index, (key, (value, digest))) in fields.into_iter().enumerate() {
            if index != 0 { writer.write_all(b",")?; }
            serde_json::to_writer(&mut *writer, &key)?;
            writer.write_all(b":")?;
            ensure!(stack.insert(digest.clone()), "检测到循环分片引用 {digest}");
            let result = write_canonical(store, value, depth + 1, stack, found, writer);
            stack.remove(&digest);
            result?;
        }
        writer.write_all(b"}")?;
    }
    Ok(())
}

/// 先持久化不可变分片，再返回同盘、已同步且尚未替换根文件的临时文件。
pub fn stage(path: &Path, value: &Value) -> Result<NamedTempFile> {
    stage_with_limits(path, value, INLINE_LIMIT, PART_LIMIT)
}

/// 单根原子替换；多根事务由调用方使用 stage 统一协调。
pub fn write(path: &Path, value: &Value) -> Result<()> {
    stage(path, value)?
        .persist(path)
        .map_err(|error| error.error)
        .context("无法原子替换根 JSON")?;
    Ok(())
}

fn stage_with_limits(
    path: &Path,
    value: &Value,
    inline_limit: usize,
    part_limit: usize,
) -> Result<NamedTempFile> {
    ensure!(
        part_limit >= 2 && part_limit <= inline_limit && inline_limit <= INLINE_LIMIT,
        "无效的 JSON 分片大小限制"
    );
    let store = Store::for_path(path, true)?;
    let bytes = encode(&store, value, 0, inline_limit, part_limit)?;
    ensure!(bytes.len() <= inline_limit, "编码后的根 JSON 超过内联限制");
    let mut staged = NamedTempFile::new_in(&store.parent).context("无法创建根 JSON 临时文件")?;
    staged.write_all(&bytes).context("无法写入根 JSON 临时文件")?;
    staged.flush().context("无法刷新根 JSON 临时文件")?;
    staged.as_file().sync_all().context("无法同步根 JSON 临时文件")?;
    Ok(staged)
}

struct Store {
    parent: PathBuf,
}

impl Store {
    fn for_path(path: &Path, create: bool) -> Result<Self> {
        ensure!(path.file_name().is_some(), "根 JSON 路径缺少文件名");
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        if create {
            fs::create_dir_all(parent).context("无法创建根 JSON 目录")?;
        }
        let parent = fs::canonicalize(parent).context("无法定位根 JSON 目录")?;
        ensure!(parent.is_dir(), "根 JSON 的父路径不是目录");
        Ok(Self { parent })
    }

    fn directory(&self, create: bool) -> Result<PathBuf> {
        let directory = self.parent.join(".fireworks-json");
        match fs::symlink_metadata(&directory) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
                match fs::create_dir(&directory) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error).context("无法创建 .fireworks-json"),
                }
            }
            Err(error) => return Err(error).context("无法检查 .fireworks-json"),
        }
        reject_link(&directory, true).context(".fireworks-json 不是安全目录")?;
        let canonical = fs::canonicalize(&directory).context("无法定位 .fireworks-json")?;
        ensure!(canonical == directory, ".fireworks-json 目录逃逸");
        Ok(directory)
    }

    fn part_bytes(&self, digest: &str, expected: u64) -> Result<Vec<u8>> {
        validate_digest(digest)?;
        ensure!(expected > 0 && expected <= INLINE_LIMIT as u64, "分片 {digest} 的 bytes 超出范围");
        let directory = self.directory(false)?;
        let path = directory.join(format!("{digest}.json"));
        reject_link(&path, false).with_context(|| format!("分片 {digest} 不是安全的常规文件"))?;
        ensure!(
            fs::canonicalize(&path).with_context(|| format!("无法定位分片 {digest}"))? == path,
            "分片 {digest} 路径逃逸"
        );
        let file = File::open(&path).with_context(|| format!("无法打开分片 {digest}"))?;
        let metadata = file.metadata().with_context(|| format!("无法检查分片 {digest}"))?;
        ensure!(metadata.is_file() && metadata.len() == expected, "分片 {digest} 长度不符");
        // 打开后再次检查路径；不接受符号链接或 Windows junction/reparse point。
        self.directory(false)?;
        reject_link(&path, false).with_context(|| format!("分片 {digest} 路径已改变"))?;
        ensure!(fs::canonicalize(&path).context("无法重新定位分片")? == path, "分片 {digest} 路径逃逸");
        let capacity = usize::try_from(expected).context("分片长度无法表示")?;
        let mut bytes = Vec::with_capacity(capacity);
        file.take(expected + 1).read_to_end(&mut bytes).with_context(|| format!("无法读取分片 {digest}"))?;
        ensure!(bytes.len() == capacity, "分片 {digest} 实际长度不符");
        ensure!(hash(&bytes) == digest, "分片 {digest} 的 SHA-256 不符");
        Ok(bytes)
    }

    fn put(&self, bytes: &[u8]) -> Result<Value> {
        ensure!(bytes.len() <= PART_LIMIT, "写入分片超过 4MiB 限制");
        let digest = hash(bytes);
        let expected = u64::try_from(bytes.len()).context("分片长度无法表示")?;
        let directory = self.directory(true)?;
        let path = directory.join(format!("{digest}.json"));
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                self.part_bytes(&digest, expected)?;
                return Ok(json!({ "sha256": digest, "bytes": expected }));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).with_context(|| format!("无法检查分片 {digest}")),
        }
        let mut temporary = NamedTempFile::new_in(&directory).context("无法创建分片临时文件")?;
        temporary.write_all(bytes).with_context(|| format!("无法写入分片 {digest}"))?;
        temporary.flush().context("无法刷新分片临时文件")?;
        temporary.as_file().sync_all().context("无法同步分片临时文件")?;
        self.directory(false)?;
        match temporary.persist_noclobber(&path) {
            Ok(_) => {}
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                self.part_bytes(&digest, expected)?;
            }
            Err(error) => return Err(error.error).with_context(|| format!("无法持久化分片 {digest}")),
        }
        Ok(json!({ "sha256": digest, "bytes": expected }))
    }
}

fn reject_link(path: &Path, directory: bool) -> Result<()> {
    let metadata = fs::symlink_metadata(path).context("无法检查文件类型")?;
    ensure!(!metadata.file_type().is_symlink() && !is_reparse_point(&metadata), "不允许符号链接或重解析点");
    ensure!(if directory { metadata.is_dir() } else { metadata.is_file() }, "文件类型不符");
    Ok(())
}

#[cfg(windows)]
fn is_reparse_point(metadata: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_metadata: &Metadata) -> bool {
    false
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn validate_digest(digest: &str) -> Result<()> {
    ensure!(
        digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "分片 SHA-256 必须是小写 64 位十六进制"
    );
    Ok(())
}

// 通常只编码一次；仅父容器必须分片且某条目放不下时，借用原逻辑
// 子容器按更小阈值重新编码。叶子不拆分，不 clone 整棵逻辑树。
struct EncodedItem<'a> {
    bytes: Vec<u8>,
    value: &'a Value,
    prefix: usize,
    depth: usize,
}

struct Container<'a> {
    object: bool,
    pending: Vec<EncodedItem<'a>>,
    size: usize,
    sharded: bool,
    chunk: Vec<u8>,
    parts: Vec<Value>,
}

impl<'a> Container<'a> {
    fn new(object: bool) -> Self {
        Self { object, pending: Vec::new(), size: 2, sharded: false, chunk: Vec::new(), parts: Vec::new() }
    }

    fn add(&mut self, store: &Store, item: EncodedItem<'a>, inline: usize, part: usize) -> Result<()> {
        if self.sharded {
            return self.add_part_item(store, item, part);
        }
        self.size = self.size.checked_add(item.bytes.len()).and_then(|n| n.checked_add(usize::from(!self.pending.is_empty())))
            .context("JSON 容器长度溢出")?;
        self.pending.push(item);
        if self.size > inline {
            self.sharded = true;
            for item in std::mem::take(&mut self.pending) {
                self.add_part_item(store, item, part)?;
            }
        }
        Ok(())
    }

    fn add_part_item(&mut self, store: &Store, mut item: EncodedItem<'a>, limit: usize) -> Result<()> {
        if item.bytes.len() > limit - 2 && (item.value.is_array() || item.value.is_object()) {
            let available = limit.checked_sub(2).and_then(|n| n.checked_sub(item.prefix))
                .filter(|n| *n >= 2).context("JSON 对象键超过分片上限；无法容纳子容器")?;
            let child = encode(store, item.value, item.depth, available, limit)?;
            item.bytes.truncate(item.prefix);
            item.bytes.extend_from_slice(&child);
        }
        let item = item.bytes;
        ensure!(item.len() <= limit - 2, "单个 JSON 条目超过分片上限（含容器边界）；不能拆分字符串、键或数字叶子");
        let separator = usize::from(!self.chunk.is_empty());
        if self.chunk.len() + separator + item.len() + 2 > limit {
            self.flush(store)?;
        }
        if !self.chunk.is_empty() {
            self.chunk.push(b',');
        }
        self.chunk.extend_from_slice(&item);
        Ok(())
    }

    fn flush(&mut self, store: &Store) -> Result<()> {
        if self.chunk.is_empty() {
            return Ok(());
        }
        let mut bytes = Vec::with_capacity(self.chunk.len() + 2);
        bytes.push(if self.object { b'{' } else { b'[' });
        bytes.append(&mut self.chunk);
        bytes.push(if self.object { b'}' } else { b']' });
        self.parts.push(store.put(&bytes)?);
        Ok(())
    }

    fn finish(mut self, store: &Store, inline: usize) -> Result<Vec<u8>> {
        if self.sharded {
            self.flush(store)?;
            let bytes = serde_json::to_vec(&json!({
                "$fireworks_shards": 1,
                "kind": if self.object { "object" } else { "array" },
                "parts": self.parts,
            })).context("无法编码分片引用")?;
            ensure!(bytes.len() <= inline, "分片引用本身超过内联限制");
            return Ok(bytes);
        }
        let mut bytes = Vec::with_capacity(self.size);
        bytes.push(if self.object { b'{' } else { b'[' });
        for (index, item) in self.pending.into_iter().enumerate() {
            if index != 0 {
                bytes.push(b',');
            }
            bytes.extend_from_slice(&item.bytes);
        }
        bytes.push(if self.object { b'}' } else { b']' });
        Ok(bytes)
    }
}

fn encode(store: &Store, value: &Value, depth: usize, inline: usize, part: usize) -> Result<Vec<u8>> {
    ensure!(depth < MAX_DEPTH, "JSON 嵌套深度超过安全限制");
    let mut container = match value {
        Value::Array(_) => Container::new(false),
        Value::Object(object) => {
            ensure!(!object.contains_key(MARKER), "逻辑 JSON 不允许保留键 $fireworks_shards；请先 read 解码");
            Container::new(true)
        }
        _ => {
            let bytes = serde_json::to_vec(value).context("无法编码 JSON 叶子")?;
            ensure!(bytes.len() <= inline, "单个 JSON 叶子超过内联上限；不能安全分片");
            return Ok(bytes);
        }
    };
    match value {
        Value::Array(values) => {
            for value in values {
                let bytes = encode(store, value, depth + 1, inline, part)?;
                container.add(store, EncodedItem { bytes, value, prefix: 0, depth: depth + 1 }, inline, part)?;
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                let mut item = serde_json::to_vec(key).context("无法编码 JSON 键")?;
                item.push(b':');
                let prefix = item.len();
                item.extend_from_slice(&encode(store, value, depth + 1, inline, part)?);
                container.add(store, EncodedItem { bytes: item, value, prefix, depth: depth + 1 }, inline, part)?;
            }
        }
        _ => unreachable!(),
    }
    container.finish(store, inline)
}

fn decode(store: &Store, value: &mut Value, depth: usize, stack: &mut HashSet<String>, found: &mut bool) -> Result<()> {
    ensure!(depth < MAX_DEPTH, "JSON 嵌套深度超过安全限制");
    if value.as_object().is_some_and(|object| object.contains_key(MARKER)) {
        *found = true;
        *value = decode_envelope(store, value, depth, stack, found)?;
        return Ok(());
    }
    match value {
        Value::Array(values) => {
            for value in values {
                decode(store, value, depth + 1, stack, found)?;
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                decode(store, value, depth + 1, stack, found)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn decode_envelope(store: &Store, envelope: &Value, depth: usize, stack: &mut HashSet<String>, found: &mut bool) -> Result<Value> {
    let object = envelope.as_object().context("分片引用必须是对象")?;
    ensure!(object.len() == 3 && object.get(MARKER).and_then(Value::as_u64) == Some(1), "无效的分片版本或引用字段");
    let kind = object.get("kind").and_then(Value::as_str).context("分片 kind 必须是字符串")?;
    ensure!(kind == "array" || kind == "object", "无效的分片 kind");
    let parts = object.get("parts").and_then(Value::as_array).context("分片 parts 必须是数组")?;
    ensure!(!parts.is_empty(), "分片 parts 不能为空");
    let mut result = if kind == "array" { Value::Array(Vec::new()) } else { Value::Object(Map::new()) };
    for part in parts {
        let reference = part.as_object().context("分片条目必须是对象")?;
        ensure!(reference.len() == 2 && reference.contains_key("sha256") && reference.contains_key("bytes"), "分片条目只能包含 sha256 和 bytes，禁止 path 等字段");
        let digest = reference.get("sha256").and_then(Value::as_str).context("分片 sha256 必须是字符串")?;
        validate_digest(digest)?;
        let expected = reference.get("bytes").and_then(Value::as_u64).context("分片 bytes 必须是非负整数")?;
        ensure!(stack.len() < MAX_DEPTH, "分片引用深度超过安全限制");
        ensure!(stack.insert(digest.to_owned()), "检测到循环分片引用 {digest}");
        let decoded = (|| -> Result<Value> {
            let bytes = store.part_bytes(digest, expected)?;
            let mut value = parse(bytes.as_slice()).with_context(|| format!("分片 {digest} JSON 无效"))?;
            ensure!(
                (kind == "array" && value.is_array()) || (kind == "object" && value.is_object()),
                "分片 {digest} 容器类型与 kind 不符"
            );
            ensure!(!value.as_object().is_some_and(|map| map.contains_key(MARKER)), "分片 {digest} 顶层必须是原始容器，而不是另一个引用");
            decode(store, &mut value, depth, stack, found)?;
            Ok(value)
        })();
        stack.remove(digest);
        match (&mut result, decoded?) {
            (Value::Array(output), Value::Array(mut values)) => output.append(&mut values),
            (Value::Object(output), Value::Object(values)) => {
                for (key, value) in values {
                    ensure!(!output.contains_key(&key), "对象分片 {digest} 出现重复键");
                    output.insert(key, value);
                }
            }
            _ => bail!("分片 {digest} 解码后类型不符"),
        }
    }
    Ok(result)
}

// serde_json::Value 默认会覆盖重复键；在任何根或分片 JSON 中都拒绝它们。
struct UniqueValue(Value);

impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct UniqueVisitor;
        impl<'de> Visitor<'de> for UniqueVisitor {
            type Value = UniqueValue;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("无重复键的 JSON 值")
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<Self::Value, E> { Ok(UniqueValue(Value::Null)) }
            fn visit_bool<E: de::Error>(self, value: bool) -> std::result::Result<Self::Value, E> { Ok(UniqueValue(Value::Bool(value))) }
            fn visit_i64<E: de::Error>(self, value: i64) -> std::result::Result<Self::Value, E> { Ok(UniqueValue(Value::Number(value.into()))) }
            fn visit_u64<E: de::Error>(self, value: u64) -> std::result::Result<Self::Value, E> { Ok(UniqueValue(Value::Number(value.into()))) }
            fn visit_f64<E: de::Error>(self, value: f64) -> std::result::Result<Self::Value, E> {
                Number::from_f64(value).map(|number| UniqueValue(Value::Number(number))).ok_or_else(|| E::custom("JSON 数字无效"))
            }
            fn visit_str<E: de::Error>(self, value: &str) -> std::result::Result<Self::Value, E> { Ok(UniqueValue(Value::String(value.to_owned()))) }
            fn visit_string<E: de::Error>(self, value: String) -> std::result::Result<Self::Value, E> { Ok(UniqueValue(Value::String(value))) }
            fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(UniqueValue(value)) = sequence.next_element()? {
                    values.push(value);
                }
                Ok(UniqueValue(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<Self::Value, A::Error> {
                let mut values = Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(de::Error::custom("JSON 对象出现重复键"));
                    }
                    let UniqueValue(value) = map.next_value()?;
                    values.insert(key, value);
                }
                Ok(UniqueValue(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(UniqueVisitor)
    }
}

fn parse(reader: impl Read) -> Result<Value> {
    Ok(serde_json::from_reader::<_, UniqueValue>(reader)?.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const TEST_INLINE: usize = 4096;
    const TEST_PART: usize = 1024;

    fn save(path: &Path, value: &Value) -> Result<()> {
        stage_with_limits(path, value, TEST_INLINE, TEST_PART)?.persist(path).map_err(|error| error.error)?;
        Ok(())
    }

    fn records() -> Value {
        Value::Array((0..100).map(|index| json!({ "课程": "重复课程".repeat(8), "顺序": index / 2 })).collect())
    }

    fn root(path: &Path) -> Value {
        serde_json::from_reader(BufReader::new(File::open(path).unwrap())).unwrap()
    }

    fn reference(store: &Store, bytes: &[u8]) -> Value {
        store.put(bytes).unwrap()
    }

    fn envelope(kind: &str, parts: Vec<Value>) -> Value {
        json!({ "$fireworks_shards": 1, "kind": kind, "parts": parts })
    }

    fn write_raw(path: &Path, value: &Value) {
        fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
    }

    #[test]
    fn small_legacy_json_and_public_write_preserve_semantics() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("legacy.json");
        let value = json!({ "中文": [null, true, -3, 1.5, "重复", "重复"], "空": {} });
        fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
        assert_eq!(read(&path).unwrap(), value);
        write(&path, &value).unwrap();
        assert_eq!(read(&path).unwrap(), value);
        assert!(!dir.path().join(".fireworks-json").exists());
    }

    #[test]
    fn canonical_hash_matches_logical_json_without_expanding_nested_parts() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("fingerprint.json");
        let store = Store::for_path(&path, true).unwrap();
        let shared = reference(&store, br#"{"b":[1,1,2]}"#);
        let nested = envelope("object", vec![shared.clone()]);
        let first = reference(&store, &serde_json::to_vec(&json!({"z":nested,"a":"中文\\\""})).unwrap());
        write_raw(&path, &envelope("object", vec![first, shared]));
        let expected = json!({"a":"中文\\\"","b":[1,1,2],"z":{"b":[1,1,2]}});
        assert_eq!(read(&path).unwrap(), expected);
        assert_eq!(canonical_sha256(&path).unwrap(), crate::curriculum::value_sha256(&expected));
        for value in [records(), json!({"nested":records(),"empty":{},"null":null,"float":1.5})] {
            save(&path, &value).unwrap();
            assert_eq!(canonical_sha256(&path).unwrap(), crate::curriculum::value_sha256(&value));
        }
    }

    #[test]
    fn array_parts_preserve_order_duplicates_and_utf8_byte_limits() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("array.json");
        let value = records();
        save(&path, &value).unwrap();
        let encoded = root(&path);
        let parts = encoded["parts"].as_array().unwrap();
        assert!(parts.len() > 1);
        assert!(parts.iter().all(|part| part["bytes"].as_u64().unwrap() <= TEST_PART as u64));
        assert_eq!(read(&path).unwrap(), value);
    }

    #[test]
    fn object_parts_and_nested_arrays_use_original_root_anchor() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nested.json");
        let mut object = Map::new();
        for index in 0..80 {
            object.insert(format!("key-{index:03}"), Value::String("对象".repeat(50)));
        }
        let value = json!({ "对象": object, "嵌套": { "课程": records() } });
        save(&path, &value).unwrap();
        let encoded = root(&path);
        assert_eq!(encoded["对象"]["kind"], "object");
        assert_eq!(encoded["嵌套"]["课程"]["kind"], "array");
        assert_eq!(read(&path).unwrap(), value);
        assert!(!dir.path().join(".fireworks-json/.fireworks-json").exists());
    }

    #[test]
    fn roots_share_immutable_content_addressed_parts() {
        let dir = TempDir::new().unwrap();
        let first = dir.path().join("first.json");
        let second = dir.path().join("second.json");
        let value = records();
        save(&first, &value).unwrap();
        let before = fs::read_dir(dir.path().join(".fireworks-json")).unwrap().count();
        save(&second, &value).unwrap();
        assert_eq!(before, fs::read_dir(dir.path().join(".fireworks-json")).unwrap().count());
        assert_eq!(root(&first), root(&second));
        assert_eq!(read(&first).unwrap(), read(&second).unwrap());
    }

    #[test]
    fn dropping_staged_root_never_replaces_existing_root() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("root.json");
        let old = json!({ "原始": [1, 2, 2] });
        write(&path, &old).unwrap();
        let temporary = stage_with_limits(&path, &records(), TEST_INLINE, TEST_PART).unwrap();
        let temporary_path = temporary.path().to_owned();
        assert_eq!(read(&path).unwrap(), old);
        drop(temporary);
        assert!(!temporary_path.exists());
        assert_eq!(read(&path).unwrap(), old);
    }

    #[test]
    fn tampered_hash_size_and_missing_parts_are_rejected() {
        for corruption in ["hash", "size", "missing"] {
            let dir = TempDir::new().unwrap();
            let path = dir.path().join("root.json");
            save(&path, &records()).unwrap();
            let encoded = root(&path);
            let first = &encoded["parts"][0];
            let part_path = dir.path().join(".fireworks-json").join(format!("{}.json", first["sha256"].as_str().unwrap()));
            match corruption {
                "missing" => fs::remove_file(part_path).unwrap(),
                "size" => fs::write(part_path, b"[]").unwrap(),
                _ => {
                    let mut bytes = fs::read(&part_path).unwrap();
                    bytes[0] = b' ';
                    fs::write(part_path, bytes).unwrap();
                }
            }
            assert!(read(&path).is_err(), "{corruption}");
            assert!(canonical_sha256(&path).is_err(), "{corruption}");
        }
    }

    #[test]
    fn invalid_protocol_fields_and_duplicate_object_keys_are_rejected() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("root.json");
        let store = Store::for_path(&path, true).unwrap();
        let part = reference(&store, br#"{"a":1}"#);
        let valid = envelope("object", vec![part.clone()]);
        let mut cases = Vec::new();
        for (key, value) in [(MARKER, json!(2)), ("kind", json!("string")), ("parts", json!([]))] {
            let mut invalid = valid.clone();
            invalid[key] = value;
            cases.push(invalid);
        }
        for invalid_hash in ["../escape", &"A".repeat(64), &"0".repeat(63)] {
            let mut invalid = valid.clone();
            invalid["parts"][0]["sha256"] = json!(invalid_hash);
            cases.push(invalid);
        }
        for bad_bytes in [json!(-1), json!(1.5), json!(0), json!(INLINE_LIMIT + 1)] {
            let mut invalid = valid.clone();
            invalid["parts"][0]["bytes"] = bad_bytes;
            cases.push(invalid);
        }
        let mut path_field = valid.clone();
        path_field["parts"][0]["path"] = json!("../credentials.json");
        cases.push(path_field);
        cases.push(envelope("object", vec![part.clone(), part]));
        cases.push(envelope("array", vec![reference(&store, br#"{"a":1}"#)]));
        cases.push(envelope("object", vec![reference(&store, br#"{"a":1,"a":2}"#)]));
        for invalid in cases {
            write_raw(&path, &invalid);
            assert!(read(&path).is_err(), "{invalid}");
            assert!(canonical_sha256(&path).is_err(), "{invalid}");
        }
        fs::write(&path, br#"{"a":1,"a":2}"#).unwrap();
        assert!(read(&path).is_err());
        assert!(canonical_sha256(&path).is_err());
        assert!(stage(&path, &valid).is_err());
    }

    #[test]
    fn repeated_array_part_is_valid_but_active_stack_reentry_is_rejected() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("root.json");
        let store = Store::for_path(&path, true).unwrap();
        let part = reference(&store, b"[1,1,2]");
        let encoded = envelope("array", vec![part.clone(), part.clone()]);
        write_raw(&path, &encoded);
        assert_eq!(read(&path).unwrap(), json!([1, 1, 2, 1, 1, 2]));
        let mut stack = HashSet::from([part["sha256"].as_str().unwrap().to_owned()]);
        let mut found = false;
        let error = decode_envelope(&store, &encoded, 0, &mut stack, &mut found).unwrap_err();
        assert!(error.to_string().contains("循环"));
    }

    #[test]
    fn damaged_existing_part_is_never_overwritten() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("root.json");
        let store = Store::for_path(&path, true).unwrap();
        let bytes = b"[1,2,3]";
        let part = reference(&store, bytes);
        let part_path = dir.path().join(".fireworks-json").join(format!("{}.json", part["sha256"].as_str().unwrap()));
        fs::write(&part_path, b"[3,2,1]").unwrap();
        assert!(store.put(bytes).is_err());
        assert_eq!(fs::read(part_path).unwrap(), b"[3,2,1]");
    }

    #[test]
    fn oversized_indivisible_items_fail_without_replacing_root() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("root.json");
        write(&path, &json!(["原始"])).unwrap();
        let value = json!(["x".repeat(3000), "y".repeat(3000)]);
        assert!(stage_with_limits(&path, &value, TEST_INLINE, TEST_PART).is_err());
        assert!(stage_with_limits(&path, &json!("x".repeat(TEST_INLINE)), TEST_INLINE, TEST_PART).is_err());
        assert_eq!(read(&path).unwrap(), json!(["原始"]));
    }

    #[test]
    fn oversized_inline_child_containers_are_sharded_when_parent_needs_parts() {
        let dir = TempDir::new().unwrap();
        let child_array = json!(["中".repeat(110), "中".repeat(110), "中".repeat(110), "中".repeat(110)]);
        let child_object = json!({ "甲": "文".repeat(110), "乙": "文".repeat(110), "丙": "文".repeat(110), "丁": "文".repeat(110) });
        for child in [&child_array, &child_object] {
            let length = serde_json::to_vec(child).unwrap().len();
            assert!(length > TEST_PART && length < 2 * TEST_PART);
        }
        let array_root = json!([child_array, child_object, child_array, child_object]);
        let object_root = json!({ "第一项中文键": child_array, "第二项中文键": child_object, "第三项中文键": child_array, "第四项中文键": child_object });
        for (index, value) in [array_root, object_root].into_iter().enumerate() {
            assert!(serde_json::to_vec(&value).unwrap().len() > TEST_INLINE);
            let path = dir.path().join(format!("container-{index}.json"));
            save(&path, &value).unwrap();
            let encoded = root(&path);
            assert_eq!(encoded[MARKER], 1);
            assert!(!encoded["parts"].as_array().unwrap().is_empty());
            assert_eq!(read(&path).unwrap(), value);
        }
        for entry in fs::read_dir(dir.path().join(".fireworks-json")).unwrap() {
            assert!(entry.unwrap().metadata().unwrap().len() <= TEST_PART as u64);
        }
    }

    #[test]
    fn inline_parent_keeps_child_above_part_limit_unsharded() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("inline.json");
        let child = json!(["x".repeat(400), "x".repeat(400), "x".repeat(400)]);
        let value = json!({ "child": child });
        assert!(serde_json::to_vec(&value["child"]).unwrap().len() > TEST_PART);
        save(&path, &value).unwrap();
        assert_eq!(root(&path), value);
        assert_eq!(read(&path).unwrap(), value);
        assert!(!dir.path().join(".fireworks-json").exists());
    }

    #[test]
    fn concurrent_identical_parts_are_reused() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("root.json");
        let parent = Store::for_path(&path, true).unwrap().parent;
        std::thread::scope(|scope| {
            let first = scope.spawn(|| Store { parent: parent.clone() }.put(b"[1,2,2,3]"));
            let second = scope.spawn(|| Store { parent: parent.clone() }.put(b"[1,2,2,3]"));
            assert_eq!(first.join().unwrap().unwrap(), second.join().unwrap().unwrap());
        });
        assert_eq!(fs::read_dir(parent.join(".fireworks-json")).unwrap().count(), 1);
    }
}
