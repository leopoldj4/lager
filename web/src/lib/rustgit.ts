import "server-only";

import { redirect } from "next/navigation";

import { getSession, type Session } from "@/lib/session";

// Client for the rustgit admin API (`/_rustgit/api/`, see
// docs/decisions/0004-users-in-storage.md). Only server code calls it.

export type Access = "none" | "read" | "write" | "force" | "admin";

export const ACCESS_LEVELS: Access[] = ["none", "read", "write", "force", "admin"];

export type Repo = {
  name: string;
  id: string;
  default_branch: string;
  anonymous_read: boolean;
  customer: string | null;
  ref_rules: number;
  generation: number | null;
  source: "catalog" | "config";
};

export type User = {
  name: string;
  access: Record<string, Access>;
  source: "config" | "storage";
  created_ms: number | null;
  updated_ms: number | null;
  updated_by: string | null;
};

export type Users = { managed: boolean; users: User[] };

export type Me = { name: string; admin: boolean };

export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
  }
}

export function serverUrl(): string {
  return (process.env.RUSTGIT_URL ?? "http://127.0.0.1:8080").replace(/\/+$/, "");
}

/** The URL clients clone from; the API may be reached on another address. */
export function publicUrl(): string {
  return (process.env.RUSTGIT_PUBLIC_URL ?? serverUrl()).replace(/\/+$/, "");
}

export async function call<T>(
  session: Session,
  path: string,
  init: { method?: string; body?: unknown } = {},
): Promise<T> {
  let resp: Response;
  try {
    resp = await fetch(`${serverUrl()}/_rustgit/api${path}`, {
      method: init.method ?? "GET",
      headers: {
        authorization: `Basic ${Buffer.from(`${session.user}:${session.password}`).toString("base64")}`,
        ...(init.body === undefined ? {} : { "content-type": "application/json" }),
      },
      body: init.body === undefined ? undefined : JSON.stringify(init.body),
      cache: "no-store",
    });
  } catch {
    throw new ApiError(503, `rustgit at ${serverUrl()} does not answer`);
  }
  if (resp.status === 204) return undefined as T;
  const text = await resp.text();
  let body: unknown;
  try {
    body = text ? JSON.parse(text) : undefined;
  } catch {
    body = undefined;
  }
  if (!resp.ok) {
    const message =
      (body as { error?: string } | undefined)?.error ?? (text.trim() || `rustgit answered ${resp.status}`);
    throw new ApiError(resp.status, message);
  }
  return body as T;
}

/** For pages: the signed-in admin's session, or a redirect to the login. */
export async function requireSession(): Promise<Session> {
  const session = await getSession();
  if (!session) redirect("/login");
  return session;
}

/** For pages: loads data, sending signed-out users to the login. */
export async function load<T>(session: Session, path: string): Promise<T> {
  try {
    return await call<T>(session, path);
  } catch (e) {
    if (e instanceof ApiError && (e.status === 401 || e.status === 403)) redirect("/login?expired=1");
    throw e;
  }
}
