{
  description = "Generative UI Suite — agents publish interactive HTML surfaces into a directory; a Rust server discovers, serves, and routes backflow back into the same directory.";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        # Pinned toolchain comes straight from rust-toolchain.toml.
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        rustPlatform = pkgs.makeRustPlatform {
          cargo = rustToolchain;
          rustc = rustToolchain;
        };

        # One workspace build emits every binary (gui-server, gui-mcp, gui-cli)
        # into $out/bin. Per-binary packages below are thin views over it.
        workspace = rustPlatform.buildRustPackage {
          pname = "generative-ui";
          version = "0.1.0";
          src = ./.;
          cargoLock = {
            lockFile = ./Cargo.lock;
            # The optional `direct-access` example depends on swiftide from git.
            outputHashes = {
              "swiftide-0.32.1" = "sha256-KL73CcfDKcfz5vqZ8HX3FjIjxyMMuPWsBTHRolmotXk=";
            };
          };
          # Build only the core binaries. `direct-access` (git swiftide, edition-2024 manifest
          # directive) needs a nightly cargo and is built with `cargo +nightly` outside Nix.
          cargoBuildFlags = [ "--bin" "gui-server" "--bin" "gui-mcp" "--bin" "gui" ];
          doCheck = false;
        };

        binPkg = name:
          pkgs.runCommand "${name}-0.1.0" { } ''
            mkdir -p $out/bin
            ln -s ${workspace}/bin/${name} $out/bin/${name}
          '';

        app = name: {
          type = "app";
          program = "${workspace}/bin/${name}";
        };
      in
      {
        packages = {
          default = workspace;
          generative-ui = workspace;
          gui-server = binPkg "gui-server";
          gui-mcp = binPkg "gui-mcp";
          gui-cli = binPkg "gui"; # the gui-cli crate's binary is named `gui`
        };

        apps = {
          gui-server = app "gui-server";
          gui-mcp = app "gui-mcp";
          gui-cli = app "gui";
        };

        devShells.default = pkgs.mkShell {
          packages = [
            rustToolchain
            pkgs.rust-analyzer
            pkgs.cargo-watch
            pkgs.nodejs_22
          ];
          RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
        };
      });
}
