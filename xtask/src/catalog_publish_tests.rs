use super::*;
use crate::catalog_publish::fakes::{FakeR2, FakeRelease, FakeReleases};
use crate::catalog_publish::{
    GenerationPlan, PublishOptions, plan_generation, publish_generation, sha256_hex,
};
use std::collections::BTreeMap;

const GENERATION: u64 = 9;
const PREVIOUS_MASTER: &str = "catalog-v3.8.ffffffffffff.min.json.zst";
const MASTER: &str = "catalog-v3.9.0123456789ab.min.json.zst";
const PACK: &str = "trash.abcdefabcdef.min.json.zst";
const TARGET: &str = "0000000000000000000000000000000000000abc";

struct Fixture {
    _dir: tempfile::TempDir,
    ctx: TaskContext,
    dist: PathBuf,
}

fn write(path: &Path, bytes: impl AsRef<[u8]>) {
    fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
    fs::write(path, bytes).expect("write fixture");
}

fn generation_mirror(name: &str) -> String {
    github_release_asset_url(
        OFFICIAL_GITHUB_REPO,
        &central_catalog_v3_generation_tag(GENERATION),
        name,
    )
}

fn redirect_for(
    master: &str,
    generation: u64,
    mirror: impl Fn(&str) -> String,
) -> CatalogV3Redirect {
    let base = format!(
        "{}/{}/{generation}",
        public_catalog_base_url(),
        central_catalog_v3_path_prefix()
    );
    let signature = signature_bundle_file_name(master);
    CatalogV3Redirect {
        schema_version: CATALOG_V3_REDIRECT_SCHEMA.to_string(),
        catalog_version: generation,
        artifacts: vec![CatalogV3RedirectArtifact {
            url: format!("{base}/{master}"),
            mirror_urls: vec![mirror(master)],
            signature_url: format!("{base}/{signature}"),
            signature_mirror_urls: vec![mirror(&signature)],
        }],
    }
}

fn fixture_with_mirror(mirror: impl Fn(&str) -> String + Copy) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = TaskContext {
        repo_root: dir.path().to_path_buf(),
    };
    let dist = dir.path().join("dist");

    for name in [MASTER, PACK] {
        write(&dist.join(name), format!("{name} bytes"));
        write(
            &dist.join(signature_bundle_file_name(name)),
            format!("{name} bundle"),
        );
        write(
            &dist.join(format!("{name}.sigstore.json")),
            format!("{name} sigstore"),
        );
    }

    let pack_base = format!("{}/rule-packs/trash/v1.0.0", public_catalog_base_url());
    let pack_artifact = CatalogV3Artifact {
        url: format!("{pack_base}/{PACK}"),
        mirror_urls: vec![mirror(PACK)],
        signature_url: format!("{pack_base}/{}", signature_bundle_file_name(PACK)),
        signature_mirror_urls: vec![mirror(&signature_bundle_file_name(PACK))],
        digests: file_digests(&dist.join(PACK)).expect("digests"),
    };
    write(
        &dist.join(PREPARED_RULE_PACK_ARTIFACTS),
        serde_json::to_vec(&[&pack_artifact]).expect("serialize"),
    );
    let catalog = CatalogV3 {
        schema_version: CATALOG_V3_SCHEMA.to_string(),
        catalog_version: GENERATION,
        plugins: Vec::new(),
        community_sources: Vec::new(),
        rule_packs: vec![CatalogV3RulePackEntry {
            id: "trash".to_string(),
            name: "Trash".to_string(),
            description: "Fixture".to_string(),
            author: "fixture".to_string(),
            releases: vec![CatalogV3RulePackRelease {
                version: "1.0.0".to_string(),
                min_scryer_version: None,
                rule_pack_digests: Vec::new(),
                rule_pack_bytes: None,
                _customizable: true,
                artifacts: vec![pack_artifact],
            }],
        }],
    };
    write(
        &dist.join(CATALOG_V3_SNIPPET_JSON),
        serde_json::to_vec_pretty(&catalog).expect("serialize"),
    );
    write(
        &dist.join(CATALOG_V3_MODERN_REDIRECT_JSON),
        serde_json::to_vec_pretty(&redirect_for(MASTER, GENERATION, mirror)).expect("serialize"),
    );
    write(
        &dist.join("catalog-v3.modern.redirect.bundle.json"),
        "redirect bundle g9",
    );
    write(
        &dist.join("catalog-v3.modern.redirect.sigstore.json"),
        "redirect bundle g9",
    );
    Fixture {
        _dir: dir,
        ctx,
        dist,
    }
}

fn fixture() -> Fixture {
    fixture_with_mirror(generation_mirror)
}

fn previous_redirect_bytes() -> Vec<u8> {
    serde_json::to_vec_pretty(&redirect_for(PREVIOUS_MASTER, 8, |name| {
        github_release_asset_url(OFFICIAL_GITHUB_REPO, "catalog/v3", name)
    }))
    .expect("serialize")
}

/// Live state before the publish: R2 serves generation 8 and the stable
/// release carries its redirect.
fn live_stores(plan: &GenerationPlan) -> (FakeR2, FakeReleases) {
    let r2 = FakeR2::default();
    r2.objects
        .borrow_mut()
        .insert(plan.redirect.json_r2_url.clone(), previous_redirect_bytes());
    r2.objects.borrow_mut().insert(
        plan.redirect.bundle_r2_url.clone(),
        b"redirect bundle g8".to_vec(),
    );
    let releases = FakeReleases::default();
    releases.releases.borrow_mut().insert(
        plan.redirect_tag.clone(),
        FakeRelease {
            assets: stable_release_assets(),
        },
    );
    (r2, releases)
}

fn stable_release_assets() -> BTreeMap<String, Vec<u8>> {
    BTreeMap::from([
        (
            CATALOG_V3_MODERN_REDIRECT_JSON.to_string(),
            previous_redirect_bytes(),
        ),
        (
            "catalog-v3.modern.redirect.bundle.json".to_string(),
            b"redirect bundle g8".to_vec(),
        ),
        (
            "catalog-v3.modern.redirect.sigstore.json".to_string(),
            b"redirect bundle g8".to_vec(),
        ),
    ])
}

fn options() -> PublishOptions {
    PublishOptions {
        expected_merge_base: Some(PREVIOUS_MASTER.to_string()),
        target_commit: TARGET.to_string(),
    }
}

fn plan(fixture: &Fixture) -> GenerationPlan {
    plan_generation(&fixture.ctx, &fixture.dist).expect("plan generation")
}

fn local(plan: &GenerationPlan, name: &str) -> Vec<u8> {
    let asset = plan
        .assets
        .iter()
        .find(|asset| asset.name == name)
        .expect("asset in plan");
    fs::read(&asset.local).expect("read asset")
}

fn assert_redirects_unmoved(plan: &GenerationPlan, r2: &FakeR2, releases: &FakeReleases) {
    assert_eq!(
        r2.objects.borrow().get(&plan.redirect.json_r2_url),
        Some(&previous_redirect_bytes()),
        "R2 redirect must not move"
    );
    assert_eq!(
        releases.releases.borrow()[&plan.redirect_tag].assets,
        stable_release_assets(),
        "GitHub redirect must not move"
    );
}

#[test]
fn plan_lists_every_hashed_file_with_its_sigstore_bundle() {
    let fixture = fixture();
    let plan = plan(&fixture);

    assert_eq!(plan.generation, GENERATION);
    assert_eq!(
        plan.generation_tag,
        central_catalog_v3_generation_tag(GENERATION)
    );
    assert_eq!(plan.master_name, MASTER);
    let names = plan
        .assets
        .iter()
        .map(|asset| asset.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec![
            MASTER,
            &signature_bundle_file_name(MASTER),
            &format!("{MASTER}.sigstore.json"),
            PACK,
            &signature_bundle_file_name(PACK),
            &format!("{PACK}.sigstore.json"),
        ]
    );
    assert!(
        plan.assets
            .iter()
            .all(|asset| !asset.name.contains("redirect")),
        "redirect files are never generation assets"
    );
}

#[test]
fn plan_rejects_mirrors_that_name_another_release() {
    let fixture = fixture_with_mirror(|name| {
        github_release_asset_url(OFFICIAL_GITHUB_REPO, "catalog/v3", name)
    });
    let error = plan_generation(&fixture.ctx, &fixture.dist)
        .expect_err("mirrors on the stable release must be refused");
    assert!(error.to_string().contains("not the generation release"));
}

#[test]
fn fresh_publish_uploads_everything_and_moves_both_redirects() {
    let fixture = fixture();
    let plan = plan(&fixture);
    let (r2, releases) = live_stores(&plan);

    let report = publish_generation(&plan, &r2, &releases, &options()).expect("publish");

    assert_eq!(report.r2_uploaded.len(), plan.assets.len());
    assert_eq!(report.github_uploaded.len(), plan.assets.len());
    assert!(report.generation_release_created);
    assert!(report.r2_redirect_moved);
    for asset in &plan.assets {
        assert_eq!(
            r2.objects.borrow().get(&asset.r2_url),
            Some(&local(&plan, &asset.name))
        );
    }

    let created = releases.created.borrow();
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].tag, plan.generation_tag);
    assert!(created[0].prerelease);
    assert_eq!(created[0].target, TARGET);
    assert!(created[0].title.contains(&GENERATION.to_string()));

    let all = releases.releases.borrow();
    let generation_release = &all[&plan.generation_tag];
    assert_eq!(generation_release.assets.len(), plan.assets.len());

    let stable = &all[&plan.redirect_tag];
    assert_eq!(
        stable.assets.keys().cloned().collect::<Vec<_>>(),
        vec![
            "catalog-v3.modern.redirect.bundle.json".to_string(),
            CATALOG_V3_MODERN_REDIRECT_JSON.to_string(),
            "catalog-v3.modern.redirect.sigstore.json".to_string(),
        ],
        "only the redirect files ever land on the stable release"
    );
    assert_eq!(
        stable.assets[CATALOG_V3_MODERN_REDIRECT_JSON],
        fs::read(&plan.redirect.json).expect("read redirect")
    );
    assert_eq!(
        r2.objects.borrow().get(&plan.redirect.json_r2_url),
        Some(&fs::read(&plan.redirect.json).expect("read redirect"))
    );

    // The redirect moves after every hashed asset reached both stores.
    let puts = r2.puts.borrow();
    assert_eq!(
        puts[puts.len() - 2..],
        [
            plan.redirect.json_r2_url.clone(),
            plan.redirect.bundle_r2_url.clone()
        ]
    );
    let uploads = releases.uploads.borrow();
    assert_eq!(uploads.len(), 2);
    assert_eq!(uploads[0].0, plan.generation_tag);
    assert!(!uploads[0].2, "generation assets are never clobbered");
    assert_eq!(uploads[1].0, plan.redirect_tag);
    assert!(uploads[1].2, "the redirect replaces the previous one");
}

#[test]
fn resume_with_complete_r2_and_partial_github_uploads_only_what_is_missing() {
    let fixture = fixture();
    let plan = plan(&fixture);
    let (r2, releases) = live_stores(&plan);
    for asset in &plan.assets {
        r2.objects
            .borrow_mut()
            .insert(asset.r2_url.clone(), local(&plan, &asset.name));
    }
    let published = [MASTER.to_string(), signature_bundle_file_name(MASTER)];
    releases.releases.borrow_mut().insert(
        plan.generation_tag.clone(),
        FakeRelease {
            assets: published
                .iter()
                .map(|name| (name.clone(), local(&plan, name)))
                .collect(),
        },
    );

    let report = publish_generation(&plan, &r2, &releases, &options()).expect("resume");

    assert!(report.r2_uploaded.is_empty());
    assert_eq!(report.r2_skipped.len(), plan.assets.len());
    assert!(!report.generation_release_created);
    assert_eq!(report.github_skipped, published.to_vec());
    assert_eq!(report.github_uploaded.len(), plan.assets.len() - 2);
    assert!(
        r2.puts
            .borrow()
            .iter()
            .all(|url| url == &plan.redirect.json_r2_url || url == &plan.redirect.bundle_r2_url),
        "no hashed asset is re-uploaded to R2"
    );
    assert_eq!(
        releases.releases.borrow()[&plan.generation_tag]
            .assets
            .len(),
        plan.assets.len()
    );
}

#[test]
fn a_conflicting_digest_on_r2_is_rejected_before_anything_moves() {
    let fixture = fixture();
    let plan = plan(&fixture);
    let (r2, releases) = live_stores(&plan);
    r2.objects
        .borrow_mut()
        .insert(plan.assets[3].r2_url.clone(), b"other bytes".to_vec());

    let error = publish_generation(&plan, &r2, &releases, &options())
        .expect_err("different bytes under a hashed name must fail");
    let message = error.to_string();
    assert!(message.contains(PACK), "{message}");
    assert!(message.contains(&sha256_hex(b"other bytes")), "{message}");
    assert!(
        message.contains(&sha256_hex(&local(&plan, PACK))),
        "{message}"
    );
    assert!(r2.puts.borrow().is_empty(), "nothing is written");
    assert!(releases.created.borrow().is_empty());
    assert!(releases.uploads.borrow().is_empty());
    assert_redirects_unmoved(&plan, &r2, &releases);
}

#[test]
fn a_conflicting_digest_on_github_is_rejected_before_anything_moves() {
    let fixture = fixture();
    let plan = plan(&fixture);
    let (r2, releases) = live_stores(&plan);
    releases.releases.borrow_mut().insert(
        plan.generation_tag.clone(),
        FakeRelease {
            assets: BTreeMap::from([(MASTER.to_string(), b"tampered".to_vec())]),
        },
    );

    let error = publish_generation(&plan, &r2, &releases, &options())
        .expect_err("different bytes on the generation release must fail");
    let message = error.to_string();
    assert!(message.contains(MASTER), "{message}");
    assert!(message.contains(&plan.generation_tag), "{message}");
    assert!(message.contains(&sha256_hex(b"tampered")), "{message}");
    assert!(r2.puts.borrow().is_empty(), "nothing is written");
    assert!(releases.uploads.borrow().is_empty());
    assert_redirects_unmoved(&plan, &r2, &releases);
}

#[test]
fn an_existing_generation_release_with_identical_assets_is_not_reuploaded() {
    let fixture = fixture();
    let plan = plan(&fixture);
    let (r2, releases) = live_stores(&plan);
    releases.releases.borrow_mut().insert(
        plan.generation_tag.clone(),
        FakeRelease {
            assets: plan
                .assets
                .iter()
                .map(|asset| (asset.name.clone(), local(&plan, &asset.name)))
                .collect(),
        },
    );

    let report = publish_generation(&plan, &r2, &releases, &options()).expect("publish");

    assert!(report.github_uploaded.is_empty());
    assert_eq!(report.github_skipped.len(), plan.assets.len());
    assert!(releases.created.borrow().is_empty());
    let uploads = releases.uploads.borrow();
    assert_eq!(uploads.len(), 1, "only the redirect upload happens");
    assert_eq!(uploads[0].0, plan.redirect_tag);
}

#[test]
fn a_compare_and_set_mismatch_aborts_before_the_redirect_moves() {
    let fixture = fixture();
    let plan = plan(&fixture);
    let (r2, releases) = live_stores(&plan);
    let concurrent = serde_json::to_vec_pretty(&redirect_for(
        "catalog-v3.9.999999999999.min.json.zst",
        9,
        generation_mirror,
    ))
    .expect("serialize");
    r2.objects
        .borrow_mut()
        .insert(plan.redirect.json_r2_url.clone(), concurrent.clone());

    let error = publish_generation(&plan, &r2, &releases, &options())
        .expect_err("a moved redirect must stop the publish");
    assert!(error.to_string().contains("another publish landed"));
    assert_eq!(
        r2.objects.borrow().get(&plan.redirect.json_r2_url),
        Some(&concurrent),
        "the concurrent redirect stays"
    );
    assert!(
        !r2.puts.borrow().contains(&plan.redirect.json_r2_url)
            && !r2.puts.borrow().contains(&plan.redirect.bundle_r2_url)
    );
    assert_eq!(
        releases.releases.borrow()[&plan.redirect_tag].assets,
        stable_release_assets()
    );
}

#[test]
fn a_github_redirect_failure_after_r2_moved_reports_and_a_rerun_converges() {
    let fixture = fixture();
    let plan = plan(&fixture);
    let (r2, mut releases) = live_stores(&plan);
    releases.fail_uploads_to = Some(plan.redirect_tag.clone());

    let error = publish_generation(&plan, &r2, &releases, &options())
        .expect_err("the stable release upload fails");
    let message = error.to_string();
    assert!(message.contains("R2 now serves generation 9"), "{message}");
    assert!(message.contains("was not updated"), "{message}");
    assert_eq!(
        r2.objects.borrow().get(&plan.redirect.json_r2_url),
        Some(&fs::read(&plan.redirect.json).expect("read redirect"))
    );
    assert_eq!(
        releases.releases.borrow()[&plan.redirect_tag].assets,
        stable_release_assets(),
        "the GitHub redirect is unchanged"
    );

    // The rerun finds its own redirect live; that passes the compare-and-set.
    releases.fail_uploads_to = None;
    let report = publish_generation(&plan, &r2, &releases, &options()).expect("rerun converges");
    assert!(report.r2_uploaded.is_empty());
    assert!(report.github_uploaded.is_empty());
    assert!(!report.r2_redirect_moved, "R2 already serves the redirect");
    assert_eq!(
        releases.releases.borrow()[&plan.redirect_tag].assets[CATALOG_V3_MODERN_REDIRECT_JSON],
        fs::read(&plan.redirect.json).expect("read redirect")
    );
}

#[test]
fn publish_v3_generation_parses_its_arguments() {
    let cli = Cli::try_parse_from([
        "xtask",
        "catalog",
        "publish-v3-generation",
        "--dist",
        "dist/catalog-v3",
        "--expected-merge-base",
        PREVIOUS_MASTER,
    ])
    .expect("parse publish-v3-generation");
    match cli.command {
        Commands::Catalog(CatalogArgs {
            command: CatalogCommand::PublishV3Generation(args),
        }) => {
            assert_eq!(args.dist, PathBuf::from("dist/catalog-v3"));
            assert_eq!(args.expected_merge_base.as_deref(), Some(PREVIOUS_MASTER));
        }
        _ => panic!("expected catalog publish-v3-generation"),
    }
}
