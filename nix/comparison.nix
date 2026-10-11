{
  pkgs,
  lib,
  package,
  goSource,
  revision,
}:
let
  goTool =
    name: src: subPackage:
    package.overrideAttrs {
      pname = "togi-comparison-${name}";
      inherit src;
      goModules = package.goModules;
      subPackages = [ subPackage ];
      doCheck = false;
      postInstall = "";
    };
  # The simulator's inputs only: a change to a test, a testdata file or another tool leaves the corpus alone.
  simSource = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.difference (lib.fileset.intersection
      (lib.fileset.unions [
        ../go.mod
        ../go.sum
        ../version.txt
        ../version.go
        ../internal
        ../tools/sim
        ../tools/trialfacts
        ../tools/bench/suite.json
        ../tools/bench/machines
        ../tools/bench/facts
      ])
      (
        lib.fileset.fromSource (
          lib.sources.cleanSourceWith {
            src = ../.;
            filter = path: type: type != "directory" || builtins.baseNameOf path != "testdata";
          }
        )
      )
    ) (lib.fileset.fileFilter (f: lib.hasSuffix "_test.go" f.name) ../.);
  };
  sim = goTool "sim" simSource "tools/sim";
  compare = goTool "compare" goSource "tools/compare";
  harness = pkgs.runCommand "togi-comparison-harness" { } ''
    install -D -m755 ${../tools/compare/harness.bash} $out/bin/harness
    patchShebangs $out/bin
  '';
  manifests = ../tools/compare/manifests;
  shellEnv = {
    nativeBuildInputs = [
      harness
      compare
      pkgs.zstd
    ];
    COMPARE = "${compare}/bin/compare";
    SOURCE = goSource;
    MANIFESTS = manifests;
  };

  corpus =
    pkgs.runCommand "togi-comparison-corpus"
      {
        nativeBuildInputs = [
          harness
          pkgs.jq
          pkgs.zstd
        ];
      }
      ''
        cd ${simSource}
        HARNESS_SIM=${sim}/bin/sim HARNESS_JOBS=$NIX_BUILD_CORES harness corpus $out
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
          SUBJECT = "${compare}/bin/compare";
          HARNESS_JOBS = "4";
        }
        // env
      )
      ''
        ${script}
        touch $out
      '';

  flip = pkgs.writeShellScript "flip-subject" ''
    ${compare}/bin/compare "$@"
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
  # A crate's comparison check, registered in flake.nix so that this protected file stays unchanged:
  # `comparison-journal = comparison.check "journal" { SUBJECT = "${subject}/bin/compare-journal"; }`.
  # HARNESS_SHARDS and HARNESS_SHARD in the attributes shard it.
  check = name: env: verify name env "harness check";

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

    comparison-selftest = verify "selftest" { } ''
      harness check
      echo ${corpus} ${expected} > $out
    '';

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
          # Controls of the driver's own checks run the unmodified Go subject, so that only the driver can fail, and
          # each asserts its diagnostic on stderr, not just a non-zero exit.
          go=${compare}/bin/compare
          control() {
            local what=$1 want=$2
            shift 2
            if env "$@" harness check 2>err; then
              echo "negative control: $what went undetected" >&2
              exit 1
            fi
            grep -qF -- "$want" err || {
              echo "negative control: $what failed without '$want':" >&2
              cat err >&2
              exit 1
            }
          }
          cp -rs ${expected} extra
          chmod -R +w extra
          mkdir -p extra/suite/bogus/dev-1
          touch extra/suite/bogus/dev-1/state.json
          control 'an extra expected journal with only state.json' 'suite/bogus/dev-1/state.json' \
            SUBJECT=$go EXPECTED=$PWD/extra
          rm extra/suite/bogus/dev-1/state.json
          touch extra/suite/bogus/dev-1/encoded.jsonl.zst
          control 'an extra expected journal with only encoded.jsonl.zst' 'suite/bogus/dev-1/encoded.jsonl.zst' \
            SUBJECT=$go EXPECTED=$PWD/extra
          rm -r extra/suite/bogus
          rm extra/fixture/internal/defect/testdata/power-off.jsonl/encoded.jsonl.zst
          control 'a journal without encoded.jsonl.zst that the selection leaves out' 'fixture/internal/defect/testdata/power-off.jsonl/encoded.jsonl.zst' \
            SUBJECT=$go EXPECTED=$PWD/extra HARNESS_SELECT='^fixture/cmd/togi/testdata/events[.]jsonl$'
          rm -r extra/fixture/internal/defect/testdata/power-off.jsonl
          control 'a missing expected journal' 'fixture/internal/defect/testdata/power-off.jsonl/state.json' \
            SUBJECT=$go EXPECTED=$PWD/extra
          cp -rs ${goSource} source
          chmod -R +w source
          rm source/internal/defect/testdata/power-off.jsonl
          control 'a comparison worker that failed before it compared' 'check: a comparison worker failed' \
            SUBJECT=$go SOURCE=$PWD/source HARNESS_SELECT='^fixture/internal/defect/testdata/power-off[.]jsonl$'
        '';
  };
}
