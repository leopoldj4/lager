{
  description = "worktree: a Git server in Rust backed by S3-compatible storage";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    crane.url = "github:ipetkov/crane";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      crane,
      rust-overlay,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };
        inherit (pkgs) lib;

        # The toolchain version is pinned in rust-toolchain.toml so that cargo
        # outside Nix (rustup) and inside Nix agree.
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        # The server runs git upload-pack / receive-pack, so Git is a runtime
        # dependency. Pinning it through flake.lock gives the fixed Git version
        # the implementation plan requires. The server needs only Git's core
        # commands: gitMinimal leaves out Perl, Python, and documentation,
        # which keeps the container image small.
        git = pkgs.gitMinimal;

        src = craneLib.cleanCargoSource ./.;

        commonArgs = {
          inherit src;
          # The root Cargo.toml is a workspace without a package name.
          pname = "worktree";
          version = "0.1.0";
          strictDeps = true;
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = lib.optionals pkgs.stdenv.hostPlatform.isDarwin [ pkgs.libiconv ];
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        worktree = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
            doCheck = false;
            nativeBuildInputs = commonArgs.nativeBuildInputs ++ [ pkgs.makeWrapper ];
            # The server runs Git for clients; the command line runs it for
            # snapshots.
            postInstall = ''
              for bin in worktree worktree-server; do
                wrapProgram $out/bin/$bin --prefix PATH : ${lib.makeBinPath [ git ]}
              done
            '';
            meta.mainProgram = "worktree";
          }
        );
        # The web interface (docs/decisions/0014-web-interface.md): static
        # files that the server serves with `[web] dir`. Dependencies come
        # from web/package-lock.json.
        web = pkgs.buildNpmPackage {
          pname = "worktree-web";
          version = "0.1.0";
          src = lib.fileset.toSource {
            root = ./web;
            fileset = lib.fileset.difference ./web (
              lib.fileset.unions [
                (lib.fileset.maybeMissing ./web/node_modules)
                (lib.fileset.maybeMissing ./web/dist)
              ]
            );
          };
          npmDeps = pkgs.importNpmLock { npmRoot = ./web; };
          npmConfigHook = pkgs.importNpmLock.npmConfigHook;
          installPhase = ''
            cp -r dist/client $out
          '';
        };

        # Local mode (IMPLEMENTATION_PLAN.md, Stage 1): `nix run .#local`
        # starts the pinned SeaweedFS as the S3-compatible service,
        # PostgreSQL for managed users, NATS for change events, and one
        # server with development
        # credentials. Storage data, database, and caches use separate
        # directories under $WORKTREE_LOCAL_DIR (default: ./.local). Stored
        # data is kept between runs. Remove the cache directory at any time:
        # the server recovers from storage.
        local = pkgs.writeShellApplication {
          name = "worktree-local";
          runtimeInputs = [
            worktree
            pkgs.seaweedfs
            pkgs.postgresql
            pkgs.nats-server
            git
            pkgs.coreutils
            pkgs.gnugrep
          ];
          text = ''
            root="''${WORKTREE_LOCAL_DIR:-$PWD/.local}"
            storage_dir="$root/storage"
            cache_dir="$root/cache"
            listen="''${WORKTREE_LISTEN:-127.0.0.1:8080}"
            s3_port="''${WORKTREE_S3_PORT:-8333}"
            pg_port="''${WORKTREE_PG_PORT:-5433}"
            nats_port="''${WORKTREE_NATS_PORT:-4222}"
            pg_dir="$root/postgres"
            mkdir -p "$storage_dir" "$cache_dir"
            # The built interface of this flake, also for an older config file.
            ln -sfn ${web} "$root/web"

            s3_config="$root/s3.json"
            cat >"$s3_config" <<'EOF'
            {"identities": [{"name": "worktree-dev",
              "credentials": [{"accessKey": "worktree-dev", "secretKey": "worktree-dev-secret"}],
              "actions": ["Admin", "Read", "Write", "List", "Tagging"]}]}
            EOF

            config="$root/worktree.toml"
            if [[ ! -f "$config" ]]; then
              cat >"$config" <<EOF
            # Local development configuration. Plain passwords work only on the
            # loopback address; use password_hash (worktree hash-password) elsewhere.
            [server]
            listen = "$listen"
            cache_dir = "$cache_dir"
            drain_delay_secs = 0

            [storage]
            prefix = "local"

            [storage.s3]
            endpoint = "http://127.0.0.1:$s3_port"
            bucket = "worktree"
            access_key_id = "worktree-dev"
            secret_access_key = "worktree-dev-secret"
            allow_http = true

            [catalog]
            kind = "storage"                  # repositories and managed users in storage
            cache_secs = 1
            create_on_push = "write"          # git push to a new name creates the repository

            [database]                        # users for worktree user / worktree token
            url = "postgres://worktree@127.0.0.1:$pg_port/worktree"

            [bus]                             # change events between servers
            url = "nats://127.0.0.1:$nats_port"

            [web]                             # web interface on http://$listen/
            dir = "$root/web"
            session_secret = "$(head -c 32 /dev/urandom | base64)"
            origins = ["http://localhost:3000"] # npm run dev in web/

            [[users]]
            name = "dev"
            password = "dev"
            access = { "*" = "force" }
            EOF
            fi

            weed mini -dir="$storage_dir" -ip=127.0.0.1 -ip.bind=127.0.0.1 \
              -s3.port="$s3_port" -master.port="''${WORKTREE_WEED_MASTER_PORT:-9333}" \
              -volume.port="''${WORKTREE_WEED_VOLUME_PORT:-8081}" \
              -filer.port="''${WORKTREE_WEED_FILER_PORT:-8888}" \
              -admin.ui=false -webdav=false -s3.config="$s3_config" -bucket=worktree \
              >"$root/storage.log" 2>&1 &
            weed_pid=$!
            nats-server -a 127.0.0.1 -p "$nats_port" >"$root/nats.log" 2>&1 &
            nats_pid=$!
            server_pid=""
            cleanup() {
              kill ''${server_pid:+"$server_pid"} "$weed_pid" "$nats_pid" 2>/dev/null || true
              pg_ctl -D "$pg_dir" -m fast stop >/dev/null 2>&1 || true
              wait 2>/dev/null || true
            }
            trap cleanup EXIT
            trap 'exit 130' INT TERM

            # PostgreSQL on the loopback address only, without a password, and without
            # a Unix socket. worktree creates its tables when it starts.
            if [[ ! -f "$pg_dir/PG_VERSION" ]]; then
              initdb -D "$pg_dir" -U worktree --auth=trust >"$root/postgres-init.log"
            fi
            pg_ctl -D "$pg_dir" -l "$root/postgres.log" -w \
              -o "-h 127.0.0.1 -p $pg_port -k '''" start >/dev/null
            createdb -h 127.0.0.1 -p "$pg_port" -U worktree worktree 2>/dev/null || true

            # Wait until the storage service answers.
            for _ in $(seq 60); do
              if repos=$(worktree -c "$config" repo list 2>/dev/null); then
                break
              fi
              if ! kill -0 "$weed_pid" 2>/dev/null; then
                echo "SeaweedFS stopped; see $root/storage.log" >&2
                exit 1
              fi
              sleep 1
            done
            repos=$(worktree -c "$config" repo list)

            if ! grep -q '^demo' <<<"$repos"; then
              worktree -c "$config" repo create demo
            fi

            echo "clone with: git clone http://dev:dev@$listen/demo.git"
            echo "web interface: http://$listen/ (sign in as dev / dev)"
            worktree-server -c "$config" &
            server_pid=$!
            wait "$server_pid"
          '';
        };
        # The built web interface at /share/worktree/web, for `[web] dir`.
        webShare = pkgs.runCommand "worktree-web-share" { } ''
          mkdir -p $out/share/worktree
          ln -s ${web} $out/share/worktree/web
        '';
        # Container image (Linux only). `nix build .#docker` gives a script
        # that streams the image: `./result | docker load`; deploy/ has a
        # Compose file for it. tini is PID 1 because Git starts processes
        # that worktree does not reap when they are orphaned; it forwards
        # SIGTERM for the clean stop.
        docker = pkgs.dockerTools.streamLayeredImage {
          name = "worktree";
          # /bin/worktree for administration (`docker compose exec worktree
          # worktree user create ...`), and the web interface.
          contents = [
            pkgs.dockerTools.caCertificates
            worktree
            webShare
          ];
          fakeRootCommands = ''
            mkdir -p etc tmp var/cache/worktree var/empty
            chmod 1777 tmp
            chown 10001:10001 var/cache/worktree
            echo 'root:x:0:0::/var/empty:/bin/false' > etc/passwd
            echo 'worktree:x:10001:10001::/var/empty:/bin/false' >> etc/passwd
            echo 'root:x:0:' > etc/group
            echo 'worktree:x:10001:' >> etc/group
          '';
          config = {
            Entrypoint = [
              "${pkgs.tini}/bin/tini"
              "--"
              "${worktree}/bin/worktree-server"
            ];
            Cmd = [
              "-c"
              "/etc/worktree/worktree.toml"
            ];
            User = "10001:10001";
            Env = [ "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt" ];
            ExposedPorts."8080/tcp" = { };
            Volumes."/var/cache/worktree" = { };
          };
        };
      in
      {
        packages = {
          default = worktree;
          inherit local web;
        }
        // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux { inherit docker; };

        apps.default = flake-utils.lib.mkApp { drv = worktree; };
        apps.local = flake-utils.lib.mkApp { drv = local; };

        checks = {
          inherit worktree;
          clippy = craneLib.cargoClippy (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--all-targets -- --deny warnings";
            }
          );
          fmt = craneLib.cargoFmt { inherit (commonArgs) src pname version; };
        };

        devShells.default = craneLib.devShell {
          checks = self.checks.${system};
          packages = [
            git
            # Local S3-compatible storage service. Supports conditional PUT
            # (If-None-Match / If-Match), which the head CAS depends on.
            pkgs.seaweedfs
            pkgs.awscli2 # for poking at buckets and conditional writes by hand
            pkgs.postgresql # managed users ([database])
            pkgs.nats-server # change events between servers ([bus])
            pkgs.nodejs # the web interface (web/)
            pkgs.cargo-watch
          ];
          RUST_BACKTRACE = "1";
        };

        formatter = pkgs.nixfmt-rfc-style;
      }
    );
}
