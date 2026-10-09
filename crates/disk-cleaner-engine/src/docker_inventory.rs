//! Docker images and build cache with "last used" times.
//!
//! Docker does not record when an image was last run, so it is derived from:
//! containers made from the image (created / started / finished), the image's
//! `Metadata.LastTagTime` (pull or tag), its build time, and — when the usage
//! tracker runs — recorded `docker events`. Build cache records carry BuildKit's
//! own `LastUsedAt`, which is what `docker builder prune --filter until=` uses.

use crate::cmd::{run_command, which_bin};
use crate::config::Config;
use crate::docker_track::{self, TrackerStatus, UsageStore};
use crate::util::parse_size;
use chrono::{DateTime, NaiveDateTime};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DockerImage {
    /// Full id, `sha256:…`.
    pub id: String,
    pub short_id: String,
    pub tags: Vec<String>,
    pub bytes: u64,
    pub created: Option<i64>,
    pub last_tag_time: Option<i64>,
    pub last_used: Option<i64>,
    /// `running`, `container`, `tracker`, `pulled`, or `built`.
    pub last_used_source: String,
    /// Names of containers (any state) using this image; such images are kept.
    pub containers: Vec<String>,
    pub running: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerInfo {
    pub id: String,
    pub name: String,
    pub image_id: String,
    pub image_ref: String,
    pub created: Option<i64>,
    pub started: Option<i64>,
    pub finished: Option<i64>,
    pub running: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildCacheRecord {
    pub id: String,
    pub bytes: u64,
    pub created: Option<i64>,
    /// Approximate (buildx prints "3 weeks ago"); None = never used since creation.
    pub last_used: Option<i64>,
    pub reclaimable: bool,
    pub description: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BuildCacheSummary {
    pub available: bool,
    pub records: usize,
    pub total_bytes: u64,
    pub reclaimable_bytes: u64,
    /// Reclaimable records not used for more than `days`.
    pub stale_bytes: u64,
    pub stale_records: usize,
    pub days: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DockerInventory {
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub images: Vec<DockerImage>,
    pub build_cache: BuildCacheSummary,
    pub tracker: TrackerStatus,
    pub now: i64,
}

fn s(v: &str) -> String {
    v.to_string()
}

/// RFC 3339 → unix seconds. Docker's zero time (`0001-01-01…`) → None.
pub fn parse_rfc3339(v: &str) -> Option<i64> {
    let ts = DateTime::parse_from_rfc3339(v.trim()).ok()?.timestamp();
    (ts > 0).then_some(ts)
}

/// buildx `CreatedAt`, e.g. `2026-10-09 15:51:40.731700177 +0000 UTC`.
pub fn parse_buildx_time(v: &str) -> Option<i64> {
    let v = v.trim().trim_end_matches(" UTC");
    DateTime::parse_from_str(v, "%Y-%m-%d %H:%M:%S%.f %z")
        .map(|d| d.timestamp())
        .or_else(|_| {
            NaiveDateTime::parse_from_str(v, "%Y-%m-%d %H:%M:%S%.f")
                .map(|d| d.and_utc().timestamp())
        })
        .ok()
}

/// Go `units.HumanDuration` + " ago" → seconds before `now`. Empty → None.
pub fn parse_human_ago(v: &str, now: i64) -> Option<i64> {
    let v = v.trim().to_ascii_lowercase();
    let v = v.strip_suffix(" ago").unwrap_or(&v).trim();
    if v.is_empty() {
        return None;
    }
    if v.starts_with("less than a second") {
        return Some(now);
    }
    let (n, unit) = if let Some(rest) = v.strip_prefix("about a ") {
        (1i64, rest)
    } else if let Some(rest) = v.strip_prefix("about an ") {
        (1i64, rest)
    } else if let Some(rest) = v.strip_prefix("a ").or_else(|| v.strip_prefix("an ")) {
        (1i64, rest)
    } else {
        let (n, unit) = v.split_once(' ')?;
        (n.parse().ok()?, unit)
    };
    let secs = match unit.trim_end_matches('s') {
        "second" => 1,
        "minute" => 60,
        "hour" => 3600,
        "day" => 86_400,
        "week" => 7 * 86_400,
        "month" => 30 * 86_400,
        "year" => 365 * 86_400,
        _ => return None,
    };
    Some(now - n * secs)
}

#[derive(Deserialize)]
struct RawImage {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "RepoTags", default)]
    repo_tags: Option<Vec<String>>,
    #[serde(rename = "Created", default)]
    created: String,
    #[serde(rename = "Size", default)]
    size: u64,
    #[serde(rename = "Metadata", default)]
    metadata: Option<RawImageMeta>,
}

#[derive(Deserialize, Default)]
struct RawImageMeta {
    #[serde(rename = "LastTagTime", default)]
    last_tag_time: String,
}

/// Parse `docker image inspect` JSON (an array).
pub fn parse_image_inspect(json: &str) -> Result<Vec<DockerImage>, String> {
    let raw: Vec<RawImage> = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(raw
        .into_iter()
        .map(|r| {
            let short_id =
                r.id.trim_start_matches("sha256:")
                    .chars()
                    .take(12)
                    .collect();
            DockerImage {
                short_id,
                tags: r
                    .repo_tags
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|t| t != "<none>:<none>")
                    .collect(),
                bytes: r.size,
                created: parse_rfc3339(&r.created),
                last_tag_time: r.metadata.and_then(|m| parse_rfc3339(&m.last_tag_time)),
                last_used: None,
                last_used_source: String::new(),
                containers: Vec::new(),
                running: false,
                id: r.id,
            }
        })
        .collect())
}

#[derive(Deserialize)]
struct RawContainer {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Name", default)]
    name: String,
    #[serde(rename = "Image", default)]
    image: String,
    #[serde(rename = "Created", default)]
    created: String,
    #[serde(rename = "Config", default)]
    config: Option<RawContainerConfig>,
    #[serde(rename = "State", default)]
    state: Option<RawContainerState>,
}

#[derive(Deserialize, Default)]
struct RawContainerConfig {
    #[serde(rename = "Image", default)]
    image: String,
}

#[derive(Deserialize, Default)]
struct RawContainerState {
    #[serde(rename = "Running", default)]
    running: bool,
    #[serde(rename = "StartedAt", default)]
    started_at: String,
    #[serde(rename = "FinishedAt", default)]
    finished_at: String,
}

/// Parse `docker inspect <containers>` JSON (an array).
pub fn parse_container_inspect(json: &str) -> Result<Vec<ContainerInfo>, String> {
    let raw: Vec<RawContainer> = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(raw
        .into_iter()
        .map(|r| {
            let state = r.state.unwrap_or_default();
            ContainerInfo {
                id: r.id,
                name: r.name.trim_start_matches('/').to_string(),
                image_id: r.image,
                image_ref: r.config.map(|c| c.image).unwrap_or_default(),
                created: parse_rfc3339(&r.created),
                started: parse_rfc3339(&state.started_at),
                finished: parse_rfc3339(&state.finished_at),
                running: state.running,
            }
        })
        .collect())
}

/// Parse `docker buildx du --format json` (one JSON object per line).
pub fn parse_buildx_du(stdout: &str, now: i64) -> Vec<BuildCacheRecord> {
    #[derive(Deserialize)]
    struct Raw {
        #[serde(rename = "ID", default)]
        id: String,
        #[serde(rename = "Size", default)]
        size: String,
        #[serde(rename = "CreatedAt", default)]
        created_at: String,
        #[serde(rename = "LastUsedAt", default)]
        last_used_at: String,
        #[serde(rename = "Reclaimable", default)]
        reclaimable: bool,
        #[serde(rename = "Description", default)]
        description: String,
    }
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<Raw>(l.trim()).ok())
        .map(|r| BuildCacheRecord {
            id: r.id,
            bytes: parse_size(&r.size).unwrap_or(0),
            created: parse_buildx_time(&r.created_at),
            last_used: parse_human_ago(&r.last_used_at, now),
            reclaimable: r.reclaimable,
            description: r.description,
        })
        .collect()
}

pub fn summarize_build_cache(
    records: &[BuildCacheRecord],
    days: u32,
    now: i64,
) -> BuildCacheSummary {
    let cutoff = now - days as i64 * 86_400;
    let mut sum = BuildCacheSummary {
        available: true,
        records: records.len(),
        days,
        ..Default::default()
    };
    for r in records {
        sum.total_bytes += r.bytes;
        if !r.reclaimable {
            continue;
        }
        sum.reclaimable_bytes += r.bytes;
        let last = r.last_used.or(r.created).unwrap_or(now);
        if last < cutoff {
            sum.stale_bytes += r.bytes;
            sum.stale_records += 1;
        }
    }
    sum
}

/// Normalize an image reference so `ubuntu` and `ubuntu:latest` match.
fn normalize_ref(r: &str) -> String {
    let r = r
        .trim()
        .trim_start_matches("docker.io/library/")
        .trim_start_matches("docker.io/");
    let last = r.rsplit('/').next().unwrap_or(r);
    if r.contains('@') || last.contains(':') {
        r.to_string()
    } else {
        format!("{r}:latest")
    }
}

/// Fill `last_used`, `containers`, and `running` on each image.
pub fn attach_usage(
    images: &mut [DockerImage],
    containers: &[ContainerInfo],
    store: &UsageStore,
    now: i64,
) {
    for img in images.iter_mut() {
        let tags: Vec<String> = img.tags.iter().map(|t| normalize_ref(t)).collect();
        let mut best: Option<(i64, &str)> = None;
        let mut consider = |ts: Option<i64>, src: &'static str| {
            if let Some(ts) = ts {
                if best.is_none_or(|(b, _)| ts > b) {
                    best = Some((ts, src));
                }
            }
        };
        consider(img.created, "built");
        consider(img.last_tag_time, "pulled");
        for c in containers {
            let matches =
                c.image_id == img.id || tags.iter().any(|t| *t == normalize_ref(&c.image_ref));
            if !matches {
                continue;
            }
            img.containers.push(c.name.clone());
            if c.running {
                img.running = true;
            }
            consider(c.created, "container");
            consider(c.started, "container");
            consider(c.finished, "container");
        }
        for (key, ts) in &store.images {
            if *key == img.id || tags.iter().any(|t| *t == normalize_ref(key)) {
                consider(Some(*ts), "tracker");
            }
        }
        if img.running {
            best = Some((now, "running"));
        }
        img.last_used = best.map(|b| b.0);
        img.last_used_source = best.map(|b| b.1.to_string()).unwrap_or_default();
    }
}

/// Images with no containers whose last use is older than `days`.
pub fn stale_images(images: &[DockerImage], days: u32, now: i64) -> Vec<&DockerImage> {
    let cutoff = now - days as i64 * 86_400;
    images
        .iter()
        .filter(|i| i.containers.is_empty() && i.last_used.is_none_or(|t| t < cutoff))
        .collect()
}

fn lines(stdout: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    stdout
        .split_whitespace()
        .map(String::from)
        .filter(|l| seen.insert(l.clone()))
        .collect()
}

fn inspect_json(kind: &str, ids: &[String], cfg: &Config) -> Result<String, String> {
    if ids.is_empty() {
        return Ok("[]".into());
    }
    let mut cmd = vec![s("docker")];
    if kind == "image" {
        cmd.push(s("image"));
    }
    cmd.push(s("inspect"));
    cmd.extend(ids.iter().cloned());
    let cp = run_command(&cmd, cfg);
    if cp.status != 0 {
        return Err(format!(
            "docker {kind} inspect failed: {}",
            cp.stderr.trim()
        ));
    }
    Ok(cp.stdout)
}

pub fn list_containers(cfg: &Config) -> Result<Vec<ContainerInfo>, String> {
    let cp = run_command(&[s("docker"), s("ps"), s("-aq"), s("--no-trunc")], cfg);
    if cp.status != 0 {
        return Err(format!("docker ps failed: {}", cp.stderr.trim()));
    }
    parse_container_inspect(&inspect_json("container", &lines(&cp.stdout), cfg)?)
}

pub fn list_images(cfg: &Config, store: &UsageStore, now: i64) -> Result<Vec<DockerImage>, String> {
    let cp = run_command(
        &[s("docker"), s("image"), s("ls"), s("-q"), s("--no-trunc")],
        cfg,
    );
    if cp.status != 0 {
        return Err(format!("docker image ls failed: {}", cp.stderr.trim()));
    }
    let mut images = parse_image_inspect(&inspect_json("image", &lines(&cp.stdout), cfg)?)?;
    let containers = list_containers(cfg)?;
    attach_usage(&mut images, &containers, store, now);
    images.sort_by_key(|i| std::cmp::Reverse(i.bytes));
    Ok(images)
}

/// Build cache records via `docker buildx du`, or None when buildx is unavailable.
pub fn list_build_cache(cfg: &Config, now: i64) -> Option<Vec<BuildCacheRecord>> {
    let cp = run_command(
        &[s("docker"), s("buildx"), s("du"), s("--format"), s("json")],
        cfg,
    );
    (cp.status == 0).then(|| parse_buildx_du(&cp.stdout, now))
}

pub fn inventory(cfg: &Config, days: u32) -> DockerInventory {
    let now = chrono::Utc::now().timestamp();
    let tracker = docker_track::status();
    let mut inv = DockerInventory {
        now,
        tracker,
        build_cache: BuildCacheSummary {
            days,
            ..Default::default()
        },
        ..Default::default()
    };
    if which_bin("docker").is_none() {
        inv.error = Some("docker not found".into());
        return inv;
    }
    let store = UsageStore::load(&docker_track::store_path());
    match list_images(cfg, &store, now) {
        Ok(images) => {
            inv.available = true;
            inv.images = images;
        }
        Err(e) => {
            inv.error = Some(e);
            return inv;
        }
    }
    if let Some(records) = list_build_cache(cfg, now) {
        inv.build_cache = summarize_build_cache(&records, days, now);
    }
    inv
}

/// `docker builder prune` for cache unused longer than `days` (BuildKit last-used time).
pub fn builder_prune_command(days: u32) -> Vec<String> {
    vec![
        s("docker"),
        s("builder"),
        s("prune"),
        s("-af"),
        s("--filter"),
        crate::util::docker_until_filter(days),
    ]
}

/// One `docker rmi` per image. Refuses images that still have containers.
pub fn image_remove_actions(
    images: &[DockerImage],
    ids: &[String],
) -> Result<Vec<crate::action::Action>, String> {
    let by_id: HashMap<&str, &DockerImage> = images.iter().map(|i| (i.id.as_str(), i)).collect();
    let mut out = Vec::new();
    for id in ids {
        let img = by_id
            .get(id.as_str())
            .ok_or_else(|| format!("unknown image {id}"))?;
        if !img.containers.is_empty() {
            return Err(format!(
                "image {} is used by container(s): {}",
                img.short_id,
                img.containers.join(", ")
            ));
        }
        let label = img
            .tags
            .first()
            .cloned()
            .unwrap_or_else(|| img.short_id.clone());
        // -f: an image with several tags can only be removed by id with force;
        // safe because no container uses it.
        out.push(
            crate::action::Action::new(
                "docker",
                "docker_cmd",
                format!("image:{label}"),
                img.bytes,
                "remove image",
            )
            .with_command(vec![s("docker"), s("rmi"), s("-f"), img.id.clone()]),
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_791_561_463; // 2026-10-09

    #[test]
    fn human_ago() {
        assert_eq!(parse_human_ago("26 seconds ago", NOW), Some(NOW - 26));
        assert_eq!(parse_human_ago("About a minute ago", NOW), Some(NOW - 60));
        assert_eq!(parse_human_ago("About an hour ago", NOW), Some(NOW - 3600));
        assert_eq!(parse_human_ago("3 weeks ago", NOW), Some(NOW - 21 * 86_400));
        assert_eq!(
            parse_human_ago("2 months ago", NOW),
            Some(NOW - 60 * 86_400)
        );
        assert_eq!(parse_human_ago("1 year ago", NOW), Some(NOW - 365 * 86_400));
        assert_eq!(parse_human_ago("Less than a second ago", NOW), Some(NOW));
        assert_eq!(parse_human_ago("", NOW), None);
        assert_eq!(parse_human_ago("someday", NOW), None);
    }

    #[test]
    fn timestamps() {
        assert_eq!(parse_rfc3339("0001-01-01T00:00:00Z"), None);
        assert_eq!(
            parse_rfc3339("2026-03-23T21:33:59.562202219Z"),
            Some(1_774_301_639)
        );
        assert_eq!(
            parse_buildx_time("2026-10-09 15:51:40.731700177 +0000 UTC"),
            Some(1_791_561_100)
        );
    }

    #[test]
    fn image_inspect_real_shape() {
        let json = r#"[{"Id":"sha256:5e23090353324d887c48ad5e5c56d294eab81588df9605b07d1afe895f9cc8f8","RepoTags":["hello-world:latest"],"Created":"2026-03-23T21:33:59.562202219Z","Size":15077,"Metadata":{"LastTagTime":"2026-10-08T06:55:41.454333959Z"}},
                       {"Id":"sha256:aaaa","RepoTags":[],"Created":"2024-01-01T00:00:00Z","Size":10,"Metadata":{"LastTagTime":"0001-01-01T00:00:00Z"}}]"#;
        let imgs = parse_image_inspect(json).unwrap();
        assert_eq!(imgs.len(), 2);
        assert_eq!(imgs[0].short_id, "5e2309035332");
        assert_eq!(imgs[0].tags, vec!["hello-world:latest"]);
        assert_eq!(imgs[0].bytes, 15077);
        assert!(imgs[0].last_tag_time.unwrap() > imgs[0].created.unwrap());
        assert!(imgs[1].tags.is_empty());
        assert_eq!(imgs[1].last_tag_time, None);
        assert!(parse_image_inspect("not json").is_err());
    }

    #[test]
    fn container_inspect_real_shape() {
        let json = r#"[{"Id":"d743","Name":"/cool_aryabhata","Image":"sha256:5e23","Created":"2026-10-08T06:55:41.479872178Z",
            "Config":{"Image":"hello-world"},"State":{"Running":false,"StartedAt":"2026-10-08T06:55:41.573545221Z","FinishedAt":"2026-10-08T06:55:41.675472511Z"}}]"#;
        let c = &parse_container_inspect(json).unwrap()[0];
        assert_eq!(c.name, "cool_aryabhata");
        assert_eq!(c.image_ref, "hello-world");
        assert!(!c.running);
        assert!(c.finished.unwrap() >= c.started.unwrap());
    }

    fn img(id: &str, tags: &[&str], created: i64, tagged: Option<i64>) -> DockerImage {
        DockerImage {
            id: id.into(),
            short_id: id.trim_start_matches("sha256:").into(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            bytes: 1000,
            created: Some(created),
            last_tag_time: tagged,
            last_used: None,
            last_used_source: String::new(),
            containers: vec![],
            running: false,
        }
    }

    fn container(
        name: &str,
        image_id: &str,
        image_ref: &str,
        finished: i64,
        running: bool,
    ) -> ContainerInfo {
        ContainerInfo {
            id: format!("c-{name}"),
            name: name.into(),
            image_id: image_id.into(),
            image_ref: image_ref.into(),
            created: Some(finished - 100),
            started: Some(finished - 50),
            finished: Some(finished),
            running,
        }
    }

    #[test]
    fn last_used_prefers_latest_signal() {
        let day = 86_400;
        let mut images = vec![
            // Built 2 years ago but pulled yesterday: NOT stale (old code got this wrong).
            img(
                "sha256:pulled",
                &["clickhouse:23"],
                NOW - 730 * day,
                Some(NOW - day),
            ),
            // Built 90 days ago, never run: stale.
            img("sha256:old", &["vv-pipeline:old"], NOW - 90 * day, None),
            // Old, but a stopped container exists: kept, last used by container time.
            img("sha256:stopped", &["svc:1"], NOW - 200 * day, None),
            // Old, running container by tag reference.
            img("sha256:run", &["postgres:15-alpine"], NOW - 300 * day, None),
            // Old, but tracker saw it run 2 days ago via short ref.
            img("sha256:tracked", &["ubuntu:latest"], NOW - 400 * day, None),
        ];
        let containers = vec![
            container("stopped1", "sha256:stopped", "svc:1", NOW - 40 * day, false),
            container("pg", "sha256:other", "postgres:15-alpine", NOW, true),
        ];
        let mut store = UsageStore::default();
        store.images.insert("ubuntu".into(), NOW - 2 * day);
        attach_usage(&mut images, &containers, &store, NOW);

        assert_eq!(images[0].last_used, Some(NOW - day));
        assert_eq!(images[0].last_used_source, "pulled");
        assert_eq!(images[1].last_used_source, "built");
        assert_eq!(images[2].containers, vec!["stopped1"]);
        assert_eq!(images[2].last_used, Some(NOW - 40 * day));
        assert_eq!(images[2].last_used_source, "container");
        assert!(images[3].running);
        assert_eq!(images[3].last_used_source, "running");
        assert_eq!(images[4].last_used_source, "tracker");

        let stale: Vec<&str> = stale_images(&images, 30, NOW)
            .iter()
            .map(|i| i.id.as_str())
            .collect();
        assert_eq!(stale, vec!["sha256:old"]);
    }

    #[test]
    fn build_cache_summary_by_last_used() {
        let out = r#"{"CreatedAt":"2026-06-01 10:00:00.0 +0000 UTC","ID":"a","LastUsedAt":"2 months ago","Reclaimable":true,"Size":"1GB","Description":"old"}
{"CreatedAt":"2026-10-09 10:00:00.0 +0000 UTC","ID":"b","LastUsedAt":"3 hours ago","Reclaimable":true,"Size":"500MB","Description":"fresh"}
{"CreatedAt":"2026-01-01 10:00:00.0 +0000 UTC","ID":"c","LastUsedAt":"","Reclaimable":true,"Size":"200MB","Description":"never used, created long ago"}
{"CreatedAt":"2026-01-01 10:00:00.0 +0000 UTC","ID":"d","LastUsedAt":"1 year ago","Reclaimable":false,"Size":"300MB","Description":"in use"}
garbage"#;
        let recs = parse_buildx_du(out, NOW);
        assert_eq!(recs.len(), 4);
        let sum = summarize_build_cache(&recs, 30, NOW);
        let g = |v: &str| parse_size(v).unwrap();
        assert_eq!(
            sum.total_bytes,
            g("1GB") + g("500MB") + g("200MB") + g("300MB")
        );
        assert_eq!(sum.reclaimable_bytes, g("1GB") + g("500MB") + g("200MB"));
        assert_eq!(sum.stale_bytes, g("1GB") + g("200MB"));
        assert_eq!(sum.stale_records, 2);
    }

    #[test]
    fn prune_command_uses_until_hours() {
        assert_eq!(
            builder_prune_command(30),
            vec![
                "docker",
                "builder",
                "prune",
                "-af",
                "--filter",
                "until=720h"
            ]
        );
    }

    #[test]
    fn remove_actions_refuse_images_with_containers() {
        let mut a = img("sha256:a", &["a:1", "a:2"], NOW, None);
        a.bytes = 42;
        let mut b = img("sha256:b", &[], NOW, None);
        b.containers = vec!["web".into()];
        let images = vec![a, b];

        let acts = image_remove_actions(&images, &["sha256:a".into()]).unwrap();
        assert_eq!(acts.len(), 1);
        assert_eq!(acts[0].bytes, 42);
        assert_eq!(acts[0].path, "image:a:1");
        assert_eq!(
            acts[0].command.as_ref().unwrap(),
            &vec!["docker", "rmi", "-f", "sha256:a"]
        );

        let err = image_remove_actions(&images, &["sha256:b".into()]).unwrap_err();
        assert!(err.contains("web"));
        assert!(image_remove_actions(&images, &["sha256:zzz".into()]).is_err());
    }

    #[test]
    fn ref_normalization() {
        assert_eq!(normalize_ref("ubuntu"), "ubuntu:latest");
        assert_eq!(normalize_ref("docker.io/library/ubuntu"), "ubuntu:latest");
        assert_eq!(
            normalize_ref("localhost:5000/app"),
            "localhost:5000/app:latest"
        );
        assert_eq!(
            normalize_ref("nvcr.io/nvidia/tritonserver:26.03-py3"),
            "nvcr.io/nvidia/tritonserver:26.03-py3"
        );
    }
}
