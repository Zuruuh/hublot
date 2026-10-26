self:
{
  config,
  lib,
  pkgs,
  ...
}:

let
  inherit (lib)
    mkEnableOption
    mkIf
    mkOption
    types
    literalExpression
    ;

  cfg = config.services.hublot;
  stateDir = "/var/lib/hublot";
  defaultOutputDir = "${stateDir}/site";

  registryType = types.submodule {
    options = {
      username = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Registry username. Omit for anonymous access.";
      };

      password = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Registry password or token. Prefer passwordFile: literal values
          end up in the Nix store.
        '';
      };

      passwordFile = mkOption {
        type = types.nullOr types.path;
        default = null;
        description = ''
          Path to a file containing the password or token. The file is read
          when the check runs, so the secret does not enter the Nix store.
          It must be readable by the hublot service user.
        '';
      };
    };
  };

  generatedConfig = (pkgs.formats.toml { }).generate "hublot-config.toml" {
    images_file = toString cfg.imagesFile;
    database = "${stateDir}/hublot.db";
    output_dir = toString cfg.outputDir;
    check = {
      concurrency = cfg.concurrency;
      timeout_secs = cfg.timeoutSecs;
    };
    registries = lib.mapAttrs (
      host: registry:
      lib.filterAttrs (_: value: value != null) {
        username = registry.username;
        password = registry.password;
        password_file = if registry.passwordFile != null then toString registry.passwordFile else null;
      }
    ) cfg.registries;
  };

  outputDirOutsideState = !(lib.hasPrefix "${stateDir}/" (toString cfg.outputDir));

  commonService = {
    User = cfg.user;
    Group = cfg.group;
    StateDirectory = "hublot";
    StateDirectoryMode = "0750";
    UMask = "0027";
    Environment = [ "RUST_LOG=${cfg.logLevel}" ];

    NoNewPrivileges = true;
    PrivateTmp = true;
    PrivateDevices = true;
    ProtectHome = true;
    ProtectSystem = "strict";
    ProtectKernelTunables = true;
    ProtectKernelModules = true;
    ProtectControlGroups = true;
    ProtectClock = true;
    RestrictAddressFamilies = [
      "AF_INET"
      "AF_INET6"
      "AF_UNIX"
    ];
    RestrictNamespaces = true;
    RestrictRealtime = true;
    LockPersonality = true;
    MemoryDenyWriteExecute = true;
    SystemCallArchitectures = "native";
    CapabilityBoundingSet = [ "" ];
    AmbientCapabilities = [ ];
  };
in
{
  options.services.hublot = {
    enable = mkEnableOption "hublot Docker image update checker";

    package = mkOption {
      type = types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = literalExpression "inputs.hublot.packages.${pkgs.system}.default";
      description = "hublot package to use.";
    };

    imagesFile = mkOption {
      type = types.path;
      default = "/etc/hublot/images.txt";
      description = ''
        Line-delimited file of Docker image references. Blank lines and
        comments are ignored.
      '';
    };

    outputDir = mkOption {
      type = types.path;
      default = defaultOutputDir;
      description = ''
        Directory where the static site is written. Defaults to a directory
        under the hublot state directory.
      '';
    };

    concurrency = mkOption {
      type = types.ints.positive;
      default = 4;
      description = "Maximum number of images checked concurrently.";
    };

    timeoutSecs = mkOption {
      type = types.ints.positive;
      default = 30;
      description = "Per-request timeout in seconds.";
    };

    registries = mkOption {
      type = types.attrsOf registryType;
      default = { };
      description = "Per-registry credentials, keyed by registry host.";
    };

    user = mkOption {
      type = types.str;
      default = "hublot";
      description = "User that runs the check.";
    };

    group = mkOption {
      type = types.str;
      default = "caddy";
      description = ''
        Primary group for the check service and generated files. The default
        lets Caddy read the static site. Override if you serve it differently.
      '';
    };

    logLevel = mkOption {
      type = types.str;
      default = "info";
      description = "RUST_LOG filter for the check service.";
    };

    checkTimer = {
      enable = mkOption {
        type = types.bool;
        default = true;
        description = "Enable the systemd timer that runs hublot check.";
      };

      onCalendar = mkOption {
        type = types.str;
        default = "daily";
        description = ''
          Default OnCalendar value for the check timer. You can override this
          option or set the timer's systemd configuration directly.
        '';
      };
    };
  };

  config = mkIf cfg.enable {
    assertions = lib.concatLists (
      lib.mapAttrsToList (host: registry: [
        {
          assertion = registry.password == null || registry.passwordFile == null;
          message = "services.hublot.registries.${host}: set only one of password or passwordFile";
        }
        {
          assertion =
            (registry.password == null && registry.passwordFile == null) || registry.username != null;
          message = "services.hublot.registries.${host}: username is required when a password is set";
        }
      ]) cfg.registries
    );

    users.groups.${cfg.group} = { };

    users.users = mkIf (cfg.user == "hublot") {
      hublot = {
        isSystemUser = true;
        group = cfg.group;
      };
    };

    environment.etc."hublot/config.toml".source = generatedConfig;

    systemd.tmpfiles.rules = lib.optional outputDirOutsideState "d ${toString cfg.outputDir} 0750 ${cfg.user} ${cfg.group} -";

    systemd.services.hublot-check = {
      description = "hublot Docker image update check";

      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];

      serviceConfig = commonService // {
        Type = "oneshot";
        ExecStart = "${cfg.package}/bin/hublot --config /etc/hublot/config.toml check";
        ReadWritePaths = lib.optional outputDirOutsideState (toString cfg.outputDir);
      };
    };

    systemd.timers.hublot-check = mkIf cfg.checkTimer.enable {
      description = "hublot Docker image update check timer";
      wantedBy = [ "timers.target" ];

      timerConfig = {
        OnCalendar = lib.mkDefault cfg.checkTimer.onCalendar;
        Persistent = true;
        RandomizedDelaySec = "5m";
      };
    };
  };
}
