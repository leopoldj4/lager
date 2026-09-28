"use server";

import { revalidatePath } from "next/cache";
import { redirect } from "next/navigation";

import { ACCESS_LEVELS, ApiError, call, type Access, type Me } from "@/lib/rustgit";
import { createSession, deleteSession, getSession } from "@/lib/session";

export type ActionResult = { ok: true } | { ok: false; error: string };

export type LoginState = { error?: string; user?: string } | undefined;

export async function login(_: LoginState, form: FormData): Promise<LoginState> {
  const user = String(form.get("user") ?? "").trim();
  const password = String(form.get("password") ?? "");
  if (!user || !password) return { error: "Enter your user name and password.", user };
  let me: Me;
  try {
    me = await call<Me>({ user, password }, "/me");
  } catch (e) {
    if (e instanceof ApiError && e.status === 401) return { error: "Wrong user name or password.", user };
    if (e instanceof ApiError && e.status === 404)
      return { error: "The admin API is turned off on this server (server.admin_api).", user };
    return { error: e instanceof Error ? e.message : "Sign-in failed.", user };
  }
  if (!me.admin) {
    return { error: `${me.name} has no admin access to every repository ("*" = "admin").`, user };
  }
  await createSession({ user, password });
  redirect("/repos");
}

export async function logout() {
  await deleteSession();
  redirect("/login");
}

async function run(path: string, init: { method: string; body?: unknown }): Promise<ActionResult> {
  const session = await getSession();
  if (!session) return { ok: false, error: "Your session has ended. Sign in again." };
  try {
    await call(session, path, init);
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : "The change failed." };
  }
  revalidatePath("/users");
  return { ok: true };
}

function cleanAccess(access: Record<string, string>): Record<string, Access> {
  const out: Record<string, Access> = {};
  for (const [repo, level] of Object.entries(access)) {
    if (repo && ACCESS_LEVELS.includes(level as Access)) out[repo] = level as Access;
  }
  return out;
}

export async function createUser(input: {
  name: string;
  password: string;
  access: Record<string, string>;
}): Promise<ActionResult> {
  return run("/users", {
    method: "POST",
    body: { name: input.name.trim(), password: input.password, access: cleanAccess(input.access) },
  });
}

export async function setUserAccess(name: string, access: Record<string, string>): Promise<ActionResult> {
  return run(`/users/${encodeURIComponent(name)}`, {
    method: "PATCH",
    body: { access: cleanAccess(access) },
  });
}

export async function setUserPassword(name: string, password: string): Promise<ActionResult> {
  return run(`/users/${encodeURIComponent(name)}`, { method: "PATCH", body: { password } });
}

export async function deleteUser(name: string): Promise<ActionResult> {
  return run(`/users/${encodeURIComponent(name)}`, { method: "DELETE" });
}
