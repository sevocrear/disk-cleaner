import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import App from "./App";

describe("App navigation", () => {
  it("has Overview and Docker tabs next to the existing panels", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByRole("heading", { name: "Deep cleanup" });
    const sidebar = within(screen.getByRole("complementary"));
    const nav = ["Deep clean", "Review", "Overview", "Docker", "Disks", "Trash", "Settings"];
    expect(sidebar.getAllByRole("button").map((b) => b.textContent)).toEqual(nav);

    await user.click(sidebar.getByRole("button", { name: "Overview" }));
    expect(screen.getByRole("heading", { name: "Overview" })).toBeInTheDocument();
    expect(sidebar.getByRole("button", { name: "Overview" })).toHaveClass("active");

    await user.click(sidebar.getByRole("button", { name: "Docker" }));
    expect(screen.getByRole("heading", { name: "Docker" })).toBeInTheDocument();
    await screen.findByRole("table", { name: "Docker images" });

    await user.click(sidebar.getByRole("button", { name: "Deep clean" }));
    await waitFor(() => expect(screen.getByRole("heading", { name: "Deep cleanup" })).toBeInTheDocument());
  });
});
