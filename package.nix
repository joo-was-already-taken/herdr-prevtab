{ lib
, rustPlatform
}:

let
  cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
in
rustPlatform.buildRustPackage {
  pname = "herdr-prevtab";
  version = cargoToml.package.version;
  src = ./.;

  cargoLock.lockFile = ./Cargo.lock;

  postInstall = ''
    sed -i '/^\[\[build\]\]/,/^command = /d' herdr-plugin.toml
    substituteInPlace herdr-plugin.toml \
      --replace-fail 'target/release/herdr-prevtab' "$out/bin/herdr-prevtab"
    cp herdr-plugin.toml $out/
  '';

  meta = {
    description = cargoToml.package.description;
    homepage = "https://github.com/joo-was-already-taken/herdr-prevtab";
    license = lib.licenses.mit;
  };
}
