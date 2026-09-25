{
  description = "Apple Virtualization container runtime for macOS";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/25.05";
    # Supplies the exact Rust release pinned by rust-toolchain.toml.
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { nixpkgs, rust-overlay, ... }:
    let
      systems = [
        "aarch64-darwin"
        "x86_64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      pkgsFor = system: nixpkgs.legacyPackages.${system};
      source = nixpkgs.lib.cleanSource ./.;
      hamnVersion = "0.1.2"; # x-release-please-version
      actionlintVersion = "1.7.12";
      actionlintArchives = {
        "aarch64-darwin" = {
          platform = "darwin_arm64";
          sha256 = "aba9ced2dee8d27fecca3dc7feb1a7f9a52caefa1eb46f3271ea66b6e0e6953f";
        };
        "x86_64-darwin" = {
          platform = "darwin_amd64";
          sha256 = "5b44c3bc2255115c9b69e30efc0fecdf498fdb63c5d58e17084fd5f16324c644";
        };
        "aarch64-linux" = {
          platform = "linux_arm64";
          sha256 = "325e971b6ba9bfa504672e29be93c24981eeb1c07576d730e9f7c8805afff0c6";
        };
        "x86_64-linux" = {
          platform = "linux_amd64";
          sha256 = "8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8";
        };
      };
      actionlintFor = pkgs:
        let
          archive = actionlintArchives.${pkgs.system};
        in
        pkgs.stdenvNoCC.mkDerivation {
          pname = "actionlint";
          version = actionlintVersion;
          src = pkgs.fetchurl {
            url = "https://github.com/rhysd/actionlint/releases/download/v${actionlintVersion}/actionlint_${actionlintVersion}_${archive.platform}.tar.gz";
            sha256 = archive.sha256;
          };
          sourceRoot = ".";
          dontConfigure = true;
          dontBuild = true;
          installPhase = ''
            mkdir -p $out/bin
            install -m 0755 actionlint $out/bin/actionlint
          '';
        };
      # The channel, profile and components declared in rust-toolchain.toml.
      rustToolchainFor = pkgs:
        (rust-overlay.lib.mkRustBin { } pkgs).fromRustupToolchainFile ./rust-toolchain.toml;
      # Hamn links against the macOS SDK and is signed with Apple's codesign.
      # These names resolve to Apple's /usr/bin tools, never a Nix compiler.
      appleToolchainFor = pkgs: pkgs.linkFarm "hamn-apple-toolchain" (map
        (tool: { name = "bin/${tool}"; path = "/usr/bin/${tool}"; })
        [ "ar" "c++" "cc" "clang" "clang++" "codesign" "ld" "otool" "ranlib" "xcrun" ]);
      # Darwin shells: stdenv puts GNU coreutils/findutils/sed/grep/awk/tar
      # ahead of the caller's PATH, but Hamn's scripts target the macOS (BSD)
      # userland. Order PATH as pinned Nix tools, macOS system directories,
      # then the caller's PATH, and select the system SDK for every build.
      darwinShellHook = pkgs: packages:
        let
          gnuUserland = pkgs.lib.subtractLists packages pkgs.stdenvNoCC.initialPath;
        in
        ''
          hamn_nix= hamn_rest=
          IFS=: read -ra hamn_dirs <<<"$PATH"
          for hamn_dir in "''${hamn_dirs[@]}"; do
            case "$hamn_dir" in
            ${pkgs.lib.concatMapStringsSep " | " (p: "\"${p}/bin\"") gnuUserland}) ;;
            /nix/store/*) hamn_nix=$hamn_nix$hamn_dir: ;;
            *) hamn_rest=$hamn_rest:$hamn_dir ;;
            esac
          done
          export PATH=$hamn_nix/usr/bin:/bin:/usr/sbin:/sbin$hamn_rest
          unset hamn_nix hamn_rest hamn_dirs hamn_dir
          case "''${DEVELOPER_DIR:-}" in /nix/store/*) unset DEVELOPER_DIR ;; esac
          HAMN_SYSTEM_SDKROOT=$(/usr/bin/env -u SDKROOT -u DEVELOPER_DIR \
            /usr/bin/xcrun --sdk macosx --show-sdk-path) || HAMN_SYSTEM_SDKROOT=
          case "$HAMN_SYSTEM_SDKROOT" in
          /nix/store/*)
            echo "FAIL: Hamn must not compile against the Nix Apple SDK" >&2
            exit 1
            ;;
          /*) [ -d "$HAMN_SYSTEM_SDKROOT" ] ;;
          *) false ;;
          esac || {
            echo "FAIL: the system macOS SDK is unavailable; run xcode-select --install" >&2
            exit 1
          }
          export SDKROOT=$HAMN_SYSTEM_SDKROOT HAMN_SYSTEM_SDKROOT
        '';
      commonPackages = pkgs: with pkgs; [
        (actionlintFor pkgs)
        bash
        curl
        docker-client # includes the Compose and buildx CLI plugins
        git
        gnumake
        jq
        openssh
        python3
        ripgrep
        ruby
        (rustToolchainFor pkgs)
      ];
    in
    {
      devShells = forAllSystems (system:
        let
          pkgs = pkgsFor system;
          inherit (pkgs) lib stdenv;
          shellWith = extraPackages:
            let
              packages = lib.optionals stdenv.isDarwin [ (appleToolchainFor pkgs) ]
                ++ commonPackages pkgs
                ++ lib.optionals stdenv.isLinux [ pkgs.coreutils pkgs.gcc ]
                ++ extraPackages;
            in
            pkgs.mkShellNoCC {
              inherit packages;
              HAMN_VERSION = hamnVersion;
              shellHook = lib.optionalString stdenv.isDarwin (darwinShellHook pkgs packages);
            };
        in
        {
          default = shellWith [ ];

          ci = shellWith [ ];

          # Real VM, Docker, Compose, buildx and disposable kind validation.
          live = shellWith (with pkgs; [
            kind
            kubectl
          ]);

          release = shellWith (with pkgs; [
            go_1_23
            kubectl
            maven
            nodejs_22
          ]);
        });

      checks = forAllSystems (system:
        let
          pkgs = pkgsFor system;
        in
        {
          workflows = pkgs.runCommand "hamn-workflows" {
            nativeBuildInputs = [ (actionlintFor pkgs) ];
          } ''
            actionlint -config-file ${source}/.github/actionlint.yaml \
              ${source}/.github/workflows/*.yml
            touch $out
          '';
        });
    };
}
