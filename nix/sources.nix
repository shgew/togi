# Cargo source groups exclude unrelated workspace members and fixture files, so edits outside a build's
# dependency closure leave its derivation hash alone.
#
# A source keeps the workspace manifests and `Cargo.lock`, so Cargo resolves the workspace and `--locked` holds, and
# the files of the members the derivation builds. A member built but not tested (`code`) leaves out its test-only
# files, so a test edit does not change the derivation of the package, the simulator or the VM checks. Lint
# configuration (`clippy.toml`) is a file only the lint check asks for. Cargo still insists that every member has its
# declared target files, and nextest's filters see only the targets Cargo finds, so `postPatch` creates an empty file
# at each target path the source leaves out. The stubs are made at build time, not in a source derivation, so crane
# reads the source without import-from-derivation.
{ lib }:
let
  inherit (lib) fileset;
  root = ../.;
  cratesDir = ../crates;
  workspace = lib.importTOML ../Cargo.toml;
  # Workspace members the manifest excludes, such as fuzz crates; no derivation builds them.
  excluded = fileset.unions (
    map (path: fileset.maybeMissing (root + "/${path}")) workspace.workspace.exclude
  );
  members = lib.attrNames (
    lib.filterAttrs (
      name: type: type == "directory" && builtins.pathExists (cratesDir + "/${name}/Cargo.toml")
    ) (builtins.readDir cratesDir)
  );
  dir = name: cratesDir + "/${name}";
  manifest = name: lib.importTOML (dir name + "/Cargo.toml");
  # Test-only sources: files compiled only under `cfg(test)`. By name, `tests.rs`, `*_tests.rs` and any `tests` or
  # `*_tests` directory under a member's `src`; the rest are listed in `crates/test-only.txt`, which the journal's
  # snapshot fingerprint (`crates/togi-journal/build/fingerprint.rs`) reads by the same rule.
  testNamed =
    path:
    lib.concatLists (
      lib.mapAttrsToList (
        entry: type:
        if type == "directory" then
          if entry == "tests" || lib.hasSuffix "_tests" entry then
            [ (path + "/${entry}") ]
          else
            testNamed (path + "/${entry}")
        else if type == "regular" && (entry == "tests.rs" || lib.hasSuffix "_tests.rs" entry) then
          [ (path + "/${entry}") ]
        else
          [ ]
      ) (builtins.readDir path)
    );
  testListed = lib.filter (line: line != "" && !lib.hasPrefix "#" line) (
    map lib.trim (lib.splitString "\n" (builtins.readFile ../crates/test-only.txt))
  );
  # A listed path that does not exist fails evaluation.
  testOnly =
    crate:
    fileset.unions (
      lib.optionals (builtins.pathExists (dir crate + "/src")) (testNamed (dir crate + "/src"))
      ++ map (line: cratesDir + "/${line}") (lib.filter (line: lib.hasPrefix "${crate}/" line) testListed)
    );

  rustIn = path: fileset.intersection (fileset.fileFilter (file: file.hasExt "rs") cratesDir) path;

  # A member's production `.rs` files under `src`.
  productionSrc = crate: fileset.difference (rustIn (dir crate + "/src")) (testOnly crate);

  # A member's `.rs` files under `src` and `benches` without its test-only files.
  productionRs =
    crate:
    fileset.difference (rustIn (
      fileset.unions (
        map (sub: fileset.maybeMissing (dir crate + "/${sub}")) [
          "src"
          "benches"
        ]
      )
    )) (testOnly crate);

  # Files outside its directory that a crate reads when it compiles. The journal's build script hashes the
  # production source of the reducer crates, its test-only list and its fingerprint module.
  compileInputs = {
    togi-version = [ ../version.txt ];
    togi-journal = [
      ../version.txt
      ../crates/test-only.txt
      ../crates/togi-journal/build.rs
      ../crates/togi-journal/build
    ]
    ++
      lib.concatMap
        (name: [
          (productionSrc name)
          (dir name + "/Cargo.toml")
        ])
        [
          "togi-machine"
          "togi-journal"
          "togi-config"
          "togi-requests"
          "togi-tuner"
          "togi-defect"
          "togi-render"
          "togi-watch"
          "togi-session"
        ];
  };

  memberDeps =
    tables: name:
    let
      toml = manifest name;
      sections =
        map (table: toml.${table} or { }) tables
        ++ lib.concatMap (target: map (table: target.${table} or { }) tables) (
          lib.attrValues (toml.target or { })
        );
    in
    lib.filter (dep: lib.elem dep members) (lib.concatMap lib.attrNames sections);

  # The members `roots` need to compile, through normal and build dependencies.
  closure =
    roots:
    map (item: item.key) (
      builtins.genericClosure {
        startSet = map (key: { inherit key; }) roots;
        operator =
          { key }:
          map (dep: { key = dep; }) (
            memberDeps [
              "dependencies"
              "build-dependencies"
            ] key
          );
      }
    );

  # `roots` with the members their tests and benches also need, which the dev dependencies add.
  closureWithDev =
    roots: closure (roots ++ lib.concatMap (memberDeps [ "dev-dependencies" ]) (closure roots));

  # The paths of a member's library and of every target Cargo declares or discovers. Stubbing the discovered ones
  # too keeps `cargo metadata` of a narrowed source equal to the workspace's, so a nextest filter naming a test
  # binary of a member left out still matches it.
  targetPaths =
    name:
    let
      toml = manifest name;
      declared =
        table: default: map (target: target.path or (default target.name)) (toml.${table} or [ ]);
      discovered =
        flag: sub:
        let
          path = dir name + "/${sub}";
          entries = if builtins.pathExists path then builtins.readDir path else { };
          target =
            entry: type:
            if type == "regular" && lib.hasSuffix ".rs" entry then
              [ "${sub}/${entry}" ]
            else if type == "directory" && builtins.pathExists (path + "/${entry}/main.rs") then
              [ "${sub}/${entry}/main.rs" ]
            else
              [ ];
        in
        lib.optionals (toml.package.${flag} or true) (lib.concatLists (lib.mapAttrsToList target entries));
      mainBin = lib.optional (
        (toml.package.autobins or true) && builtins.pathExists (dir name + "/src/main.rs")
      ) "src/main.rs";
    in
    lib.unique (
      [ (toml.lib.path or "src/lib.rs") ]
      ++ declared "bin" (
        target: if target == toml.package.name then "src/main.rs" else "src/bin/${target}.rs"
      )
      ++ declared "bench" (target: "benches/${target}.rs")
      ++ declared "test" (target: "tests/${target}.rs")
      ++ declared "example" (target: "examples/${target}.rs")
      ++ mainBin
      ++ discovered "autobins" "src/bin"
      ++ discovered "autobenches" "benches"
      ++ discovered "autotests" "tests"
      ++ discovered "autoexamples" "examples"
    );

  # `code`: members built but not tested, as their production `.rs` files under `src` and `benches`, without the
  # test-only files.
  # `rs`: members as all their `.rs` files, tests included, for tools that read code and no data.
  # `full`: members as their whole directory, test data included.
  # `files`: other files, such as fixtures a test reads. Every target path a member's selection leaves out is
  # stubbed: all of them for members in none of the three, the tests and examples of `code` members.
  source =
    {
      code ? [ ],
      rs ? [ ],
      full ? [ ],
      files ? [ ],
    }:
    let
      stubs = lib.concatMap (crate: map (path: "crates/${crate}/${path}") (targetPaths crate)) (
        lib.subtractLists (rs ++ full) members
      );
      included = lib.unique (code ++ rs ++ full);
    in
    {
      src = fileset.toSource {
        inherit root;
        fileset = fileset.difference (fileset.unions (
          [
            ../Cargo.toml
            ../Cargo.lock
            ../rust-toolchain.toml
          ]
          ++ map (crate: dir crate + "/Cargo.toml") members
          ++ map productionRs code
          ++ map (crate: rustIn (dir crate)) rs
          ++ map dir full
          ++ lib.concatMap (crate: compileInputs.${crate} or [ ]) included
          ++ files
        )) excluded;
      };
      postPatch = ''
        for stub in ${lib.escapeShellArgs stubs}; do
          if [[ ! -e "$stub" ]]; then
            mkdir -p "$(dirname "$stub")"
            touch "$stub"
          fi
        done
      '';
    };
in
{
  inherit
    closure
    closureWithDev
    members
    source
    ;
}
