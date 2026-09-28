import { NextResponse, type NextRequest } from "next/server";

// An optimistic check only: pages verify the session with rustgit itself.
export function proxy(request: NextRequest) {
  if (!request.cookies.has("rustgit_session")) {
    return NextResponse.redirect(new URL("/login", request.url));
  }
}

export const config = {
  matcher: ["/", "/repos/:path*", "/users/:path*"],
};
