{
  description = "obsidian-web-server: edit a git-managed Obsidian vault over HTTP";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      rust-overlay,
      crane,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        # Keep Cargo sources plus our embedded HTML/JS/CSS assets.
        assetFilter =
          path: _type:
          builtins.match ".*/(src/assets/[^/]+|src/assets)$" path != null
          || builtins.match ".*\\.(html|css|js)$" path != null;

        srcFilter = path: type: (craneLib.filterCargoSources path type) || (assetFilter path type);

        src = pkgs.lib.cleanSourceWith {
          src = ./.;
          filter = srcFilter;
          name = "source";
        };

        commonArgs = {
          inherit src;
          strictDeps = true;
          # No extra system deps; we shell out to `git` at runtime, not link to it.

          # Workaround: this host's rustc 1.95 segfaults under parallel codegen at
          # opt-level=3 (reproducible with `cargo build --release` outside Nix as
          # well, fixed there by `CARGO_BUILD_JOBS=1`). Force one rustc job at a
          # time so `nix build` works on this machine. Healthy machines can drop
          # this and rebuild faster.
          CARGO_BUILD_JOBS = "1";
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        obsidian-web-server = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
            doCheck = true;
          }
        );
      in
      {
        packages = {
          default = obsidian-web-server;
          obsidian-web-server = obsidian-web-server;
        };

        apps.default = {
          type = "app";
          program = "${obsidian-web-server}/bin/obsidian-web-server";
        };

        devShells.default = craneLib.devShell {
          inputsFrom = [ obsidian-web-server ];
          packages = with pkgs; [
            rust-analyzer
            git
          ];
        };

        checks = {
          inherit obsidian-web-server;
          obsidian-web-server-clippy = craneLib.cargoClippy (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--all-targets -- --deny warnings";
            }
          );
          obsidian-web-server-fmt = craneLib.cargoFmt {
            inherit src;
          };
        };

        formatter = pkgs.nixfmt-rfc-style;
      }
    );
}
