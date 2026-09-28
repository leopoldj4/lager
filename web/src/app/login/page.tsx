import type { Metadata } from "next";
import { redirect } from "next/navigation";

import { LoginForm } from "@/app/login/login-form";
import { serverUrl } from "@/lib/rustgit";
import { getSession } from "@/lib/session";

export const metadata: Metadata = { title: "Sign in" };

export default async function LoginPage({ searchParams }: PageProps<"/login">) {
  const { expired } = await searchParams;
  if (!expired && (await getSession())) redirect("/repos");
  return (
    <main className="flex flex-1 items-center justify-center p-6">
      <LoginForm server={serverUrl()} expired={Boolean(expired)} />
    </main>
  );
}
