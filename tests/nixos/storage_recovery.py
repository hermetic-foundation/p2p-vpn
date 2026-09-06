def assert_membership_storage_recovery(machine, instance, dns_name):
    import shlex

    service = f"p2p-vpn-{instance}.service"
    state = f"/var/lib/p2p-vpn/{instance}/membership-state.json"
    machine.wait_for_file(state)
    digest = machine.succeed(f"sha256sum {state} | cut -d' ' -f1").strip()
    machine.succeed(f"systemctl stop {service}")
    restarts = int(machine.succeed(f"systemctl show {service} -p NRestarts --value"))
    cursor = machine.succeed(
        f"journalctl -u {service} -n 0 --show-cursor | sed -n 's/^-- cursor: //p'"
    ).strip()
    assert cursor, "journal cursor must identify this failure attempt"

    machine.succeed(f"chmod 0644 {state}")
    machine.succeed(f"systemctl start --no-block {service}")
    machine.wait_until_succeeds(
        f"journalctl -u {service} --after-cursor={shlex.quote(cursor)} "
        "| grep -F membership_state_load_failed",
        timeout=30,
    )
    machine.wait_until_succeeds(
        f"test $(systemctl show {service} -p NRestarts --value) -gt {restarts}",
        timeout=30,
    )

    # Repair only the fixture; systemd must recover without another start command.
    machine.succeed(f"chmod 0600 {state}")
    machine.wait_until_succeeds(f"systemctl is-active --quiet {service}", timeout=60)
    machine.wait_until_succeeds(
        f"systemctl is-active --quiet p2p-vpn-{instance}-resolved.service", timeout=60
    )
    machine.succeed("resolvectl flush-caches")
    machine.wait_until_succeeds(
        f"resolvectl query {dns_name} | grep -Eq '100\\.64\\.|10\\.50\\.|fd00:'",
        timeout=60,
    )
    machine.succeed(f"systemctl is-active --quiet {service}")
    pid = machine.succeed(f"systemctl show {service} -p MainPID --value").strip()
    assert int(pid) > 0
    machine.succeed(
        f"sleep 6; test $(systemctl show {service} -p MainPID --value) = {pid}"
    )
    machine.succeed(f"test $(sha256sum {state} | cut -d' ' -f1) = {digest}")
    machine.succeed(f"test $(stat -c %a {state}) = 600")
