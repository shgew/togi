{
  pkgs,
  package,
  goSource,
  revision,
}:
let
  tools = package.overrideAttrs {
    pname = "togi-comparison-tools";
    src = goSource;
    goModules = package.goModules;
    subPackages = [
      "tools/sim"
      "tools/compare"
    ];
    doCheck = false;
    postInstall = "";
  };
  harness = pkgs.runCommand "togi-comparison-harness" { } ''
    install -D -m755 ${../tools/compare/harness.bash} $out/bin/harness
    patchShebangs $out/bin
  '';
  manifests = ../tools/compare/manifests;
  shellEnv = {
    nativeBuildInputs = [
      harness
      tools
      pkgs.zstd
    ];
    COMPARE = "${tools}/bin/compare";
    SOURCE = goSource;
    MANIFESTS = manifests;
  };

  corpus = pkgs.runCommand "togi-comparison-corpus" shellEnv ''
    cd ${goSource}
    HARNESS_SIM=${tools}/bin/sim HARNESS_JOBS=$NIX_BUILD_CORES harness corpus $out
    {
      echo "go_sources_sha256 $(find . -type f | LC_ALL=C sort | xargs sha256sum | sha256sum | cut -d' ' -f1)"
      echo "go_version ${pkgs.go_1_27.version}"
    } > $out/reference
  '';

  expected = pkgs.runCommand "togi-comparison-expected" (shellEnv // { CORPUS = corpus; }) ''
    HARNESS_JOBS=$NIX_BUILD_CORES harness expect $out
  '';

  verify =
    name: env: script:
    pkgs.runCommand "togi-comparison-${name}"
      (
        shellEnv
        // {
          CORPUS = corpus;
          EXPECTED = expected;
          SUBJECT = "${tools}/bin/compare";
          HARNESS_JOBS = "4";
        }
        // env
      )
      ''
        ${script}
        touch $out
      '';

  flip = pkgs.writeShellScript "flip-subject" ''
    ${tools}/bin/compare "$@"
    out=$3
    case $HARNESS_ID in
      fixture/cmd/togi/testdata/events.jsonl)
        printf '\0' | dd of="$out/encoded.jsonl" bs=1 count=1 conv=notrunc status=none ;;
      fixture/internal/tuner/testdata/hunt-live-failure-causes.jsonl)
        printf '\0' | dd of="$out/state.json" bs=1 seek=3 count=1 conv=notrunc status=none ;;
      fixture/tools/stats/testdata/recovered-state/events.jsonl)
        rm "$out/encoded.jsonl" "$out/state.json" ;;
    esac
  '';
in
{
  packages = {
    comparison-corpus = corpus;
    comparison-expected = expected;
    comparison-reference =
      pkgs.runCommand "togi-comparison-reference" { }
        "{ cat ${corpus}/reference; echo 'revision ${revision}'; } > $out";
  };

  checks = {
    comparison-manifests = pkgs.runCommand "togi-comparison-manifests" shellEnv ''
      cd ${goSource}
      compare suite tools/bench/suite.json | diff - ${manifests}/suite.txt
      for kind in fixtures goldens inputs; do
        compare "$kind" . | diff - ${manifests}/$kind.txt
      done
      touch $out
    '';

    comparison-selftest = verify "selftest" { } "harness check";

    comparison-negative =
      verify "negative"
        {
          SUBJECT = flip;
          HARNESS_SELECT = "^fixture/(cmd/togi/testdata/events|internal/tuner/testdata/hunt-live-failure-causes|tools/stats/testdata/recovered-state/events|internal/defect/testdata/power-off)[.]jsonl$";
        }
        ''
          if harness check 2>failures; then
            echo 'negative control: an altered byte went undetected' >&2
            exit 1
          fi
          diff failures - <<'EOF'
          FAIL fixture/cmd/togi/testdata/events.jsonl: encoded.jsonl differs
          FAIL fixture/internal/tuner/testdata/hunt-live-failure-causes.jsonl: state.json differs
          FAIL fixture/tools/stats/testdata/recovered-state/events.jsonl: subject wrote no encoded.jsonl or state.json
          EOF
          cp -rs ${expected} extra
          chmod -R +w extra
          mkdir -p extra/suite/bogus/dev-1
          touch extra/suite/bogus/dev-1/state.json
          if EXPECTED=$PWD/extra harness check 2>/dev/null; then
            echo 'negative control: an extra expected journal went undetected' >&2
            exit 1
          fi
          rm -r extra/suite/bogus extra/fixture/internal/defect/testdata/power-off.jsonl
          if EXPECTED=$PWD/extra harness check 2>/dev/null; then
            echo 'negative control: a missing expected journal went undetected' >&2
            exit 1
          fi
        '';
  };
}
