{ pkgs, self }:

# NixOS VM test: confirms the module works when the vault directory is reached
# through a symlink (the same shape systemd's CacheDirectory creates under
# DynamicUser, where /var/cache/<name> is a symlink to /var/cache/private/<name>).
#
# Regression test for "vault error: path is outside the vault" caused by
# Vault::resolve canonicalizing request paths through the symlink while the
# Vault root was held in non-canonical form.
pkgs.testers.runNixOSTest {
  name = "obsidian-web-server-symlinked-vault";

  nodes.machine =
    { pkgs, ... }:
    {
      imports = [ self.nixosModules.default ];

      users.users.notes = {
        isSystemUser = true;
        group = "notes";
        home = "/var/lib/notes-real";
        createHome = false;
      };
      users.groups.notes = { };

      # Pre-seed the "real" vault on disk and expose it through a symlink.
      # `tmpfiles` runs before the service starts, so by the time the unit
      # is activated /var/lib/notes resolves through the symlink.
      systemd.tmpfiles.rules = [
        "d /var/lib/notes-real 0755 notes notes -"
        "L+ /var/lib/notes - - - - /var/lib/notes-real"
      ];

      systemd.services.seed-vault = {
        description = "Seed the test vault with a git repo";
        wantedBy = [ "multi-user.target" ];
        before = [ "obsidian-web-server.service" ];
        path = [ pkgs.git ];
        serviceConfig = {
          Type = "oneshot";
          RemainAfterExit = true;
          User = "notes";
          Group = "notes";
        };
        script = ''
          set -eu
          cd /var/lib/notes-real
          if [ ! -d .git ]; then
            git init -q -b main
            git config user.email bot@example.com
            git config user.name bot
            printf 'hello from the vault\n' > fiction.md
            git add fiction.md
            git commit -q -m "seed"
          fi
        '';
      };

      services.obsidian-web-server = {
        enable = true;
        # Path goes through the symlink. This is the bug reproducer.
        vault = "/var/lib/notes";
        gitUserName = "bot";
        gitUserEmail = "bot@example.com";
        user = "notes";
        group = "notes";
      };

      environment.systemPackages = [
        pkgs.curl
        pkgs.jq
        pkgs.git
      ];
    };

  testScript = ''
    machine.start()
    machine.wait_for_unit("seed-vault.service")
    machine.wait_for_unit("obsidian-web-server.service")
    machine.wait_for_open_port(8080)

    # The actual regression assertion: GET a file by its relative path.
    # Pre-fix, this returned a 4xx with "path is outside the vault: fiction.md".
    out = machine.succeed(
        "curl -fsS 'http://127.0.0.1:8080/api/file?path=fiction.md'"
    )
    print("api/file response:", out)
    assert '"path":"fiction.md"' in out, f"unexpected response: {out}"
    assert "hello from the vault" in out, f"file contents missing: {out}"

    # Tree endpoint should also list it.
    tree = machine.succeed("curl -fsS http://127.0.0.1:8080/api/tree")
    print("api/tree response:", tree)
    assert "fiction.md" in tree, f"fiction.md missing from tree: {tree}"

    # Mutation: write a note and confirm a commit lands in the underlying repo.
    machine.succeed(
        "curl -fsS -X PUT -H 'content-type: application/json' "
        "-d '{\"path\":\"fiction.md\",\"content\":\"edited via api\"}' "
        "http://127.0.0.1:8080/api/file"
    )
    git_log = machine.succeed("sudo -u notes git -C /var/lib/notes-real log --oneline")
    print("git log:", git_log)
    assert git_log.count("\n") >= 2, f"expected a new commit, got: {git_log}"
  '';
}
