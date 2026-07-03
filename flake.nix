{
  description = "x86jit — an x86-64 -> host recompiler (JIT) as a Rust library";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        # Pin a stable toolchain. cranelift/iced-x86/memmap2 are pure Rust,
        # so no extra native libraries are needed at build time.
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" ];
          # Host targets covered by the spec (x86-64 and ARM64 hosts, §1).
          targets = [ "x86_64-unknown-linux-gnu" "aarch64-unknown-linux-gnu" ];
        };

        nativeBuildInputs = [ rustToolchain pkgs.pkg-config ];
        # memmap2 uses mmap/mprotect from libc only; nothing else required.
        buildInputs = [ ];
      in
      {
        devShells.default = pkgs.mkShell {
          inherit nativeBuildInputs buildInputs;
          packages = [ pkgs.cargo-nextest ];

          RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";

          shellHook = ''
            echo "x86jit dev shell — rust $(rustc --version)"
          '';
        };

        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "x86jit";
          version = "0.3.0";
          src = self;
          cargoLock.lockFile = ./Cargo.lock;
          inherit nativeBuildInputs buildInputs;
        };

        formatter = pkgs.nixpkgs-fmt;
      });
}
