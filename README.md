# herdr-prevtab

A plugin for [Herdr](https://herdr.dev/) that allows you to jump back
to the previously focused tab (similar to `select-window -t !` in tmux) or workspace.

## Features
- **Jump back**: Exposes a `jump_back` action that instantly focuses your last used tab,
    allowing quick toggling between two tabs.
- **Workspace jump back** (new in *0.2.0*): Exposes a `workspace_jump_back` action that focuses your last used workspace.

## Requirements
- Herdr v0.7.0 - v0.8.x (broken on v0.9.0 due to regression - [GitHub Issue](https://github.com/herdrdev/herdr/issues/3801))
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

[[keys.command]]
key = "prefix+shift+b"
type = "plugin_action"
command = "prevtab.workspace_jump_back"
```
