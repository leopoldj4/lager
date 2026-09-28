"use client";

import {
  FileLockIcon,
  KeyRoundIcon,
  Loader2Icon,
  MoreHorizontalIcon,
  ShieldIcon,
  Trash2Icon,
  UserPlusIcon,
  WandSparklesIcon,
} from "lucide-react";
import { useState, useTransition } from "react";
import { toast } from "sonner";

import {
  AccessBadges,
  AccessEditor,
  toAccess,
  toGrants,
  type Grant,
} from "@/app/(admin)/users/access-editor";
import { createUser, deleteUser, setUserAccess, setUserPassword, type ActionResult } from "@/app/actions";
import { CopyButton } from "@/components/copy-button";
import { PageHeader } from "@/components/page-header";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import type { User } from "@/lib/rustgit";

const MIN_PASSWORD = 12;
const NAME_PATTERN = /^[A-Za-z0-9_][A-Za-z0-9._-]{0,63}$/;

type Dialogs =
  | { kind: "create" }
  | { kind: "access"; user: User }
  | { kind: "password"; user: User }
  | { kind: "delete"; user: User }
  | undefined;

export function UsersView({
  users,
  managed,
  repos,
  me,
}: {
  users: User[];
  managed: boolean;
  repos: string[];
  me: string;
}) {
  const [open, setOpen] = useState<Dialogs>();
  const close = () => setOpen(undefined);

  return (
    <>
      <PageHeader
        title="Users"
        description="Who can sign in to Git over HTTPS, and what each user may do per repository."
      >
        <Button onClick={() => setOpen({ kind: "create" })} disabled={!managed}>
          <UserPlusIcon />
          New user
        </Button>
      </PageHeader>

      {!managed && (
        <Alert className="mb-6">
          <FileLockIcon />
          <AlertTitle>Users are read-only on this server</AlertTitle>
          <AlertDescription>
            Users can be managed here only with <code>catalog.kind = &quot;storage&quot;</code> and without an
            identity service.
          </AlertDescription>
        </Alert>
      )}

      <Card className="py-0">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead className="pl-4">User</TableHead>
              <TableHead>Access</TableHead>
              <TableHead>Last change</TableHead>
              <TableHead className="w-12 pr-4">
                <span className="sr-only">Actions</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {users.map((u) => (
              <TableRow key={u.name}>
                <TableCell className="pl-4">
                  <div className="flex items-center gap-2">
                    <span className="font-medium">{u.name}</span>
                    {u.name === me && <Badge variant="outline">you</Badge>}
                    {u.source === "config" && (
                      <Tooltip>
                        <TooltipTrigger asChild>
                          <Badge variant="secondary">
                            <FileLockIcon />
                            config file
                          </Badge>
                        </TooltipTrigger>
                        <TooltipContent>Defined in rustgit.toml; change it there.</TooltipContent>
                      </Tooltip>
                    )}
                  </div>
                </TableCell>
                <TableCell>
                  <AccessBadges access={u.access} />
                </TableCell>
                <TableCell className="text-sm text-muted-foreground">
                  {u.updated_ms ? (
                    <>
                      {new Date(u.updated_ms).toLocaleString()}
                      {u.updated_by && <span> by {u.updated_by}</span>}
                    </>
                  ) : (
                    "—"
                  )}
                </TableCell>
                <TableCell className="pr-4 text-right">
                  {u.source === "storage" && managed && (
                    <DropdownMenu>
                      <DropdownMenuTrigger asChild>
                        <Button variant="ghost" size="icon-sm" aria-label={`Actions for ${u.name}`}>
                          <MoreHorizontalIcon />
                        </Button>
                      </DropdownMenuTrigger>
                      <DropdownMenuContent align="end" className="min-w-44">
                        <DropdownMenuItem onSelect={() => setOpen({ kind: "access", user: u })}>
                          <ShieldIcon />
                          Change access
                        </DropdownMenuItem>
                        <DropdownMenuItem onSelect={() => setOpen({ kind: "password", user: u })}>
                          <KeyRoundIcon />
                          Set password
                        </DropdownMenuItem>
                        <DropdownMenuSeparator />
                        <DropdownMenuItem
                          variant="destructive"
                          disabled={u.name === me}
                          onSelect={() => setOpen({ kind: "delete", user: u })}
                        >
                          <Trash2Icon />
                          Delete user
                        </DropdownMenuItem>
                      </DropdownMenuContent>
                    </DropdownMenu>
                  )}
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </Card>

      <CreateUserDialog open={open?.kind === "create"} onClose={close} repos={repos} />
      {open?.kind === "access" && <AccessDialog user={open.user} onClose={close} repos={repos} />}
      {open?.kind === "password" && <PasswordDialog user={open.user} onClose={close} />}
      {open?.kind === "delete" && <DeleteDialog user={open.user} onClose={close} />}
    </>
  );
}

/** Runs a server action; returns true on success and reports errors. */
function useAction() {
  const [pending, start] = useTransition();
  const [error, setError] = useState<string>();
  const run = (action: () => Promise<ActionResult>, success: string, then: () => void) => {
    setError(undefined);
    start(async () => {
      const result = await action();
      if (result.ok) {
        toast.success(success);
        then();
      } else {
        setError(result.error);
      }
    });
  };
  return { pending, error, setError, run };
}

function generatePassword(): string {
  const alphabet = "ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
  const bytes = crypto.getRandomValues(new Uint32Array(24));
  return Array.from(bytes, (b) => alphabet[b % alphabet.length]).join("");
}

function PasswordField({
  value,
  onChange,
  autoFocus,
}: {
  value: string;
  onChange: (v: string) => void;
  autoFocus?: boolean;
}) {
  const [visible, setVisible] = useState(false);
  return (
    <div className="grid gap-2">
      <Label htmlFor="password">Password</Label>
      <div className="flex gap-2">
        <Input
          id="password"
          type={visible ? "text" : "password"}
          className="font-mono"
          autoComplete="new-password"
          value={value}
          autoFocus={autoFocus}
          onChange={(e) => onChange(e.target.value)}
        />
        {visible && value && <CopyButton value={value} label="Copy password" />}
        <Button
          type="button"
          variant="outline"
          onClick={() => {
            onChange(generatePassword());
            setVisible(true);
          }}
        >
          <WandSparklesIcon />
          Generate
        </Button>
      </div>
      <p className="text-xs text-muted-foreground">
        At least {MIN_PASSWORD} characters. It is shown only now; rustgit keeps a salted hash.
      </p>
    </div>
  );
}

function ErrorText({ error }: { error?: string }) {
  if (!error) return null;
  return (
    <Alert variant="destructive">
      <AlertDescription>{error}</AlertDescription>
    </Alert>
  );
}

function CreateUserDialog({ open, onClose, repos }: { open: boolean; onClose: () => void; repos: string[] }) {
  const [name, setName] = useState("");
  const [password, setPassword] = useState("");
  const [grants, setGrants] = useState<Grant[]>([]);
  const { pending, error, setError, run } = useAction();

  const reset = () => {
    setName("");
    setPassword("");
    setGrants([]);
    setError(undefined);
  };

  const submit = (e: React.FormEvent) => {
    e.preventDefault();
    if (!NAME_PATTERN.test(name)) {
      setError("Use 1 to 64 letters, digits, '.', '_', or '-', not starting with '.' or '-'.");
      return;
    }
    if (password.length < MIN_PASSWORD) {
      setError(`The password needs at least ${MIN_PASSWORD} characters.`);
      return;
    }
    run(() => createUser({ name, password, access: toAccess(grants) }), `Created ${name}`, () => {
      onClose();
      reset();
    });
  };

  return (
    <Dialog
      open={open}
      onOpenChange={(o) => {
        if (!o) {
          onClose();
          reset();
        }
      }}
    >
      <DialogContent className="sm:max-w-lg">
        <form onSubmit={submit} className="grid gap-5">
          <DialogHeader>
            <DialogTitle>New user</DialogTitle>
            <DialogDescription>The user signs in to Git with this name and password.</DialogDescription>
          </DialogHeader>
          <div className="grid gap-2">
            <Label htmlFor="name">User name</Label>
            <Input
              id="name"
              value={name}
              autoComplete="off"
              autoFocus
              onChange={(e) => setName(e.target.value.trim())}
              placeholder="e.g. ci-bot"
            />
          </div>
          <PasswordField value={password} onChange={setPassword} />
          <div className="grid gap-2">
            <Label>Access</Label>
            <AccessEditor grants={grants} onChange={setGrants} repos={repos} />
          </div>
          <ErrorText error={error} />
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              Cancel
            </Button>
            <Button type="submit" disabled={pending}>
              {pending && <Loader2Icon className="animate-spin" />}
              Create user
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function AccessDialog({ user, onClose, repos }: { user: User; onClose: () => void; repos: string[] }) {
  const [grants, setGrants] = useState<Grant[]>(() => toGrants(user.access));
  const { pending, error, run } = useAction();
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <form
          className="grid gap-5"
          onSubmit={(e) => {
            e.preventDefault();
            run(() => setUserAccess(user.name, toAccess(grants)), `Changed access of ${user.name}`, onClose);
          }}
        >
          <DialogHeader>
            <DialogTitle>Access of {user.name}</DialogTitle>
            <DialogDescription>
              A grant for one repository overrides the grant for all repositories. Changes apply to the next Git
              request.
            </DialogDescription>
          </DialogHeader>
          <AccessEditor grants={grants} onChange={setGrants} repos={repos} />
          <ErrorText error={error} />
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              Cancel
            </Button>
            <Button type="submit" disabled={pending}>
              {pending && <Loader2Icon className="animate-spin" />}
              Save access
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function PasswordDialog({ user, onClose }: { user: User; onClose: () => void }) {
  const [password, setPassword] = useState("");
  const { pending, error, setError, run } = useAction();
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent>
        <form
          className="grid gap-5"
          onSubmit={(e) => {
            e.preventDefault();
            if (password.length < MIN_PASSWORD) {
              setError(`The password needs at least ${MIN_PASSWORD} characters.`);
              return;
            }
            run(() => setUserPassword(user.name, password), `Set a new password for ${user.name}`, onClose);
          }}
        >
          <DialogHeader>
            <DialogTitle>New password for {user.name}</DialogTitle>
            <DialogDescription>The old password stops working at once.</DialogDescription>
          </DialogHeader>
          <PasswordField value={password} onChange={setPassword} autoFocus />
          <ErrorText error={error} />
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              Cancel
            </Button>
            <Button type="submit" disabled={pending}>
              {pending && <Loader2Icon className="animate-spin" />}
              Set password
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function DeleteDialog({ user, onClose }: { user: User; onClose: () => void }) {
  const { pending, error, run } = useAction();
  return (
    <AlertDialog open onOpenChange={(o) => !o && onClose()}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>Delete {user.name}?</AlertDialogTitle>
          <AlertDialogDescription>
            {user.name} can no longer sign in. Repositories and their history are not changed. The name can be
            used again later.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <ErrorText error={error} />
        <AlertDialogFooter>
          <AlertDialogCancel disabled={pending}>Cancel</AlertDialogCancel>
          <AlertDialogAction
            variant="destructive"
            disabled={pending}
            onClick={(e) => {
              e.preventDefault();
              run(() => deleteUser(user.name), `Deleted ${user.name}`, onClose);
            }}
          >
            {pending && <Loader2Icon className="animate-spin" />}
            Delete user
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
