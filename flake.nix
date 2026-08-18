{
  description = "Apple Virtualization container runtime for macOS";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/25.05";

  outputs = { nixpkgs, ... }:
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
      hamnVersion = "0.0.1"; # x-release-please-version
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
      commonPackages = pkgs: with pkgs; [
        (actionlintFor pkgs)
        bash
        curl
        git
        gnumake
        jq
        openssh
        python3
        ripgrep
        ruby
      ];
    in
    {
      devShells = forAllSystems (system:
        let
          pkgs = pkgsFor system;
          ciPackages = commonPackages pkgs
            ++ pkgs.lib.optionals pkgs.stdenv.isLinux [
              pkgs.coreutils
              pkgs.gcc
            ];
        in
        {
          default = pkgs.mkShellNoCC {
            packages = ciPackages;
            HAMN_VERSION = hamnVersion;
          };

          ci = pkgs.mkShellNoCC {
            packages = ciPackages;
            HAMN_VERSION = hamnVersion;
          };

          release = pkgs.mkShellNoCC {
            packages = ciPackages ++ (with pkgs; [
              go_1_23
              kubectl
              maven
              nodejs_22
            ]);
            HAMN_VERSION = hamnVersion;
          };
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
