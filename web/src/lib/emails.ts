async function request(path: string, init?: RequestInit) {
  const response = await fetch(`/-/api/emails${path}`, {
    ...init,
    headers: { "content-type": "application/json", ...init?.headers },
  })
  if (!response.ok) {
    const error = await response.json().catch(() => ({}))
    throw new Error(
      error.message ?? `Email request failed (${response.status})`
    )
  }
  if (response.status === 204) return
  return response.json()
}

export function listEmails(): Promise<{ emails: string[] }> {
  return request("")
}

export function addEmail(email: string): Promise<{ email: string }> {
  return request("", { method: "POST", body: JSON.stringify({ email }) })
}

export function removeEmail(email: string) {
  return request(`/${encodeURIComponent(email)}`, { method: "DELETE" })
}
