import * as commands from "../../backend/commands";
import type { ServerView } from "../../backend/types";
import { selectReady, useStore } from "../../store";
import { Section } from "../common/Section";
import { SwitchRow } from "../common/SwitchRow";
import { ServerNameField } from "./ServerNameField";

/** Settings for this computer: the name devices see, and whether it stays awake. */
export function ServerSection({ server }: { server: ServerView }) {
  const ready = useStore(selectReady);
  const run = useStore((state) => state.run);

  return (
    <Section title="This server">
      <ServerNameField server={server} />
      <SwitchRow
        title="Keep this computer awake"
        hint="Even when nothing is playing."
        checked={server.keepAwake}
        disabled={!ready}
        onChange={(on) => run(commands.setKeepAwake, on)}
      />
    </Section>
  );
}
