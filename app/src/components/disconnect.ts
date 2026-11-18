import { api, errMsg } from "../api";
import type { Toast } from "../store";
import type { Status } from "../types";

/**
 * Signs out. The app goes back to the sign-in screen either way, but a token that could not be removed
 * from the OS keychain (locked, or out of reach) is still a working credential on the user's server, so
 * the backend's warning about it is shown as a toast, which stays until it is dismissed.
 */
export async function disconnect(
  push: (tone: Toast["tone"], text: string, detail?: string[]) => void,
  onDisconnected: (s: Status) => void,
): Promise<void> {
  try {
    const s = await api.logout();
    onDisconnected(s);
    if (s.warnings.length) push("error", "Disconnected, but the sign-in token is still stored", s.warnings);
  } catch (e) {
    push("error", "Couldn't disconnect", [errMsg(e)]);
  }
}
