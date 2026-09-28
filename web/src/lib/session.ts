import "server-only";

import { createCipheriv, createDecipheriv, createHash, randomBytes } from "node:crypto";
import { cookies } from "next/headers";

// The rustgit admin API takes HTTP Basic credentials on every request, so the
// session holds them, encrypted and authenticated with AES-256-GCM. The
// cookie is httpOnly: browser code never sees it.

const COOKIE = "rustgit_session";
const MAX_AGE_SECS = 8 * 60 * 60;

export type Session = { user: string; password: string };

type Payload = Session & { exp: number };

let generatedSecret: string | undefined;

function key(): Buffer {
  let secret = process.env.SESSION_SECRET;
  if (!secret) {
    if (process.env.NODE_ENV === "production") {
      throw new Error("SESSION_SECRET is not set");
    }
    // Development only: sessions end when the server restarts.
    generatedSecret ??= randomBytes(32).toString("hex");
    secret = generatedSecret;
  }
  return createHash("sha256").update(secret).digest();
}

function seal(payload: Payload): string {
  const iv = randomBytes(12);
  const cipher = createCipheriv("aes-256-gcm", key(), iv);
  const data = Buffer.concat([cipher.update(JSON.stringify(payload), "utf8"), cipher.final()]);
  return Buffer.concat([iv, cipher.getAuthTag(), data]).toString("base64url");
}

function unseal(value: string): Payload | undefined {
  try {
    const raw = Buffer.from(value, "base64url");
    const decipher = createDecipheriv("aes-256-gcm", key(), raw.subarray(0, 12));
    decipher.setAuthTag(raw.subarray(12, 28));
    const text = Buffer.concat([decipher.update(raw.subarray(28)), decipher.final()]).toString("utf8");
    const payload = JSON.parse(text) as Payload;
    return payload.exp > Date.now() ? payload : undefined;
  } catch {
    return undefined;
  }
}

export async function getSession(): Promise<Session | undefined> {
  const value = (await cookies()).get(COOKIE)?.value;
  const payload = value ? unseal(value) : undefined;
  return payload && { user: payload.user, password: payload.password };
}

export async function createSession(session: Session) {
  const exp = Date.now() + MAX_AGE_SECS * 1000;
  (await cookies()).set(COOKIE, seal({ ...session, exp }), {
    httpOnly: true,
    secure: process.env.NODE_ENV === "production",
    sameSite: "lax",
    path: "/",
    maxAge: MAX_AGE_SECS,
  });
}

export async function deleteSession() {
  (await cookies()).delete(COOKIE);
}

export const SESSION_COOKIE = COOKIE;
