"use client";

import { GitBranchIcon, Loader2Icon } from "lucide-react";
import { useActionState } from "react";

import { login } from "@/app/actions";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";

export function LoginForm({ server, expired }: { server: string; expired: boolean }) {
  const [state, action, pending] = useActionState(login, undefined);
  const error = state?.error ?? (expired ? "Your session ended or your access changed. Sign in again." : undefined);
  return (
    <Card className="w-full max-w-sm">
      <CardHeader>
        <div className="mb-2 flex size-9 items-center justify-center rounded-lg bg-primary text-primary-foreground">
          <GitBranchIcon className="size-5" />
        </div>
        <CardTitle className="text-xl">rustgit admin</CardTitle>
        <CardDescription>
          Sign in with a rustgit user that has admin access to every repository.
        </CardDescription>
      </CardHeader>
      <CardContent>
        <form action={action} className="grid gap-4">
          {error && (
            <Alert variant="destructive">
              <AlertDescription>{error}</AlertDescription>
            </Alert>
          )}
          <div className="grid gap-2">
            <Label htmlFor="user">User name</Label>
            <Input id="user" name="user" autoComplete="username" defaultValue={state?.user} required autoFocus />
          </div>
          <div className="grid gap-2">
            <Label htmlFor="password">Password</Label>
            <Input id="password" name="password" type="password" autoComplete="current-password" required />
          </div>
          <Button type="submit" disabled={pending} className="w-full">
            {pending && <Loader2Icon className="animate-spin" />}
            Sign in
          </Button>
          <p className="text-center text-xs text-muted-foreground">
            Server <span className="font-mono">{server}</span>
          </p>
        </form>
      </CardContent>
    </Card>
  );
}
