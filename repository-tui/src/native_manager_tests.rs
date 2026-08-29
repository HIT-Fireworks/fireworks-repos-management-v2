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
            ("notes/a.txt", "A"),
            ("notes/b.txt", "B"),
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
                "member_resource_group_ids":["group-a","group-b"],
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
            {"repo_id":"COURSE-A","path":"notes/a.txt","course_codes":["A1"],"size":1},
            {"repo_id":"COURSE-A","path":"notes/b.txt","course_codes":["B1"],"size":1}
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
            "member_resource_group_ids":["group-a","group-b"]
        }],
        "resource_groups":[
            {"resource_group_id":"group-a","display_name":"课程甲","course_names":["课程甲"],"course_codes":["A1"]},
            {"resource_group_id":"group-b","display_name":"课程乙","course_names":["课程乙"],"course_codes":["B1"]}
        ],
        "course_descriptors":[
            {"course_code":"A1","course_name":"课程甲"},
            {"course_code":"B1","course_name":"课程乙"}
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
    assert_eq!(options.groups.len(), 2);
    assert_eq!(options.loose_files.len(), 1);
    let targets = vec![
        SplitTarget {
            repo_id: "COURSE-A".into(),
            display_name: "课程甲资料".into(),
            resource_group_ids: vec!["group-a".into()],
            paths: vec!["README.md".into()],
        },
        SplitTarget {
            repo_id: "MANAGED-B".into(),
            display_name: "课程乙资料".into(),
            resource_group_ids: vec!["group-b".into()],
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
}

#[test]
fn tampered_native_journal_is_rejected() {
    let (_temp, mut manager, _) = fixture();
    let targets = vec![
        SplitTarget {
            repo_id: "COURSE-A".into(),
            display_name: "课程甲资料".into(),
            resource_group_ids: vec!["group-a".into()],
            paths: vec!["README.md".into()],
        },
        SplitTarget {
            repo_id: "MANAGED-B".into(),
            display_name: "课程乙资料".into(),
            resource_group_ids: vec!["group-b".into()],
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
            resource_group_ids: vec!["group-a".into()],
            paths: vec!["README.md".into()],
        },
        SplitTarget {
            repo_id: "MANAGED-B".into(),
            display_name: "课程乙资料".into(),
            resource_group_ids: vec!["group-b".into()],
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
            resource_group_ids: vec!["group-a".into()],
            paths: vec!["README.md".into()],
        },
        SplitTarget {
            repo_id: "MANAGED-B".into(),
            display_name: "课程乙资料".into(),
            resource_group_ids: vec!["group-b".into()],
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
