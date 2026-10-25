{
  description = "One Billion Row Challenge in Rust";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { nixpkgs, rust-overlay, ... }:
    let
      forAllSystems = nixpkgs.lib.genAttrs [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
    in
    {
      devShells = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system}.extend rust-overlay.overlays.default;
          # the same toolchain rustup uses, from rust-toolchain.toml
          rust = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        in
        {
          default = pkgs.mkShell {
            packages =
              with pkgs;
              [
                rust
                hyperfine
                samply
              ]
              ++ lib.optionals stdenv.hostPlatform.isLinux [ perf ];

            RUST_SRC_PATH = "${rust}/lib/rustlib/src/rust/library";
          };

          # The Mojo port: Mojo comes from PyPI, pinned in mojo/requirements.txt, into mojo/.venv
          mojo = pkgs.mkShell {
            packages =
              with pkgs;
              [
                python3
                hyperfine
              ]
              ++ lib.optionals stdenv.hostPlatform.isLinux [ perf ];

            # Mojo links its executables with the C compiler of the shell, against its libstdc++
            LD_LIBRARY_PATH = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux (
              pkgs.lib.makeLibraryPath [ pkgs.stdenv.cc.cc.lib ]
            );

            shellHook = ''
              if ! cmp -s mojo/requirements.txt mojo/.venv/requirements.txt; then
                python3 -m venv mojo/.venv
                mojo/.venv/bin/pip install --quiet --requirement mojo/requirements.txt
                cp mojo/requirements.txt mojo/.venv/requirements.txt
              fi
              export PATH="$PWD/mojo/.venv/bin:$PATH"
            '';
          };
        }
      );

      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixfmt);
    };
}
