use super::*;
use crate::curriculum::{self, ChangeKind, ChangeType};
use crate::jwts::{CandidatePlan, CandidateSnapshot, CurriculumCatalog, PlanKind};

fn fixture() -> (TempDir, Manager) {
    let (temporary, mut manager) = super::curriculum_rebuild_tests::fixture();
    manager.manifest["curriculum_plans"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "plan_id":"untouched-plan","major_name":"另一个专业","school_name":"计算机学院"
        }));
    manager.manifest["curriculum_records"].as_array_mut().unwrap().extend([
        json!({"record_id":"REC-OLD2","source_plan":"plan-a","source_ordinal":1,"course_code":"OLD2","course_name":"旧课程","credit":2}),
        json!({"record_id":"REC-OTHER","source_plan":"untouched-plan","source_ordinal":0,"course_code":"A1","course_name":"程序设计","credit":3}),
    ]);
    manager.manifest["course_descriptors"].as_array_mut().unwrap().push(json!({
        "descriptor_id":"course-code:OLD2","course_code":"OLD2","course_name":"旧课程",
        "resource_group_id":"group-a","physical_repository_id":"physical-a","repo_id":"COURSE-A","record_ids":["REC-OLD2"]
    }));
    manager.manifest["resource_groups"][0]["course_codes"] = json!(["A1", "OLD2"]);
    manager.manifest["repositories"][0]["course_codes"] = json!(["A1", "OLD2"]);
    manager.topology["repositories"]["COURSE-A"]["course_codes"] = json!(["A1", "OLD2"]);
    manager.routes["course_code_routes"][0]["has_material"] = json!(true);
    manager.routes["course_code_routes"].as_array_mut().unwrap().push(json!({
        "component_id":"old-a","course_code":"OLD2","has_material":true,"physical_repository_id":"physical-a","repo_id":"COURSE-A"
    }));
    manager.routes["files"] = json!([{
        "repo_id":"COURSE-A","path":"notes/shared.pdf","course_codes":["A1","OLD2"],
        "route_kind":"curriculum-course","route_keys":["A1","OLD2"],"size":15,"sha256":"original-file-digest"
    }]);
    atomic_json(&manager.manifest_path, &manager.manifest).unwrap();
    atomic_json(&manager.topology_path, &manager.topology).unwrap();
    atomic_json(&manager.routes_path, &manager.routes).unwrap();
    manager.reload().unwrap();
    (temporary, manager)
}

fn review(manager: &Manager, mutate: impl FnOnce(&mut CandidateSnapshot)) -> UpdateSession {
    let current = curriculum::baseline_snapshot(&manager.manifest).unwrap();
    let mut candidate = current.clone();
    candidate.generated_at = "candidate-2026".into();
    candidate.base_url = "local-fixture".into();
    mutate(&mut candidate);
    let diff = curriculum::diff_snapshots(current, candidate.clone()).unwrap();
    let change_count = diff.changes.len();
    UpdateSession {
        client: None,
        kind: PlanKind::Curriculum,
        catalog: CurriculumCatalog::default(),
        selections: Vec::new(),
        candidate: Some(candidate),
        decisions: curriculum::DecisionSet {
            diff_identity_sha256: diff.diff_identity_sha256.clone(),
            decisions: BTreeMap::new(),
        },
        diff: Some(diff),
        assignments: BTreeMap::new(),
        status: CurriculumUpdateStatus {
            stage: "review".into(),
            change_count,
            pending_decision_count: change_count,
            updated_at: now(),
            ..Default::default()
        },
    }
}

fn changed_plan(snapshot: &mut CandidateSnapshot) {
    let plan = snapshot
        .plans
        .iter_mut()
        .find(|p| p.plan_id == "plan-a")
        .unwrap();
    plan.info["major_full_name"] = json!("计算机科学与技术【本】");
    plan.info["program_type"] = json!("本");
    plan.info["source_capture"] = json!({"complete":true});
    plan.info["academic_structure"] =
        json!({"graduation_requirements":[{"module":"专业核心","credits":7}]});
    plan.courses = vec![
        json!({"course_code":"A1","course_name":"程序设计","credit":4}),
        json!({"course_code":"NEW2","course_name":"程序设计","credit":2,"offering_college":"计算机学院"}),
        json!({"course_code":"","course_name":"创新实践","credit":1}),
    ];
}

#[test]
fn changes_restore_assign_apply_and_repeat_without_losing_history_or_files() {
    let (temporary, mut manager) = fixture();
    let original_files = manager.routes["files"].clone();
    let original_disk = manager.disk_workspace_identity().unwrap();
    let mut session = review(&manager, changed_plan);
    let diff = session.diff.as_ref().unwrap();
    assert!(diff
        .changes
        .iter()
        .any(|c| c.change_type == ChangeType::PlanMetadata));
    assert!(diff
        .changes
        .iter()
        .any(|c| c.course_code.as_deref() == Some("A1") && c.kind == ChangeKind::Changed));
    assert!(diff
        .changes
        .iter()
        .any(|c| c.course_code.as_deref() == Some("OLD2") && c.kind == ChangeKind::Removed));
    assert!(diff.changes.iter().all(|c| c.plan_id != "untouched-plan"));
    assert!(manager.materialize_curriculum_update(&mut session).is_err());
    let path = manager.save_curriculum_review(&session).unwrap();
    let saved = fs::read_to_string(&path).unwrap();
    assert!(!saved.contains("cookie") && !saved.contains("password"));
    assert_eq!(manager.curriculum_reviews().unwrap().len(), 1);
    let mut restarted =
        Manager::new(temporary.path()).with_remote_template(manager.remote_template.clone());
    restarted.reload().unwrap();
    let mut resumed = restarted.resume_curriculum_review(&path).unwrap();
    assert!(resumed.client.is_none());
    resumed.accept_all().unwrap();
    restarted.save_curriculum_review(&resumed).unwrap();
    let pending = restarted.pending_course_assignments(&resumed).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].course_code, "NEW2");
    assert!(restarted
        .materialize_curriculum_update(&mut resumed)
        .is_err());
    restarted
        .assign_course(
            &mut resumed,
            "NEW2",
            CourseAssignment::Existing {
                repo_id: "COURSE-A".into(),
            },
        )
        .unwrap();
    let preview = restarted
        .materialize_curriculum_update(&mut resumed)
        .unwrap();
    assert_eq!(original_disk, manager.disk_workspace_identity().unwrap());
    assert!(preview.create_repositories.is_empty() && preview.archive_repositories.is_empty());
    assert_eq!(preview.routes["files"], original_files);
    let descriptors = preview.manifest["course_descriptors"].as_array().unwrap();
    let added = descriptors
        .iter()
        .find(|d| d["course_code"] == "NEW2")
        .unwrap();
    assert_eq!(added["descriptor_id"], "course-code:NEW2");
    assert_eq!(added["repo_id"], "COURSE-A");
    assert_ne!(added["resource_group_id"], "group-a");
    let old = descriptors
        .iter()
        .find(|d| d["course_code"] == "OLD2")
        .unwrap();
    assert_eq!(old["status"], "historical-not-current");
    assert!(old["record_ids"].as_array().unwrap().is_empty());
    assert_eq!(old["historical_record_ids"], json!(["REC-OLD2"]));
    let records = preview.manifest["curriculum_records"].as_array().unwrap();
    assert_eq!(
        records
            .iter()
            .find(|r| r["source_plan"] == "plan-a" && r["course_code"] == "A1")
            .unwrap()["record_id"],
        "REC-OLD"
    );
    assert_eq!(
        records
            .iter()
            .find(|r| r["source_plan"] == "untouched-plan")
            .unwrap()["record_id"],
        "REC-OTHER"
    );
    assert_eq!(
        preview.manifest["curriculum_history"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(registry_dynamic_tree(&preview)
        .unwrap()
        .keys()
        .any(|p| p.starts_with("curriculum/history/")));
    assert!(preview.topology["repositories"]["COURSE-A"]["course_codes"]
        .as_array()
        .unwrap()
        .contains(&json!("NEW2")));
    assert!(preview.manifest["repositories"][0]["course_codes"]
        .as_array()
        .unwrap()
        .contains(&json!("NEW2")));
    assert!(preview.routes["course_code_routes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["course_code"] == "NEW2" && r["has_material"] == false));
    restarted.apply_repository_sync_preview(&preview).unwrap();
    assert!(restarted.curriculum_reviews().unwrap().is_empty());
    assert!(restarted.resume_curriculum_review(&path).is_err());
    restarted.apply_repository_sync_preview(&preview).unwrap();
    assert_eq!(
        restarted.manifest["curriculum_history"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(restarted.routes["files"], original_files);
    manager.reload().unwrap();
    assert_eq!(manager.workspace_identity(), restarted.workspace_identity());
}

#[test]
fn same_new_code_across_plans_has_one_explicit_binding() {
    let (_temporary, mut manager) = fixture();
    let mut session = review(&manager, |snapshot| {
        for plan in &mut snapshot.plans {
            plan.courses
                .push(json!({"course_code":"NEW2","course_name":"程序设计","credit":2}));
        }
    });
    session.accept_all().unwrap();
    assert_eq!(
        manager.pending_course_assignments(&session).unwrap().len(),
        1
    );
    manager
        .assign_course(
            &mut session,
            "NEW2",
            CourseAssignment::Existing {
                repo_id: "COURSE-A".into(),
            },
        )
        .unwrap();
    assert!(manager
        .assign_course(
            &mut session,
            "NEW2",
            CourseAssignment::Existing {
                repo_id: "COURSE-A".into()
            }
        )
        .is_err());
    let preview = manager.materialize_curriculum_update(&mut session).unwrap();
    assert_eq!(
        preview.manifest["course_descriptors"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|d| d["course_code"] == "NEW2")
            .count(),
        1
    );
    assert_eq!(
        preview.manifest["curriculum_records"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["course_code"] == "NEW2")
            .count(),
        2
    );
    assert_eq!(preview.routes["files"], manager.routes["files"]);
}

#[test]
fn bulk_assignments_validate_entire_batch_and_resume_without_file_sharing() {
    let (_temporary, mut manager) = fixture();
    let mut session = review(&manager, |snapshot| {
        snapshot.plans[0].courses.extend([
            json!({"course_code":"BULK1","course_name":"新课程一"}),
            json!({"course_code":"BULK2","course_name":"新课程二"}),
        ]);
    });
    session.accept_all().unwrap();
    let path = manager.save_curriculum_review(&session).unwrap();
    let before = fs::read(&path).unwrap();
    let valid = CourseAssignment::Existing { repo_id: "COURSE-A".into() };
    let invalid = BTreeMap::from([
        ("BULK1".into(), valid.clone()),
        ("BULK2".into(), CourseAssignment::Existing { repo_id: "missing".into() }),
    ]);
    assert!(manager.assign_courses(&mut session, invalid).is_err());
    assert!(session.assignments.is_empty());
    assert_eq!(fs::read(&path).unwrap(), before);
    manager.assign_courses(&mut session, BTreeMap::from([
        ("BULK1".into(), valid.clone()), ("BULK2".into(), valid),
    ])).unwrap();
    let mut resumed = manager.resume_curriculum_review(&path).unwrap();
    assert_eq!(resumed.assignments.len(), 2);
    assert!(manager.pending_course_assignments(&resumed).unwrap().is_empty());
    let preview = manager.materialize_curriculum_update(&mut resumed).unwrap();
    assert_eq!(preview.routes["files"], manager.routes["files"]);
    for code in ["BULK1", "BULK2"] {
        let route = preview.routes["course_code_routes"].as_array().unwrap()
            .iter().find(|route| route["course_code"] == code).unwrap();
        assert_eq!(route["has_material"], false);
    }
}

#[test]
fn assignment_rejects_old_unknown_and_control_codes_and_preserves_distinct_new_names() {
    let (_temporary, mut manager) = fixture();
    manager.manifest["repositories"].as_array_mut().unwrap().push(json!({"repo_id":"CONTROL","repo_type":"control","physical_repository_id":"control-physical"}));
    manager.topology["repositories"]["CONTROL"] = json!({"repo_id":"CONTROL","repo_type":"control","physical_repository_id":"control-physical"});
    atomic_json(&manager.manifest_path, &manager.manifest).unwrap();
    atomic_json(&manager.topology_path, &manager.topology).unwrap();
    let mut session = review(&manager, |snapshot| {
        snapshot.plans[0].courses.extend([
            json!({"course_code":"NEW2","course_name":"同名新课程"}),
            json!({"course_code":"NEW3","course_name":"同名新课程"}),
        ]);
    });
    session.accept_all().unwrap();
    for code in ["A1", "unknown"] {
        assert!(manager
            .assign_course(
                &mut session,
                code,
                CourseAssignment::Existing {
                    repo_id: "COURSE-A".into()
                }
            )
            .is_err());
    }
    assert!(manager
        .assign_course(
            &mut session,
            "NEW2",
            CourseAssignment::Existing {
                repo_id: "CONTROL".into()
            }
        )
        .is_err());
    assert!(manager
        .assign_course(
            &mut session,
            "NEW2",
            CourseAssignment::New { title: " ".into() }
        )
        .is_err());
    for code in ["NEW2", "NEW3"] {
        manager
            .assign_course(
                &mut session,
                code,
                CourseAssignment::New {
                    title: "同名新课程".into(),
                },
            )
            .unwrap();
    }
    let preview = manager.materialize_curriculum_update(&mut session).unwrap();
    assert_eq!(preview.create_repositories.len(), 2);
    let codes = preview.manifest["course_descriptors"].as_array().unwrap();
    let new2 = codes.iter().find(|c| c["course_code"] == "NEW2").unwrap();
    let new3 = codes.iter().find(|c| c["course_code"] == "NEW3").unwrap();
    assert_ne!(new2["repo_id"], new3["repo_id"]);
    assert_ne!(new2["resource_group_id"], new3["resource_group_id"]);
    let again = manager.materialize_curriculum_update(&mut session).unwrap();
    assert_eq!(preview.create_repositories, again.create_repositories);
}

#[test]
fn rejected_changes_are_not_assigned_and_disk_drift_blocks_resume_or_apply() {
    let (_temporary, mut manager) = fixture();
    let mut session = review(&manager, changed_plan);
    session.reject_all().unwrap();
    assert!(manager
        .pending_course_assignments(&session)
        .unwrap()
        .is_empty());
    assert!(manager
        .assign_course(
            &mut session,
            "NEW2",
            CourseAssignment::Existing {
                repo_id: "COURSE-A".into()
            }
        )
        .is_err());
    session.accept_all().unwrap();
    manager
        .assign_course(
            &mut session,
            "NEW2",
            CourseAssignment::Existing {
                repo_id: "COURSE-A".into(),
            },
        )
        .unwrap();
    let preview = manager.materialize_curriculum_update(&mut session).unwrap();
    let review_path = manager.save_curriculum_review(&session).unwrap();
    let mut changed = manager.manifest.clone();
    changed["external_edit"] = json!(true);
    atomic_json(&manager.manifest_path, &changed).unwrap();
    let disk_before = manager.disk_workspace_identity().unwrap();
    assert!(manager.resume_curriculum_review(&review_path).is_err());
    assert!(manager.save_curriculum_review(&session).is_err());
    assert!(manager.apply_repository_sync_preview(&preview).is_err());
    assert_eq!(manager.disk_workspace_identity().unwrap(), disk_before);
}

#[test]
fn changed_review_or_preview_cannot_be_applied() {
    let (_temporary, mut manager) = fixture();
    let mut session = review(&manager, changed_plan);
    session.accept_all().unwrap();
    manager
        .assign_course(
            &mut session,
            "NEW2",
            CourseAssignment::Existing {
                repo_id: "COURSE-A".into(),
            },
        )
        .unwrap();
    let mut preview = manager.materialize_curriculum_update(&mut session).unwrap();
    let review_path = manager.save_curriculum_review(&session).unwrap();
    let mut payload = read_json(&review_path).unwrap();
    payload["diff"]["changes"][0]["title"] = json!("tampered");
    atomic_json(&review_path, &payload).unwrap();
    assert!(manager.resume_curriculum_review(&review_path).is_err());
    preview.manifest["curriculum_plans"][0]["major_name"] = json!("tampered");
    let before = manager.disk_workspace_identity().unwrap();
    assert!(manager.apply_repository_sync_preview(&preview).is_err());
    assert_eq!(before, manager.disk_workspace_identity().unwrap());
}

#[test]
fn empty_execution_and_changed_requirements_preserve_other_scopes() {
    let (_temporary, mut manager) = fixture();
    let mut session = review(&manager, |snapshot| {
        snapshot.plans.push(CandidatePlan {
            plan_id:"hit:execution:2024:01:CS".into(),
            info:json!({"plan_id":"hit:execution:2024:01:CS","source_kind":"execution","entry_cohort":"2024","department_code":"01","school_name":"计算机学院","major_code":"CS","major_name":"计算机科学与技术","major_full_name":"计算机科学与技术【本】","program_type":"本","source_capture":{"complete":true},"academic_structure":{"modules":[{"name":"专业核心","credits":7}]}}),
            courses:Vec::new(),
        });
    });
    session.accept_all().unwrap();
    let preview = manager.materialize_curriculum_update(&mut session).unwrap();
    assert_eq!(preview.plan_count, 3);
    assert_eq!(preview.record_count, 3);
    assert!(preview.manifest["curriculum_plans"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["entry_cohort"] == "2024"
            && p["academic_structure"]["modules"][0]["credits"] == 7));
    assert_eq!(preview.routes["files"], manager.routes["files"]);
}

#[test]
fn joint_preview_combines_disjoint_reviews_without_changing_production() {
    let (_temporary, mut manager) = fixture();
    let before = manager.disk_workspace_identity().unwrap();
    let mut curriculum = review(&manager, |snapshot| {
        snapshot.plans.iter_mut().find(|p| p.plan_id == "plan-a").unwrap().courses[0]["credit"] = json!(5);
    });
    let mut execution = review(&manager, |snapshot| {
        snapshot.plans.push(CandidatePlan {
            plan_id: "extra-plan".into(), info: json!({"major_name":"新增专业"}),
            courses: vec![json!({"course_code":"B1","course_name":"新增一"}), json!({"course_code":"B2","course_name":"新增二"})],
        });
    });
    curriculum.accept_all().unwrap();
    execution.accept_all().unwrap();
    manager.assign_courses(&mut execution, BTreeMap::from([
        ("B1".into(), CourseAssignment::NewGroup { repo_id:"COURSES-NEW".into(), title:"新学院课程".into() }),
        ("B2".into(), CourseAssignment::NewGroup { repo_id:"COURSES-NEW".into(), title:"新学院课程".into() }),
    ])).unwrap();
    let preview = manager.materialize_curriculum_updates(&mut [curriculum.clone(), execution]).unwrap();
    assert_eq!(preview.plan_count, 3);
    assert_eq!(preview.create_repositories, vec!["COURSES-NEW"]);
    assert_eq!(preview.manifest["curriculum_history"].as_array().unwrap().len(), 2);
    assert_eq!(preview.routes["files"], manager.routes["files"]);
    assert_eq!(manager.disk_workspace_identity().unwrap(), before);
    let records = preview.manifest["curriculum_records"].as_array().unwrap();
    assert!(records.iter().any(|r| r["record_id"] == "REC-OLD" && r["credit"] == 5));
    let descriptors = preview.manifest["course_descriptors"].as_array().unwrap();
    let b1 = descriptors.iter().find(|r| r["course_code"] == "B1").unwrap();
    let b2 = descriptors.iter().find(|r| r["course_code"] == "B2").unwrap();
    assert_eq!(b1["repo_id"], b2["repo_id"]);
    assert_ne!(b1["resource_group_id"], b2["resource_group_id"]);
    assert!(manager.materialize_curriculum_updates(&mut [curriculum.clone(), curriculum]).is_err());
}

#[test]
fn failed_multi_file_preflight_keeps_every_original_in_place() {
    let temporary = TempDir::new().unwrap();
    let a = temporary.path().join("a.json");
    let b = temporary.path().join("b.json");
    atomic_json(&a, &json!({"a":1})).unwrap();
    atomic_json(&b, &json!({"b":2})).unwrap();
    atomic_json(
        &b.with_extension("json.update-backup"),
        &json!({"old":true}),
    )
    .unwrap();
    assert!(atomic_json_many(&[(&a, &json!({"a":3})), (&b, &json!({"b":4}))]).is_err());
    assert_eq!(read_json(&a).unwrap(), json!({"a":1}));
    assert_eq!(read_json(&b).unwrap(), json!({"b":2}));
    assert!(!a.with_extension("json.update-backup").exists());
}

fn offline_selection(kind: PlanKind, grade: &str) -> crate::jwts::CrawlSelection {
    crate::jwts::CrawlSelection {
        grade: grade.into(),
        college_code: "01".into(),
        college_name: "计算机学院".into(),
        major_code: "CS".into(),
        major_name: "计算机科学与技术【本】".into(),
        kind,
    }
}

fn captured_plan(selection: &crate::jwts::CrawlSelection) -> CandidatePlan {
    let (source_kind, grade_key, endpoint, identity_key) = match selection.kind {
        PlanKind::Curriculum => ("curriculum", "pageBbh", "/pyfa/queryPykc", "plan_version"),
        PlanKind::Execution => ("execution", "pageNj", "/zxjh/queryZxkc", "entry_cohort"),
    };
    let mut scope = serde_json::Map::new();
    scope.insert(grade_key.into(), json!(selection.grade));
    scope.insert("pageYxdm".into(), json!(selection.college_code));
    scope.insert("pageZydm".into(), json!(selection.major_code));
    let mut info = json!({
        "plan_id":selection.plan_id(),"campus":"hit","source_kind":source_kind,
        "department_code":selection.college_code,"school_name":selection.college_name,
        "major_code":selection.major_code,"major_name":"计算机科学与技术",
        "major_full_name":selection.major_name,"program_type":"本",
        "source_capture":{"complete":true,"endpoint":endpoint,"pages":[1],"rows":1,
            "unpaged_rows":0,"scope":scope,"checks":{"authenticated":true,
            "expected_headers":true,"pagination_consistent":true,"confirmed_empty":false}}
    });
    info[identity_key] = json!(selection.grade);
    if selection.kind == PlanKind::Execution {
        info["academic_structure"] = json!({"module_tree":[],"module_details":[],
            "direction_requirements":[],"notes":{},"double_degree_minor_requirements":{
            "courses":[],"metadata":{},"source_capture":{"complete":true,
            "endpoint":"/zxjh/queryZxkcSxw","pages":[0],"rows":0,"unpaged_rows":0}}});
    }
    CandidatePlan {
        plan_id: selection.plan_id(),
        info,
        courses: vec![json!({"course_code":"A1","course_name":"程序设计","credit":4,
            "source_section":if selection.kind == PlanKind::Curriculum {
                "curriculum-main"
            } else {
                "execution-main"
            }})],
    }
}

fn offline_session(kind: PlanKind, base_url: &str) -> UpdateSession {
    UpdateSession {
        client: None,
        kind,
        catalog: CurriculumCatalog::default(),
        selections: Vec::new(),
        candidate: None,
        diff: None,
        decisions: curriculum::DecisionSet::default(),
        assignments: BTreeMap::new(),
        status: CurriculumUpdateStatus {
            stage: "captured".into(),
            message: "已完成真实采集".into(),
            base_url: base_url.into(),
            updated_at: "capture-status".into(),
            ..Default::default()
        },
    }
}

#[test]
fn offline_snapshot_preserves_unselected_plan_and_legacy_identity_and_resumes() {
    let (temporary, manager) = fixture();
    let selection = offline_selection(PlanKind::Curriculum, "2022版");
    let mut session = offline_session(PlanKind::Curriculum, "https://jwts.example.edu/");
    manager.stage_curriculum_snapshot(&mut session, vec![selection.clone()], CandidateSnapshot {
        generated_at:"2026-09-06T01:02:03Z".into(), base_url:"https://jwts.example.edu".into(),
        plans:vec![captured_plan(&selection)]
    }).unwrap();
    let candidate = session.candidate.as_ref().unwrap();
    assert_eq!(candidate.generated_at, "2026-09-06T01:02:03Z");
    assert!(candidate.plans.iter().any(|plan| plan.plan_id == "plan-a"));
    assert!(candidate.plans.iter().any(|plan| plan.plan_id == "untouched-plan"));
    assert_eq!(candidate.plans.len(), 2);
    let path = PathBuf::from(manager.curriculum_reviews().unwrap().pop().unwrap().path);
    let mut restarted = Manager::new(temporary.path());
    restarted.reload().unwrap();
    let resumed = restarted.resume_curriculum_review(&path).unwrap();
    assert!(resumed.client.is_none());
    assert_eq!(resumed.candidate.unwrap().generated_at, "2026-09-06T01:02:03Z");
}

#[test]
fn snapshot_plan_set_must_be_complete_exact_and_unique() {
    for case in ["missing", "extra", "duplicate"] {
        let (_temporary, manager) = fixture();
        let selection = offline_selection(PlanKind::Curriculum, "2022版");
        let mut plans = if case == "missing" { Vec::new() } else { vec![captured_plan(&selection)] };
        if case == "extra" {
            plans.push(captured_plan(&offline_selection(PlanKind::Curriculum, "2023版")));
        } else if case == "duplicate" {
            plans.push(captured_plan(&selection));
        }
        let mut session = offline_session(PlanKind::Curriculum, "https://jwts.example.edu");
        assert!(manager.stage_curriculum_snapshot(&mut session, vec![selection], CandidateSnapshot {
            generated_at:"captured".into(), base_url:"https://jwts.example.edu".into(), plans
        }).is_err(), "{case}");
        assert!(manager.curriculum_reviews().unwrap().is_empty(), "{case}");
    }
}

#[test]
fn snapshot_rejects_kind_source_and_origin_mismatches() {
    let base_url = "https://jwts.example.edu";
    let selection = offline_selection(PlanKind::Curriculum, "2022版");
    let (_temporary, manager) = fixture();
    let mut session = offline_session(PlanKind::Execution, base_url);
    assert!(manager.stage_curriculum_snapshot(&mut session, vec![selection.clone()], CandidateSnapshot {
        generated_at:"captured".into(), base_url:base_url.into(), plans:vec![captured_plan(&selection)]
    }).is_err());
    let (_temporary, manager) = fixture();
    let mut wrong_source = captured_plan(&selection);
    wrong_source.info["source_kind"] = json!("execution");
    let mut session = offline_session(PlanKind::Curriculum, base_url);
    assert!(manager.stage_curriculum_snapshot(&mut session, vec![selection.clone()], CandidateSnapshot {
        generated_at:"captured".into(), base_url:base_url.into(), plans:vec![wrong_source]
    }).is_err());
    let (_temporary, manager) = fixture();
    let mut session = offline_session(PlanKind::Curriculum, base_url);
    assert!(manager.stage_curriculum_snapshot(&mut session, vec![selection.clone()], CandidateSnapshot {
        generated_at:"captured".into(), base_url:"https://other.example.edu".into(),
        plans:vec![captured_plan(&selection)]
    }).is_err());
    let (_temporary, manager) = fixture();
    let mut session = offline_session(PlanKind::Curriculum, "https://jwts.example.edu/service-a");
    assert!(manager.stage_curriculum_snapshot(&mut session, vec![selection.clone()], CandidateSnapshot {
        generated_at:"captured".into(), base_url:"https://jwts.example.edu/service-b".into(),
        plans:vec![captured_plan(&selection)]
    }).is_err());
    let (_temporary, manager) = fixture();
    let mut session = offline_session(PlanKind::Curriculum, base_url);
    assert!(manager.stage_curriculum_snapshot(&mut session, vec![selection.clone()], CandidateSnapshot {
        generated_at:"captured".into(), base_url:"https://user@jwts.example.edu".into(),
        plans:vec![captured_plan(&selection)]
    }).is_err());
    assert!(manager.curriculum_reviews().unwrap().is_empty());
}

#[test]
fn execution_cohort_does_not_replace_same_value_curriculum_version() {
    let (_temporary, manager) = fixture();
    let selection = offline_selection(PlanKind::Execution, "2022");
    let mut session = offline_session(PlanKind::Execution, "https://jwts.example.edu");
    manager.stage_curriculum_snapshot(&mut session, vec![selection.clone()], CandidateSnapshot {
        generated_at:"captured".into(), base_url:"https://jwts.example.edu/".into(),
        plans:vec![captured_plan(&selection)]
    }).unwrap();
    let candidate = session.candidate.unwrap();
    assert!(candidate.plans.iter().any(|plan| plan.plan_id == "plan-a"));
    assert!(candidate.plans.iter().any(|plan| plan.plan_id.starts_with("hit:execution:")));
}

#[test]
fn snapshot_rejects_tampered_capture_before_writing_review() {
    let (_temporary, manager) = fixture();
    let selection = offline_selection(PlanKind::Curriculum, "2022版");
    let mut plan = captured_plan(&selection);
    plan.info["source_capture"]["scope"]["pageZydm"] = json!("IMPOSTOR");
    let mut session = offline_session(PlanKind::Curriculum, "https://jwts.example.edu");
    assert!(manager.stage_curriculum_snapshot(&mut session, vec![selection], CandidateSnapshot {
        generated_at:"captured".into(), base_url:"https://jwts.example.edu".into(), plans:vec![plan]
    }).is_err());
    assert!(manager.curriculum_reviews().unwrap().is_empty());
}
