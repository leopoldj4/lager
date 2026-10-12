import { AlertCircle } from "lucide-react"

import { LiveError } from "@/lib/live"
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"

export function QueryError({ error }: { error: Error }) {
  const code = error instanceof LiveError ? error.code : "error"
  const title =
    code === "not_found"
      ? "Not found"
      : code === "busy" || code === "unavailable"
        ? "Try again shortly"
        : "Something went wrong"
  return (
    <Alert variant={code === "not_found" ? "default" : "destructive"}>
      <AlertCircle />
      <AlertTitle>{title}</AlertTitle>
      <AlertDescription>{error.message}</AlertDescription>
    </Alert>
  )
}
