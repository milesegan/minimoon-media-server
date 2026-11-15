// What a shared folder's row says on its right: two short lines, or an Offline pill and why.
// Healthy folders show only their size; anything unusual replaces or follows it.

import type { FolderView } from "../backend/types";
import { formatNumber, plural } from "./format";

/** How a line reads: plain, indigo for work in progress, amber for a problem, or muted. */
export type Tone = "plain" | "busy" | "warn" | "quiet";

export interface StatusLine {
  text: string;
  tone: Tone;
}

export type FolderStatus =
  | { kind: "offline"; reason: string }
  | { kind: "lines"; main: StatusLine; sub: StatusLine | null };

/** Whether the folder's disk or share can't be reached. */
export function isOffline(folder: FolderView): boolean {
  return !folder.shared && !!folder.note?.startsWith("Offline");
}

export function describeFolder(folder: FolderView): FolderStatus {
  const files = plural(folder.files, "file");

  if (isOffline(folder)) {
    return {
      kind: "offline",
      reason: folder.note?.replace(/^Offline:?\s*/, "") ?? "",
    };
  }

  if (folder.activity) {
    const main: StatusLine = { text: folder.activity, tone: "busy" };
    if (folder.progress) {
      const [done, total] = folder.progress;
      const text = `${formatNumber(done)} of ${formatNumber(total)}`;
      return { kind: "lines", main, sub: { text, tone: "plain" } };
    }
    return {
      kind: "lines",
      main,
      sub: folder.files > 0 ? { text: files, tone: "plain" } : null,
    };
  }

  if (!folder.shared) {
    // "Not shared: why" is a problem, split over the two lines; waiting to be indexed isn't.
    const why = folder.note?.match(/^Not shared: (.*)$/s)?.[1];
    if (why) {
      return {
        kind: "lines",
        main: { text: "Not shared", tone: "warn" },
        sub: { text: why, tone: "plain" },
      };
    }
    return {
      kind: "lines",
      main: { text: folder.note ?? "Not shared", tone: "quiet" },
      sub: null,
    };
  }

  const unreadable =
    folder.unreadable > 0
      ? {
          text: `${formatNumber(folder.unreadable)} with unreadable metadata`,
          tone: "warn" as const,
        }
      : null;
  return {
    kind: "lines",
    main: { text: files, tone: "plain" },
    sub: unreadable,
  };
}
