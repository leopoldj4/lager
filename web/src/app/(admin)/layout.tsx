import { GitBranchIcon } from "lucide-react";

import { AdminNav, ThemeToggle, UserMenu } from "@/app/(admin)/nav";
import { publicUrl, requireSession } from "@/lib/rustgit";

export default async function AdminLayout({ children }: LayoutProps<"/">) {
  const session = await requireSession();
  return (
    <div className="flex min-h-full flex-1 flex-col">
      <header className="sticky top-0 z-10 border-b bg-background/90 backdrop-blur">
        <div className="mx-auto flex h-14 max-w-6xl items-center gap-4 px-4">
          <div className="flex items-center gap-2 font-semibold">
            <div className="flex size-7 items-center justify-center rounded-md bg-primary text-primary-foreground">
              <GitBranchIcon className="size-4" />
            </div>
            <span className="hidden sm:inline">rustgit</span>
          </div>
          <AdminNav />
          <div className="ml-auto flex items-center gap-1">
            <span className="mr-2 hidden font-mono text-xs text-muted-foreground md:inline">{publicUrl()}</span>
            <ThemeToggle />
            <UserMenu user={session.user} />
          </div>
        </div>
      </header>
      <main className="mx-auto w-full max-w-6xl flex-1 px-4 py-8">{children}</main>
    </div>
  );
}
