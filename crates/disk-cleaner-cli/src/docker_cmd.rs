//! `disk-cleaner docker` and `disk-cleaner docker-track`.

use disk_cleaner_engine::config::Config;
use disk_cleaner_engine::docker_inventory::{inventory, stale_images};
use disk_cleaner_engine::docker_track;
use disk_cleaner_engine::util::format_bytes;

pub fn age(now: i64, ts: Option<i64>) -> String {
    let Some(ts) = ts else { return "never".into() };
    let d = (now - ts).max(0);
    match d {
        d if d < 3600 => format!("{}m ago", d / 60),
        d if d < 86_400 => format!("{}h ago", d / 3600),
        d => format!("{}d ago", d / 86_400),
    }
}

pub fn list(cfg: &Config, json: bool) -> Result<(), String> {
    let days = cfg.docker_unused_days;
    let inv = inventory(cfg, days);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&inv).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    if let Some(e) = &inv.error {
        return Err(e.clone());
    }
    println!(
        "{:<44} {:>9}  {:<12} {:<10} containers",
        "image", "size", "last used", "source"
    );
    for i in &inv.images {
        let name = i
            .tags
            .first()
            .cloned()
            .unwrap_or_else(|| format!("<none> {}", i.short_id));
        println!(
            "{:<44} {:>9}  {:<12} {:<10} {}",
            truncate(&name, 44),
            format_bytes(i.bytes),
            age(inv.now, i.last_used),
            i.last_used_source,
            i.containers.join(",")
        );
    }
    let stale = stale_images(&inv.images, days, inv.now);
    println!(
        "\n{} image(s) unused >{days}d with no containers: {}",
        stale.len(),
        format_bytes(stale.iter().map(|i| i.bytes).sum())
    );
    let bc = &inv.build_cache;
    if bc.available {
        println!(
            "Build cache: {} total, {} reclaimable, {} unused >{days}d ({} records)",
            format_bytes(bc.total_bytes),
            format_bytes(bc.reclaimable_bytes),
            format_bytes(bc.stale_bytes),
            bc.stale_records
        );
    }
    let t = &inv.tracker;
    println!(
        "Usage tracker: {}{}",
        if t.active {
            "running"
        } else if t.installed {
            "installed, not running"
        } else {
            "off (disk-cleaner docker-track --install)"
        },
        if t.tracked_images > 0 {
            format!(", {} images recorded", t.tracked_images)
        } else {
            String::new()
        }
    );
    println!("\nClean with: disk-cleaner --only-docker --docker-unused-days {days} --apply");
    Ok(())
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n - 1).collect();
        t.push('…');
        t
    }
}

pub fn track(cfg: &Config, install: bool, uninstall: bool, status: bool) -> Result<(), String> {
    if install {
        let bin = std::env::current_exe().map_err(|e| e.to_string())?;
        let st = docker_track::install(&bin)?;
        println!(
            "Installed {} (active: {})",
            docker_track::UNIT_NAME,
            st.active
        );
        return Ok(());
    }
    if uninstall {
        docker_track::uninstall()?;
        println!("Removed {}", docker_track::UNIT_NAME);
        return Ok(());
    }
    if status {
        let st = docker_track::status();
        println!(
            "{}",
            serde_json::to_string_pretty(&st).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    let path = docker_track::store_path();
    eprintln!("docker-track: recording image usage to {}", path.display());
    docker_track::run_tracker(&path, cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages() {
        assert_eq!(age(10_000, None), "never");
        assert_eq!(age(10_000, Some(10_000 - 120)), "2m ago");
        assert_eq!(age(100_000, Some(100_000 - 7200)), "2h ago");
        assert_eq!(age(10_000_000, Some(10_000_000 - 3 * 86_400)), "3d ago");
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
    }
}
