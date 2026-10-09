import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { apiGetConfig } from "./api";
import { resetMockTree } from "./mocks";
import { OverviewPanel } from "./OverviewPanel";
import type { Config } from "./types";

let config: Config;

beforeEach(async () => {
  resetMockTree();
  config = await apiGetConfig();
});

function rows() {
  return within(screen.getByRole("grid", { name: "Folder contents" })).getAllByRole("row");
}

function nameOf(row: HTMLElement) {
  return row.querySelector(".name-btn, .name-plain")!.textContent!.replace(/^\S+\s/, "");
}

function rowNames() {
  return rows().map(nameOf);
}

function rowFor(name: string) {
  const row = rows().find((r) => nameOf(r) === name);
  if (!row) throw new Error(`no row ${name}: ${rowNames().join(", ")}`);
  return row;
}

/** Folder name button inside the listing (not the crumb or the Open button). */
function folder(name: string) {
  return within(screen.getByRole("grid", { name: "Folder contents" })).getByRole("button", { name });
}

async function scanHome(user = userEvent.setup(), onChanged = vi.fn()) {
  render(<OverviewPanel config={config} onChanged={onChanged} />);
  expect(screen.getByLabelText("Folder to scan")).toHaveValue("/home/user");
  await user.click(screen.getByRole("button", { name: "Scan" }));
  await screen.findByRole("grid", { name: "Folder contents" });
  return { user, onChanged };
}

describe("OverviewPanel", () => {
  it("shows a prompt before the first scan", () => {
    render(<OverviewPanel config={config} onChanged={vi.fn()} />);
    expect(screen.getByText(/Pick a folder or disk and scan/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Move to Trash" })).toBeDisabled();
  });

  it("scans and lists children largest first with shares", async () => {
    await scanHome();
    expect(rowNames()).toEqual([".cache", "Downloads", "screen-recording.mp4", "projects", "empty", "link-to-data"]);
    const cache = rowFor(".cache");
    expect(cache.querySelector(".tree-size")).toHaveTextContent("26.2GB");
    expect(cache.querySelector(".tree-share")).toHaveTextContent(/^\d+\.\d%$/);
    expect(screen.getByRole("button", { name: "Rescan" })).toBeInTheDocument();
    // Accounting card for the filesystem is shown.
    expect(screen.getByRole("region", { name: "Disk usage" })).toHaveTextContent("used of");
  });

  it("navigates into folders and back with breadcrumbs", async () => {
    const { user } = await scanHome();
    await user.click(folder(".cache"));
    await waitFor(() => expect(rowNames()).toEqual(["uv", "huggingface", "pip", "(3120 smaller files)"]));
    const crumbs = screen.getByRole("navigation", { name: "Breadcrumbs" });
    expect(within(crumbs).getByRole("button", { name: ".cache" })).toHaveAttribute("aria-current", "location");

    await user.click(folder("uv"));
    await waitFor(() => expect(rowNames()).toEqual(["archive-v0.tar", "wheels.bin"]));
    await user.click(within(crumbs).getByRole("button", { name: "/home/user" }));
    await waitFor(() => expect(rowNames()[0]).toBe(".cache"));
    expect(screen.getByRole("button", { name: "Up one level" })).toBeDisabled();
  });

  it("marks protected, aggregate and blocked entries", async () => {
    const { user } = await scanHome();
    await user.click(folder(".cache"));
    await waitFor(() => expect(rowFor("huggingface")).toBeTruthy());
    expect(within(rowFor("huggingface")).getByText("protected")).toBeInTheDocument();
    expect(within(rowFor("huggingface")).getByRole("checkbox")).toBeEnabled();
    expect(within(rowFor("(3120 smaller files)")).getByRole("checkbox")).toBeDisabled();
  });

  it("scanning / explains invisible space and locks system paths", async () => {
    const user = userEvent.setup();
    render(<OverviewPanel config={config} onChanged={vi.fn()} />);
    const input = screen.getByLabelText("Folder to scan");
    await user.clear(input);
    await user.type(input, "/{Enter}");
    await screen.findByRole("grid", { name: "Folder contents" });

    const card = screen.getByRole("region", { name: "Disk usage" });
    expect(card).toHaveTextContent("Docker data (/var/lib/docker)");
    expect(card).toHaveTextContent("Deleted files still held open");
    expect(card).toHaveTextContent("where a normal user can't look");

    expect(within(rowFor("usr")).getByRole("checkbox")).toBeDisabled();
    expect(within(rowFor("usr")).getByText("locked")).toHaveAttribute("title", "system path");
    expect(within(rowFor("home")).getByRole("checkbox")).toBeDisabled();
    expect(within(rowFor("media")).getByText("mount")).toBeInTheDocument();
  });

  it("deletes after confirming and warns about programs using the path", async () => {
    const { user, onChanged } = await scanHome();
    await user.click(folder(".cache"));
    await waitFor(() => expect(rowFor("uv")).toBeTruthy());
    await user.click(screen.getByRole("checkbox", { name: "Select uv" }));
    await user.click(screen.getByRole("checkbox", { name: "Select pip" }));
    expect(screen.getByText("2 selected")).toBeInTheDocument();
    expect(screen.getByText("21.6GB")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Delete permanently" }));
    const dialog = await screen.findByRole("dialog", { name: "Delete permanently?" });
    expect(dialog).toHaveTextContent("2 items");
    await waitFor(() => expect(within(dialog).getByRole("alert")).toHaveTextContent("uvx (4242, mapped)"));
    expect(dialog).toHaveTextContent("This cannot be undone.");

    await user.click(within(dialog).getByRole("button", { name: "Delete permanently" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(screen.getByRole("status")).toHaveTextContent("Deleted 2 items · 21.6GB");
    expect(rowNames()).toEqual(["huggingface", "(3120 smaller files)"]);
    expect(screen.getByText("0 selected")).toBeInTheDocument();
    expect(onChanged).toHaveBeenCalledTimes(1);
  });

  it("cancel keeps everything; Escape closes the dialog", async () => {
    const { user } = await scanHome();
    await user.click(screen.getByRole("checkbox", { name: "Select Downloads" }));
    await user.click(screen.getByRole("button", { name: "Move to Trash" }));
    let dialog = await screen.findByRole("dialog", { name: "Move to Trash?" });
    expect(dialog).toHaveTextContent("space is freed only then");
    await waitFor(() => expect(dialog).toHaveTextContent("No running program uses these paths."));
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Move to Trash" }));
    dialog = await screen.findByRole("dialog");
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(rowNames()).toContain("Downloads");
    expect(screen.getByText("1 selected")).toBeInTheDocument();
  });

  it("supports keyboard navigation, selection and delete", async () => {
    const { user } = await scanHome();
    const grid = screen.getByRole("grid", { name: "Folder contents" });
    grid.focus();
    expect(rows()[0]).toHaveAttribute("aria-selected", "true");

    await user.keyboard("{ArrowDown}");
    expect(rows()[1]).toHaveAttribute("aria-selected", "true");
    await user.keyboard(" "); // select Downloads, cursor moves on
    expect(screen.getByRole("checkbox", { name: "Select Downloads" })).toBeChecked();
    expect(rows()[2]).toHaveAttribute("aria-selected", "true");

    await user.keyboard("{ArrowUp}{ArrowUp}{Enter}"); // open .cache
    await waitFor(() => expect(rowNames()[0]).toBe("uv"));
    await user.keyboard("{Backspace}"); // back, cursor on .cache
    await waitFor(() => expect(rowNames()[0]).toBe(".cache"));
    expect(rows()[0]).toHaveAttribute("aria-selected", "true");

    await user.keyboard("{Delete}");
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent("Downloads");
  });

  it("delete key on an unselected row asks for that row only", async () => {
    const { user } = await scanHome();
    screen.getByRole("grid", { name: "Folder contents" }).focus();
    await user.keyboard("{End}{ArrowUp}{ArrowUp}{Delete}");
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent("/home/user/projects");
    expect(dialog).toHaveTextContent("1 item ·");
  });

  it("reports scan errors", async () => {
    const user = userEvent.setup();
    render(<OverviewPanel config={config} onChanged={vi.fn()} />);
    const input = screen.getByLabelText("Folder to scan");
    await user.clear(input);
    await user.type(input, "/nope{Enter}");
    expect(await screen.findByRole("alert")).toHaveTextContent("No such file or directory");
  });

  it("rescan resets selection; quick root chips scan directly", async () => {
    const { user } = await scanHome();
    await user.click(screen.getByRole("checkbox", { name: "Select Downloads" }));
    await user.click(screen.getByRole("button", { name: "Home" }));
    expect(screen.getByText("0 selected")).toBeInTheDocument();
    await screen.findByRole("button", { name: "Rescan" });
    await user.click(screen.getByRole("button", { name: "/" }));
    await waitFor(() => expect(rowNames()).toContain("usr"));
  });
});
