{
  description = "AutoNet — the IP address other devices on your network can actually reach";

  # Deliberately a single input. Every extra flake input is another thing that
  # can drift, and AutoNet needs nothing from the Rust ecosystem overlays that
  # nixpkgs does not already provide.
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-24.11";

  outputs = { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];

      forAllSystems = f:
        nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (pkgs: rec {
        default = autonet;

        # Nix is how AutoNet is *built* reproducibly. It is not a runtime
        # dependency: the result is an ordinary native executable, and users who
        # install it from a release tarball or `cargo install` get exactly the
        # same binary behaviour.
        autonet = pkgs.rustPlatform.buildRustPackage {
          pname = "autonet";
          version = "0.1.0";
          src = ./.;

          cargoLock.lockFile = ./Cargo.lock;

          # The tests that touch the live network are `#[ignore]`d, so the suite
          # is safe to run inside the build sandbox, where the only interface is
          # loopback.
          doCheck = true;

          meta = with pkgs.lib; {
            description = "Find the IP address other devices on your network can reach";
            homepage = "https://github.com/osworks/autonet";
            license = with licenses; [ mit asl20 ];
            mainProgram = "autonet";
            platforms = platforms.unix ++ platforms.windows;
          };
        };
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            # Cargo stays the build system; Nix only guarantees that everyone
            # has the same one.
            rustc
            cargo
            clippy
            rustfmt
            rust-analyzer
            cargo-nextest

            pkg-config

            # For working with `--json` output by hand.
            jq
          ];

          # rust-analyzer cannot find the standard library sources without this
          # when rustc comes from nixpkgs rather than rustup.
          RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";

          shellHook = ''
            echo "AutoNet dev shell · $(rustc --version)"
          '';
        };
      });

      formatter = forAllSystems (pkgs: pkgs.nixpkgs-fmt);
    };
}
