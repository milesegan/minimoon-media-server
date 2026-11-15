import { useStore } from "../store";

/** The brief message at the bottom of the window; the store decides what and for how long. */
export function Toast() {
  const toast = useStore((state) => state.toast);
  return (
    <div
      className={toast?.error ? "toast error" : "toast"}
      role="status"
      hidden={!toast}
    >
      {toast?.message}
    </div>
  );
}
