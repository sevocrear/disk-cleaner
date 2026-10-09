// In-browser stand-ins for the Tauri backend (vite dev, UI tests, e2e).
// They mimic the Rust engine's rules closely enough to exercise the UI flows.
import type {
  DeleteOutcome,
  DirListing,
  DockerInventory,
  InUse,
  NodeKind,
  OverviewScanDone,
  TreeEntry,
} from "./types";

const GB = 1024 ** 3;
const MB = 1024 ** 2;
const DAY = 86_400;
const NOW = Math.floor(Date.UTC(2026, 9, 9, 12) / 1000);

type MockNode = {
  name: string;
  kind: NodeKind;
  bytes?: number;
  items?: number;
  mtime?: number;
  unreadable?: boolean;
  children?: MockNode[];
};

const dir = (name: string, children: MockNode[], extra: Partial<MockNode> = {}): MockNode => ({
  name,
  kind: "dir",
  children,
  ...extra,
});
const file = (name: string, bytes: number, ageDays = 10): MockNode => ({
  name,
  kind: "file",
  bytes,
  mtime: NOW - ageDays * DAY,
});

function seedTree(): MockNode {
  return dir("/", [
    dir("home", [
      dir("user", [
        dir(".cache", [
          dir("uv", [file("archive-v0.tar", 14 * GB), file("wheels.bin", 6 * GB)]),
          dir("huggingface", [dir("hub", [file("model.safetensors", 4.5 * GB, 3)])]),
          dir("pip", [file("http-cache.db", 1.6 * GB, 90)]),
          { name: "(3120 smaller files)", kind: "collapsed", bytes: 120 * MB, items: 3120, mtime: NOW - DAY },
        ]),
        dir("Downloads", [file("ubuntu-24.04.iso", 5.8 * GB, 200), file("old-backup.zip", 300 * MB, 400)]),
        dir("projects", [dir("app", [dir("node_modules", [file("big.node", 1.2 * GB, 30)]), file("README.md", 4096, 2)])]),
        file("screen-recording.mp4", 2.1 * GB, 45),
        { name: "link-to-data", kind: "symlink", bytes: 0, mtime: NOW - DAY },
        dir("empty", []),
      ]),
    ]),
    dir("usr", [file("lib.so", 9 * GB, 100)]),
    dir("var", [dir("lib", [dir("docker", [], { unreadable: true })])]),
    { name: "media", kind: "other_fs", bytes: 0, mtime: NOW },
  ]);
}

let tree = seedTree();
let scannedRoot: string | null = null;

const DENY = ["/usr", "/bin", "/sbin", "/lib", "/etc", "/boot", "/sys", "/proc", "/dev", "/run", "/snap", "/var/lib/docker"];
const MOCK_HOME = "/home/user";

function join(parent: string, name: string): string {
  return parent === "/" ? `/${name}` : `${parent}/${name}`;
}

function sizeOf(n: MockNode): number {
  if (n.kind === "dir") return (n.children ?? []).reduce((s, c) => s + sizeOf(c), 4096);
  return n.bytes ?? 0;
}

function itemsOf(n: MockNode): number {
  if (n.kind === "dir") return (n.children ?? []).reduce((s, c) => s + (c.kind === "dir" ? 1 + itemsOf(c) : itemsOf(c)), 0);
  return n.items ?? 1;
}

function find(path: string): MockNode | null {
  if (path === "/") return tree;
  let node: MockNode | undefined = tree;
  for (const part of path.split("/").filter(Boolean)) {
    node = node?.children?.find((c) => c.name === part);
    if (!node) return null;
  }
  return node ?? null;
}

export function mockBlockReason(path: string, kind: NodeKind): string | null {
  if (kind === "collapsed") return "aggregate of small files";
  if (kind === "other_fs") return "another filesystem (mount point)";
  if (path === "/") return "filesystem root";
  if (DENY.some((p) => path === p || path.startsWith(`${p}/`))) return "system path";
  if (MOCK_HOME === path || MOCK_HOME.startsWith(`${path}/`)) return "home directory or its parent";
  return null;
}

function isProtected(path: string): boolean {
  return /\/(huggingface|torch|transformers|\.ssh)(\/|$)/.test(path);
}

export function mockListing(path: string): DirListing {
  const node = find(path);
  if (!node || node.kind !== "dir" || !scannedRoot) throw new Error(`${path} is not in the scanned tree`);
  const entries: TreeEntry[] = (node.children ?? [])
    .map((c) => {
      const p = c.kind === "collapsed" ? "" : join(path, c.name);
      return {
        name: c.name,
        path: p,
        kind: c.kind,
        bytes: sizeOf(c),
        items: itemsOf(c),
        mtime: c.mtime ?? NOW - 5 * DAY,
        unreadable: Boolean(c.unreadable),
        protected: c.kind !== "collapsed" && isProtected(p),
        blocked: mockBlockReason(p, c.kind),
      };
    })
    .sort((a, b) => b.bytes - a.bytes || a.name.localeCompare(b.name));
  const parent = path === scannedRoot ? null : path.slice(0, path.lastIndexOf("/")) || "/";
  return {
    root: scannedRoot,
    path,
    parent,
    bytes: sizeOf(node),
    items: itemsOf(node),
    unreadable: Boolean(node.unreadable),
    entries,
  };
}

export async function mockOverviewScan(path: string): Promise<OverviewScanDone> {
  await new Promise((r) => setTimeout(r, 150));
  const node = find(path);
  if (!node || node.kind !== "dir") throw new Error(`${path}: No such file or directory (os error 2)`);
  scannedRoot = path;
  const total = sizeOf(node);
  const used = 265 * GB;
  return {
    root: path,
    total_bytes: total,
    cancelled: false,
    unreadable_dirs: 1,
    accounting: {
      fs: { mount_point: "/", total_bytes: 458 * GB, used_bytes: used, available_bytes: 170 * GB, reserved_bytes: 23 * GB },
      root: path,
      is_mount_root: path === "/",
      scanned_bytes: total,
      unaccounted_bytes: path === "/" ? used - total : 0,
      sources:
        path === "/"
          ? [
              { kind: "docker", label: "Docker data (/var/lib/docker)", bytes: 13.7 * GB, detail: "Images 3.5GB · Local Volumes 10.2GB" },
              { kind: "deleted_open", label: "Deleted files still held open", bytes: 1.2 * GB, detail: "obsidian 1.2GB — freed when these processes exit" },
            ]
          : [],
      unreadable_dirs: 1,
    },
    listing: mockListing(path),
  };
}

export function mockInUse(paths: string[]): InUse[] {
  return paths
    .filter((p) => p.includes("/.cache/uv"))
    .map((p) => ({ path: p, processes: [{ pid: 4242, name: "uvx", how: "mapped" }] }));
}

export async function mockOverviewDelete(paths: string[], useTrash: boolean): Promise<DeleteOutcome> {
  await new Promise((r) => setTimeout(r, 100));
  const out: DeleteOutcome = { removed: [], bytes_freed: 0, failures: [], trashed: useTrash };
  for (const p of [...paths].sort()) {
    if (out.removed.some((r) => p.startsWith(`${r}/`))) continue;
    const node = find(p);
    if (!node) {
      out.failures.push({ path: p, error: "not in the scanned tree" });
      continue;
    }
    const reason = mockBlockReason(p, node.kind);
    if (reason) {
      out.failures.push({ path: p, error: reason });
      continue;
    }
    const parent = find(p.slice(0, p.lastIndexOf("/")) || "/");
    parent!.children = parent!.children!.filter((c) => c !== node);
    out.removed.push(p);
    out.bytes_freed += sizeOf(node);
  }
  return out;
}

export function resetMockTree(): void {
  tree = seedTree();
  scannedRoot = null;
}

// ---------- Docker ----------

function seedDocker(): DockerInventory {
  return {
    available: true,
    now: NOW,
    images: [
      {
        id: "sha256:triton",
        short_id: "4f1e2d3c4b5a",
        tags: ["nvcr.io/nvidia/tritonserver:26.03-py3", "vv-tritonserver:26.03-py3"],
        bytes: 21.7 * GB,
        created: NOW - 190 * DAY,
        last_tag_time: NOW - 180 * DAY,
        last_used: NOW - 95 * DAY,
        last_used_source: "container",
        containers: [],
        running: false,
      },
      {
        id: "sha256:pipeline",
        short_id: "9a8b7c6d5e4f",
        tags: ["vv-pipeline:latest"],
        bytes: 9.9 * GB,
        created: NOW - 40 * DAY,
        last_tag_time: null,
        last_used: NOW - 40 * DAY,
        last_used_source: "built",
        containers: [],
        running: false,
      },
      {
        id: "sha256:decoder",
        short_id: "1a2b3c4d5e6f",
        tags: ["vv-decoder:8065840"],
        bytes: 2.3 * GB,
        created: NOW - 2 * DAY,
        last_tag_time: null,
        last_used: NOW - 2 * DAY,
        last_used_source: "built",
        containers: [],
        running: false,
      },
      {
        id: "sha256:postgres",
        short_id: "0f9e8d7c6b5a",
        tags: ["postgres:15-alpine"],
        bytes: 417 * MB,
        created: NOW - 300 * DAY,
        last_tag_time: NOW - 21 * DAY,
        last_used: NOW,
        last_used_source: "running",
        containers: ["stock_cv_pg"],
        running: true,
      },
      {
        id: "sha256:hello",
        short_id: "5e2309035332",
        tags: ["hello-world:latest"],
        bytes: 15 * 1024,
        created: NOW - 200 * DAY,
        last_tag_time: NOW - 60 * DAY,
        last_used: NOW - 60 * DAY,
        last_used_source: "container",
        containers: ["cool_aryabhata"],
        running: false,
      },
    ],
    build_cache: {
      available: true,
      records: 391,
      total_bytes: 103 * GB,
      reclaimable_bytes: 90 * GB,
      stale_bytes: 41.5 * GB,
      stale_records: 210,
      days: 30,
    },
    tracker: {
      installed: false,
      active: false,
      store_path: "/home/user/.local/share/disk-cleaner/docker-usage.json",
      tracked_images: 0,
      since: null,
    },
  };
}

let docker = seedDocker();

export async function mockDockerInventory(days: number): Promise<DockerInventory> {
  await new Promise((r) => setTimeout(r, 120));
  const scale = days <= 0 ? 1 : Math.min(1, 30 / days);
  return {
    ...docker,
    build_cache: {
      ...docker.build_cache,
      days,
      stale_bytes: docker.build_cache.stale_bytes * scale,
      stale_records: Math.round(docker.build_cache.stale_records * scale),
    },
    images: docker.images.map((i) => ({ ...i, containers: [...i.containers] })),
  };
}

export async function mockDockerRemoveImages(ids: string[]): Promise<number> {
  const busy = docker.images.find((i) => ids.includes(i.id) && i.containers.length);
  if (busy) throw new Error(`image ${busy.short_id} is used by container(s): ${busy.containers.join(", ")}`);
  const freed = docker.images.filter((i) => ids.includes(i.id)).reduce((s, i) => s + i.bytes, 0);
  docker = { ...docker, images: docker.images.filter((i) => !ids.includes(i.id)) };
  return freed;
}

export async function mockDockerPruneCache(): Promise<number> {
  const freed = docker.build_cache.stale_bytes;
  docker = {
    ...docker,
    build_cache: {
      ...docker.build_cache,
      total_bytes: docker.build_cache.total_bytes - freed,
      reclaimable_bytes: docker.build_cache.reclaimable_bytes - freed,
      records: docker.build_cache.records - docker.build_cache.stale_records,
      stale_bytes: 0,
      stale_records: 0,
    },
  };
  return freed;
}

export function mockTracker(installed: boolean) {
  docker = {
    ...docker,
    tracker: { ...docker.tracker, installed, active: installed, since: installed ? NOW : null },
  };
  return docker.tracker;
}

export function resetMockDocker(): void {
  docker = seedDocker();
}
