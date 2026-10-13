# Narrowed Cargo sources: what a derivation compiles and reads, nothing else, so an edit elsewhere in the workspace
# leaves its derivation hash alone.
#
# A source keeps the workspace manifests and `Cargo.lock`, so Cargo resolves the workspace and `--locked` holds, and
# the files of the members the derivation builds. Cargo still insists that every member has its target files, so
# `postPatch` creates an empty file at each target path of the members left out. The stubs are made at build time,
# not in a source derivation, so crane reads the source without import-from-derivation.
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
    lib.filterAttrs (name: type: type == "directory" && builtins.pathExists (cratesDir + "/${name}/Cargo.toml")) (
      builtins.readDir cratesDir
    )
  );
  dir = name: cratesDir + "/${name}";
  manifest = name: lib.importTOML (dir name + "/Cargo.toml");
  # Files outside its directory that a crate reads when it compiles.
  compileInputs = {
    togi-version = [ ../version.txt ];
  };

  memberDeps =
    tables: name:
    let
      toml = manifest name;
      sections =
        map (table: toml.${table} or { }) tables
        ++ lib.concatMap (target: map (table: target.${table} or { }) tables) (lib.attrValues (toml.target or { }));
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

  # The paths Cargo requires to exist for a member to load: its library and every declared target.
  targetPaths =
    name:
    let
      toml = manifest name;
      declared = table: default: map (target: target.path or (default target.name)) (toml.${table} or [ ]);
    in
    lib.unique (
      [ (toml.lib.path or "src/lib.rs") ]
      ++ declared "bin" (target: if target == toml.package.name then "src/main.rs" else "src/bin/${target}.rs")
      ++ declared "bench" (target: "benches/${target}.rs")
      ++ declared "test" (target: "tests/${target}.rs")
      ++ declared "example" (target: "examples/${target}.rs")
    );

  rustIn = path: fileset.intersection (fileset.fileFilter (file: file.hasExt "rs") cratesDir) path;

  # `code`: members built but not tested, as their `.rs` files under `src` and `benches`.
  # `rs`: members as all their `.rs` files, tests included, for tools that read code and no data.
  # `full`: members as their whole directory, test data included.
  # `files`: other files, such as fixtures a test reads. Members in none of the three are stubbed.
  source =
    {
      code ? [ ],
      rs ? [ ],
      full ? [ ],
      files ? [ ],
    }:
    let
      included = lib.unique (code ++ rs ++ full);
      stubs = lib.concatMap (crate: map (path: "crates/${crate}/${path}") (targetPaths crate)) (
        lib.subtractLists included members
      );
    in
    {
      src = fileset.toSource {
        inherit root;
        fileset = fileset.difference (fileset.unions (
          [
            ../Cargo.toml
            ../Cargo.lock
            ../rust-toolchain.toml
            ../clippy.toml
          ]
          ++ map (crate: dir crate + "/Cargo.toml") members
          ++ map (
            crate: rustIn (fileset.unions (map (sub: fileset.maybeMissing (dir crate + "/${sub}")) [ "src" "benches" ]))
          ) code
          ++ map (crate: rustIn (dir crate)) rs
          ++ map dir full
          ++ lib.concatMap (crate: compileInputs.${crate} or [ ]) included
          ++ files
        )) excluded;
      };
      postPatch = ''
        for stub in ${lib.escapeShellArgs stubs}; do
          mkdir -p "$(dirname "$stub")"
          touch "$stub"
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
