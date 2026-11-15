import { createFileRoute, Link, useNavigate } from "@tanstack/react-router"
import { useQuery } from "@tanstack/react-query"
import { useState } from "react"

import {
  githubSignInHref,
  githubSignUpLogin,
  signUpWithGithub,
} from "@/lib/session"
import { Alert, AlertDescription } from "@/components/ui/alert"
import { Button } from "@/components/ui/button"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { Crumbs, PageHeader } from "@/components/frame"
import { GithubMark } from "@/components/github-mark"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Skeleton } from "@/components/ui/skeleton"

/** Picks the user name of an account that GitHub signs in (decision 31). */
export const Route = createFileRoute("/-/sign-up_/github")({
  component: GithubSignUp,
})

function GithubSignUp() {
  const navigate = useNavigate()
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const login = useQuery({
    queryKey: ["github-sign-up"],
    queryFn: githubSignUpLogin,
    retry: false,
    staleTime: Infinity,
  })

  async function submit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault()
    const user = String(new FormData(e.currentTarget).get("user"))
    setBusy(true)
    setError(null)
    try {
      await signUpWithGithub(user)
      navigate({ to: "/" })
    } catch (err) {
      setError(err instanceof Error ? err.message : "sign-up failed")
    } finally {
      setBusy(false)
    }
  }

  return (
    <>
      <PageHeader>
        <Crumbs items={[{ label: "Sign up" }]} />
      </PageHeader>
      <div className="flex justify-center px-4 py-16">
        <Card className="w-full max-w-sm">
          <CardHeader>
            <CardTitle>Create an account</CardTitle>
            {login.data && (
              <CardDescription className="flex items-center gap-1.5">
                <GithubMark className="size-4" />@{login.data}
              </CardDescription>
            )}
          </CardHeader>
          <CardContent>
            {login.isPending ? (
              <Skeleton className="h-24" />
            ) : !login.data ? (
              <div className="grid gap-4">
                <p className="text-sm text-muted-foreground">
                  The GitHub sign-in expired.
                </p>
                <Button variant="outline" asChild>
                  <a href={githubSignInHref()}>
                    <GithubMark />
                    Sign in with GitHub
                  </a>
                </Button>
              </div>
            ) : (
              <form onSubmit={submit} className="grid gap-4">
                {error && (
                  <Alert variant="destructive">
                    <AlertDescription>{error}</AlertDescription>
                  </Alert>
                )}
                <div className="grid gap-2">
                  <Label htmlFor="user">User name</Label>
                  <Input
                    id="user"
                    name="user"
                    autoComplete="username"
                    defaultValue={login.data}
                    required
                    maxLength={64}
                    pattern="[A-Za-z0-9_][A-Za-z0-9._\-]*"
                    title="Letters, digits, '.', '_', or '-', not starting with '.' or '-'"
                    disabled={busy}
                  />
                </div>
                <Button type="submit" disabled={busy}>
                  {busy ? "Creating…" : "Create account"}
                </Button>
              </form>
            )}
            <p className="mt-4 text-center text-sm text-muted-foreground">
              Already have an account?{" "}
              <Link to="/-/sign-in" className="underline">
                Sign in
              </Link>
            </p>
          </CardContent>
        </Card>
      </div>
    </>
  )
}
