//! Publishing one prepared central catalog generation.
//!
//! A generation is the set of hashed, immutable assets `catalog prepare-v3`
//! staged (the master catalog and any newly prepared rule-pack artifacts, each
//! with its signature bundle and sigstore bundle) plus the modern redirect
//! that points clients at them. Hashed assets go to R2 and to a GitHub release
//! of their own (`<redirect tag>-g<N>`); only the redirect lands on the stable
//! redirect release, so that release never accumulates assets.
//!
//! The phases run strictly in order and the redirect moves last:
//!
//! 1. hashed assets to R2,
//! 2. hashed assets to the generation release,
//! 3. both copies downloaded back through their public URLs and compared,
//! 4. the modern redirect to R2, after a compare-and-set on the live one,
//! 5. the modern redirect to the stable GitHub release.
//!
//! Every phase is idempotent. An asset already present with the same bytes is
//! skipped; one present with different bytes fails the run before anything
//! moves. Nothing is ever deleted except the redirect files replaced in
//! phase 5.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};

use crate::{
    CATALOG_V3_MODERN_REDIRECT_JSON, CATALOG_V3_SNIPPET_JSON, CatalogV3Redirect,
    OFFICIAL_GITHUB_REPO, R2Config, TaskContext, central_catalog_v3_generation_tag,
    central_catalog_v3_path_prefix, central_catalog_v3_release_tag, github_release_asset_url, ok,
    prepared_rule_pack_artifacts_for_upload, public_catalog_base_url, read_catalog_v3_from_path,
    redirect_signature_bundle_file_name, run_capture, run_checked, step, upload_file_to_r2,
    url_file_name, url_path_key, warn,
};

/// The R2 bucket behind the public CDN base URL.
pub trait R2Store {
    /// The object stored behind `public_url`, read from the bucket itself so
    /// no CDN cache can answer for it. `None` when the object does not exist.
    fn get(&self, public_url: &str) -> Result<Option<Vec<u8>>>;
    fn put(&self, local: &Path, public_url: &str) -> Result<()>;
    /// Download through the public URL, the way clients do.
    fn fetch_public(&self, public_url: &str) -> Result<Vec<u8>>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewRelease {
    pub tag: String,
    pub title: String,
    pub notes: String,
    pub target: String,
    pub prerelease: bool,
}

/// GitHub releases on the official repository.
pub trait ReleaseStore {
    /// Asset names on the release, or `None` when the release does not exist.
    fn asset_names(&self, tag: &str) -> Result<Option<Vec<String>>>;
    /// Require `tag` to be an existing annotated tag whose signature GitHub
    /// verifies and which points directly at `target`. A release is only ever
    /// created on such a tag; the release API never creates one.
    fn validate_tag(&self, tag: &str, target: &str) -> Result<()>;
    fn create_release(&self, release: &NewRelease) -> Result<()>;
    fn download_asset(&self, tag: &str, name: &str) -> Result<Vec<u8>>;
    fn upload_assets(&self, tag: &str, files: &[PathBuf], clobber: bool) -> Result<()>;
    /// Download through the public release download URL, the way clients do.
    fn fetch_public(&self, tag: &str, name: &str) -> Result<Vec<u8>>;
}

/// One immutable file of the generation.
#[derive(Clone, Debug)]
pub struct HashedAsset {
    pub name: String,
    pub local: PathBuf,
    pub r2_url: String,
    /// For a signature companion (`.bundle.zst`, `.sigstore.json`): the name
    /// of the data asset it signs. Signing is not deterministic, so a
    /// companion already published beside byte-identical data is adopted
    /// instead of being treated as a conflict.
    pub signature_of: Option<String>,
}

#[derive(Clone, Debug)]
pub struct RedirectFiles {
    pub json: PathBuf,
    pub json_r2_url: String,
    pub bundle: PathBuf,
    pub bundle_r2_url: String,
    pub sigstore: PathBuf,
}

#[derive(Clone, Debug)]
pub struct GenerationPlan {
    pub generation: u64,
    pub generation_tag: String,
    pub redirect_tag: String,
    /// File name of this generation's master catalog: the last rung of the
    /// redirect being published.
    pub master_name: String,
    pub assets: Vec<HashedAsset>,
    pub redirect: RedirectFiles,
}

#[derive(Clone, Debug, Default)]
pub struct PublishOptions {
    /// File name of the master catalog the generation was merged onto. The
    /// live redirect must still point at it when the redirect moves.
    pub expected_merge_base: Option<String>,
    /// Commit the generation release is created at, when it does not exist.
    pub target_commit: String,
}

/// What a run changed, for logs and tests.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PublishReport {
    pub r2_uploaded: Vec<String>,
    pub r2_skipped: Vec<String>,
    pub github_uploaded: Vec<String>,
    pub github_skipped: Vec<String>,
    pub generation_release_created: bool,
    pub r2_redirect_moved: bool,
    /// Signature companions whose already-published bytes replaced the
    /// freshly signed local copy, as `<name> from <store>`.
    pub adopted_signatures: Vec<String>,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn read_local(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).with_context(|| format!("failed to read {}", path.display()))
}

fn require_file(path: &Path) -> Result<()> {
    if !path.is_file() {
        bail!("missing prepared catalog file {}", path.display());
    }
    Ok(())
}

/// Release asset names are matched as `gh` glob patterns; keep them literal.
fn validate_asset_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        bail!("unexpected catalog asset name '{name}'");
    }
    Ok(())
}

/// Read a prepared `catalog prepare-v3` output directory into a plan, and
/// refuse one whose mirror URLs name a different generation release.
pub fn plan_generation(ctx: &TaskContext, dist: &Path) -> Result<GenerationPlan> {
    let redirect_path = dist.join(CATALOG_V3_MODERN_REDIRECT_JSON);
    let redirect: CatalogV3Redirect = serde_json::from_slice(&read_local(&redirect_path)?)
        .with_context(|| format!("failed to parse {}", redirect_path.display()))?;
    let generation = redirect.catalog_version;
    let catalog = read_catalog_v3_from_path(ctx, &dist.join(CATALOG_V3_SNIPPET_JSON))?;
    if catalog.catalog_version != generation {
        bail!(
            "prepared catalog is generation {} but its modern redirect is generation {generation}",
            catalog.catalog_version
        );
    }
    let generation_tag = central_catalog_v3_generation_tag(generation);
    let mirror_for =
        |name: &str| github_release_asset_url(OFFICIAL_GITHUB_REPO, &generation_tag, name);

    let mut assets = Vec::new();
    let mut names = BTreeSet::new();
    let mut push = |name: String,
                    r2_url: String,
                    mirrors: Option<&[String]>,
                    signature_of: Option<&str>|
     -> Result<()> {
        validate_asset_name(&name)?;
        if let Some(mirrors) = mirrors {
            let expected = mirror_for(&name);
            if !mirrors.contains(&expected) {
                bail!(
                    "{name} was prepared with mirrors {mirrors:?}, not the generation release \
                     {generation_tag} ({expected}); re-run catalog prepare-v3"
                );
            }
        }
        let local = dist.join(&name);
        require_file(&local)?;
        if !names.insert(name.clone()) {
            bail!("catalog asset {name} is listed twice");
        }
        assets.push(HashedAsset {
            name,
            local,
            r2_url,
            signature_of: signature_of.map(str::to_string),
        });
        Ok(())
    };

    let master = redirect
        .artifacts
        .last()
        .ok_or_else(|| anyhow!("{} has no artifacts", redirect_path.display()))?;
    let master_name = url_file_name(&master.url)?;
    if !master_name.starts_with(&format!("catalog-v3.{generation}.")) {
        bail!("master catalog {master_name} does not carry generation {generation} in its name");
    }

    let rule_pack_artifacts = prepared_rule_pack_artifacts_for_upload(dist, &catalog)?;
    let locations = redirect
        .artifacts
        .iter()
        .map(|rung| {
            (
                rung.url.as_str(),
                rung.mirror_urls.as_slice(),
                rung.signature_url.as_str(),
                rung.signature_mirror_urls.as_slice(),
            )
        })
        .chain(rule_pack_artifacts.iter().map(|artifact| {
            (
                artifact.url.as_str(),
                artifact.mirror_urls.as_slice(),
                artifact.signature_url.as_str(),
                artifact.signature_mirror_urls.as_slice(),
            )
        }))
        .collect::<Vec<_>>();
    for (url, mirrors, signature_url, signature_mirrors) in locations {
        let name = url_file_name(url)?;
        push(name.clone(), url.to_string(), Some(mirrors), None)?;
        push(
            url_file_name(signature_url)?,
            signature_url.to_string(),
            Some(signature_mirrors),
            Some(&name),
        )?;
        // The raw sigstore bundle has no catalog URL; it sits beside the asset.
        let sigstore = format!("{name}.sigstore.json");
        push(
            sigstore.clone(),
            format!("{url}.sigstore.json"),
            None,
            Some(&name),
        )?;
    }

    let base = format!(
        "{}/{}",
        public_catalog_base_url(),
        central_catalog_v3_path_prefix()
    );
    let bundle_name = redirect_signature_bundle_file_name(CATALOG_V3_MODERN_REDIRECT_JSON);
    let sigstore_name = CATALOG_V3_MODERN_REDIRECT_JSON.replace(".json", ".sigstore.json");
    let redirect_files = RedirectFiles {
        json: redirect_path,
        json_r2_url: format!("{base}/{CATALOG_V3_MODERN_REDIRECT_JSON}"),
        bundle: dist.join(&bundle_name),
        bundle_r2_url: format!("{base}/{bundle_name}"),
        sigstore: dist.join(&sigstore_name),
    };
    for path in [
        &redirect_files.json,
        &redirect_files.bundle,
        &redirect_files.sigstore,
    ] {
        require_file(path)?;
    }

    Ok(GenerationPlan {
        generation,
        generation_tag,
        redirect_tag: central_catalog_v3_release_tag(),
        master_name,
        assets,
        redirect: redirect_files,
    })
}

enum RemoteState {
    Absent,
    Identical,
    Conflict,
}

fn classify(
    destination: &str,
    asset: &HashedAsset,
    local_digest: &str,
    remote: Option<Vec<u8>>,
    conflicts: &mut Vec<String>,
) -> RemoteState {
    match remote {
        None => RemoteState::Absent,
        Some(bytes) => {
            let remote_digest = sha256_hex(&bytes);
            if remote_digest == local_digest {
                RemoteState::Identical
            } else {
                conflicts.push(format!(
                    "{} on {destination}: local sha256 {local_digest}, published sha256 \
                     {remote_digest}",
                    asset.name
                ));
                RemoteState::Conflict
            }
        }
    }
}

fn last_artifact_name(bytes: &[u8]) -> Result<String> {
    let redirect: CatalogV3Redirect =
        serde_json::from_slice(bytes).context("failed to parse the live modern redirect")?;
    let last = redirect
        .artifacts
        .last()
        .ok_or_else(|| anyhow!("the live modern redirect has no artifacts"))?;
    url_file_name(&last.url)
}

/// Compare every asset with one store. Returns the indexes of assets absent
/// there. A signature companion whose data asset is byte-identical on the
/// store adopts the published signature bytes (first store wins; a later
/// store disagreeing with adopted bytes is a conflict).
fn preflight_store(
    destination: &str,
    plan: &GenerationPlan,
    digests: &mut [String],
    adopted: &mut BTreeSet<String>,
    conflicts: &mut Vec<String>,
    report: &mut PublishReport,
    fetch: impl Fn(&HashedAsset) -> Result<Option<Vec<u8>>>,
) -> Result<Vec<usize>> {
    let mut missing = Vec::new();
    let mut identical_data = BTreeSet::new();
    // Data assets first, so companions know whether their data matched.
    let order = plan
        .assets
        .iter()
        .enumerate()
        .filter(|(_, asset)| asset.signature_of.is_none())
        .chain(
            plan.assets
                .iter()
                .enumerate()
                .filter(|(_, asset)| asset.signature_of.is_some()),
        );
    for (index, asset) in order {
        let remote = fetch(asset)?;
        let adoptable = asset
            .signature_of
            .as_ref()
            .is_some_and(|data| identical_data.contains(data) && !adopted.contains(&asset.name));
        if let (Some(bytes), true) = (&remote, adoptable) {
            if sha256_hex(bytes) != digests[index] {
                fs::write(&asset.local, bytes)
                    .with_context(|| format!("failed to adopt {}", asset.local.display()))?;
                digests[index] = sha256_hex(bytes);
                warn(format!(
                    "{} already signed on {destination}; adopting the published signature",
                    asset.name
                ));
                report
                    .adopted_signatures
                    .push(format!("{} from {destination}", asset.name));
            }
            adopted.insert(asset.name.clone());
        }
        match classify(destination, asset, &digests[index], remote, conflicts) {
            RemoteState::Absent => missing.push(index),
            RemoteState::Identical => {
                if asset.signature_of.is_none() {
                    identical_data.insert(asset.name.clone());
                }
            }
            RemoteState::Conflict => {}
        }
    }
    Ok(missing)
}

pub fn publish_generation(
    plan: &GenerationPlan,
    r2: &dyn R2Store,
    releases: &dyn ReleaseStore,
    options: &PublishOptions,
) -> Result<PublishReport> {
    let mut report = PublishReport::default();
    let mut digests = plan
        .assets
        .iter()
        .map(|asset| Ok(sha256_hex(&read_local(&asset.local)?)))
        .collect::<Result<Vec<_>>>()?;

    // Check both destinations before writing to either, so a conflict on one
    // never leaves the other half-published for nothing.
    step(format!(
        "Checking published state of generation {} ({} hashed assets)",
        plan.generation,
        plan.assets.len()
    ));
    if releases.asset_names(&plan.redirect_tag)?.is_none() {
        bail!(
            "redirect release {} does not exist; the publisher only replaces its redirect files \
             and never creates it",
            plan.redirect_tag
        );
    }
    let generation_release = releases.asset_names(&plan.generation_tag)?;
    if generation_release.is_none() {
        // Fail on a bad tag before a single byte reaches either store.
        releases.validate_tag(&plan.generation_tag, &options.target_commit)?;
    }
    let published_names = generation_release
        .clone()
        .unwrap_or_default()
        .into_iter()
        .collect::<BTreeSet<_>>();

    let mut conflicts = Vec::new();
    let mut adopted = BTreeSet::new();
    let r2_missing = preflight_store(
        "R2",
        plan,
        &mut digests,
        &mut adopted,
        &mut conflicts,
        &mut report,
        |asset| r2.get(&asset.r2_url),
    )?;
    let github_missing = preflight_store(
        &format!("GitHub release {}", plan.generation_tag),
        plan,
        &mut digests,
        &mut adopted,
        &mut conflicts,
        &mut report,
        |asset| {
            if published_names.contains(&asset.name) {
                releases
                    .download_asset(&plan.generation_tag, &asset.name)
                    .map(Some)
            } else {
                Ok(None)
            }
        },
    )?;
    if !conflicts.is_empty() {
        bail!(
            "refusing to publish generation {}: published assets differ from the prepared ones \
             and hashed assets are immutable; no redirect was moved:\n  {}",
            plan.generation,
            conflicts.join("\n  ")
        );
    }
    let r2_missing = r2_missing
        .iter()
        .map(|index| &plan.assets[*index])
        .collect::<Vec<_>>();
    let github_missing = github_missing
        .iter()
        .map(|index| &plan.assets[*index])
        .collect::<Vec<_>>();

    step("Phase 1/5: hashed assets to R2");
    for asset in &plan.assets {
        if r2_missing.iter().any(|missing| missing.name == asset.name) {
            r2.put(&asset.local, &asset.r2_url)?;
            ok(format!("uploaded {}", asset.name));
            report.r2_uploaded.push(asset.name.clone());
        } else {
            ok(format!("{} already on R2 with identical bytes", asset.name));
            report.r2_skipped.push(asset.name.clone());
        }
    }

    step(format!(
        "Phase 2/5: hashed assets to GitHub release {}",
        plan.generation_tag
    ));
    if generation_release.is_none() {
        releases.create_release(&NewRelease {
            tag: plan.generation_tag.clone(),
            title: format!("Scryer plugin catalog v3 generation {}", plan.generation),
            notes: format!(
                "Immutable assets of Scryer plugin catalog v3 generation {}. Clients find them \
                 through the modern redirect on {}.",
                plan.generation, plan.redirect_tag
            ),
            target: options.target_commit.clone(),
            prerelease: true,
        })?;
        ok(format!("created release {}", plan.generation_tag));
        report.generation_release_created = true;
    }
    for asset in &plan.assets {
        if !github_missing
            .iter()
            .any(|missing| missing.name == asset.name)
        {
            ok(format!(
                "{} already on GitHub with identical bytes",
                asset.name
            ));
            report.github_skipped.push(asset.name.clone());
        }
    }
    if !github_missing.is_empty() {
        let files = github_missing
            .iter()
            .map(|asset| asset.local.clone())
            .collect::<Vec<_>>();
        // Clobber: an asset GitHub lists but did not serve (a half-finished
        // upload) is absent from the preflight and must be replaceable.
        releases.upload_assets(&plan.generation_tag, &files, true)?;
        for asset in &github_missing {
            ok(format!("uploaded {}", asset.name));
            report.github_uploaded.push(asset.name.clone());
        }
    }

    step("Phase 3/5: verifying public copies");
    let mut mismatches = Vec::new();
    for (asset, digest) in plan.assets.iter().zip(&digests) {
        let from_r2 = sha256_hex(&r2.fetch_public(&asset.r2_url)?);
        if &from_r2 != digest {
            mismatches.push(format!(
                "{} via {}: expected sha256 {digest}, got {from_r2}",
                asset.name, asset.r2_url
            ));
        }
        let from_github = sha256_hex(&releases.fetch_public(&plan.generation_tag, &asset.name)?);
        if &from_github != digest {
            mismatches.push(format!(
                "{} via GitHub release {}: expected sha256 {digest}, got {from_github}",
                asset.name, plan.generation_tag
            ));
        }
    }
    if !mismatches.is_empty() {
        bail!(
            "public copies of generation {} do not match the prepared assets; no redirect was \
             moved:\n  {}",
            plan.generation,
            mismatches.join("\n  ")
        );
    }
    ok(format!(
        "{} assets verified on R2 and GitHub",
        plan.assets.len()
    ));

    step("Phase 4/5: modern redirect to R2");
    let redirect_bytes = read_local(&plan.redirect.json)?;
    let bundle_bytes = read_local(&plan.redirect.bundle)?;
    let live = r2.get(&plan.redirect.json_r2_url)?;
    let live_bundle = r2.get(&plan.redirect.bundle_r2_url)?;
    match &live {
        None => {
            if let Some(expected) = &options.expected_merge_base {
                bail!(
                    "no live modern redirect at {}, but the generation was merged onto {expected}; \
                     refusing to move the redirect",
                    plan.redirect.json_r2_url
                );
            }
        }
        Some(bytes) => {
            let live_name = last_artifact_name(bytes)?;
            // A rerun after phase 5 failed finds its own redirect live.
            let resumed = live_name == plan.master_name;
            match &options.expected_merge_base {
                Some(expected) if expected != &live_name && !resumed => bail!(
                    "the live modern redirect now serves {live_name}, not the merge base \
                     {expected}: another publish landed in between. No redirect was moved. \
                     Rerunning this run would republish the same preparation; publish again \
                     from a new signed release trigger tag so it merges onto {live_name}"
                ),
                Some(_) => {}
                None => warn(format!(
                    "no --expected-merge-base given; replacing live redirect serving {live_name} \
                     without a compare-and-set"
                )),
            }
        }
    }
    if live.as_deref() == Some(redirect_bytes.as_slice())
        && live_bundle.as_deref() == Some(bundle_bytes.as_slice())
    {
        ok("R2 already serves this redirect");
    } else {
        r2.put(&plan.redirect.json, &plan.redirect.json_r2_url)?;
        r2.put(&plan.redirect.bundle, &plan.redirect.bundle_r2_url)?;
        ok(format!(
            "R2 modern redirect now serves generation {}",
            plan.generation
        ));
        report.r2_redirect_moved = true;
    }

    step(format!(
        "Phase 5/5: modern redirect to GitHub release {}",
        plan.redirect_tag
    ));
    // The redirect release was required to exist in the preflight.
    let phase5 = || -> Result<()> {
        releases.upload_assets(
            &plan.redirect_tag,
            &[
                plan.redirect.json.clone(),
                plan.redirect.bundle.clone(),
                plan.redirect.sigstore.clone(),
            ],
            true,
        )
    };
    phase5().map_err(|error| {
        anyhow!(
            "R2 now serves generation {} but the GitHub {} redirect was not updated: {error:#}. \
             Re-run catalog publish-v3-generation with the same --dist to finish",
            plan.generation,
            plan.redirect_tag
        )
    })?;
    ok(format!(
        "GitHub {} redirect now serves generation {}",
        plan.redirect_tag, plan.generation
    ));
    Ok(report)
}

fn temp_download_path(dir: &tempfile::TempDir, name: &str) -> PathBuf {
    dir.path().join(name)
}

fn curl_download(url: &str) -> Result<Vec<u8>> {
    let dir = tempfile::tempdir()?;
    let path = temp_download_path(&dir, "download");
    run_checked(
        Command::new("curl")
            .arg("--fail")
            .arg("--silent")
            .arg("--show-error")
            .arg("--location")
            .arg("--retry")
            .arg("5")
            .arg("--retry-all-errors")
            .arg("--output")
            .arg(&path)
            .arg(url),
    )?;
    read_local(&path)
}

/// R2 through the `aws` CLI; public reads through `curl`.
pub struct AwsR2Store<'a> {
    pub ctx: &'a TaskContext,
    pub config: R2Config,
}

impl AwsR2Store<'_> {
    fn aws(&self) -> Command {
        let mut command = self.ctx.command("aws");
        command
            .env("AWS_ACCESS_KEY_ID", &self.config.access_key_id)
            .env("AWS_SECRET_ACCESS_KEY", &self.config.secret_access_key)
            .env("AWS_DEFAULT_REGION", "auto")
            .env("AWS_EC2_METADATA_DISABLED", "true");
        command
    }
}

impl R2Store for AwsR2Store<'_> {
    fn get(&self, public_url: &str) -> Result<Option<Vec<u8>>> {
        let key = url_path_key(public_url)?;
        let dir = tempfile::tempdir()?;
        let path = temp_download_path(&dir, "object");
        let mut command = self.aws();
        command
            .arg("s3api")
            .arg("get-object")
            .arg("--bucket")
            .arg(&self.config.bucket)
            .arg("--key")
            .arg(&key)
            .arg("--endpoint-url")
            .arg(&self.config.endpoint_url)
            .arg(&path);
        let output = command
            .output()
            .with_context(|| format!("failed to run aws for {key}"))?;
        if output.status.success() {
            return Ok(Some(read_local(&path)?));
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("NoSuchKey") || stderr.contains("(404)") || stderr.contains("Not Found")
        {
            return Ok(None);
        }
        bail!("failed to read R2 object {key}: {stderr}")
    }

    fn put(&self, local: &Path, public_url: &str) -> Result<()> {
        upload_file_to_r2(self.ctx, &self.config, local, public_url)
    }

    fn fetch_public(&self, public_url: &str) -> Result<Vec<u8>> {
        curl_download(public_url)
    }
}

pub fn validate_release_tag(tag: &serde_json::Value, target: &str) -> Result<()> {
    if tag["verification"]["verified"].as_bool() != Some(true) {
        bail!("release tag signature is not verified by GitHub");
    }
    if tag["object"]["type"].as_str() != Some("commit")
        || tag["object"]["sha"].as_str() != Some(target)
    {
        bail!("release tag does not point directly at publication commit {target}");
    }
    Ok(())
}

/// GitHub releases through the `gh` CLI; public reads through `curl`.
pub struct GhReleaseStore<'a> {
    pub ctx: &'a TaskContext,
    pub repo: String,
}

impl ReleaseStore for GhReleaseStore<'_> {
    fn asset_names(&self, tag: &str) -> Result<Option<Vec<String>>> {
        let output = self
            .ctx
            .command("gh")
            .args(["release", "view", tag, "--repo", &self.repo])
            // Only assets GitHub actually serves; a half-finished upload is
            // listed in another state and is treated as absent.
            .args([
                "--json",
                "assets",
                "--jq",
                r#".assets[] | select(.state == "uploaded") | .name"#,
            ])
            .output()
            .context("failed to run gh release view")?;
        if output.status.success() {
            return Ok(Some(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(str::to_string)
                    .collect(),
            ));
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("release not found") {
            return Ok(None);
        }
        bail!("failed to read GitHub release {tag}: {stderr}")
    }

    fn validate_tag(&self, tag: &str, target: &str) -> Result<()> {
        // Resolve an existing annotated tag, then require GitHub's verified
        // signature and exact target commit. Never let the API create a tag.
        let tag_object = run_capture(self.ctx.command_in("git", &self.ctx.repo_root).args([
            "rev-parse",
            "--verify",
            &format!("refs/tags/{tag}^{{tag}}"),
        ]))
        .with_context(|| {
            format!(
                "release tag {tag} is not an annotated tag in this checkout; a generation \
                 release needs the signed release trigger tag, not a branch or commit"
            )
        })?;
        let json = run_capture(self.ctx.command("gh").args([
            "api",
            &format!("repos/{}/git/tags/{}", self.repo, tag_object.trim()),
        ]))?;
        validate_release_tag(&serde_json::from_str(&json)?, target)
            .with_context(|| format!("release tag {tag} cannot carry a generation release"))
    }

    fn create_release(&self, release: &NewRelease) -> Result<()> {
        self.validate_tag(&release.tag, &release.target)?;
        let mut command = self.ctx.command("gh");
        command
            .args(["release", "create", &release.tag, "--repo", &self.repo])
            .args(["--title", &release.title, "--notes", &release.notes])
            .args(["--target", &release.target, "--verify-tag"]);
        if release.prerelease {
            command.arg("--prerelease");
        }
        run_checked(&mut command)
    }

    fn download_asset(&self, tag: &str, name: &str) -> Result<Vec<u8>> {
        let dir = tempfile::tempdir()?;
        run_capture(
            self.ctx
                .command("gh")
                .args(["release", "download", tag, "--repo", &self.repo])
                .args(["--pattern", name])
                .arg("--dir")
                .arg(dir.path()),
        )?;
        read_local(&dir.path().join(name))
    }

    fn upload_assets(&self, tag: &str, files: &[PathBuf], clobber: bool) -> Result<()> {
        let mut command = self.ctx.command("gh");
        command
            .args(["release", "upload", tag])
            .args(files)
            .args(["--repo", &self.repo]);
        if clobber {
            command.arg("--clobber");
        }
        run_checked(&mut command)
    }

    fn fetch_public(&self, tag: &str, name: &str) -> Result<Vec<u8>> {
        curl_download(&github_release_asset_url(&self.repo, tag, name))
    }
}

/// In-memory stores for tests.
#[cfg(test)]
pub mod fakes {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    #[derive(Default)]
    pub struct FakeR2 {
        pub objects: RefCell<BTreeMap<String, Vec<u8>>>,
        pub puts: RefCell<Vec<String>>,
    }

    impl R2Store for FakeR2 {
        fn get(&self, public_url: &str) -> Result<Option<Vec<u8>>> {
            Ok(self.objects.borrow().get(public_url).cloned())
        }

        fn put(&self, local: &Path, public_url: &str) -> Result<()> {
            self.objects
                .borrow_mut()
                .insert(public_url.to_string(), read_local(local)?);
            self.puts.borrow_mut().push(public_url.to_string());
            Ok(())
        }

        fn fetch_public(&self, public_url: &str) -> Result<Vec<u8>> {
            self.get(public_url)?
                .ok_or_else(|| anyhow!("404 {public_url}"))
        }
    }

    #[derive(Clone, Debug, Default)]
    pub struct FakeRelease {
        pub assets: BTreeMap<String, Vec<u8>>,
    }

    #[derive(Default)]
    pub struct FakeReleases {
        pub releases: RefCell<BTreeMap<String, FakeRelease>>,
        pub created: RefCell<Vec<NewRelease>>,
        pub uploads: RefCell<Vec<(String, Vec<String>, bool)>>,
        /// Uploads to this tag fail.
        pub fail_uploads_to: Option<String>,
        /// Tags that fail validation (unsigned, or not at the target).
        pub invalid_tags: Vec<String>,
        pub validated_tags: RefCell<Vec<(String, String)>>,
    }

    impl ReleaseStore for FakeReleases {
        fn asset_names(&self, tag: &str) -> Result<Option<Vec<String>>> {
            Ok(self
                .releases
                .borrow()
                .get(tag)
                .map(|release| release.assets.keys().cloned().collect()))
        }

        fn validate_tag(&self, tag: &str, target: &str) -> Result<()> {
            self.validated_tags
                .borrow_mut()
                .push((tag.to_string(), target.to_string()));
            if self.invalid_tags.iter().any(|invalid| invalid == tag) {
                bail!("release tag {tag} is not verified at {target}");
            }
            Ok(())
        }

        fn create_release(&self, release: &NewRelease) -> Result<()> {
            self.validate_tag(&release.tag, &release.target)?;
            let mut releases = self.releases.borrow_mut();
            if releases.contains_key(&release.tag) {
                bail!("release {} already exists", release.tag);
            }
            releases.insert(release.tag.clone(), FakeRelease::default());
            self.created.borrow_mut().push(release.clone());
            Ok(())
        }

        fn download_asset(&self, tag: &str, name: &str) -> Result<Vec<u8>> {
            self.releases
                .borrow()
                .get(tag)
                .and_then(|release| release.assets.get(name).cloned())
                .ok_or_else(|| anyhow!("no asset {name} on {tag}"))
        }

        fn upload_assets(&self, tag: &str, files: &[PathBuf], clobber: bool) -> Result<()> {
            if self.fail_uploads_to.as_deref() == Some(tag) {
                bail!("simulated upload failure to {tag}");
            }
            let mut releases = self.releases.borrow_mut();
            let release = releases
                .get_mut(tag)
                .ok_or_else(|| anyhow!("release {tag} not found"))?;
            let mut names = Vec::new();
            for file in files {
                let name = file
                    .file_name()
                    .and_then(|value| value.to_str())
                    .ok_or_else(|| anyhow!("bad file {}", file.display()))?
                    .to_string();
                if release.assets.contains_key(&name) && !clobber {
                    bail!("asset {name} already exists on {tag}");
                }
                release.assets.insert(name.clone(), read_local(file)?);
                names.push(name);
            }
            self.uploads
                .borrow_mut()
                .push((tag.to_string(), names, clobber));
            Ok(())
        }

        fn fetch_public(&self, tag: &str, name: &str) -> Result<Vec<u8>> {
            self.download_asset(tag, name)
        }
    }
}
