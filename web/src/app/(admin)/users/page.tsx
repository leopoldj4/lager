import type { Metadata } from "next";

import { UsersView } from "@/app/(admin)/users/users-view";
import { ApiErrorAlert, PageHeader } from "@/components/page-header";
import { ApiError, load, requireSession, type Repo, type Users } from "@/lib/rustgit";

export const metadata: Metadata = { title: "Users" };

export default async function UsersPage() {
  const session = await requireSession();
  let data: { users: Users; repos: Repo[] } | undefined;
  let error: unknown;
  try {
    const [users, repos] = await Promise.all([load<Users>(session, "/users"), load<Repo[]>(session, "/repos")]);
    data = { users, repos };
  } catch (e) {
    if (!(e instanceof ApiError)) throw e;
    error = e;
  }
  if (!data) {
    return (
      <>
        <PageHeader title="Users" />
        <ApiErrorAlert error={error} />
      </>
    );
  }
  return (
    <UsersView
      users={data.users.users}
      managed={data.users.managed}
      repos={data.repos.map((r) => r.name)}
      me={session.user}
    />
  );
}
