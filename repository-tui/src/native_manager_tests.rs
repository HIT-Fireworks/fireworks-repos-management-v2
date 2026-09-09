use super::*;
use std::fs;

fn git(cwd: &Path, arguments: &[&str]) -> String {
    run_git(cwd, arguments, None, &[]).expect("git command")
}

fn seed_remote(root: &Path, repo_id: &str, files: &[(&str, &str)]) -> (PathBuf, String) {
    let remote = root.join("remotes").join(format!("{repo_id}.git"));
    fs::create_dir_all(remote.parent().unwrap()).unwrap();
    git(
        remote.parent().unwrap(),
        &[
            "init",
            "--bare",
            remote.file_name().unwrap().to_str().unwrap(),
        ],
    );
    let work = root.join(format!("{repo_id}-work"));
    fs::create_dir_all(&work).unwrap();
    git(&work, &["init"]);
    git(&work, &["config", "user.name", "Rust Manager Test"]);
    git(
        &work,
        &["config", "user.email", "rust-manager@example.invalid"],
    );
    for (path, content) in files {
        let target = work.join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, content).unwrap();
    }
    git(&work, &["add", "."]);
    git(&work, &["commit", "-m", "test: seed"]);
    git(&work, &["branch", "-M", "main"]);
    git(
        &work,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&work, &["push", "origin", "main"]);
    let head = git(&work, &["rev-parse", "HEAD"]).trim().to_string();
    (remote, head)
}

fn write_value(path: &Path, value: &Value) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

fn fixture() -> (TempDir, Manager, String) {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let (_, head) = seed_remote(
        root,
        "COURSE-A",
        &[
            ("README.md", "source"),
            ("a.txt", "A"),
            ("b.txt", "B"),
            ("LICENSE", "license preserved"),
            (".github/workflows/check.yml", "workflow preserved"),
            ("private-notes.txt", "unmanaged preserved"),
        ],
    );
    let workspace = root.join("workspace");
    let topology = json!({
        "schema_version":1,
        "generation":1,
        "organization":"LOCAL",
        "repositories":{
            "COURSE-A":{
                "repo_id":"COURSE-A",
                "repo_type":"course",
                "display_name":"课程 A",
                "physical_repository_id":"physical-a",
                "course_codes":["A1","B1"],
                "lineage":{"kind":"fixture","source_repo_ids":["COURSE-A"]}
            }
        }
    });
    let routes = json!({
        "schema_version":1,
        "generation":1,
        "inventory_complete_repositories":["COURSE-A"],
        "repository_heads":{"COURSE-A":head},
        "files":[
            {"repo_id":"COURSE-A","path":"README.md","course_codes":[],"size":6},
            {"repo_id":"COURSE-A","path":"a.txt","course_codes":["A1"],"size":1},
            {"repo_id":"COURSE-A","path":"b.txt","course_codes":["B1"],"size":1}
        ],
        "course_code_routes":[
            {"course_code":"A1","repo_id":"COURSE-A","physical_repository_id":"physical-a"},
            {"course_code":"B1","repo_id":"COURSE-A","physical_repository_id":"physical-a"}
        ]
    });
    let manifest = json!({
        "schema_version":1,
        "organization":"LOCAL",
        "repositories":[{
            "repo_id":"COURSE-A",
            "repo_type":"course",
            "display_name":"课程 A",
            "description":"课程 A",
            "course_codes":["A1","B1"],
            "physical_repository_id":"physical-a"
        }],
        "course_descriptors":[
            {"course_code":"A1","course_name":"课程甲","repo_id":"COURSE-A","physical_repository_id":"physical-a"},
            {"course_code":"B1","course_name":"课程乙","repo_id":"COURSE-A","physical_repository_id":"physical-a"}
        ],
        "virtual_collections":[]
    });
    write_value(&workspace.join(DEFAULT_MANIFEST), &manifest);
    write_value(&workspace.join(DEFAULT_TOPOLOGY), &topology);
    write_value(&workspace.join(DEFAULT_ROUTES), &routes);
    let template = root
        .join("remotes")
        .join("{repo_id}.git")
        .to_string_lossy()
        .to_string();
    let mut manager = Manager::new(&workspace).with_remote_template(template);
    manager.reload().unwrap();
    (temp, manager, head)
}

#[test]
fn native_split_runs_without_python_and_preserves_routes() {
    let (temp, mut manager, source_head) = fixture();
    let options = manager.split_options("COURSE-A").unwrap();
    assert_eq!(options.courses.iter().map(|course| course.course_code.as_str()).collect::<Vec<_>>(), vec!["A1", "B1"]);
    assert!(options.loose_files.iter().any(|file| file.internal_path == "private-notes.txt"));
    let targets = vec![
        SplitTarget {
            repo_id: "COURSE-A".into(),
            display_name: "课程甲资料".into(),
            course_codes: vec!["A1".into()],
            paths: vec!["README.md".into(), "private-notes.txt".into()],
        },
        SplitTarget {
            repo_id: "MANAGED-B".into(),
            display_name: "课程乙资料".into(),
            course_codes: vec!["B1".into()],
            paths: vec![],
        },
    ];
    let plan = manager.plan_split("COURSE-A", &targets).unwrap();
    assert!(plan.path.is_empty());
    let result = manager.apply(&plan).unwrap();
    assert_eq!(string_field(&result, "status"), "completed");
    assert_eq!(
        remote_head(&temp.path().join("remotes/COURSE-A.git").to_string_lossy())
            .unwrap()
            .is_some(),
        true
    );
    assert_eq!(
        remote_head(&temp.path().join("remotes/MANAGED-B.git").to_string_lossy())
            .unwrap()
            .is_some(),
        true
    );
    assert_eq!(
        manager.routes["course_code_routes"][0]["repo_id"],
        json!("COURSE-A")
    );
    assert_eq!(
        manager.routes["course_code_routes"][1]["repo_id"],
        json!("MANAGED-B")
    );
    assert_eq!(
        manager.routes["repository_heads"]["COURSE-A"]
            .as_str()
            .unwrap()
            != source_head,
        true
    );
    let journals = manager.journals().unwrap();
    assert_eq!(journals.len(), 1);
    assert_eq!(journals[0].recovery_state, "completed");
    manager.verify(&journals[0]).unwrap();
    let source = temp.path().join("remotes/COURSE-A.git");
    let target = temp.path().join("remotes/MANAGED-B.git");
    assert_eq!(git(&source, &["show", "main:LICENSE"]), "license preserved");
    assert_eq!(git(&source, &["show", "main:private-notes.txt"]), "unmanaged preserved");
    assert_eq!(git(&source, &["show", "main:.github/workflows/check.yml"]), "workflow preserved");
    assert_eq!(git(&target, &["show", "main:b.txt"]), "B");
    assert!(!git(&source, &["ls-tree", "-r", "--name-only", "main"]).lines().any(|path| path == "b.txt"));
    assert_eq!(manager.manifest["course_descriptors"][1]["repo_id"], "MANAGED-B");
    let merged = manager.plan_merge(&["COURSE-A".into(), "MANAGED-B".into()], "COURSE-A", "合并课程资料").unwrap();
    manager.apply(&merged).unwrap();
    assert_eq!(git(&source, &["show", "main:b.txt"]), "B");
    assert!(manager.routes["course_code_routes"].as_array().unwrap().iter().all(|route| route["repo_id"] == "COURSE-A"));
    assert!(manager.manifest["course_descriptors"].as_array().unwrap().iter().all(|descriptor| descriptor["repo_id"] == "COURSE-A"));
}

#[test]
fn tampered_native_journal_is_rejected() {
    let (_temp, mut manager, _) = fixture();
    let targets = vec![
        SplitTarget {
            repo_id: "COURSE-A".into(),
            display_name: "课程甲资料".into(),
            course_codes: vec!["A1".into()],
            paths: vec!["README.md".into(), "private-notes.txt".into()],
        },
        SplitTarget {
            repo_id: "MANAGED-B".into(),
            display_name: "课程乙资料".into(),
            course_codes: vec!["B1".into()],
            paths: vec![],
        },
    ];
    let plan = manager.plan_split("COURSE-A", &targets).unwrap();
    manager.apply(&plan).unwrap();
    let summary = manager.journals().unwrap().remove(0);
    let path = PathBuf::from(&summary.path);
    let mut journal = read_json(&path).unwrap();
    journal["git"]["targets"]["COURSE-A"]["remote_url"] = json!("attacker.git");
    journal["status"] = json!("failed");
    atomic_json(&path, &journal).unwrap();
    let mut forged = summary;
    forged.recovery_state = "resumable".into();
    assert!(manager.resume(&forged).is_err());
}

#[test]
fn created_empty_target_is_valid_during_resume() {
    let (_temp, manager, _) = fixture();
    let targets = vec![
        SplitTarget {
            repo_id: "COURSE-A".into(),
            display_name: "课程甲资料".into(),
            course_codes: vec!["A1".into()],
            paths: vec!["README.md".into(), "private-notes.txt".into()],
        },
        SplitTarget {
            repo_id: "MANAGED-B".into(),
            display_name: "课程乙资料".into(),
            course_codes: vec!["B1".into()],
            paths: vec![],
        },
    ];
    let plan = manager.plan_split("COURSE-A", &targets).unwrap();
    let target_remote = plan
        .plan
        .pointer("/core/remote_baseline/targets/MANAGED-B/remote_url")
        .and_then(Value::as_str)
        .unwrap()
        .to_string();
    manager
        .ensure_target_repository("MANAGED-B", &target_remote)
        .unwrap();
    assert_eq!(
        remote_revision(&target_remote).unwrap()["exists"],
        json!(true)
    );
    assert!(remote_revision(&target_remote).unwrap()["head"].is_null());
    manager.validate_remote_baseline(&plan.plan, None).unwrap();
}

#[test]
fn remote_baseline_freezes_actor_and_source_tree() {
    let (_temp, manager, _) = fixture();
    let targets = vec![
        SplitTarget {
            repo_id: "COURSE-A".into(),
            display_name: "课程甲资料".into(),
            course_codes: vec!["A1".into()],
            paths: vec!["README.md".into(), "private-notes.txt".into()],
        },
        SplitTarget {
            repo_id: "MANAGED-B".into(),
            display_name: "课程乙资料".into(),
            course_codes: vec!["B1".into()],
            paths: vec![],
        },
    ];
    let plan = manager.plan_split("COURSE-A", &targets).unwrap();
    assert_eq!(
        plan.plan.pointer("/core/github_actor"),
        Some(&json!("local-test"))
    );
    let source = plan
        .plan
        .pointer("/core/remote_baseline/sources/COURSE-A")
        .unwrap();
    assert_eq!(source["exists"], json!(true));
    assert!(source["head"]
        .as_str()
        .is_some_and(|value| is_hex(value, 40)));
    assert!(source["tree"]
        .as_str()
        .is_some_and(|value| is_hex(value, 40)));
}

fn direct_targets() -> Vec<SplitTarget> {
    vec![
        SplitTarget { repo_id: "COURSE-A".into(), display_name: "课程甲".into(), course_codes: vec!["A1".into()], paths: vec!["README.md".into(), "private-notes.txt".into()] },
        SplitTarget { repo_id: "MANAGED-B".into(), display_name: "课程乙".into(), course_codes: vec!["B1".into()], paths: vec![] },
    ]
}

#[test]
fn shared_file_requires_all_codes_in_one_target_even_with_explicit_path() {
    let (_temp, mut manager, _) = fixture();
    manager.routes["files"][1]["course_codes"] = json!(["A1", "B1"]);
    let mut targets = direct_targets();
    targets[0].paths.push("a.txt".into());
    assert!(manager.build_split_plan("COURSE-A", &targets).is_err());
    targets[0].course_codes.push("B1".into());
    targets[1].course_codes.clear();
    targets[0].paths.clear();
    targets[1].paths.push("README.md".into());
    let plan = manager.build_split_plan("COURSE-A", &targets).unwrap();
    assert!(plan["after"]["routes"]["files"].as_array().unwrap().iter()
        .filter(|file| !string_array(file, "course_codes").is_empty()).all(|file| file["repo_id"] == "COURSE-A"));
}

#[test]
fn material_free_code_splits_without_inventing_files() {
    let (temp, mut manager, _) = fixture();
    manager.routes["files"][2]["course_codes"] = json!(["A1"]);
    manager.routes["course_code_routes"][1]["has_material"] = json!(false);
    atomic_json(&manager.routes_path, &manager.routes).unwrap();
    let plan = manager.plan_split("COURSE-A", &direct_targets()).unwrap();
    manager.apply(&plan).unwrap();
    assert!(!manager.routes["files"].as_array().unwrap().iter().any(|file| file["repo_id"] == "MANAGED-B"));
    assert_eq!(manager.routes["course_code_routes"][1]["repo_id"], "MANAGED-B");
    assert_eq!(git(&temp.path().join("remotes/MANAGED-B.git"), &["ls-tree", "-r", "--name-only", "main"]), "");
}

#[test]
fn merge_rejects_same_path_without_synthesizing_container_directories() {
    let (_temp, mut manager, _) = fixture();
    let split = manager.plan_split("COURSE-A", &direct_targets()).unwrap();
    manager.apply(&split).unwrap();
    let other = manager.routes["files"].as_array_mut().unwrap().iter_mut().find(|file| file["repo_id"] == "MANAGED-B").unwrap();
    other["path"] = json!("a.txt");
    assert!(manager.build_merge_plan(&["COURSE-A".into(), "MANAGED-B".into()], "COURSE-A", "合并").is_err());
}
