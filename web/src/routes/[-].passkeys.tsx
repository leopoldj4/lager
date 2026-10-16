import { createFileRoute, redirect } from "@tanstack/react-router"

// The page moved into the settings.
export const Route = createFileRoute("/-/passkeys")({
  beforeLoad: () => {
    throw redirect({ to: "/-/settings/passkeys" })
  },
})
