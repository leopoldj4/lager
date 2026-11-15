// The cache of the answers, and the live connection's state for components.

import { useSyncExternalStore } from "react"
import { QueryClient } from "@tanstack/react-query"
import { live, LiveError } from "./live"

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 60_000,
      retry: (n, e) =>
        !(e instanceof LiveError && e.code !== "unavailable") && n < 3,
    },
  },
})

export function useLive() {
  return useSyncExternalStore(
    live?.subscribe ?? (() => () => {}),
    () => live?.getState(),
    () => null
  )
}
