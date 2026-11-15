import * as commands from "../../backend/commands";
import type { ServerView } from "../../backend/types";
import { selectReady, useStore } from "../../store";
import { Section } from "../common/Section";
import { SwitchRow } from "../common/SwitchRow";
import { DeviceRow } from "./DeviceRow";

/**
 * Who can connect: the pairing-code switch, then the paired devices. With pairing on and none
 * paired yet, a hint says where codes will appear.
 */
export function DevicesSection({ server }: { server: ServerView }) {
  const ready = useStore(selectReady);
  const run = useStore((state) => state.run);
  const { devices } = server;

  return (
    <Section title="Devices">
      <SwitchRow
        title="Require a pairing code"
        hint="New devices enter a code shown in this window."
        checked={server.pairing}
        disabled={!ready}
        onChange={(on) => run(commands.setPairing, on)}
      />
      {devices.length > 0 && (
        <ul className="list" aria-label="Paired devices">
          {devices.map((device) => (
            <DeviceRow key={device.clientId} device={device} />
          ))}
        </ul>
      )}
      {server.pairing && devices.length === 0 && (
        <p className="empty">
          No devices paired yet. When a device asks to connect, its code appears
          at the top of this window.
        </p>
      )}
    </Section>
  );
}
