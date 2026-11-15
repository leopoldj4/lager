import { useQuery } from "@tanstack/react-query"
import { live } from "./live"

export function useProfile(user: string) {
  return useQuery({
    queryKey: ["profile", user],
    queryFn: () => live.request({ op: "profile", user }),
  })
}
