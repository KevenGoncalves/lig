self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.lig;
  toml = pkgs.formats.toml { };
  lig = lib.getExe cfg.package;
  # The markers `lig install` uses, so `lig doctor` sees the integration.
  block = part: line: ''
    # >>> lig ${part} >>>
    ${line}
    # <<< lig ${part} <<<
  '';
  posix = shell: part: block part ''eval "$(${lig} init ${shell} ${part})"'';
  fish = part: block part "eval (${lig} init fish ${part} | string collect)";
  # nu can't eval a command's output: the code is written to files when the config is built.
  nu = part: block part "source ${pkgs.runCommand "lig-${part}.nu" { } "${lig} init nu ${part} > $out"}";
in
{
  options.programs.lig = {
    enable = lib.mkEnableOption "lig, IDE-style autocomplete for the terminal";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      description = "The lig package to use.";
    };

    settings = lib.mkOption {
      inherit (toml) type;
      default = { };
      description = "Written to {file}`$XDG_CONFIG_HOME/lig/config.toml`. `lig config` lists every option.";
    };

    enableBashIntegration = lib.hm.shell.mkBashIntegrationOption { inherit config; };
    enableFishIntegration = lib.hm.shell.mkFishIntegrationOption { inherit config; };
    enableNushellIntegration = lib.hm.shell.mkNushellIntegrationOption { inherit config; };
    enableZshIntegration = lib.hm.shell.mkZshIntegrationOption { inherit config; };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];

    xdg.configFile."lig/config.toml" = lib.mkIf (cfg.settings != { }) {
      source = toml.generate "lig-config.toml" cfg.settings;
    };

    # `pre` re-runs the shell inside lig, so it goes first; `post` goes last.
    programs.zsh.initContent = lib.mkIf cfg.enableZshIntegration (
      lib.mkMerge [
        (lib.mkBefore (posix "zsh" "pre"))
        (lib.mkAfter (posix "zsh" "post"))
      ]
    );
    programs.bash.bashrcExtra = lib.mkIf cfg.enableBashIntegration (lib.mkBefore (posix "bash" "pre"));
    programs.bash.initExtra = lib.mkIf cfg.enableBashIntegration (lib.mkAfter (posix "bash" "post"));
    programs.fish.shellInit = lib.mkIf cfg.enableFishIntegration (lib.mkBefore (fish "pre"));
    programs.fish.shellInitLast = lib.mkIf cfg.enableFishIntegration (lib.mkAfter (fish "post"));
    programs.nushell.extraConfig = lib.mkIf cfg.enableNushellIntegration (
      lib.mkMerge [
        (lib.mkBefore (nu "pre"))
        (lib.mkAfter (nu "post"))
      ]
    );
  };
}
