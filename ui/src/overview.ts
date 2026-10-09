import type { TreeEntry } from "./types";

export type Crumb = { label: string; path: string };

/** Breadcrumbs from the scan root down to `path` (root shown as its full path). */
export function breadcrumbs(root: string, path: string): Crumb[] {
  const crumbs: Crumb[] = [{ label: root, path: root }];
  if (path === root || !path.startsWith(root === "/" ? "/" : `${root}/`)) return crumbs;
  const rest = path.slice(root === "/" ? 1 : root.length + 1);
  let cur = root;
  for (const part of rest.split("/").filter(Boolean)) {
    cur = cur === "/" ? `/${part}` : `${cur}/${part}`;
    crumbs.push({ label: part, path: cur });
  }
  return crumbs;
}

/** Share of `bytes` in `total`, 0..1. */
export function share(bytes: number, total: number): number {
  if (total <= 0) return 0;
  return Math.min(1, Math.max(0, bytes / total));
}

export function canSelect(e: TreeEntry): boolean {
  return !e.blocked && e.kind !== "collapsed" && e.path !== "";
}

/** Path → bytes for everything picked, possibly across directories. */
export type PickMap = Map<string, number>;

export function togglePick(picks: PickMap, e: TreeEntry): PickMap {
  const next = new Map(picks);
  if (next.has(e.path)) next.delete(e.path);
  else if (canSelect(e)) next.set(e.path, e.bytes);
  return next;
}

/** Drop picks nested inside another pick (deleting the parent covers them). */
export function topLevelPicks(picks: PickMap): [string, number][] {
  const paths = [...picks.keys()];
  return [...picks.entries()].filter(([p]) => !paths.some((q) => q !== p && p.startsWith(`${q}/`)));
}

export function pickedBytes(picks: PickMap): number {
  return topLevelPicks(picks).reduce((s, [, b]) => s + b, 0);
}

/** Remove picks that were deleted (or sit inside something deleted). */
export function prunePicks(picks: PickMap, removed: string[]): PickMap {
  const next = new Map(picks);
  for (const p of picks.keys()) {
    if (removed.some((r) => p === r || p.startsWith(`${r}/`))) next.delete(p);
  }
  return next;
}

export function formatAge(now: number, ts: number | null | undefined): string {
  if (ts == null || ts <= 0) return "never";
  const d = Math.max(0, now - ts);
  if (d < 3600) return `${Math.floor(d / 60)}m ago`;
  if (d < 86_400) return `${Math.floor(d / 3600)}h ago`;
  if (d < 60 * 86_400) return `${Math.floor(d / 86_400)}d ago`;
  if (d < 730 * 86_400) return `${Math.floor(d / (30 * 86_400))}mo ago`;
  return `${Math.floor(d / (365 * 86_400))}y ago`;
}

export function entryBadges(e: TreeEntry): { label: string; tone: "warn" | "muted"; title?: string }[] {
  const out: { label: string; tone: "warn" | "muted"; title?: string }[] = [];
  if (e.protected) out.push({ label: "protected", tone: "warn", title: "Matches a protect rule (models, keys, browsers)" });
  if (e.unreadable) out.push({ label: "unreadable", tone: "muted", title: "Permission denied — size may be incomplete" });
  if (e.kind === "other_fs") out.push({ label: "mount", tone: "muted", title: e.blocked ?? undefined });
  else if (e.blocked && e.kind !== "collapsed") out.push({ label: "locked", tone: "muted", title: e.blocked });
  if (e.kind === "symlink") out.push({ label: "link", tone: "muted" });
  return out;
}
