import { describe, expect, it } from "vitest";
import type { Action } from "./types";
import {
  AUTO_SELECT_LIMIT,
  actionable,
  canClean,
  cleanButtonLabel,
  defaultItemSelection,
  emptyItemSelection,
  isCommandAction,
  isDefaultSelected,
  isItemSelected,
  isSelectionEmpty,
  pruneItemSelection,
  resolveSelectedActions,
  selectionAfterEnablingCategory,
  selectionWithAll,
  selectionWithSized,
  toggleItemSelection,
} from "./selection";

function action(partial: Partial<Action> & Pick<Action, "id">): Action {
  return {
    phase: "caches",
    kind: "unlink",
    path: `/tmp/${partial.id}`,
    bytes: 1024,
    detail: "test",
    command: null,
    ...partial,
  };
}

describe("selection helpers", () => {
  it("filters rmdir_if_empty from actionable lists", () => {
    const actions = [
      action({ id: "a", kind: "unlink" }),
      action({ id: "b", kind: "rmdir_if_empty", bytes: 0 }),
    ];
    expect(actionable(actions).map((a) => a.id)).toEqual(["a"]);
  });

  it("marks command kinds and command payloads as commands", () => {
    expect(isCommandAction(action({ id: "1", kind: "docker_cmd" }))).toBe(true);
    expect(isCommandAction(action({ id: "2", kind: "unlink", command: ["rm", "-rf"] }))).toBe(true);
    expect(isCommandAction(action({ id: "3", kind: "unlink" }))).toBe(false);
  });

  it("default-selects sized items and network_prune", () => {
    expect(isDefaultSelected(action({ id: "1", bytes: 10 }))).toBe(true);
    expect(isDefaultSelected(action({ id: "2", bytes: 0, path: "network_prune" }))).toBe(true);
    expect(isDefaultSelected(action({ id: "3", bytes: 0, path: "/tmp/x" }))).toBe(false);
  });

  it("large scan auto-selects via includeAll so Move to Trash works without Select all", () => {
    const many = Array.from({ length: AUTO_SELECT_LIMIT + 1 }, (_, i) =>
      action({ id: `f${i}`, bytes: 100 }),
    );
    const sel = defaultItemSelection(many);
    expect(sel.includeAll).toBe(true);
    expect(isSelectionEmpty(sel)).toBe(false);
    expect(resolveSelectedActions(many, sel)).toHaveLength(AUTO_SELECT_LIMIT + 1);
    expect(canClean(resolveSelectedActions(many, sel).length, false, false)).toBe(true);
  });

  it("small scan keeps sized id selection", () => {
    const actions = [action({ id: "a", bytes: 1 }), action({ id: "b", bytes: 0 })];
    const sel = defaultItemSelection(actions);
    expect(sel.includeAll).toBe(false);
    expect([...sel.ids]).toEqual(["a"]);
  });

  it("enabling a category after empty selection selects all (checkbox → cleanable)", () => {
    const empty = emptyItemSelection();
    expect(isSelectionEmpty(empty)).toBe(true);
    const next = selectionAfterEnablingCategory(empty);
    expect(next.includeAll).toBe(true);
    expect(canClean(3, false, false)).toBe(true);
  });

  it("includeAll selects every action without building an id set", () => {
    const actions = [action({ id: "a" }), action({ id: "b", bytes: 0 })];
    const sel = selectionWithAll();
    expect(resolveSelectedActions(actions, sel)).toEqual(actions);
    expect(isItemSelected("a", sel)).toBe(true);
  });

  it("toggle while includeAll uses excluded set", () => {
    const actions = [action({ id: "a" }), action({ id: "b" }), action({ id: "c" })];
    let sel = selectionWithAll();
    sel = toggleItemSelection(sel, "b", false);
    expect(isItemSelected("b", sel)).toBe(false);
    expect(resolveSelectedActions(actions, sel).map((a) => a.id)).toEqual(["a", "c"]);
    sel = toggleItemSelection(sel, "b", true);
    expect(resolveSelectedActions(actions, sel)).toEqual(actions);
  });

  it("select sized then prune drops ids outside allowed categories", () => {
    const sel = selectionWithSized([
      action({ id: "keep", bytes: 10 }),
      action({ id: "drop", bytes: 10 }),
    ]);
    const pruned = pruneItemSelection(sel, new Set(["keep"]));
    expect([...pruned.ids]).toEqual(["keep"]);
    expect(pruned.includeAll).toBe(false);
  });
});

describe("clean button", () => {
  it("is disabled with zero selection", () => {
    expect(canClean(0, false, false)).toBe(false);
    expect(canClean(3, false, false)).toBe(true);
    expect(canClean(3, true, false)).toBe(false);
    expect(canClean(3, false, true)).toBe(false);
  });

  it("always labels Move to Trash when trash mode is on", () => {
    const cmds = [action({ id: "d", kind: "docker_cmd", bytes: 0, command: ["docker", "prune"] })];
    expect(cleanButtonLabel(cmds, true, false)).toBe("Move to Trash");
    expect(cleanButtonLabel(cmds, true, true)).toBe("Cleaning…");
    expect(cleanButtonLabel(cmds, false, false)).toBe("Clean");
  });
});
