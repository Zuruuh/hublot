{
  lib,
  rustPlatform,
  stdenv,
  libiconv,
}:

let
  cargoToml = lib.importTOML ../Cargo.toml;
in
rustPlatform.buildRustPackage {
  pname = "hublot";
  version = cargoToml.package.version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../src
      ../templates
      ../static
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;

  buildInputs = lib.optional stdenv.hostPlatform.isDarwin libiconv;

  doCheck = true;

  meta = with lib; {
    description = cargoToml.package.description;
    license = licenses.mit;
    mainProgram = "hublot";
    platforms = platforms.linux ++ platforms.darwin;
  };
}
