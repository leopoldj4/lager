import { rest } from "./rest"

function request(path: string, method = "GET", body?: unknown) {
  return rest(`/emails${path}`, "Email request", method, body)
}

export interface EmailInfo {
  email: string
  /**
   * How the address was verified: by the connected GitHub account, which
   * keeps it in step, or by a link in a mail. Unverified ones count nowhere.
   */
  verified: "github" | "mail" | null
}

export interface EmailList {
  /** The server sends mail, so addresses added here can be verified. */
  mail: boolean
  emails: EmailInfo[]
}

export function listEmails(): Promise<EmailList> {
  return request("")
}

/** Adds an address and mails it a link that verifies it. */
export function addEmail(
  email: string
): Promise<{ email: string; sent: boolean }> {
  return request("", "POST", { email })
}

export function sendVerification(email: string) {
  return request(`/${encodeURIComponent(email)}/send`, "POST")
}

export function verifyEmail(token: string): Promise<{ email: string }> {
  return request("/verify", "POST", { token })
}

export function removeEmail(email: string) {
  return request(`/${encodeURIComponent(email)}`, "DELETE")
}
