{
  description = "rustgit: a Git server in Rust backed by S3-compatible storage";

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
        # which keeps the container image small. Tests use the same Git.
        git = pkgs.gitMinimal;

        # Cargo sources, plus the README: a unit test checks its examples.
        src = lib.cleanSourceWith {
          src = ./.;
          filter = path: type: lib.hasSuffix "/README.md" path || craneLib.filterCargoSources path type;
          name = "source";
        };

        commonArgs = {
          inherit src;
          strictDeps = true;
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = lib.optionals pkgs.stdenv.hostPlatform.isDarwin [ pkgs.libiconv ];
          # Protocol tests drive real Git clients. HTTP clients need CA
          # certificates on Linux even for plain HTTP.
          nativeCheckInputs = [
            git
            pkgs.cacert
          ];
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        rustgit = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
            nativeBuildInputs = commonArgs.nativeBuildInputs ++ [ pkgs.makeWrapper ];
            postInstall = ''
              wrapProgram $out/bin/rustgit --prefix PATH : ${lib.makeBinPath [ git ]}
            '';
            meta.mainProgram = "rustgit";
          }
        );
        # Local mode: the pinned SeaweedFS as the S3-compatible service, one
        # server, development credentials. Storage data and caches live in
        # separate directories under ./.local.
        local = pkgs.writeShellApplication {
          name = "rustgit-local";
          runtimeInputs = [
            rustgit
            pkgs.seaweedfs
            git
            pkgs.coreutils
            pkgs.gnugrep
          ];
          text = builtins.readFile ./dev/start-local.sh;
        };
        # Container image (Linux only). `nix build .#docker` gives a script
        # that streams the image: `./result | docker load`. tini is PID 1
        # because Git starts processes that rustgit does not reap when they
        # are orphaned; it forwards SIGTERM for the clean stop.
        docker = pkgs.dockerTools.streamLayeredImage {
          name = "rustgit";
          contents = [ pkgs.dockerTools.caCertificates ];
          fakeRootCommands = ''
            mkdir -p etc tmp var/cache/rustgit var/empty
            chmod 1777 tmp
            chown 10001:10001 var/cache/rustgit
            echo 'root:x:0:0::/var/empty:/bin/false' > etc/passwd
            echo 'rustgit:x:10001:10001::/var/empty:/bin/false' >> etc/passwd
            echo 'root:x:0:' > etc/group
            echo 'rustgit:x:10001:' >> etc/group
          '';
          config = {
            Entrypoint = [
              "${pkgs.tini}/bin/tini"
              "--"
              "${rustgit}/bin/rustgit"
            ];
            Cmd = [
              "-c"
              "/etc/rustgit/rustgit.toml"
              "serve"
            ];
            User = "10001:10001";
            Env = [ "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt" ];
            ExposedPorts."8080/tcp" = { };
            Volumes."/var/cache/rustgit" = { };
          };
        };
      in
      {
        packages = {
          default = rustgit;
          inherit local;
        }
        // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux { inherit docker; };

        apps.default = flake-utils.lib.mkApp { drv = rustgit; };
        apps.local = flake-utils.lib.mkApp { drv = local; };

        checks = {
          inherit rustgit;
          clippy = craneLib.cargoClippy (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--all-targets -- --deny warnings";
            }
          );
          fmt = craneLib.cargoFmt { inherit src; };
        };

        devShells.default = craneLib.devShell {
          checks = self.checks.${system};
          packages = [
            git
            # Local S3-compatible storage service. Supports conditional PUT
            # (If-None-Match / If-Match), which the head CAS depends on.
            pkgs.seaweedfs
            pkgs.awscli2 # for poking at buckets and conditional writes by hand
            pkgs.cargo-nextest
            pkgs.cargo-watch
          ];
          RUST_BACKTRACE = "1";
        };

        formatter = pkgs.nixfmt-rfc-style;
      }
    );
}
