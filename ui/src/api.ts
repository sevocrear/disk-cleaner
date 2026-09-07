import { invoke } from "@tauri-apps/api/core";
import type { Action, ApplySummary, Config, DiskInfo, PhaseResult, TrashItem } from "./types";

const browser = typeof window !== "undefined" && !(window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;

const mockConfig = (): Config => ({
  apply: false,
  yes: false,
  docker_unused_days: 14,
  file_unused_days: 60,
  app_unused_days: 60,
  min_file_size: 1024 * 1024,
  min_dupe_size: 1024 * 1024,
  dedupe_keep: "newest",
  extra_roots: [],
  dedupe_roots: [],
  protect_globs: [],
  protect_apps: [],
  skip_docker: false,
  skip_caches: false,
  skip_files: false,
  skip_apps: false,
  skip_dupes: false,
  skip_media: false,
  journal_vacuum: "7d",
  include_docker_volumes: false,
  include_hf_cache: false,
  include_opt_apps: false,
  confirm_above: 5 * 1024 ** 3,
  cross_fs: false,
  home: "/home/user",
  cmd_timeout_sec: 120,
  docker_timeout_sec: 600,
  include_pkg_managers: false,
  gui_use_trash: true,
});

export async function apiGetConfig(): Promise<Config> {
  if (browser) return mockConfig();
  return invoke("get_config");
}

export async function apiSaveConfig(config: Config): Promise<void> {
  if (browser) return;
  return invoke("save_config", { config });
}

export async function apiListDisks(): Promise<DiskInfo[]> {
  if (browser) {
    return [
      {
        name: "/dev/nvme0n1p2",
        mount_point: "/",
        total_bytes: 512 * 1024 ** 3,
        available_bytes: 180 * 1024 ** 3,
        used_bytes: 332 * 1024 ** 3,
        file_system: "ext4",
        is_removable: false,
      },
    ];
  }
  return invoke("list_disks");
}

export async function apiListTrash(): Promise<TrashItem[]> {
  if (browser) return [];
  return invoke("list_trash");
}

export async function apiRestoreTrash(item: TrashItem): Promise<void> {
  return invoke("restore_trash", { item });
}

export async function apiDeleteTrash(item: TrashItem): Promise<void> {
  return invoke("delete_trash", { item });
}

export async function apiEmptyTrash(): Promise<number> {
  return invoke("empty_trash");
}

export async function apiStartScan(config: Config): Promise<{ results: PhaseResult[]; total_bytes: number }> {
  if (browser) {
    await new Promise((r) => setTimeout(r, 600));
    return {
      total_bytes: 2.4 * 1024 ** 3,
      results: [
        {
          name: "caches",
          reclaimable_bytes: 1.2 * 1024 ** 3,
          notes: [],
          actions: [
            {
              id: "1",
              phase: "caches",
              kind: "delete_tree",
              path: "/home/user/.cache/old",
              bytes: 1.2 * 1024 ** 3,
              detail: "unused >60d",
            },
          ],
        },
        {
          name: "media",
          reclaimable_bytes: 1.2 * 1024 ** 3,
          notes: ["Downloads: 3 candidates"],
          actions: [
            {
              id: "2",
              phase: "media",
              kind: "delete_file",
              path: "/home/user/Downloads/old.iso",
              bytes: 1.2 * 1024 ** 3,
              detail: "Downloads: large+unused",
            },
          ],
        },
      ],
    };
  }
  return invoke("start_scan", { config });
}

export async function apiApplySelected(config: Config, actions: Action[]): Promise<{ summary: ApplySummary }> {
  if (browser) {
    return {
      summary: {
        files_deleted: actions.length,
        bytes_reclaimed: actions.reduce((s, a) => s + a.bytes, 0),
        failures: 0,
        by_phase: Object.entries(
          actions.reduce<Record<string, number>>((acc, a) => {
            acc[a.phase] = (acc[a.phase] || 0) + a.bytes;
            return acc;
          }, {}),
        ),
      },
    };
  }
  return invoke("apply_selected", { config, actions });
}

export async function apiFormatBytes(n: number): Promise<string> {
  if (browser) return formatBytesLocal(n);
  return invoke("format_bytes_cmd", { n });
}

export async function apiOpenPath(path: string): Promise<void> {
  if (browser) return;
  return invoke("open_path", { path });
}

export function formatBytesLocal(n: number): string {
  const abs = Math.abs(n);
  for (const [u, d] of [
    ["T", 1024 ** 4],
    ["G", 1024 ** 3],
    ["M", 1024 ** 2],
    ["K", 1024],
  ] as const) {
    if (abs >= d) return `${(n / d).toFixed(1)}${u}`;
  }
  return `${n}B`;
}
