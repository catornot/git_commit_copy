{
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };
  outputs =
    {
      self,
      nixpkgs,
      utils,
      rust-overlay,
    }:
    utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
          config.allowUnfree = true;
        };
        channel = "stable"; # For nightly, use "nightly".
        toolchain = pkgs.rust-bin.${channel}.latest.default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
            "clippy"
            "rustfmt"
          ];
        };
      in
      {
        formatter = pkgs.nixfmt-tree;

        packages = {
          git_commit_copy = pkgs.rustPlatform.buildRustPackage {
            pname = "git_commit_copy";
            version = "0.1.0";

            src = ./.;

            cargoLock.lockFile = ./Cargo.lock;

            nativeBuildInputs = [
              pkgs.installShellFiles
              pkgs.mold
              pkgs.pkg-config
            ];

            buildInputs = [
              pkgs.libgit2
              pkgs.openssl
            ];

            postInstall = ''
              installShellCompletion --bash --name git_commit_copy.bash <($out/bin/git_commit_copy --src . --dst . completions bash)
              installShellCompletion --fish --name git_commit_copy.fish <($out/bin/git_commit_copy --src . --dst . completions fish)
              installShellCompletion --zsh --name _git_commit_copy <($out/bin/git_commit_copy --src . --dst . completions zsh)
            '';

            meta = {
              description = "copies comits from one repo to another";
              mainProgram = "git_commit_copy";
              platforms = pkgs.lib.platforms.unix;
            };
          };

          default = self.packages.${system}.git_commit_copy;
        };

        devShells.default = pkgs.mkShell {
          nativeBuildInputs = [
            toolchain
            pkgs.mold
            pkgs.pkg-config
          ];

          buildInputs = with pkgs; [
            libgit2
            openssl
          ];
        };
      }
    );
}
