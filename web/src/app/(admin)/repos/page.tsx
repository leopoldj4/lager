import type { Metadata } from "next";
import { FolderGit2Icon, GlobeIcon, LockIcon } from "lucide-react";

import { CopyButton } from "@/components/copy-button";
import { ApiErrorAlert, PageHeader } from "@/components/page-header";
import { Badge } from "@/components/ui/badge";
import { Card } from "@/components/ui/card";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { ApiError, load, publicUrl, requireSession, type Repo } from "@/lib/rustgit";

export const metadata: Metadata = { title: "Repositories" };

export default async function ReposPage() {
  const session = await requireSession();
  let repos: Repo[] = [];
  let error: unknown;
  try {
    repos = await load<Repo[]>(session, "/repos");
  } catch (e) {
    if (!(e instanceof ApiError)) throw e;
    error = e;
  }
  const base = publicUrl();
  const source = repos[0]?.source;

  return (
    <>
      <PageHeader
        title="Repositories"
        description={
          source === "config"
            ? "Repositories from the configuration file ([[repos]])."
            : "Repositories in the storage catalog. Create and delete them with rustgit repo create and rustgit repo delete."
        }
      >
        {!error && (
          <Badge variant="secondary">
            {repos.length} {repos.length === 1 ? "repository" : "repositories"}
          </Badge>
        )}
      </PageHeader>
      {error ? (
        <ApiErrorAlert error={error} />
      ) : repos.length === 0 ? (
        <Card className="flex flex-col items-center gap-2 py-12 text-center">
          <FolderGit2Icon className="size-8 text-muted-foreground" />
          <p className="font-medium">No repositories yet</p>
          <p className="text-sm text-muted-foreground">
            Create one with <code className="font-mono">rustgit repo create &lt;name&gt;</code>.
          </p>
        </Card>
      ) : (
        <Card className="py-0">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead className="pl-4">Repository</TableHead>
                <TableHead>Default branch</TableHead>
                <TableHead className="text-right">Changes</TableHead>
                <TableHead>Reads</TableHead>
                <TableHead>Customer</TableHead>
                <TableHead className="text-right">Ref rules</TableHead>
                <TableHead className="pr-4">ID</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {repos.map((r) => {
                const clone = `${base}/${r.name}.git`;
                return (
                  <TableRow key={r.id}>
                    <TableCell className="pl-4">
                      <div className="flex items-center gap-1">
                        <span className="font-medium">{r.name}</span>
                        <CopyButton value={`git clone ${clone}`} label="Copy clone command" />
                      </div>
                      <div className="font-mono text-xs text-muted-foreground">{clone}</div>
                    </TableCell>
                    <TableCell>
                      <Badge variant="outline" className="font-mono">
                        {r.default_branch}
                      </Badge>
                    </TableCell>
                    <TableCell className="text-right tabular-nums">
                      {r.generation ?? <span className="text-muted-foreground">missing head</span>}
                    </TableCell>
                    <TableCell>
                      {r.anonymous_read ? (
                        <Badge variant="secondary">
                          <GlobeIcon />
                          Anyone
                        </Badge>
                      ) : (
                        <span className="inline-flex items-center gap-1 text-sm text-muted-foreground">
                          <LockIcon className="size-3.5" />
                          Users with access
                        </span>
                      )}
                    </TableCell>
                    <TableCell>{r.customer ?? <span className="text-muted-foreground">—</span>}</TableCell>
                    <TableCell className="text-right tabular-nums">{r.ref_rules}</TableCell>
                    <TableCell className="pr-4">
                      <div className="flex items-center gap-1">
                        <span className="font-mono text-xs text-muted-foreground">{r.id.slice(0, 8)}</span>
                        <CopyButton value={r.id} label="Copy repository ID" />
                      </div>
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        </Card>
      )}
    </>
  );
}
