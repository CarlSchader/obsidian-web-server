{ self }:
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.obsidian-web-server;

  # Mirror the classifier in src/main.rs::classify_vault_arg.
  #
  # SSH if:
  #   - starts with `ssh://`, OR
  #   - matches scp-style `[user@]host:path` where the part before the first
  #     colon contains no `/` or `\` and looks like `[user@]host` made of
  #     letters/digits/`.`/`-`/`_`, with at most one `@`, and `path` is non-empty.
  isHttp = v: lib.hasPrefix "http://" v || lib.hasPrefix "https://" v;

  isSshScheme = v: lib.hasPrefix "ssh://" v;

  # Returns true if `v` looks like scp-style `[user@]host:path`.
  # Path must not start with `/` so we don't match `https://...`.
  isScpStyle =
    v:
    let
      m = builtins.match "([A-Za-z0-9._-]+@)?([A-Za-z0-9._-]+):([^/].*)" v;
    in
    m != null;

  isSsh = v: !(isHttp v) && (isSshScheme v || isScpStyle v);

  vaultIsSsh = isSsh cfg.vault;
in
{
  options.services.obsidian-web-server = {
    enable = lib.mkEnableOption "Obsidian vault HTTP editor";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "obsidian-web-server.packages.\${pkgs.stdenv.hostPlatform.system}.default";
      description = "The obsidian-web-server package to run.";
    };

    vault = lib.mkOption {
      type = lib.types.str;
      example = "git@github.com:me/notes.git";
      description = ''
        Vault to serve. Either a local path to an existing git repository, or
        an SSH URL (`ssh://...` or scp-style `user@host:path`) to clone into
        the service's cache directory on startup. HTTPS URLs are not
        supported.
      '';
    };

    gitUserName = lib.mkOption {
      type = lib.types.str;
      example = "obsidian-bot";
      description = "Author/committer name used for commits made by the server.";
    };

    gitUserEmail = lib.mkOption {
      type = lib.types.str;
      example = "obsidian-bot@example.com";
      description = "Author/committer email used for commits made by the server.";
    };

    identityFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      example = "/run/secrets/obsidian-deploy-key";
      description = ''
        Path to an unencrypted SSH private key. Required when {option}`vault`
        is an SSH URL, and rejected otherwise.

        The file is loaded into the service via systemd's
        {manpage}`LoadCredential(5)` so it does not need to be readable by the
        (dynamic) service user, and it is not copied into the world-readable
        Nix store. Point this at a path materialised by sops-nix, agenix, or a
        plain root-owned file.

        The key must be unencrypted: ssh runs in `BatchMode=yes`, so any
        passphrase prompt will fail immediately.
      '';
    };

    host = lib.mkOption {
      type = lib.types.str;
      default = "127.0.0.1";
      example = "0.0.0.0";
      description = ''
        Address to bind the HTTP server to. Defaults to loopback because the
        server has no built-in authentication; expose it deliberately (e.g.
        behind a reverse proxy that adds auth) by setting this to `0.0.0.0`.
      '';
    };

    port = lib.mkOption {
      type = lib.types.port;
      default = 8080;
      description = "TCP port to bind the HTTP server to.";
    };

    openFirewall = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Whether to open {option}`port` in the host firewall.";
    };

    environment = lib.mkOption {
      type = lib.types.attrsOf lib.types.str;
      default = { };
      example = {
        RUST_LOG = "info,obsidian_web_server=debug";
      };
      description = "Extra environment variables for the systemd unit.";
    };

    extraArgs = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      description = "Additional CLI arguments appended to the obsidian-web-server invocation.";
    };

    stateDirectory = lib.mkOption {
      type = lib.types.str;
      default = "obsidian-web-server";
      description = ''
        Name (relative to `/var/lib` and `/var/cache`) of the systemd
        {option}`StateDirectory` and {option}`CacheDirectory` for this
        service. The state directory is used as `$HOME` (so ssh can persist
        `~/.ssh/known_hosts`); the cache directory is used as
        `$XDG_CACHE_HOME` for SSH-mode clones.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = !(isHttp cfg.vault);
        message = ''
          services.obsidian-web-server.vault: HTTPS git URLs are not supported;
          use an SSH URL (ssh://... or user@host:path) instead.
        '';
      }
      {
        assertion = vaultIsSsh -> cfg.identityFile != null;
        message = ''
          services.obsidian-web-server.identityFile must be set when `vault` is
          an SSH URL (got: ${cfg.vault}).
        '';
      }
      {
        assertion = (!vaultIsSsh) -> cfg.identityFile == null;
        message = ''
          services.obsidian-web-server.identityFile is only valid when `vault`
          is an SSH URL; got local path ${cfg.vault}.
        '';
      }
      {
        assertion = cfg.gitUserName != "";
        message = "services.obsidian-web-server.gitUserName must not be empty.";
      }
      {
        assertion = cfg.gitUserEmail != "";
        message = "services.obsidian-web-server.gitUserEmail must not be empty.";
      }
    ];

    networking.firewall.allowedTCPPorts = lib.mkIf cfg.openFirewall [ cfg.port ];

    systemd.services.obsidian-web-server = {
      description = "Obsidian vault HTTP editor";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];

      # The binary shells out to `git`, and SSH mode invokes `ssh` via
      # GIT_SSH_COMMAND. Both must be on PATH.
      path = [
        pkgs.git
        pkgs.openssh
      ];

      environment = {
        XDG_CACHE_HOME = "/var/cache/${cfg.stateDirectory}";
        HOME = "/var/lib/${cfg.stateDirectory}";
      }
      // cfg.environment;

      serviceConfig =
        let
          identityArgs = lib.optionals (cfg.identityFile != null) [
            "-i"
            "%d/identity"
          ];
          argv = [
            cfg.vault
            "-n"
            cfg.gitUserName
            "-e"
            cfg.gitUserEmail
            "--host"
            cfg.host
            "--port"
            (toString cfg.port)
          ]
          ++ identityArgs
          ++ cfg.extraArgs;
        in
        {
          ExecStart = "${cfg.package}/bin/obsidian-web-server ${lib.escapeShellArgs argv}";

          DynamicUser = true;
          StateDirectory = cfg.stateDirectory;
          CacheDirectory = cfg.stateDirectory;

          # Deliver the SSH key as a credential so it lands at %d/identity
          # (mode 0400) inside the sandbox without needing to be readable by
          # the dynamic service user.
          LoadCredential = lib.optional (cfg.identityFile != null) "identity:${cfg.identityFile}";

          Restart = "on-failure";
          RestartSec = "5s";

          # Hardening.
          NoNewPrivileges = true;
          PrivateTmp = true;
          PrivateDevices = true;
          ProtectSystem = "strict";
          ProtectHome = true;
          ProtectKernelTunables = true;
          ProtectKernelModules = true;
          ProtectControlGroups = true;
          ProtectClock = true;
          ProtectHostname = true;
          ProtectKernelLogs = true;
          ProtectProc = "invisible";
          ProcSubset = "pid";
          RestrictAddressFamilies = [
            "AF_UNIX"
            "AF_INET"
            "AF_INET6"
          ];
          RestrictNamespaces = true;
          RestrictRealtime = true;
          RestrictSUIDSGID = true;
          LockPersonality = true;
          MemoryDenyWriteExecute = true;
          SystemCallArchitectures = "native";
          SystemCallFilter = [
            "@system-service"
            "~@privileged"
            "~@resources"
          ];
          UMask = "0077";
        };
    };
  };
}
