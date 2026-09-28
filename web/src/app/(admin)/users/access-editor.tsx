"use client";

import { PlusIcon, XIcon } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import type { Access } from "@/lib/rustgit";

export const LEVELS: { value: Access; label: string; hint: string }[] = [
  { value: "none", label: "None", hint: "No access, even with a wider grant" },
  { value: "read", label: "Read", hint: "Clone and fetch" },
  { value: "write", label: "Write", hint: "Push fast-forward changes" },
  { value: "force", label: "Force", hint: "Also force pushes and tag moves" },
  { value: "admin", label: "Admin", hint: "Also maintenance and management" },
];

export type Grant = { repo: string; level: Access };

export const ALL = "*";

export function repoLabel(repo: string) {
  return repo === ALL ? "All repositories" : repo;
}

export function toGrants(access: Record<string, Access>): Grant[] {
  return Object.entries(access)
    .map(([repo, level]) => ({ repo, level }))
    .sort((a, b) => (a.repo === ALL ? -1 : b.repo === ALL ? 1 : a.repo.localeCompare(b.repo)));
}

export function toAccess(grants: Grant[]): Record<string, Access> {
  return Object.fromEntries(grants.filter((g) => g.repo).map((g) => [g.repo, g.level]));
}

export function AccessBadges({ access }: { access: Record<string, Access> }) {
  const grants = toGrants(access);
  if (grants.length === 0) return <span className="text-sm text-muted-foreground">No access</span>;
  return (
    <div className="flex flex-wrap gap-1">
      {grants.map((g) => (
        <Badge
          key={g.repo}
          variant={g.level === "admin" ? "default" : g.level === "none" ? "outline" : "secondary"}
          className="font-normal"
        >
          <span className={g.repo === ALL ? "" : "font-mono"}>{repoLabel(g.repo)}</span>
          <span className="opacity-60">·</span>
          {g.level}
        </Badge>
      ))}
    </div>
  );
}

/** Access levels per repository; a repository grant overrides "All repositories". */
export function AccessEditor({
  grants,
  onChange,
  repos,
}: {
  grants: Grant[];
  onChange: (grants: Grant[]) => void;
  repos: string[];
}) {
  const used = new Set(grants.map((g) => g.repo));
  const choices = [ALL, ...repos];
  const next = choices.find((r) => !used.has(r));
  const set = (i: number, g: Partial<Grant>) => onChange(grants.map((x, j) => (i === j ? { ...x, ...g } : x)));

  return (
    <div className="grid gap-2">
      {grants.length === 0 && (
        <p className="rounded-md border border-dashed p-3 text-center text-sm text-muted-foreground">
          No access yet. The user can sign in but sees no repository.
        </p>
      )}
      {grants.map((g, i) => (
        <div key={i} className="flex items-center gap-2">
          <Select value={g.repo} onValueChange={(repo) => set(i, { repo })}>
            <SelectTrigger className="min-w-0 flex-1" aria-label="Repository">
              <SelectValue placeholder="Repository" />
            </SelectTrigger>
            <SelectContent>
              {choices
                .filter((r) => r === g.repo || !used.has(r))
                .map((r) => (
                  <SelectItem key={r} value={r}>
                    <span className={r === ALL ? "" : "font-mono"}>{repoLabel(r)}</span>
                  </SelectItem>
                ))}
              {g.repo && !choices.includes(g.repo) && (
                <SelectItem value={g.repo}>
                  <span className="font-mono">{g.repo}</span>
                  <span className="text-muted-foreground">(not found)</span>
                </SelectItem>
              )}
            </SelectContent>
          </Select>
          <Select value={g.level} onValueChange={(level) => set(i, { level: level as Access })}>
            <SelectTrigger className="w-28" aria-label="Access level">
              {/* Only the name: the items also carry a description. */}
              <SelectValue>{LEVELS.find((l) => l.value === g.level)?.label}</SelectValue>
            </SelectTrigger>
            <SelectContent>
              {LEVELS.map((l) => (
                <SelectItem key={l.value} value={l.value}>
                  <div className="grid">
                    <span>{l.label}</span>
                    <span className="text-xs text-muted-foreground">{l.hint}</span>
                  </div>
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            aria-label={`Remove ${repoLabel(g.repo)}`}
            onClick={() => onChange(grants.filter((_, j) => j !== i))}
          >
            <XIcon />
          </Button>
        </div>
      ))}
      <Button
        type="button"
        variant="outline"
        size="sm"
        className="justify-self-start"
        disabled={!next}
        onClick={() => next && onChange([...grants, { repo: next, level: "read" }])}
      >
        <PlusIcon />
        Add access
      </Button>
    </div>
  );
}
