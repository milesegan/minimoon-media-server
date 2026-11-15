import { useEffect } from "react";
import { onPairingRequest } from "../../backend/commands";
import { useStore } from "../../store";
import { PairingCard } from "./PairingCard";

/**
 * Listens for devices asking to pair and shows a card for each. The store takes a card down
 * once its device appears as paired; the card itself handles dismissal and expiry.
 */
export function PairingRequests() {
  const pairings = useStore((state) => state.pairings);
  const addPairing = useStore((state) => state.addPairing);

  useEffect(() => {
    const unlisten = onPairingRequest(addPairing);
    return () => {
      unlisten.then((stop) => stop());
    };
  }, [addPairing]);

  // A live region, so a new card is announced; each card keeps its countdown quiet.
  return (
    <div aria-live="polite">
      {pairings.map((pairing) => (
        <PairingCard key={pairing.request.pairingId} pairing={pairing} />
      ))}
    </div>
  );
}
