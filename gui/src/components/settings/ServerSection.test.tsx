import { act, screen, waitFor } from "@testing-library/react";
import { describe, expect, test } from "vitest";
import { useStore } from "../../store";
import { running } from "../../test/backend";
import { renderApp } from "../../test/render";

const nameField = () =>
  screen.getByRole("textbox", { name: "Name shown to devices" });

describe("name shown to devices", () => {
  test("empty means the computer's name, shown as the placeholder", async () => {
    await renderApp(running({ customName: null, hostName: "Studio Mac" }));
    expect(nameField()).toHaveValue("");
    expect(nameField()).toHaveAttribute("placeholder", "Studio Mac");
  });

  test("shows the saved name", async () => {
    await renderApp(running({ customName: "Den" }));
    expect(nameField()).toHaveValue("Den");
  });

  test("Enter saves, and the new name stays up while it saves", async () => {
    let finish!: () => void;
    const { backend, user } = await renderApp(running({ customName: null }), {
      set_name: () => new Promise<void>((resolve) => (finish = resolve)),
    });
    await user.click(nameField());
    await user.keyboard("Den{Enter}");
    expect(nameField()).not.toHaveFocus();
    expect(nameField()).toHaveValue("Den");
    expect(backend.calls).toEqual([
      { command: "set_name", args: { name: "Den" } },
    ]);

    backend.status = running({ customName: "Den" });
    await act(async () => finish());
    expect(nameField()).toHaveValue("Den");
  });

  test("clearing the name saves an empty one, for the computer's name", async () => {
    const { backend, user } = await renderApp(running({ customName: "Den" }));
    await user.clear(nameField());
    await user.keyboard("{Enter}");
    await waitFor(() =>
      expect(backend.calls).toEqual([
        { command: "set_name", args: { name: "" } },
      ]),
    );
  });

  test("Escape puts the saved name back without saving", async () => {
    const { backend, user } = await renderApp(running({ customName: "Den" }));
    await user.click(nameField());
    await user.keyboard("{Control>}a{/Control}Attic{Escape}");
    expect(nameField()).toHaveValue("Den");
    expect(backend.calls).toEqual([]);

    // The cancel was for that edit only: the next one saves.
    await user.click(nameField());
    await user.keyboard("{Control>}a{/Control}Attic{Enter}");
    await waitFor(() =>
      expect(backend.calls).toEqual([
        { command: "set_name", args: { name: "Attic" } },
      ]),
    );
  });

  test("leaving it unchanged saves nothing", async () => {
    const { backend, user } = await renderApp(running({ customName: "Den" }));
    await user.click(nameField());
    await user.tab();
    expect(backend.calls).toEqual([]);
  });

  test("stays usable mid-edit while another command runs", async () => {
    const { user } = await renderApp(running());
    await user.click(nameField());
    act(() => useStore.setState({ pending: 1 }));
    expect(nameField()).toBeEnabled();
    await user.tab();
    expect(nameField()).toBeDisabled();
  });
});

test("the keep-awake switch saves the setting", async () => {
  const { backend, user } = await renderApp(running({ keepAwake: false }));
  await user.click(
    screen.getByRole("switch", { name: "Keep this computer awake" }),
  );
  await waitFor(() =>
    expect(backend.calls).toEqual([
      { command: "set_keep_awake", args: { on: true } },
    ]),
  );
});
