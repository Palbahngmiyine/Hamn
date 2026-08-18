{
  description = "Apple Virtualization container runtime for macOS";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/25.05";

  outputs = { self, nixpkgs }:
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
      commonPackages = pkgs: with pkgs; [
        actionlint
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
          default = pkgs.mkShell {
            packages = ciPackages;
          };

          ci = pkgs.mkShell {
            packages = ciPackages;
          };

          release = pkgs.mkShell {
            packages = ciPackages ++ (with pkgs; [
              go_1_23
              kubectl
              maven
              nodejs_22
            ]);
          };
        });

      packages = forAllSystems (system:
        let
          pkgs = pkgsFor system;
        in
        pkgs.lib.optionalAttrs pkgs.stdenv.isDarwin {
          hamn = pkgs.stdenv.mkDerivation {
            pname = "hamn";
            version = "0.0.1"; # x-release-please-version
            src = source;
            nativeBuildInputs = [ pkgs.gnumake ];
            dontConfigure = true;

            buildPhase = ''
              runHook preBuild
              make -j$NIX_BUILD_CORES host VERSION=$version
              runHook postBuild
            '';

            installPhase = ''
              runHook preInstall
              mkdir -p $out/bin
              install -m 0755 build/hamn $out/bin/hamn
              runHook postInstall
            '';

            doInstallCheck = true;
            installCheckPhase = ''
              $out/bin/hamn version | grep -Fx "hamn $version"
              /usr/bin/codesign --verify --strict $out/bin/hamn
            '';

            meta = {
              description = "Apple Virtualization container runtime for macOS";
              homepage = "https://github.com/Palbahngmiyine/Hamn";
              license = pkgs.lib.licenses.mit;
              mainProgram = "hamn";
              platforms = pkgs.lib.platforms.darwin;
            };
          };

          default = self.packages.${system}.hamn;
        });

      checks = forAllSystems (system:
        let
          pkgs = pkgsFor system;
        in
        {
          workflows = pkgs.runCommand "hamn-workflows" {
            nativeBuildInputs = [ pkgs.actionlint ];
          } ''
            actionlint -config-file ${source}/.github/actionlint.yaml \
              ${source}/.github/workflows/*.yml
            touch $out
          '';
        } // pkgs.lib.optionalAttrs pkgs.stdenv.isDarwin {
          package = self.packages.${system}.hamn;
        });
    };
}
