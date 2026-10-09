import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";
import { apiGetConfig } from "./api";
import { DockerPanel } from "./DockerPanel";
import { resetMockDocker } from "./mocks";
import type { Config } from "./types";

let config: Config;

beforeEach(async () => {
  resetMockDocker();
  config = { ...(await apiGetConfig()), docker_unused_days: 30 };
});

async function setup() {
  const user = userEvent.setup();
  render(<DockerPanel config={config} />);
  await screen.findByRole("table", { name: "Docker images" });
  return user;
}

function imageRow(label: string) {
  const row = screen
    .getAllByRole("row")
    .find((r) => r.querySelector(".docker-name strong")?.textContent === label);
  if (!row) throw new Error(`no row ${label}`);
  return row;
}

describe("DockerPanel", () => {
  it("lists images with last use and flags stale ones", async () => {
    await setup();
    const triton = imageRow("nvcr.io/nvidia/tritonserver:26.03-py3");
    expect(triton).toHaveTextContent("+1 tags");
    expect(triton).toHaveTextContent("unused > 30d");
    expect(triton).toHaveTextContent("3mo ago");
    expect(imageRow("vv-pipeline:latest")).toHaveTextContent("40d ago · built");
    expect(imageRow("vv-decoder:8065840")).not.toHaveTextContent("unused >");

    const pg = imageRow("postgres:15-alpine");
    expect(pg).toHaveTextContent("running");
    expect(pg).toHaveTextContent("now · running now");
    expect(within(pg).getByRole("checkbox")).toBeDisabled();
    // A stopped container still pins its image, even when old.
    const hello = imageRow("hello-world:latest");
    expect(within(hello).getByRole("checkbox")).toBeDisabled();
    expect(hello).not.toHaveTextContent("unused >");
  });

  it("selects unused images and removes them after confirming", async () => {
    const user = await setup();
    await user.click(screen.getByRole("button", { name: /Select unused > 30d \(2 · 31\.6GB\)/ }));
    expect(screen.getByText("2 selected")).toBeInTheDocument();
    expect(screen.getByText("31.6GB")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Remove selected images" }));
    const dialog = await screen.findByRole("dialog", { name: "Remove images?" });
    expect(dialog).toHaveTextContent("2 images");
    expect(dialog).toHaveTextContent("vv-pipeline:latest");
    await user.click(within(dialog).getByRole("button", { name: "Remove" }));

    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(screen.getByRole("status")).toHaveTextContent("Removed 2 images: freed 31.6GB.");
    await waitFor(() => expect(screen.queryByText("vv-pipeline:latest")).not.toBeInTheDocument());
    expect(screen.getByText("0 selected")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Select unused > 30d \(0/ })).toBeDisabled();
  });

  it("changing the threshold re-evaluates staleness", async () => {
    const user = await setup();
    const input = screen.getByLabelText("Unused for more than");
    await user.clear(input);
    await user.type(input, "60{Enter}");
    await waitFor(() => expect(screen.getByRole("button", { name: /Select unused > 60d \(1 ·/ })).toBeInTheDocument());
    expect(imageRow("vv-pipeline:latest")).not.toHaveTextContent("unused >");
    await screen.findByRole("button", { name: "Apply" }); // reload finished

    await user.clear(input);
    await user.type(input, "abc");
    expect(input).toHaveAttribute("aria-invalid", "true");
    expect(screen.getByRole("button", { name: "Apply" })).toBeDisabled();
  });

  it("manual selection toggles individual images", async () => {
    const user = await setup();
    const box = within(imageRow("vv-decoder:8065840")).getByRole("checkbox");
    await user.click(box);
    expect(screen.getByText("1 selected")).toBeInTheDocument();
    await user.click(box);
    expect(screen.getByText("0 selected")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Remove selected images" })).toBeDisabled();
  });

  it("prunes old build cache with confirmation", async () => {
    const user = await setup();
    const card = screen.getByRole("region", { name: "Build cache" });
    expect(card).toHaveTextContent("103.0GB total");
    expect(card).toHaveTextContent("41.5GB in 210 records unused for more than 30 days");
    await user.click(within(card).getByRole("button", { name: /Prune cache unused > 30d/ }));
    const dialog = await screen.findByRole("dialog", { name: "Prune build cache?" });
    await user.click(within(dialog).getByRole("button", { name: "Prune" }));
    await waitFor(() => expect(screen.getByRole("status")).toHaveTextContent("Pruned build cache: freed 41.5GB."));
    await waitFor(() => expect(within(card).getByRole("button", { name: /Prune cache/ })).toBeDisabled());
  });

  it("turns the usage tracker on and off", async () => {
    const user = await setup();
    const card = screen.getByRole("region", { name: "Usage tracker" });
    expect(card).toHaveTextContent("off");
    await user.click(within(card).getByRole("button", { name: /Turn on/ }));
    await waitFor(() => expect(within(card).getByText("on")).toBeInTheDocument());
    await user.click(within(card).getByRole("button", { name: "Turn off" }));
    await waitFor(() => expect(within(card).getByText("off")).toBeInTheDocument());
  });
});
