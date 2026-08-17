# herdr-prevtab

A plugin for [Herdr](https://herdr.dev/) that allows you to jump back
to ther previously focused tab, similar to `select-window -t !` in tmux.

## Features
- **Jump back**: Exposes a `jump_back` action that instantly focuses your last used tab,
    allowing quick toggling between two tabs.

## Requirements
- Herdr v0.7.0 or higher
- Linux or macOS
- Cargo (when not installing with Nix)

## Installation
```bash
herdr plugin install joo-was-already-taken/herdr-prevtab
```

### Installation with Nix
Derivation is already provided in `package.nix`.
In order to install you can create an activation script,
for example if using flakes and Home Manager:
```nix
{ lib, pkgs, inputs, ... }:
{
  home.activation.linkHerdrPlugins = let
    herdrPrevtab = inputs.herdr-prevtab.packages.${pkgs.stdenv.hostPlatform.system}.default;
  in lib.hm.dag.entryAfter [ "writeBoundary" ] ''
    $DRY_RUN_CMD herdr plugin link ${herdrPrevtab} > /dev/null
  '';
}
```

## Usage
Example configuration:
```toml
[[keys.command]]
key = "prefix+b"
type = "plugin_action"
command = "prevtab.jump_back"
```
