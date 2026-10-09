import { describe, expect, it } from "vitest";
import { imageLabel, imagesBytes, isStale, parseDays, sourceLabel, staleIds } from "./docker";
import type { DockerImage } from "./types";

const NOW = 1_000_000_000;
const DAY = 86_400;
const img = (over: Partial<DockerImage>): DockerImage => ({
  id: "sha256:x",
  short_id: "x",
  tags: ["x:1"],
  bytes: 10,
  last_used: NOW,
  last_used_source: "built",
  containers: [],
  running: false,
  ...over,
});

describe("docker helpers", () => {
  it("stale = no containers and last use before cutoff", () => {
    expect(isStale(img({ last_used: NOW - 31 * DAY }), 30, NOW)).toBe(true);
    expect(isStale(img({ last_used: NOW - 29 * DAY }), 30, NOW)).toBe(false);
    expect(isStale(img({ last_used: null }), 30, NOW)).toBe(true);
    expect(isStale(img({ last_used: NOW - 400 * DAY, containers: ["db"] }), 30, NOW)).toBe(false);
  });

  it("collects ids and sizes", () => {
    const images = [
      img({ id: "a", bytes: 5, last_used: NOW - 100 * DAY }),
      img({ id: "b", bytes: 7, last_used: NOW }),
      img({ id: "c", bytes: 11, last_used: NOW - 100 * DAY, containers: ["web"] }),
    ];
    expect(staleIds(images, 30, NOW)).toEqual(["a"]);
    expect(imagesBytes(images, ["a", "b", "zzz"])).toBe(12);
  });

  it("labels and day parsing", () => {
    expect(imageLabel(img({ tags: [], short_id: "abc" }))).toBe("<none> abc");
    expect(sourceLabel("pulled")).toBe("pulled / tagged");
    expect(sourceLabel("")).toBe("unknown");
    expect(parseDays("30")).toBe(30);
    expect(parseDays(" 0 ")).toBe(0);
    expect(parseDays("-1")).toBeNull();
    expect(parseDays("1.5")).toBeNull();
    expect(parseDays("abc")).toBeNull();
    expect(parseDays("99999")).toBeNull();
  });
});
