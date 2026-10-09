import { describe, expect, it } from "vitest";
import {
  breadcrumbs,
  canSelect,
  entryBadges,
  formatAge,
  pickedBytes,
  prunePicks,
  share,
  togglePick,
  topLevelPicks,
  type PickMap,
} from "./overview";
import type { TreeEntry } from "./types";

const entry = (over: Partial<TreeEntry>): TreeEntry => ({
  name: "x",
  path: "/home/u/x",
  kind: "dir",
  bytes: 100,
  items: 1,
  mtime: 0,
  unreadable: false,
  protected: false,
  blocked: null,
  ...over,
});

describe("breadcrumbs", () => {
  it("splits below the scan root", () => {
    expect(breadcrumbs("/home/u", "/home/u/.cache/uv")).toEqual([
      { label: "/home/u", path: "/home/u" },
      { label: ".cache", path: "/home/u/.cache" },
      { label: "uv", path: "/home/u/.cache/uv" },
    ]);
  });

  it("handles / as root and paths outside the root", () => {
    expect(breadcrumbs("/", "/var/lib").map((c) => c.path)).toEqual(["/", "/var", "/var/lib"]);
    expect(breadcrumbs("/home/u", "/home/u")).toHaveLength(1);
    // A sibling with a common prefix is not inside the root.
    expect(breadcrumbs("/home/u", "/home/user2/x")).toHaveLength(1);
  });
});

describe("picks", () => {
  it("toggles only selectable entries", () => {
    let picks: PickMap = new Map();
    picks = togglePick(picks, entry({ path: "/a", bytes: 5 }));
    expect([...picks]).toEqual([["/a", 5]]);
    picks = togglePick(picks, entry({ path: "/a" }));
    expect(picks.size).toBe(0);
    expect(togglePick(picks, entry({ path: "/b", blocked: "system path" })).size).toBe(0);
    expect(togglePick(picks, entry({ path: "", kind: "collapsed" })).size).toBe(0);
    expect(canSelect(entry({ kind: "other_fs", blocked: "mount" }))).toBe(false);
  });

  it("counts nested picks once and prunes deleted ones", () => {
    const picks: PickMap = new Map([
      ["/a", 100],
      ["/a/b", 40],
      ["/ab", 7],
      ["/c/d", 3],
    ]);
    expect(topLevelPicks(picks).map(([p]) => p)).toEqual(["/a", "/ab", "/c/d"]);
    expect(pickedBytes(picks)).toBe(110);
    expect([...prunePicks(picks, ["/a"]).keys()]).toEqual(["/ab", "/c/d"]);
  });
});

describe("formatting", () => {
  it("share is clamped", () => {
    expect(share(50, 200)).toBe(0.25);
    expect(share(5, 0)).toBe(0);
    expect(share(500, 200)).toBe(1);
  });

  it("formats ages", () => {
    const now = 1_000_000_000;
    expect(formatAge(now, null)).toBe("never");
    expect(formatAge(now, now - 120)).toBe("2m ago");
    expect(formatAge(now, now - 3 * 3600)).toBe("3h ago");
    expect(formatAge(now, now - 5 * 86_400)).toBe("5d ago");
    expect(formatAge(now, now - 90 * 86_400)).toBe("3mo ago");
    expect(formatAge(now, now - 800 * 86_400)).toBe("2y ago");
  });

  it("badges explain flags", () => {
    const labels = (e: TreeEntry) => entryBadges(e).map((b) => b.label);
    expect(labels(entry({ protected: true }))).toEqual(["protected"]);
    expect(labels(entry({ blocked: "system path", unreadable: true }))).toEqual(["unreadable", "locked"]);
    expect(labels(entry({ kind: "other_fs", blocked: "mount point" }))).toEqual(["mount"]);
    expect(labels(entry({ kind: "symlink" }))).toEqual(["link"]);
    expect(labels(entry({ kind: "collapsed", blocked: "aggregate" }))).toEqual([]);
  });
});
