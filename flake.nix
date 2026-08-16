{
  inputs = {
    nixpkgs.url = "nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { flake-parts, fenix, ... }@inputs:
    flake-parts.lib.mkFlake { inherit inputs; } ({ ... }: {
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];

      perSystem = { pkgs, inputs', ... }: let
        fenixChannel = inputs'.fenix.packages.stable;
        toolchain = fenixChannel.withComponents [ "cargo" "rustc" ];
        rustPlatform = pkgs.makeRustPlatform { cargo = toolchain; rustc = toolchain; };
        herdr-prevtab = pkgs.callPackage ./package.nix {
          inherit rustPlatform;
        };
      in {
        packages.herdr-prevtab = herdr-prevtab;
        packages.default = herdr-prevtab;

        checks.formatting = pkgs.stdenvNoCC.mkDerivation {
          name = "herdr-prevtab-formatting";
          src = ./.;
          nativeBuildInputs = [
            (fenixChannel.withComponents [ "cargo" "rustfmt" ])
          ];
          checkPhase = ''
            cargo fmt --check
          '';
          doCheck = true;
          installPhase = "touch $out";
        };

        checks.rust-tests = rustPlatform.buildRustPackage {
          name = "herdr-prevtab-rust-tests";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          buildType = "debug";
          installPhase = "touch $out";
          doCheck = true;
        };

        devShells.default = pkgs.mkShell {
          packages = [
            (fenixChannel.withComponents [
              "cargo"
              "clippy"
              "rust-src"
              "rustc"
              "rustfmt"
              "rust-analyzer"
            ])
            pkgs.jq
            pkgs.cargo-audit
            pkgs.cargo-edit
          ];
        };
      };
    });
}
