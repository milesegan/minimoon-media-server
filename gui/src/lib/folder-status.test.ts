import { describe, expect, test } from "vitest";
import { folder } from "../test/backend";
import { describeFolder, isOffline } from "./folder-status";

describe("isOffline", () => {
  test("is true for an unshared folder whose note says Offline", () => {
    expect(
      isOffline(folder({ shared: false, note: "Offline: disk not mounted" })),
    ).toBe(true);
  });

  test("ignores the note while the folder is shared", () => {
    expect(isOffline(folder({ shared: true, note: "Offline" }))).toBe(false);
  });

  test("is false for other reasons it isn't shared", () => {
    expect(
      isOffline(folder({ shared: false, note: "Not shared: no access" })),
    ).toBe(false);
    expect(isOffline(folder({ shared: false, note: null }))).toBe(false);
  });
});

describe("describeFolder", () => {
  test("a healthy folder shows only its file count", () => {
    expect(describeFolder(folder({ files: 1200 }))).toEqual({
      kind: "lines",
      main: { text: "1,200 files", tone: "plain" },
      sub: null,
    });
  });

  test("unreadable files follow the count as a warning", () => {
    expect(describeFolder(folder({ files: 1, unreadable: 3 }))).toEqual({
      kind: "lines",
      main: { text: "1 file", tone: "plain" },
      sub: { text: "3 with unreadable metadata", tone: "warn" },
    });
  });

  test("offline shows the reason without its prefix", () => {
    expect(
      describeFolder(
        folder({ shared: false, note: "Offline: the disk isn't connected" }),
      ),
    ).toEqual({ kind: "offline", reason: "the disk isn't connected" });
    expect(describeFolder(folder({ shared: false, note: "Offline" }))).toEqual({
      kind: "offline",
      reason: "",
    });
  });

  test("offline wins over activity", () => {
    const status = describeFolder(
      folder({ shared: false, note: "Offline: gone", activity: "Scanning" }),
    );
    expect(status.kind).toBe("offline");
  });

  test("countable activity shows done of total", () => {
    expect(
      describeFolder(
        folder({ activity: "Reading new files", progress: [1500, 48213] }),
      ),
    ).toEqual({
      kind: "lines",
      main: { text: "Reading new files", tone: "busy" },
      sub: { text: "1,500 of 48,213", tone: "plain" },
    });
  });

  test("uncountable activity shows the files so far, if any", () => {
    expect(describeFolder(folder({ activity: "Scanning", files: 12 }))).toEqual(
      {
        kind: "lines",
        main: { text: "Scanning", tone: "busy" },
        sub: { text: "12 files", tone: "plain" },
      },
    );
    expect(describeFolder(folder({ activity: "Scanning", files: 0 }))).toEqual({
      kind: "lines",
      main: { text: "Scanning", tone: "busy" },
      sub: null,
    });
  });

  test('"Not shared: why" is a warning split over the two lines', () => {
    expect(
      describeFolder(
        folder({ shared: false, note: "Not shared: permission denied" }),
      ),
    ).toEqual({
      kind: "lines",
      main: { text: "Not shared", tone: "warn" },
      sub: { text: "permission denied", tone: "plain" },
    });
  });

  test("waiting to be indexed is quiet, not a warning", () => {
    expect(
      describeFolder(
        folder({ shared: false, note: "Not shared until it's indexed" }),
      ),
    ).toEqual({
      kind: "lines",
      main: { text: "Not shared until it's indexed", tone: "quiet" },
      sub: null,
    });
    expect(describeFolder(folder({ shared: false, note: null }))).toEqual({
      kind: "lines",
      main: { text: "Not shared", tone: "quiet" },
      sub: null,
    });
  });
});
