{
  description = "Downloads development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {nixpkgs, rust-overlay, ...}: let
    systems = ["aarch64-darwin" "aarch64-linux" "x86_64-linux"];
  in {
    devShells = nixpkgs.lib.genAttrs systems (system: let
      pkgs = import nixpkgs {
        inherit system;
        overlays = [(import rust-overlay)];
      };
    in {
      default = pkgs.mkShell ({
        packages = with pkgs; [
          (rust-bin.fromRustupToolchainFile ./rust-toolchain.toml)
          pkg-config
          openssl
          python313
          uv
          just
          jq
          actionlint
          shellcheck
          docker
          docker-buildx
          cargo-deny
          nodejs
          resvg
        ] ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [pkgs.playwright-driver];
      } // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
        PLAYWRIGHT_MODULE = "${pkgs.playwright-driver}";
        PLAYWRIGHT_BROWSERS_PATH = "${pkgs.playwright-driver.selectBrowsers {
          withChromium = false;
          withFirefox = false;
          withWebkit = false;
          withFfmpeg = false;
        }}";
      });
    });
  };
}
