import {
  act,
  fireEvent,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { describe, expect, test, vi } from "vitest";
import { useStore } from "../../store";
import { folder, running } from "../../test/backend";
import { renderApp } from "../../test/render";

// The native menu can't open in jsdom: keep its items, so tests can pick one.
interface MenuItem {
  text?: string;
  action?: () => void;
}
const menu = vi.hoisted(() => ({
  items: null as MenuItem[] | null,
  popup: async () => {},
}));
vi.mock("@tauri-apps/api/menu", () => ({
  Menu: {
    new: async ({ items }: { items: MenuItem[] }) => {
      menu.items = items;
      return { popup: menu.popup };
    },
  },
}));

const music = folder({ id: "music", name: "Music" });

async function openMenu() {
  menu.items = null;
  fireEvent.click(screen.getByRole("button", { name: "Folder actions" }));
  await waitFor(() => expect(menu.items).not.toBeNull());
  return (text: RegExp) =>
    menu.items!.find((item) => item.text && text.test(item.text))!.action!();
}

describe("renaming", () => {
  test("a double-click edits the name in place, and Enter saves it trimmed", async () => {
    const { backend, user } = await renderApp(running({ folders: [music] }));
    await user.dblClick(screen.getByText("Music"));
    const field = screen.getByRole("textbox", { name: "Folder name" });
    expect(field).toHaveFocus();
    expect(field).toHaveValue("Music");
    // The old name is selected, so typing replaces it.
    expect([field as HTMLInputElement].map((f) => f.selectionEnd)).toEqual([5]);

    await user.keyboard("  Records  {Enter}");
    await waitFor(() =>
      expect(backend.calls).toEqual([
        { command: "rename_folder", args: { id: "music", name: "Records" } },
      ]),
    );
  });

  test("the new name shows while it saves, then the server's", async () => {
    let finish!: () => void;
    const { backend, user } = await renderApp(running({ folders: [music] }), {
      rename_folder: () => new Promise<void>((resolve) => (finish = resolve)),
    });
    await user.dblClick(screen.getByText("Music"));
    await user.keyboard("{Control>}a{/Control}Records{Enter}");
    expect(screen.getByText("Records")).toBeInTheDocument();
    expect(screen.queryByText("Music")).toBeNull();

    backend.status = running({ folders: [{ ...music, name: "Records!" }] });
    finish();
    expect(await screen.findByText("Records!")).toBeInTheDocument();
  });

  test("Escape cancels, and the blur that follows doesn't save", async () => {
    const { backend, user } = await renderApp(running({ folders: [music] }));
    await user.dblClick(screen.getByText("Music"));
    const field = screen.getByRole("textbox", { name: "Folder name" });
    await user.keyboard("{Control>}a{/Control}Records");
    // WebKit blurs the field as it's removed: both events land before React re-renders.
    act(() => {
      field.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
      );
      field.dispatchEvent(new FocusEvent("focusout", { bubbles: true }));
    });
    expect(screen.getByText("Music")).toBeInTheDocument();
    expect(backend.calls).toEqual([]);
  });

  test("an empty or unchanged name isn't saved", async () => {
    const { backend, user } = await renderApp(running({ folders: [music] }));
    await user.dblClick(screen.getByText("Music"));
    await user.keyboard("{Enter}");
    await user.dblClick(screen.getByText("Music"));
    await user.keyboard("{Control>}a{/Control}{Backspace}   {Enter}");
    expect(screen.getByText("Music")).toBeInTheDocument();
    expect(backend.calls).toEqual([]);
  });

  test("leaving the field saves", async () => {
    const { backend, user } = await renderApp(running({ folders: [music] }));
    await user.dblClick(screen.getByText("Music"));
    await user.keyboard("{Control>}a{/Control}Records");
    await user.click(document.body);
    await waitFor(() =>
      expect(backend.calls).toEqual([
        { command: "rename_folder", args: { id: "music", name: "Records" } },
      ]),
    );
  });

  test("doesn't start while a command is in flight", async () => {
    const { user } = await renderApp(running({ folders: [music] }));
    useStore.setState({ pending: 1 });
    await user.dblClick(screen.getByText("Music"));
    expect(screen.queryByRole("textbox", { name: "Folder name" })).toBeNull();
  });
});

describe("actions menu", () => {
  test("its items rescan, open, stop sharing and rename this folder", async () => {
    const { backend } = await renderApp(running({ folders: [music] }));
    let choose = await openMenu();
    choose(/^Rescan$/);
    choose(/^Open/);
    choose(/^Stop Sharing/);
    await waitFor(() =>
      expect(backend.calls).toEqual([
        { command: "rescan", args: { id: "music" } },
        { command: "open_folder", args: { id: "music" } },
        { command: "remove_folder", args: { id: "music" } },
      ]),
    );

    choose = await openMenu();
    act(() => choose(/^Rename$/));
    expect(screen.getByRole("textbox", { name: "Folder name" })).toHaveFocus();
  });

  test("a right-click on the row opens it, but not on the path", async () => {
    await renderApp(running({ folders: [music] }));
    menu.items = null;
    fireEvent.contextMenu(screen.getByTitle("/Users/me/Music"));
    await act(async () => {});
    expect(menu.items).toBeNull();

    fireEvent.contextMenu(screen.getByText("Music"));
    await waitFor(() => expect(menu.items).not.toBeNull());
  });
});

describe("status and extras", () => {
  test("activity adds a progress bar, which counts when it can", async () => {
    await renderApp(
      running({
        folders: [
          folder({
            activity: "Reading new files",
            progress: [25, 100],
            files: 0,
          }),
        ],
      }),
    );
    const bar = screen.getByRole("progressbar", { name: "Reading new files" });
    expect(bar).toHaveAttribute("aria-valuenow", "25");
    expect(bar).toHaveAttribute("aria-valuemax", "100");
    expect(screen.getByText("25 of 100")).toBeInTheDocument();
  });

  test("an offline folder shows a pill and its reason, with no progress", async () => {
    await renderApp(
      running({
        folders: [
          folder({
            shared: false,
            note: "Offline: the disk isn't connected",
            activity: "Scanning",
          }),
        ],
      }),
    );
    expect(screen.getByText("Offline")).toBeInTheDocument();
    expect(screen.getByText("the disk isn't connected")).toBeInTheDocument();
    expect(screen.queryByRole("progressbar")).toBeNull();
  });

  test("scan errors are listed behind a disclosure", async () => {
    const { user } = await renderApp(
      running({
        folders: [
          folder({ scanErrors: ["a.flac: bad header", "b.mkv: truncated"] }),
        ],
      }),
    );
    const summary = screen.getByText(
      "2 items couldn't be read and aren't shared",
    );
    const details = summary.closest("details")!;
    expect(details).not.toHaveAttribute("open");
    await user.click(summary);
    expect(details).toHaveAttribute("open");
    expect(within(details).getByText("b.mkv: truncated")).toBeInTheDocument();
  });

  test("with no folders, the empty state adds the first", async () => {
    const { backend, user } = await renderApp(running({ folders: [] }));
    expect(screen.getByText("No shared folders yet")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Add Folder…" }));
    await waitFor(() =>
      expect(backend.calls).toEqual([{ command: "add_folder", args: {} }]),
    );
  });
});
