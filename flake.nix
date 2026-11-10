{
  description = "Downloads development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {
    self,
    nixpkgs,
    rust-overlay,
    ...
  }: let
    systems = ["aarch64-darwin" "aarch64-linux" "x86_64-linux"];
  in {
    packages = nixpkgs.lib.genAttrs systems (system: let
      pkgs = import nixpkgs {
        inherit system;
        overlays = [(import rust-overlay)];
      };
      toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      rustPlatform = pkgs.makeRustPlatform {
        cargo = toolchain;
        rustc = toolchain;
      };
      cli = rustPlatform.buildRustPackage {
        pname = "ha-downloads";
        version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
        src = pkgs.lib.fileset.toSource {
          root = ./.;
          fileset = pkgs.lib.fileset.unions [
            ./Cargo.toml
            ./Cargo.lock
            ./src
            ./web
            ./tests
          ];
        };
        cargoLock = {
          lockFile = ./Cargo.lock;
          outputHashes."ha-app-ui-0.1.2" = "sha256-MqC+Tv1dISbN7+TuwAIWUWCI2sfwzMj+kv4fDBk0D+g=";
        };
        cargoBuildFlags = ["--bin" "ha-downloads"];
        cargoTestFlags = ["--test" "cli" "--test" "cli_client"];
        __darwinAllowLocalNetworking = true;
        nativeBuildInputs = [pkgs.pkg-config];
        meta = {
          description = "Typed JSON client for Downloads";
          license = pkgs.lib.licenses.mit;
          mainProgram = "ha-downloads";
          platforms = systems;
        };
      };
    in {
      ha-downloads = cli;
      default = cli;
    });
    apps = nixpkgs.lib.genAttrs systems (system: let
      cli = {
        type = "app";
        meta.description = "Typed JSON client for Downloads";
        program = "${self.packages.${system}.ha-downloads}/bin/ha-downloads";
      };
    in {
      ha-downloads = cli;
      default = cli;
    });
    devShells = nixpkgs.lib.genAttrs systems (system: let
      pkgs = import nixpkgs {
        inherit system;
        overlays = [(import rust-overlay)];
      };
    in {
      default = pkgs.mkShell ({
          packages = with pkgs;
            [
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
            ]
            ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [pkgs.playwright-driver];
        }
        // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
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
