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
        # the implementation plan requires.
        git = pkgs.git;

        src = craneLib.cleanCargoSource ./.;

        commonArgs = {
          inherit src;
          strictDeps = true;
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = lib.optionals pkgs.stdenv.hostPlatform.isDarwin [ pkgs.libiconv ];
          # Protocol tests drive real Git clients.
          nativeCheckInputs = [ git ];
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
      in
      {
        packages.default = rustgit;

        apps.default = flake-utils.lib.mkApp { drv = rustgit; };

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
