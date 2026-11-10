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
    # Shared UI and companion crates must resolve to one tagged Git source.
    kitSources = nixpkgs.lib.unique (
      map (package: package.source or "") (
        builtins.filter
          (package: builtins.elem package.name ["ha-app-ui" "ha-app-kit-ha"])
          (builtins.fromTOML (builtins.readFile ./Cargo.lock)).package
      )
    );
    cargoLockFile =
      assert nixpkgs.lib.assertMsg (
        builtins.length kitSources <= 1
        && builtins.all
          (source: builtins.match "git[+]https://github[.]com/jessfraz/ha-app-kit[?]tag=v[0-9]+[.][0-9]+[.][0-9]+(-[0-9A-Za-z.-]+)?#[0-9a-f]{40}" source != null)
          kitSources
      ) "App Kit crates must use one tagged release from github.com/jessfraz/ha-app-kit.";
      ./Cargo.lock;
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
          lockFile = cargoLockFile;
          allowBuiltinFetchGit = true;
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
