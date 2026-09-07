use clap::{Parser, Subcommand};
use disk_cleaner_engine::config::Config;
use disk_cleaner_engine::execute::execute_actions;
use disk_cleaner_engine::report::write_report;
use disk_cleaner_engine::run::run_phases;
use disk_cleaner_engine::util::{format_bytes, parse_size};
use disk_cleaner_engine::VERSION;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser, Debug)]
#[command(name = "disk-cleaner", version = VERSION, about = "Safe host disk cleaner")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Actually delete / run prune commands
    #[arg(long, global = true)]
    apply: bool,
    #[arg(long, global = true)]
    yes: bool,
    #[arg(long, global = true, default_value_t = 14)]
    docker_unused_days: u32,
    #[arg(long, global = true, default_value_t = 60)]
    file_unused_days: u32,
    #[arg(long, global = true, default_value_t = 60)]
    app_unused_days: u32,
    #[arg(long, global = true, default_value = "1M")]
    min_file_size: String,
    #[arg(long, global = true, default_value = "1M")]
    min_dupe_size: String,
    #[arg(long, global = true, default_value = "newest")]
    dedupe_keep: String,
    #[arg(long = "extra-root", global = true)]
    extra_roots: Vec<PathBuf>,
    #[arg(long = "dedupe-root", global = true)]
    dedupe_roots: Vec<PathBuf>,
    #[arg(long = "protect", global = true)]
    protect_globs: Vec<String>,
    #[arg(long = "protect-app", global = true)]
    protect_apps: Vec<String>,
    #[arg(long, global = true, default_value = "7d")]
    journal_vacuum: String,
    #[arg(long, global = true)]
    include_docker_volumes: bool,
    #[arg(long, global = true)]
    include_hf_cache: bool,
    #[arg(long, global = true)]
    include_opt_apps: bool,
    #[arg(long, global = true)]
    include_pkg_managers: bool,
    #[arg(long, global = true, default_value_t = 120)]
    cmd_timeout: u64,
    #[arg(long, global = true, default_value_t = 600)]
    docker_timeout: u64,
    #[arg(long, global = true, default_value = "5G")]
    confirm_above: String,
    #[arg(long, global = true)]
    cross_fs: bool,
    #[arg(long, global = true)]
    report_dir: Option<PathBuf>,
    #[arg(long, global = true)]
    skip_docker: bool,
    #[arg(long, global = true)]
    skip_caches: bool,
    #[arg(long, global = true)]
    skip_files: bool,
    #[arg(long, global = true)]
    skip_apps: bool,
    #[arg(long, global = true)]
    skip_dupes: bool,
    #[arg(long, global = true)]
    skip_media: bool,
    #[arg(long, global = true)]
    only_docker: bool,
    #[arg(long, global = true)]
    only_caches: bool,
    #[arg(long, global = true)]
    only_files: bool,
    #[arg(long, global = true)]
    only_apps: bool,
    #[arg(long, global = true)]
    only_dupes: bool,
    #[arg(long, global = true)]
    only_media: bool,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run cleaner in CLI mode (default when --cli is used from GUI binary)
    Cli,
}

fn config_from_cli(cli: &Cli) -> Result<Config, String> {
    let mut cfg = Config::default();
    cfg.apply = cli.apply;
    cfg.yes = cli.yes;
    cfg.docker_unused_days = cli.docker_unused_days;
    cfg.file_unused_days = cli.file_unused_days;
    cfg.app_unused_days = cli.app_unused_days;
    cfg.min_file_size = parse_size(&cli.min_file_size)?;
    cfg.min_dupe_size = parse_size(&cli.min_dupe_size)?;
    cfg.dedupe_keep = cli.dedupe_keep.clone();
    cfg.extra_roots = cli.extra_roots.clone();
    cfg.dedupe_roots = cli.dedupe_roots.clone();
    cfg.protect_globs = cli.protect_globs.clone();
    cfg.protect_apps = cli.protect_apps.clone();
    cfg.journal_vacuum = cli.journal_vacuum.clone();
    cfg.include_docker_volumes = cli.include_docker_volumes;
    cfg.include_hf_cache = cli.include_hf_cache;
    cfg.include_opt_apps = cli.include_opt_apps;
    cfg.include_pkg_managers = cli.include_pkg_managers;
    cfg.cmd_timeout_sec = cli.cmd_timeout;
    cfg.docker_timeout_sec = cli.docker_timeout;
    cfg.confirm_above = parse_size(&cli.confirm_above)?;
    cfg.cross_fs = cli.cross_fs;
    cfg.report_dir = cli.report_dir.clone();
    cfg.skip_docker = cli.skip_docker;
    cfg.skip_caches = cli.skip_caches;
    cfg.skip_files = cli.skip_files;
    cfg.skip_apps = cli.skip_apps;
    cfg.skip_dupes = cli.skip_dupes;
    cfg.skip_media = cli.skip_media;
    cfg.gui_use_trash = false;

    let only = [
        ("docker", cli.only_docker),
        ("caches", cli.only_caches),
        ("files", cli.only_files),
        ("apps", cli.only_apps),
        ("dupes", cli.only_dupes),
        ("media", cli.only_media),
    ]
    .into_iter()
    .find(|(_, f)| *f)
    .map(|(n, _)| n.to_string());
    cfg.only = only;
    Ok(cfg)
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let cfg = match config_from_cli(&cli) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };

    eprintln!("… scanning phases");
    let results = run_phases(&cfg);
    let mut total = 0u64;
    let mode = if cfg.apply { "APPLY" } else { "dry-run" };
    println!("=== disk-cleaner {mode} ===");
    for r in &results {
        total += r.reclaimable_bytes;
        println!(
            "{:<8} {} actions, {}",
            r.name,
            r.actions.len(),
            format_bytes(r.reclaimable_bytes)
        );
        for a in r.actions.iter().filter(|a| a.kind != "rmdir_if_empty").take(8) {
            println!(
                "  - [{}] {} ({}) {}",
                a.kind,
                a.path,
                format_bytes(a.bytes),
                a.detail
            );
        }
    }
    println!("TOTAL:   ~{}", format_bytes(total));

    if cfg.apply && total > cfg.confirm_above && !cfg.yes {
        eprintln!(
            "Refusing large apply without --yes (> {})",
            format_bytes(cfg.confirm_above)
        );
        return ExitCode::FAILURE;
    }

    let all_actions: Vec<_> = results.iter().flat_map(|r| r.actions.clone()).collect();
    let (log, summary) = execute_actions(&all_actions, &cfg, false);
    if cfg.apply {
        println!(
            "Applied: {} items, ~{} freed, {} failures",
            summary.files_deleted,
            format_bytes(summary.bytes_reclaimed),
            summary.failures
        );
    }
    match write_report(&cfg, &results, &log) {
        Ok(p) => println!("Report: {}", p.display()),
        Err(e) => eprintln!("report write failed: {e}"),
    }
    ExitCode::SUCCESS
}
