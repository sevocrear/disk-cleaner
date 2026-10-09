export type Action = {
  phase: string;
  kind: string;
  path: string;
  bytes: number;
  detail: string;
  command?: string[] | null;
  id: string;
};

export type PhaseResult = {
  name: string;
  reclaimable_bytes: number;
  actions: Action[];
  notes: string[];
};

export type Config = {
  apply: boolean;
  yes: boolean;
  docker_unused_days: number;
  file_unused_days: number;
  app_unused_days: number;
  min_file_size: number;
  min_dupe_size: number;
  dedupe_keep: string;
  extra_roots: string[];
  /** Mount points to include in Deep clean scans. Default: ["/"]. */
  scan_mounts: string[];
  dedupe_roots: string[];
  protect_globs: string[];
  protect_apps: string[];
  skip_docker: boolean;
  skip_caches: boolean;
  skip_files: boolean;
  skip_apps: boolean;
  skip_dupes: boolean;
  skip_media: boolean;
  only?: string | null;
  journal_vacuum: string;
  include_docker_volumes: boolean;
  include_hf_cache: boolean;
  include_opt_apps: boolean;
  confirm_above: number;
  cross_fs: boolean;
  home: string;
  report_dir?: string | null;
  file_roots?: string[] | null;
  cmd_timeout_sec: number;
  docker_timeout_sec: number;
  include_pkg_managers: boolean;
  gui_use_trash: boolean;
};

export type DiskInfo = {
  name: string;
  mount_point: string;
  total_bytes: number;
  available_bytes: number;
  used_bytes: number;
  file_system: string;
  is_removable: boolean;
};

export type TrashItem = {
  name: string;
  original_path: string;
  trash_path: string;
  info_path: string;
  bytes: number;
  deleted_at?: string | null;
  is_dir: boolean;
};

export type ApplySummary = {
  files_deleted: number;
  bytes_reclaimed: number;
  failures: number;
  by_phase: [string, number][];
};

export type ApplyProgress = {
  done: number;
  total: number;
  bytes_reclaimed: number;
  failures: number;
  path: string;
};

export type Panel = "scan" | "review" | "overview" | "docker" | "disks" | "trash" | "settings";

export type NodeKind = "dir" | "file" | "symlink" | "other_fs" | "collapsed";

export type TreeEntry = {
  name: string;
  /** Absolute path; empty for collapsed aggregates. */
  path: string;
  kind: NodeKind;
  bytes: number;
  items: number;
  mtime: number;
  unreadable: boolean;
  /** Matches a protect glob: deletable, but warn. */
  protected: boolean;
  /** Why it cannot be deleted. */
  blocked?: string | null;
};

export type DirListing = {
  root: string;
  path: string;
  parent?: string | null;
  bytes: number;
  items: number;
  unreadable: boolean;
  entries: TreeEntry[];
};

export type TreeScanProgress = {
  files: number;
  dirs: number;
  bytes: number;
  current: string;
};

export type FsUsage = {
  mount_point: string;
  total_bytes: number;
  used_bytes: number;
  available_bytes: number;
  reserved_bytes: number;
};

export type HiddenSource = {
  kind: "docker" | "deleted_open" | "unreadable" | string;
  label: string;
  bytes: number;
  detail: string;
};

export type SpaceAccounting = {
  fs: FsUsage;
  root: string;
  is_mount_root: boolean;
  scanned_bytes: number;
  unaccounted_bytes: number;
  sources: HiddenSource[];
  unreadable_dirs: number;
};

export type OverviewScanDone = {
  root: string;
  total_bytes: number;
  cancelled: boolean;
  unreadable_dirs: number;
  accounting?: SpaceAccounting | null;
  listing?: DirListing | null;
};

export type ProcessUse = { pid: number; name: string; how: string };
export type InUse = { path: string; processes: ProcessUse[] };

export type DeleteOutcome = {
  removed: string[];
  bytes_freed: number;
  failures: { path: string; error: string }[];
  trashed: boolean;
};

export type DockerImage = {
  id: string;
  short_id: string;
  tags: string[];
  bytes: number;
  created?: number | null;
  last_tag_time?: number | null;
  last_used?: number | null;
  last_used_source: string;
  containers: string[];
  running: boolean;
};

export type BuildCacheSummary = {
  available: boolean;
  records: number;
  total_bytes: number;
  reclaimable_bytes: number;
  stale_bytes: number;
  stale_records: number;
  days: number;
};

export type TrackerStatus = {
  installed: boolean;
  active: boolean;
  store_path: string;
  tracked_images: number;
  since?: number | null;
};

export type DockerInventory = {
  available: boolean;
  error?: string | null;
  images: DockerImage[];
  build_cache: BuildCacheSummary;
  tracker: TrackerStatus;
  now: number;
};
